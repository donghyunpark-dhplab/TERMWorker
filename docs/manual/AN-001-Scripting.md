# AN-001: Automating Board Bring-up with TermWorker Scripts

**Application Note · TermWorker 0.3.0 · DRAFT · November 2026**
DHPLab Consulting · dhplab.com

> This is a draft. The script API is new and may still be extended before release.

---

## Summary

Board bring-up and device testing involve the same console steps again and again:
- stop the bootloader,
- set the network,
- load an image,
- read back a version,
- reboot and watch the device come back.

TermWorker scripts automate these steps in JavaScript, inside the terminal you already use. There is nothing to install: the script engine is built into TermWorker.

This note covers:
- how scripts see the console, and how waiting and timeouts work;
- six worked examples: U-Boot over TFTP, a modem inventory, a reboot soak test across serial and SSH, an HDLC request/response, a binary protocol handled byte by byte, and a production PASS/FAIL station;
- practices that keep scripts reliable on real hardware.

A complete function list is in the appendix.

---

## 1. Running a script

1. Open the tab of the device, for example the board's serial console.
2. Click **Script** in the status bar, then **Run script…**, and choose the `.js` file.
3. Watch the **Script output** panel. **Stop** ends the script at any time.

A script runs from top to bottom and ends when its last line has run. You can use `await` directly at the top level. Several scripts may run at once. Each runs separately, so a script stuck in a loop does not freeze TermWorker.

**Editor support.** Copy `termworker.d.ts` from the examples folder next to your scripts. Then start each script with:

```js
/// <reference path="termworker.d.ts" />
```

VS Code then completes function names and shows mistakes as you type.

---

## 2. How a script sees the console

### 2.1 Sessions

`tab` is the session of the tab the script was started from. It is `null` if no tab was open. A script can open further sessions; each appears as a new tab, exactly as if you had opened it by hand:

```js
const con = await sessions.open("Board console");     // a saved session
const dut = await sessions.open("COM7 115200");        // quick-connect text
const ssh = await sessions.open({ type: "ssh", host: "192.168.1.1", user: "root",
                                  password: await ui.password("root password") });
```

`sessions.open` returns once the session is connected. If it cannot connect, it throws an error.

### 2.2 The receive buffer and matching

From the moment a script starts watching a session, everything the session receives is collected in a buffer. This is the same text the terminal shows. A wait such as `waitFor` searches this buffer:

- If the pattern is already in the buffer, the wait returns at once. Output that arrived before the wait is not missed.
- On a match, the buffer is consumed up to the end of the match. The next wait only sees what comes after it.
- A pattern is a string or a regular expression. With a regular expression, `$` means "end of what has arrived so far". That is what you want for prompts: `/=> $/` matches only when the U-Boot prompt is the last thing received.

```js
const m = await s.waitFor(/Bytes transferred = (\d+) /);
print(m.groups[0]);        // the captured number
print(m.before);           // everything received before the match
```

A wait matches as soon as the pattern fits the text received *so far*. Data often arrives in pieces, so a pattern that ends in an open repeat such as `(\d+)` can match a number before all of its digits have arrived: `/Bytes transferred = (\d+)/` may return `1` for `1048576`. End such a pattern with something that follows the value, for example a space or a line end: `/Bytes transferred = (\d+) /`, `/CSQ: (\d+),/`. (`\b` does not help: it also matches at the end of the text received so far.)

Received text includes the device's escape sequences, such as colors. If a prompt is colored, match on part of it, or clean the text with `stripAnsi()` before you examine it.

### 2.3 Timeouts

Every wait has a timeout. The default is 30 seconds. `{ timeout: 0 }` waits forever, which is useful for "until the board is powered on". When a timeout expires, the wait throws a `TimeoutError`. If the script does not catch it, the script stops, and the output panel shows the line and the last output seen:

```
✖ flash.js failed after 31.2 s
TimeoutError: COM3: /=> $/ did not appear within 30 s. Last output: "Starting kernel ...\r\n"
    at <anonymous> (flash.js:9:9)
```

### 2.4 Sending

| Call | Sends |
|---|---|
| `s.send("text")` | Text. `\r` is turned into the session's *Enter key sends* setting |
| `s.sendLine("cmd")` | Text plus Enter |
| `s.sendKeys("^C")` | Control keys exactly: `^C`, `<ESC>`, `<CR>`, `<UP>`, `<F1>`… |
| `s.sendBytes([0x7e, 0x01])` | Raw bytes |
| `await s.sendBreak()` | A 250 ms break (serial) |

### 2.5 command()

`command()` is the workhorse for shell-like consoles. In one call it:
1. discards unread output,
2. sends the line,
3. waits for the prompt,
4. returns the output in between, without the echoed command line.

```js
const env = await s.command("printenv ipaddr", { prompt: /=> $/ });   // "ipaddr=192.168.1.1\n"
```

Set `s.prompt` once to avoid repeating the prompt pattern:

```js
s.prompt = /root@OpenWrt:.*# $/;
const up = await s.command("uptime");
```

---

## 3. Example: U-Boot, TFTP and a version check

The task is to catch the U-Boot countdown, configure the network, load an image into RAM and check the result.

```js
/// <reference path="termworker.d.ts" />
const s = tab;
if (!s) throw new Error("Run this from the board's serial tab");
s.prompt = /=> $/;

ui.status("power the board on");
await s.waitFor("Hit any key", { timeout: 0 });
s.send(" ");
await s.waitFor(s.prompt);

const image = await ui.prompt("Image on the TFTP server", "fw.bin");
if (image === null) throw new Error("cancelled");

ui.status("TFTP");
await s.command("setenv ipaddr 192.168.1.1; setenv serverip 192.168.1.10");
s.sendLine(`tftpboot 0x80000000 ${image}`);
const r = await s.waitForAny(
  [/Bytes transferred = (\d+) /, /TIMEOUT|not found|Retry count exceeded/],
  { timeout: 120000 },
);
if (r.index !== 0) {
  await ui.result("FAIL", r.match);
  throw new Error(`TFTP failed: ${r.match}`);
}
await ui.result("PASS", `${image}: ${r.groups[0]} bytes`);
```

Notes:
- Start the script first, then power the board on. The countdown is caught however short it is.
- `waitForAny` returns whichever pattern appears first. Its `index` tells success from each failure mode.
- To load over the serial line instead of TFTP, replace the TFTP step with YMODEM:

```js
s.sendLine("loady 0x80000000");
await s.waitFor("Ready for binary");
await s.ymodem.send(image);          // returns when the transfer ends, throws if it fails
await s.waitFor(s.prompt);
```

---

## 4. Example: modem inventory over AT commands

The task is to collect identity and signal information from a cellular module into a tab-separated file.

```js
/// <reference path="termworker.d.ts" />
const s = tab ?? (await sessions.open("/dev/ttyUSB2 115200"));
const rows = [];
for (const cmd of ["ATI", "AT+CGSN", "AT+CIMI", "AT+CSQ", "AT+COPS?"]) {
  const out = await s.command(cmd, { prompt: /\r\n(OK|ERROR)\r\n/, timeout: 5000 });
  rows.push(`${cmd}\t${out.trim().replace(/\s+/g, " ")}`);
  print(rows.at(-1));
}
fs.writeText("modem.tsv", rows.join("\n") + "\n");
```

Notes:
- The "prompt" of an AT interface is the final result code. Matching `\r\n(OK|ERROR)\r\n` ends each command cleanly.
- Many modules expect CR+LF for Enter; set this in the session settings.
- Relative file paths, such as `modem.tsv`, are resolved against the script's own folder.

---

## 5. Example: reboot soak test across SSH and serial

The task is to reboot a router many times over SSH while its serial console tells when it is back. One script drives both sessions.

```js
/// <reference path="termworker.d.ts" />
const rounds = Number(await ui.prompt("How many reboots?", "100"));
const con = await sessions.open("Board console");
const password = await ui.password("root password of the board");

for (let i = 1; i <= rounds; i++) {
  ui.status(`round ${i} of ${rounds}`);
  const ssh = await sessions.open({ type: "ssh", host: "192.168.1.1", user: "root", password });
  ssh.sendLine("reboot");
  await ssh.close();

  const t0 = Date.now();
  con.discard();                                   // ignore what the console said before
  await con.waitFor("login:", { timeout: 180000 });
  const secs = ((Date.now() - t0) / 1000).toFixed(1);
  fs.appendText("soak.log", `${new Date().toISOString()}\t${i}\t${secs}\n`);
  print(`round ${i}: up in ${secs} s`);
  await sleep(20000);                              // let the network come up
}
await ui.result("PASS", `${rounds} reboots`);
```

Notes:
- `ui.password()` keeps the password in memory only. It is never shown, logged or written to disk. Do not write passwords into script files.
- To record the full console of every boot, start a timestamped log before the loop: `await con.log.start("soak-console.log")`. The status bar shows REC as usual.
- To catch a kernel panic early, watch for it and the login prompt together: `waitForAny(["login:", "Kernel panic"])`.

---

## 6. Example: HDLC request and response

Some devices speak a binary framed protocol on the same line as their console. With HDLC mode on, frames go to the script, while text still reaches the terminal.

```js
/// <reference path="termworker.d.ts" />
const s = tab;
if (!s) throw new Error("Run this from the device's tab");
await s.hdlc.enable({ fcs: 16 });                 // also opens the HDLC panel

s.hdlc.send([0x01, 0x00]);                         // "get version" request
const reply = await s.hdlc.waitFrame({
  timeout: 2000,
  filter: (f) => f.payload[0] === 0x81,            // only the matching response
});
print("version frame:", hex(reply.payload));
```

Notes:
- `waitFrame` returns good frames only. Pass `fcsOk: false` to also receive frames that failed the check.
- Frames that arrive before you wait for them are kept, up to 1000. The order is preserved.
- `s.hdlc.onFrame(fn)` calls `fn` for every frame, for logging or statistics.

---

## 7. Example: a binary protocol, byte by byte

Many devices answer in binary without HDLC framing: a bootloader in ROM, a sensor, a motor drive. Text waits do not suit them, because received text has already been decoded with the tab's character set. For these, every session also keeps the received bytes exactly as they arrived, in `s.bytes`.

Take a device whose request is `A5 <cmd> <len> <payload…> <xor>`. Its reply is `5A <status> <len> <payload…> <xor>`, where `<xor>` is the XOR of all bytes before it.

```js
/// <reference path="termworker.d.ts" />
const s = tab;
if (!s) throw new Error("Run this from the device's tab");

const xor = (bytes) => bytes.reduce((a, b) => a ^ b, 0);

const transact = async (cmd, payload = []) => {
  const req = [0xa5, cmd, payload.length, ...payload];
  s.bytes.discard();                                  // drop anything left from before
  s.sendBytes([...req, xor(req)]);

  const head = await s.bytes.read(3, { timeout: 1000 });          // 5A status len
  if (head[0] !== 0x5a) throw new Error(`bad start byte: ${hex(head)}`);
  const rest = await s.bytes.read(head[2] + 1, { timeout: 1000 }); // payload + xor
  const body = rest.subarray(0, -1);
  if (xor([...head, ...body]) !== rest[rest.length - 1]) throw new Error("checksum error");
  if (head[1] !== 0) throw new Error(`device status ${head[1]}`);
  return body;
};

const id = await transact(0x01);                       // "read ID"
print("ID:", hex(id));
await transact(0x10, [0x00, 0x20]);                    // "set rate", two parameter bytes
await ui.result("PASS", `device ${hex(id)}`);
```

Notes:
- `bytes.read(n)` returns exactly `n` bytes. Bytes that arrived before the call are not missed.
- `bytes.readUntil([0x0d, 0x0a])` reads up to and including a terminator. Use it for protocols that end each reply with a fixed byte sequence.
- `bytes.onData(fn)` is called with each chunk as it arrives, for example to count bytes or record them with `fs.appendText`.
- `bytes.available` tells how many bytes are waiting, and `bytes.take()` returns all of them at once.
- The byte buffer is separate from the text buffer: reading one does not consume the other. `s.discard()` clears both.
- Binary replies also appear on the terminal, decoded as text, so the screen may show odd characters. This is expected.
- If a reply does not come, the `TimeoutError` message shows the last unread bytes in hex.

---

## 8. Example: a production PASS/FAIL station

On a test bench, the operator scans a serial number, connects the unit and reads one large PASS or FAIL. Each result is appended to a CSV file.

```js
/// <reference path="termworker.d.ts" />
const sn = await ui.prompt("Scan the serial number");
if (!sn) throw new Error("no serial number");
const s = tab;
if (!s) throw new Error("Run this from the unit's serial tab");
s.prompt = /# $/;

const checks = [];
async function check(name, fn) {
  try {
    const detail = await fn();
    checks.push([name, "PASS", detail]);
  } catch (e) {
    checks.push([name, "FAIL", e.message]);
  }
}

await check("boot", async () => (await s.waitFor("login:", { timeout: 90000 })).match);
s.sendLine("root");
await s.waitFor(s.prompt);
await check("firmware", async () => {
  const v = (await s.command("cat /etc/version")).trim();
  if (v !== "2.4.1") throw new Error(`version ${v}`);
  return v;
});
await check("ethernet", async () => {
  const out = await s.command("ping -c 3 192.168.1.10");
  if (!/ 0% packet loss/.test(out)) throw new Error("ping failed");
  return "ok";
});

const failed = checks.filter((c) => c[1] === "FAIL");
for (const c of checks) fs.appendText("results.csv", [new Date().toISOString(), sn, ...c].join(",") + "\n");
await ui.result(failed.length ? "FAIL" : "PASS", failed.map((c) => c[0]).join(", "));
```

---

## 9. Practices for reliable scripts

1. **Anchor prompts to the end.** `/=> $/` is safer than `"=>"`, which can also appear inside output.
2. **End a pattern after the value you capture.** `/= (\d+) /`, not `/= (\d+)/`: data arrives in pieces, and an open-ended pattern can match half a number (section 2.2).
3. **Use `command()` for request/response consoles.** It clears stale output first, so an old prompt cannot end a wait too early.
4. **Give every wait a realistic timeout.** Use `timeout: 0` only where a person is expected to act, such as "power the board on".
5. **Use `waitForAny` to branch on failures** instead of waiting for success only. A script that reports *why* it failed saves a bench visit.
6. **Show progress with `ui.status()`.** The current step appears next to the script's name in the status bar.
7. **Catch errors where you can recover, and let the rest stop the script.** An uncaught error names the line and the last output, which is usually enough to see what went wrong.
8. **Keep passwords out of files.** Use `ui.password()`.
9. **Start long tests with a log.** `log.start(path, { mode: "timestamp" })` gives millisecond timestamps for every line, which is useful for boot-time measurements.
10. **Mind the Enter key.** `sendLine` uses the session's *Enter key sends* setting. To send an exact byte, use `sendKeys("<CR>")` or `sendBytes`.
11. **Timing.** Use `waitForQuiet(500)` when a device prints a burst of output with no clear end marker.

---

## 10. Limitations of this version

Not yet available:
- running a script automatically when a pattern appears (triggers),
- recording keystrokes into a script,
- a built-in editor,
- Python,
- starting scripts from the command line,
- changing the baud rate of an open session.

Also note:
- While a file transfer runs, received data goes to the transfer and is not visible to waits.
- Unread output beyond 1 MB per session is trimmed from the oldest end. The same holds for unread bytes.

---

## Appendix: API quick reference

**Globals:**
- `tab`, `sessions.open(target, { wait, timeout })`, `sessions.list()`, `sessions.find(name)`
- `ui`, `fs`, `sleep(ms)`, `print(…)`, `console.log(…)`
- `stripAnsi(text)`, `hex(bytes)`
- `TimeoutError`, `ScriptStopped`

**Session — send:**
- `send`, `sendLine`, `sendKeys`, `sendBytes`, `sendBreak`

**Session — wait:**
- `waitFor`, `waitForAny`, `readUntil`, `command`, `waitForQuiet`, `waitConnected`
- `discard`, `onData`, `onLine`

**Session — bytes:**
- `bytes.read(count)`, `bytes.readUntil(pattern)`, `bytes.available`, `bytes.take()`, `bytes.discard()`, `bytes.onData(fn)`
- `prompt` (property)

**Session — control:**
- `setDtr`, `setRts`, `setEncoding`, `activate`, `close`
- `log.start(path, { mode: "timestamp" | "text" | "raw" })`, `log.stop()`
- `zmodem` / `ymodem` / `xmodem` / `xmodem1k`: `.send(files)`, `.receive({ dir | file })`
- `hdlc.enable({ fcs, addressControl, escapeControlChars, maxFrame })`, `hdlc.disable()`, `hdlc.send(bytes)`, `hdlc.waitFrame({ timeout, filter, fcsOk })`, `hdlc.onFrame(fn)`
- `name`, `state`, `connected`

**ui:**
- `print`, `error`, `status(text)`, `result("PASS" | "FAIL", detail)`
- `prompt(msg, default)`, `password(msg)`, `confirm(msg)`, `choose(msg, options)`
- `openFile({ folder })`, `saveFile({ name })`

**fs** (paths relative to the script's folder):
- `readText`, `readBytes`, `writeText`, `appendText`, `writeBytes`, `exists`, `resolve`

`termworker.d.ts` in the examples folder describes every function, with its parameters and return value.

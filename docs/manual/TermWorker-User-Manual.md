# TermWorker User Manual

**Version 0.3.1 · DRAFT · November 2026**
DHPLab Consulting · dhplab.com

> This is a draft. Screens and wording may still change before release.

---

## Contents

1. [About TermWorker](#1-about-termworker)
2. [Installation](#2-installation)
3. [The window](#3-the-window)
4. [Connecting](#4-connecting)
5. [Working in the terminal](#5-working-in-the-terminal)
6. [Logging](#6-logging)
7. [File transfer (ZMODEM, YMODEM, XMODEM)](#7-file-transfer-zmodem-ymodem-xmodem)
8. [HDLC frames](#8-hdlc-frames)
9. [Scripts](#9-scripts)
10. [Keyboard shortcuts](#10-keyboard-shortcuts)
11. [Settings, files and privacy](#11-settings-files-and-privacy)
12. [Troubleshooting](#12-troubleshooting)

---

## 1. About TermWorker

TermWorker is a terminal for engineers who bring up boards, modems and routers. It connects over:

- **Serial**: USB-serial adapters and built-in ports, 300 to 3,000,000 baud.
- **SSH**: password, keyboard-interactive and private-key login.
- **Telnet**: with window-size and terminal-type negotiation.
- **Raw TCP**: a plain socket, for serial servers and debug ports.

Each connection opens in its own tab. Some features matter most for hardware work:

- When a USB-serial adapter is unplugged and plugged back in, the session reconnects by itself.
- Files can be sent and received with ZMODEM, YMODEM and XMODEM, including to and from U-Boot.
- HDLC frames can be shown separately from console text.
- Logs can carry a millisecond timestamp on every line.
- JavaScript scripts can automate repetitive work.

TermWorker runs on Windows 10 and 11, and on Linux.

---

## 2. Installation

### Linux

Two packages are provided. Both need Ubuntu 22.04 or later (or another distribution of the same age) with a desktop.

| Package | How to install |
|---|---|
| `TermWorker_<version>_amd64.deb` | `sudo apt install ./TermWorker_<version>_amd64.deb`. Required libraries are installed with it. TermWorker then appears in the application menu. |
| `TermWorker_<version>_amd64.AppImage` | Runs without installing: `chmod +x TermWorker_*.AppImage`, then run it. |

Each download comes with a `SHA256SUMS` file. To check it: `sha256sum -c SHA256SUMS`.

**Serial port access.** On Linux, opening a serial port needs membership of the `dialout` group. Add yourself once, then log out and back in:

```bash
sudo usermod -aG dialout $USER
```

### Windows

Run `TermWorker_<version>_x64-setup.exe` and approve the administrator prompt. TermWorker is installed for all users in `C:\Program Files\TermWorker`, and appears in the Start menu.

- The install folder is added to the system `PATH`, so `termworker` also starts from a command prompt. Terminal windows that were already open do not see the change; open a new one.
- If TermWorker 0.3.0 is installed (it was installed per user, under `%LOCALAPPDATA%\TermWorker`), the installer removes it first. Your saved sessions and settings are kept.
- To uninstall, use **Settings → Apps → Installed apps → TermWorker**. Uninstalling also takes the folder out of `PATH`. Saved sessions are kept unless you tick the option to delete app data.

TermWorker uses the WebView2 runtime, which is already part of Windows 10 (current updates) and Windows 11. The installer downloads it if it is missing.

---

## 3. The window

```
┌───────────────┬──────────────────────────────────────────────────────┐
│ Sidebar       │ Tab bar                                              │
│  Quick connect├──────────────────────────────────────────────────────┤
│  Saved        │                                                      │
│  sessions     │ Terminal                                             │
│               │                                                      │
│               ├──────────────────────────────────────────────────────┤
│               │ Script output (when shown)                           │
│               ├──────────────────────────────────────────────────────┤
│               │ Status bar                                           │
└───────────────┴──────────────────────────────────────────────────────┘
```

- **Sidebar.** Has the quick-connect box and the list of saved sessions. Hide it with Ctrl+Shift+B. Click the TermWorker logo for the About window.
- **Tab bar.** One tab per connection. The dot shows the link state: green is connected, red is disconnected. **+** opens a new session.
- **Terminal.** The remote side's output.
- **Status bar.** Shows the state of the current tab and its controls, from left to right:

| Item | What it does |
|---|---|
| State and summary | Connected, Connecting, Waiting for device, Disconnected, with the port or host |
| DTR, RTS, Break | Serial only. Toggle the modem lines; Break sends a 250 ms break |
| Script | Runs a script and shows running scripts ([section 9](#9-scripts)) |
| Transfer | Sends or receives files ([section 7](#7-file-transfer-zmodem-ymodem-xmodem)) |
| HDLC | Opens the HDLC frame panel ([section 8](#8-hdlc-frames)) |
| Options | Local echo, Treat LF as CR+LF, Clear scrollback |
| Encoding | Character set of this tab, for example UTF-8 or Korean (CP949) |
| Log / REC | Starts and stops logging ([section 6](#6-logging)) |
| Size | Terminal size in columns × rows |

---

## 4. Connecting

### 4.1 Quick connect

Type into the **Quick connect** box at the top of the sidebar (Ctrl+K) and press Enter:

| You type | Opens |
|---|---|
| `COM3` or `COM3 115200` | Serial port COM3, 115200 baud by default |
| `/dev/ttyUSB0 921600` | Serial port on Linux |
| `root@10.0.0.1` or `root@10.0.0.1:2222` | SSH |
| `ssh host` | SSH (the dialog asks for the user name) |
| `telnet host` or `telnet host:2323` | Telnet |
| `raw host:5000` or `host:5000` | Raw TCP |

The same box also filters the list of saved sessions as you type.

### 4.2 New session dialog

Click **+** (Ctrl+Shift+T) and pick the type: Serial, SSH, Telnet or Raw TCP. Then fill in the fields below.

**Serial**
- **Port.** Lists the ports with their USB description and VID:PID. Click the rescan button after plugging in an adapter. A name not in the list can be typed in.
- **Baud rate.** Pick from the list or type any value.
- **Frame.** Data bits (5–8), parity (none, even, odd), stop bits (1, 2).
- **Flow control.** None, Hardware (RTS/CTS) or Software (XON/XOFF).
- **Reconnect automatically when the device is unplugged.** On by default; see 4.4.
- **Assert DTR on open, Assert RTS on open.** Some boards reset or stop in their bootloader depending on these lines.

**SSH**
- **Host, Port** (22), **Username**.
- **Authentication.** Either Password or a private Key file. The key may be encrypted; give its Passphrase.
- If the password is left empty, TermWorker asks for it when connecting.

**Telnet, Raw TCP**
- **Host, Port.**

**All types**
- **Name.** Leave it empty for an automatic name such as `COM3` or `root@10.0.0.1`.
- **Encoding.** The character set of the device.
- **Enter key sends.** CR, LF or CR+LF. Most devices expect CR. Some modems and microcontrollers expect CR+LF.
- **Save to sessions.** Keeps the session in the sidebar.

Click **Connect** to open the tab, or **Save** to only store the session.

### 4.3 Saved sessions

Click a saved session to connect. Hover over it to **Edit**, **Duplicate** or **Delete** it.

### 4.4 Reconnecting

- **Manually.** When a session is disconnected, press **Enter** in its tab to connect again.
- **USB-serial adapters.** When an adapter is unplugged, the tab shows *Waiting for device*. When the same adapter comes back on the same port name, the tab reconnects and restores DTR and RTS.

### 4.5 SSH host keys

The first time you connect to a server, TermWorker shows its host key fingerprint (SHA-256).
- Compare it with the fingerprint on the server, for example the output of `ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub`.
- Choose **Trust** to remember the key.

If a server's key later changes, TermWorker refuses the connection. This protects you from connecting to a different machine by mistake. If the key really did change, for example after reinstalling the server, remove the old entry from TermWorker's `known_hosts` file ([section 11](#11-settings-files-and-privacy)).

---

## 5. Working in the terminal

- **Copy.** Selecting text copies it, as in PuTTY and Tera Term. Ctrl+Shift+C also copies.
- **Paste.** Right-click, or Ctrl+Shift+V. If text is selected, right-click copies it instead.
- **Find.** Ctrl+Shift+F searches the scrollback; Enter goes to the next match, Shift+Enter to the previous one. The scrollback keeps 20,000 lines.
- **Korean and other CJK text.** Two-column characters line up correctly. If the device does not send UTF-8, change the tab's **Encoding** in the status bar. The change takes effect at once.
- **Options** (status bar):
  - *Local echo*: shows what you type, for devices that do not echo.
  - *Treat LF as CR+LF*: fixes output that runs diagonally down the screen.
  - *Clear scrollback*: empties the history.
- **DTR, RTS, Break** (serial only). The lamp next to DTR and RTS shows whether the line is asserted. RTS cannot be changed by hand while hardware flow control is on.

---

## 6. Logging

Click **Log** in the status bar and choose a format:

| Format | Contents |
|---|---|
| Text with timestamps | Each line starts with `[2026-01-31 09:15:02.123]`; escape sequences are removed |
| Plain text | Escape sequences (colors, cursor movement) are removed |
| Raw bytes | Exactly what was received |

The suggested file is `Documents/TermWorker/logs/<tab name>_<date>_<time>.log`. You can choose a different one.

While logging, the button shows a red **REC**. Click it to stop. Logging continues into the same file across a manual reconnect.

---

## 7. File transfer (ZMODEM, YMODEM, XMODEM)

File transfer works over every connection type: serial, SSH, Telnet and Raw TCP.

### Starting a transfer

- **From the remote side (ZMODEM).** Run `sz file` or `rz` on the remote shell. TermWorker notices and asks which files to send or where to save. You can decline.
- **From TermWorker.** Use **Transfer** in the status bar:

| Protocol | Typical use |
|---|---|
| ZMODEM | Linux `rz` / `sz`. The fastest; resends only damaged data |
| YMODEM | U-Boot `loady`. Several files, with names |
| XMODEM | U-Boot `loadx` and other bootloaders. 128-byte blocks, checksum or CRC |
| XMODEM-1K | 1 KB blocks: a faster XMODEM |

To load an image into U-Boot:
1. Type `loady 0x80000000` (or `loadx …`) at the U-Boot prompt.
2. Choose **Transfer → Send file → YMODEM** (or XMODEM) and pick the file.

### During and after a transfer

- A panel shows the file name, progress, rate and time left. **Cancel** stops the transfer.
- Keyboard input is held back while a transfer runs, so it cannot corrupt the transfer.
- Received files keep their names, made safe for your system. An existing file is never overwritten; the new one is saved as `name (1).ext`, `name (2).ext` and so on.
- When the transfer ends, a summary line is written to the terminal.

---

## 8. HDLC frames

Some devices send binary frames on the same line as their text console. Examples are PPP-style async HDLC (RFC 1662), diagnostic protocols and bootloader protocols. TermWorker can split these frames out.

1. Click **HDLC** in the status bar to open the panel.
2. Choose the frame check (**FCS-16**, **FCS-32** or **None**) and the options below, then switch HDLC **On**:
   - *FF 03*: adds the address and control bytes on send and removes them on receive (PPP style).
   - *Escape 00–1F*: escapes control characters too (the RFC 1662 default).
3. Frames appear in the panel with time, length, contents and FCS check. Console text between frames stays in the terminal.

The panel can also:
- **Send** one frame, typed as hex (`01 02 0A FF`) or as text.
- **Send file…** as a run of frames. You set the payload size and the pause after each frame.
- **Save received payloads…** to a file. Payloads of good frames are written back to back.

Closing the panel does not turn HDLC mode off.

---

## 9. Scripts

Scripts are JavaScript files that automate work in the terminal. Typical uses:
- Stopping U-Boot and loading an image.
- Collecting modem information.
- Running reboot tests overnight.
- Producing a PASS/FAIL result on a test bench.

The script engine is built in, so there is nothing to install.

**Running a script**

Click **Script** in the status bar, then **Run script…**, and choose a `.js` file.
- The tab you are looking at is the script's `tab`. A script can also open its own sessions.
- Recently run scripts appear in the same menu.
- While a script runs, its name and current step are shown in the status bar.

**Script output**

The **Script output** panel shows:
- what the script prints,
- PASS/FAIL results,
- errors, with the script line that caused them.

Click **Stop** in the panel or in the Script menu to end a script at any time.

A short example:

```js
const s = tab;
await s.waitFor("Hit any key", { timeout: 0 });   // waits until the board is powered on
s.send(" ");
await s.waitFor(/=> $/);
print(await s.command("version", { prompt: /=> $/ }));
```

The full guide is *AN-001: Automating board bring-up with TermWorker scripts*. Example scripts and an editor definitions file (`termworker.d.ts`, for completion in VS Code) are included.

---

## 10. Keyboard shortcuts

| Keys | Action |
|---|---|
| Ctrl+Shift+T | New session |
| Ctrl+Shift+W | Close tab |
| Ctrl+Tab, Ctrl+PageDown | Next tab |
| Ctrl+Shift+Tab, Ctrl+PageUp | Previous tab |
| Ctrl+K | Quick connect |
| Ctrl+Shift+F | Find in scrollback |
| Ctrl+Shift+C / Ctrl+Shift+V | Copy / paste |
| Right click | Copy selection, or paste |
| Ctrl+Shift+B | Show or hide the sidebar |
| Enter (disconnected tab) | Reconnect |
| Esc | Close a dialog |

Plain Ctrl+C, Ctrl+V and similar keys go to the remote side, as in any terminal.

---

## 11. Settings, files and privacy

| What | Where |
|---|---|
| Saved sessions (`sessions.json`), trusted SSH host keys (`known_hosts`) | Linux: `~/.config/com.dhplab.termworker/` · Windows: `%APPDATA%\com.dhplab.termworker\` |
| Logs (suggested) | `Documents/TermWorker/logs/` |
| Received files (suggested) | `Downloads` |

**Passwords and passphrases are never written to disk.** This covers:
- passwords typed into a dialog,
- passwords given to a script,
- the passphrase of a private key.

They are kept in memory only while TermWorker runs, so after a restart TermWorker asks again.

---

## 12. Troubleshooting

| Symptom | What to do |
|---|---|
| *permission denied. Add your user to the "dialout" group…* (Linux) | Run `sudo usermod -aG dialout $USER`, then log out and back in |
| *the port is in use by another application* | Close the other program (another terminal, a modem manager). On Linux, `ModemManager` sometimes opens new modem ports |
| No serial ports in the list | Plug in the adapter and click rescan. Check that the driver is installed (Windows: Device Manager) |
| Garbled characters | Check the baud rate first, then the tab's encoding |
| Output runs diagonally down the screen | Options → *Treat LF as CR+LF* |
| The device does not react to Enter | Change *Enter key sends* (CR, LF, CR+LF) in the session settings |
| Typed characters do not appear | The device does not echo: Options → *Local echo* |
| SSH connection refused after a server reinstall | The host key changed; remove the old line for that host from `known_hosts` (section 11) |
| The AppImage does not start on a server without a desktop | The AppImage needs a desktop installation (graphics and windowing libraries) |
| Transfer stalls at the start | Make sure the other side is ready (`rz`, `loady`) and that the flow-control setting matches the device |

**Support:** donghyun.park@dhplab.com · dhplab.com

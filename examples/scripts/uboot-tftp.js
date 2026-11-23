/// <reference path="termworker.d.ts" />
// Run from a serial tab, then power the board on: stops U-Boot, sets the
// network and loads an image over TFTP.

const s = tab;
if (!s) throw new Error("Run this from the board's serial tab");

ui.status("waiting for U-Boot (power the board on)");
await s.waitFor("Hit any key", { timeout: 0 });
s.send(" ");
await s.waitFor(/=> $/);

const image = await ui.prompt("Image on the TFTP server", "fw.bin");
if (image === null) throw new Error("cancelled");

ui.status("TFTP");
await s.command("setenv serverip 192.168.1.10; setenv ipaddr 192.168.1.1", { prompt: /=> $/ });
s.sendLine(`tftpboot 0x80000000 ${image}`);
const r = await s.waitForAny([/Bytes transferred = (\d+) /, /TIMEOUT|not found|Retry count exceeded/], {
  timeout: 120000,
});
if (r.index !== 0) {
  await ui.result("FAIL", r.match);
  throw new Error(`TFTP failed: ${r.match}`);
}
await ui.result("PASS", `${image}: ${r.groups[0]} bytes`);

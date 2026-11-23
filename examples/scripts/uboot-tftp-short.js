const s = tab;                                        // the serial tab the script runs in
await s.waitFor("Hit any key", { timeout: 0 });      // power the board on
s.send(" ");                                          // stop autoboot
await s.waitFor(/=> $/);
await s.command("setenv serverip 192.168.1.10; setenv ipaddr 192.168.1.1", { prompt: /=> $/ });
s.sendLine("tftpboot 0x80000000 fw.bin");
const r = await s.waitForAny([/Bytes transferred = (\d+) /, /TIMEOUT|not found/], { timeout: 120000 });
await ui.result(r.index === 0 ? "PASS" : "FAIL", r.match);

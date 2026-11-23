/// <reference path="termworker.d.ts" />
// Run from an OpenWrt serial console tab (logged in as root). Collects the
// firmware, kernel, uptime, memory and serial ports into openwrt-info.txt.
// Read-only: it changes nothing on the device.

const s = tab;
if (!s) throw new Error("Run this from the router's console tab");

s.prompt = /root@[^:]+:[^#]*# $/;          // OpenWrt ash prompt, e.g. root@OpenWrt:~#
s.sendLine("");                             // wake the console
await s.waitFor(s.prompt, { timeout: 5000 });

const checks = [
  ["Firmware", ". /etc/openwrt_release; echo $DISTRIB_DESCRIPTION"],
  ["Model", "cat /tmp/sysinfo/model 2>/dev/null || cat /proc/device-tree/model"],
  ["Kernel", "uname -r"],
  ["Uptime", "cut -d. -f1 /proc/uptime"],
  ["Memory free (kB)", "awk '/MemAvailable/ {print $2}' /proc/meminfo"],
  ["Serial ports", "ls /dev/ttyS* /dev/ttyUSB* 2>/dev/null | tr '\\n' ' '"],
];

const report = [];
for (const [name, cmd] of checks) {
  ui.status(name);
  const out = stripAnsi(await s.command(cmd, { timeout: 5000 })).trim();
  report.push(`${name.padEnd(18)} ${out}`);
  print(report.at(-1));
}

fs.writeText("openwrt-info.txt", report.join("\n") + "\n");
await ui.result("PASS", `saved ${fs.resolve("openwrt-info.txt")}`);

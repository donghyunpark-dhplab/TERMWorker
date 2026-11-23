/// <reference path="termworker.d.ts" />
// Collects modem identity and signal over AT commands into modem.tsv.

const s = tab ?? (await sessions.open("/dev/ttyUSB2 115200"));
const rows = [];
for (const cmd of ["ATI", "AT+CGSN", "AT+CIMI", "AT+CSQ", "AT+COPS?"]) {
  const out = await s.command(cmd, { prompt: /\r\n(OK|ERROR)\r\n/, timeout: 5000 });
  rows.push(`${cmd}\t${out.trim().replace(/\s+/g, " ")}`);
  print(rows.at(-1));
}
fs.writeText("modem.tsv", rows.join("\n") + "\n");
print("saved", fs.resolve("modem.tsv"));

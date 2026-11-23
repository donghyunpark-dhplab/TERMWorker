/// <reference path="termworker.d.ts" />
// Reboots a board over SSH a number of times and checks on the serial console
// that it comes back each time. Results go to reboot-test.log.

const rounds = Number(await ui.prompt("How many reboots?", "10"));
const con = await sessions.open("Board console"); // a saved serial session
const password = await ui.password("root password of the board");

for (let i = 1; i <= rounds; i++) {
  ui.status(`round ${i} of ${rounds}`);
  const ssh = await sessions.open({ type: "ssh", host: "192.168.1.1", user: "root", password });
  ssh.sendLine("reboot");
  await ssh.close();
  const started = Date.now();
  await con.waitFor("login:", { timeout: 180000 });
  const secs = ((Date.now() - started) / 1000).toFixed(1);
  fs.appendText("reboot-test.log", `${new Date().toISOString()}\tround ${i}\tup in ${secs} s\n`);
  print(`round ${i}: up in ${secs} s`);
  await sleep(20000); // let the network come up
}
await ui.result("PASS", `${rounds} reboots`);

// Binary request/response, byte by byte (AN-001 section 7).
// Request: A5 <cmd> <len> <payload…> <xor>. Reply: 5A <status> <len> <payload…> <xor>.
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

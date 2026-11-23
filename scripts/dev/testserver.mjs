// Local raw-TCP test server for manual UI checks: colored output, Korean text, echo.
import net from "node:net";

const port = Number(process.argv[2] ?? 5555);
net
  .createServer((sock) => {
    const lines = [
      "\x1b[1;36mU-Boot 2024.01\x1b[0m (Nov 20 2026 - 09:12:44 +0900)",
      "",
      "CPU:   \x1b[33mMediaTek MT7981\x1b[0m   DRAM:  \x1b[32m512 MiB\x1b[0m",
      "Net:   eth0: \x1b[32mlink up\x1b[0m 1000Mbps full duplex",
      "한글 출력 테스트: 안녕하세요, TermWorker 입니다. ✓",
      "\x1b[41;97m ERROR \x1b[0m sample error line   \x1b[43;30m WARN \x1b[0m sample warning",
      "┌──────────────┬──────────┐",
      "│ box drawing  │ 256 col  │",
      "└──────────────┴──────────┘",
      "",
      "Hit any key to stop autoboot:  0",
      "root@OpenWrt:~# ",
    ];
    sock.write(lines.join("\r\n"));
    sock.on("data", (d) => sock.write(d.toString("utf8").replace(/\r/g, "\r\n")));
    sock.on("error", () => {});
  })
  .listen(port, "127.0.0.1", () => console.log(`listening ${port}`));

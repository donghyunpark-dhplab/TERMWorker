# TermWorker

A clean, fast terminal for **Serial, SSH, Telnet and raw TCP**, built for embedded and network engineers.
Desktop app for Windows and Linux (Tauri 2 + Rust backend + xterm.js frontend), tested on Windows 11 and
Ubuntu 22.04 to 26.04. There is no macOS build.

**Why it exists.** Bringing up boards and testing in the field, I kept wishing my terminal did a few things a little differently. TermWorker gathers those wishes into one tool. I am sharing the source as well, in the hope that it is of some help to people doing similar work.

Product page: https://dhplab.com/termworker/

**Status.** There is no binary release yet. `main` is development code: it is ahead of the last internal
build (0.1.1) and already contains scripting and the TFTP server and adapter settings planned for the next
releases. Version numbers are updated when installers are built. Linux packages (.deb, AppImage) are built
from `scripts/release/linux/`; a Windows installer is planned.

**Support** is best effort. Please report bugs and requests in
[GitHub issues](https://github.com/donghyunpark-dhplab/TERMWorker/issues); report security issues privately
as described in [SECURITY.md](SECURITY.md).

## Features

| Area | What works today |
|---|---|
| **Serial** | Port list with USB description and VID:PID · 300–3,000,000 baud · 5–8 data bits, parity, stop bits · RTS/CTS and XON/XOFF · DTR/RTS toggles and 250 ms Break in the status bar · **auto-reconnect when a USB-serial adapter is unplugged and plugged back in** (restores DTR/RTS) |
| **SSH** | Password, keyboard-interactive and private-key auth (encrypted keys supported) · host-key trust on first use with SHA-256 fingerprint · changed host keys are refused · keepalive · live window resize |
| **Telnet** | RFC 854 with RFC 1143 option negotiation: NAWS (window size), TERMINAL-TYPE, SGA, ECHO, BINARY |
| **Raw TCP** | Byte-exact socket, for serial servers and debug ports |
| **Terminal** | xterm.js with WebGL rendering · correct Korean/CJK wide characters · 20k-line scrollback · find in scrollback · copy-on-select and right-click paste (PuTTY / Tera Term style) |
| **Encodings** | UTF-8, CP949/EUC-KR, GB18030, Big5, Latin-1 and more, switchable live per tab |
| **Logging** | Raw bytes, plain text (escape sequences stripped), or **text with a millisecond timestamp per line** |
| **File transfer** | ZMODEM (CRC-32, streaming, resends only damaged data), YMODEM batch, XMODEM (checksum, CRC, 1K) — send and receive over every connection type · **ZMODEM starts by itself when you run `sz` or `rz`** on the remote side · progress panel with rate and time left · received names are sanitized and never overwrite existing files · implemented in-house (no GPL code) |
| **HDLC frames** | RFC 1662 async HDLC (7E flags, 7D escapes, FCS-16/FCS-32, optional FF 03): received frames listed in a side panel with time, length and FCS check while console text between them stays in the terminal · send a frame from hex or text · send a file as a run of frames (size and gap per frame) · save received payloads to a file |
| **Sessions** | Tabs · saved sessions with search · quick connect (`COM3 115200`, `root@10.0.0.1`, `telnet host:2323`, `raw host:5000`) · press Enter to reconnect a dropped session |
| **Scripts** | JavaScript (built-in QuickJS engine, nothing to install): `waitFor` with strings or regular expressions and timeouts, `command()` that returns a command's output, several sessions in one script (e.g. serial console + SSH), file transfer, HDLC frames, raw bytes for binary protocols (`bytes.read`, `bytes.readUntil`), DTR/RTS, logging, input dialogs, PASS/FAIL results · errors point at the script line · Stop ends any script · TypeScript definitions for editor completion ([docs/SCRIPTING.md](docs/SCRIPTING.md), [examples/scripts/](examples/scripts/)) |
| **TFTP server** | Built in, for U-Boot `tftpboot` and busybox `tftp`: serves one folder (RFC 1350 with blksize, timeout and tsize options) · uploads only when turned on, never over an existing file, never outside the folder · lists this PC's addresses and types `setenv serverip …` into the tab · warns when the PC's address goes away (a board reboot can drop the DHCP lease) · transfers with progress and speed · also from scripts (`tftp.start`, `tftp.waitFor`) · on Linux, port 69 needs permission (or use a port above 1023 with `setenv tftpdstp`) |
| **PC network adapters** | Give the adapter cabled to a board a static address on the board's network, or put it back on DHCP, from the app (Windows: netsh after a UAC prompt; Linux: NetworkManager) · "Board link: no internet route" keeps the board from becoming the PC's default route · TermWorker remembers what it changed and can restore the previous setting |

Passwords and passphrases are never written to disk. TermWorker sends nothing over the network on its own:
no update checks, no telemetry.

## Documentation

| Document | Language | |
|---|---|---|
| [docs/manual/TermWorker-User-Manual.md](docs/manual/TermWorker-User-Manual.md) | English | draft |
| [docs/manual/AN-001-Scripting.md](docs/manual/AN-001-Scripting.md) | English | draft application note: scripting |
| [docs/SCRIPTING.md](docs/SCRIPTING.md) | Korean | script API reference |
| [docs/LINUX.md](docs/LINUX.md) | Korean | building and testing on Ubuntu/Debian |
| [examples/scripts/](examples/scripts/) | English | example scripts and `termworker.d.ts` |

## Roadmap

1. Legacy SSH algorithms (old Dropbear on routers), SSH agent / Pageant, jump hosts, port forwarding, SFTP browser.
2. Triggers (e.g. run a script on `Hit any key`), keystroke recording to script, Python scripts over the same API, command-line script runs; hex view, multi-port split view.
3. Settings UI (fonts, themes, keybindings), OS keychain for saved passwords.
4. Releases: Windows installer, code signing, auto-update.

## Development

Prerequisites: Rust (stable), Node.js 20+, and on Windows the MSVC build tools and WebView2 (preinstalled on Windows 11).

```bash
npm install
npm run tauri dev        # run the app with hot reload
npm run tauri build      # release build + installers
cd src-tauri && cargo test   # backend tests, incl. end-to-end SSH / Telnet / TCP against local servers
npm run typecheck        # frontend type check
```

### Layout

```
src/                      React + TypeScript UI
  terminal/TermSession.ts   one tab: xterm.js <-> backend session (data path bypasses React)
  components/               sidebar, tabs, status bar, dialogs
  lib/                      API wrappers, types, profiles, quick-connect parser
src-tauri/src/            Rust backend
  commands.rs               Tauri command surface used by the UI
  session.rs                decoding, logging, batching, UI event channel
  codec.rs / logger.rs      character sets, log writer + ANSI stripper
  transfer/                 xymodem.rs, zmodem.rs, crc.rs: file transfer protocols (blocking code on their own thread)
                            link.rs: connects a transfer to a session with flow control
  transport/                serial.rs, ssh.rs, telnet.rs, tcp.rs (actor per connection)
                            serial_win.rs: overlapped Windows serial I/O · serial_unix.rs: serialport
                            serial_hw_tests.rs: hardware-in-the-loop tests (ignored by default)
  script/                   JavaScript scripts: QuickJS engine per run (mod.rs), user API (prelude.js), tests
  tftp/                     built-in TFTP server and its tests
  netcfg.rs                 this PC's adapters: addresses, static IP / DHCP (netsh, nmcli)
examples/scripts/         example scripts and termworker.d.ts (editor completion)
```

Saved sessions live in the OS app-config directory (`sessions.json`); TermWorker keeps its own `known_hosts` next to it.

## License

TermWorker is licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
TermWorker by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.

The TermWorker name and the DHPLab logo (`public/dhplab-logo-dark.svg`) are not covered by these
licences and may not be used for other products without permission.

Third-party components and their licences are listed in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).


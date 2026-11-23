# 스크립트 (JavaScript)

내장 JavaScript 엔진(QuickJS)으로 터미널 작업을 자동화합니다. 1단계가 구현되었습니다(2026-11-22).

- 예제와 자동 완성용 타입 정의: [examples/scripts/](../examples/scripts/)
- 구현: 백엔드 [src-tauri/src/script/](../src-tauri/src/script/)(엔진·`prelude.js`·시험), UI [src/lib/script.ts](../src/lib/script.ts), [src/components/Script.tsx](../src/components/Script.tsx)

## 1. 실행

- 상태 표시줄의 **Script** 버튼 → **Run script…**로 `.js` 파일을 고릅니다. 최근 실행한 스크립트 8개가 같은 메뉴에 나옵니다.
- 지금 보고 있는 탭이 스크립트의 `tab`이 됩니다. 탭이 없으면 `tab`은 `null`이고, 스크립트가 `sessions.open()`으로 직접 엽니다.
- 실행 중에는 상태 표시줄에 스크립트 이름과 `ui.status()` 문구가 나옵니다. 출력 패널(터미널 아래)에 출력, 오류, PASS/FAIL이 나오고, 거기서 **Stop**을 누를 수 있습니다.
- 파일의 최상위에서 바로 `await`를 씁니다(모듈로 실행). 최상위 코드가 끝나면 스크립트도 끝납니다.
- 오류가 나면 출력 패널에 오류 종류, 메시지, **스크립트의 줄 번호**가 나옵니다. 예: `TimeoutError: COM3: "login:" did not appear within 30 s. Last output: "…"` / `at <anonymous> (flash.js:12:9)`
- 여러 스크립트를 동시에 실행할 수 있습니다. 각 스크립트는 자기 스레드와 엔진에서 돌기 때문에 무한 루프에 빠져도 화면과 다른 스크립트는 멈추지 않습니다. Stop은 무한 루프와 대기 중인 스크립트를 모두 끊습니다.
- 메모리 한도 256 MB.

## 2. 전역

| 이름 | 뜻 |
|---|---|
| `tab` | 실행한 탭의 `Session` 또는 `null` |
| `sessions` | `open(대상, {wait, timeout})`, `list()`, `find(이름)` |
| `ui` | 출력, 입력 대화상자, 상태 문구, PASS/FAIL |
| `fs` | 파일 읽기·쓰기. 상대 경로는 **스크립트가 있는 폴더** 기준 |
| `sleep(ms)` | `sleep(Infinity)`는 Stop할 때까지 대기 |
| `print(...)`, `console.log(...)` | 출력 패널 |
| `stripAnsi(text)`, `hex(bytes)` | 보조 함수 |
| `TimeoutError`, `ScriptStopped` | 오류 종류 (`catch (e) { if (e instanceof TimeoutError) … }`) |

## 3. Session

**열기** — `sessions.open(대상)`은 새 탭을 열고 연결될 때까지 기다립니다(기본 60초, 실패하면 예외).
```js
await sessions.open("Router console");                     // 저장된 세션 이름
await sessions.open("COM3 115200");                        // 빠른 연결과 같은 문법
await sessions.open("root@10.0.0.1");                      // SSH, telnet host:23, raw host:5000
await sessions.open({ type: "serial", port: "/dev/ttyUSB0", baud: 921600 });
await sessions.open({ type: "ssh", host: "10.0.0.1", user: "root", password: await ui.password() });
```
비밀번호는 메모리에만 둡니다(대화상자에 입력한 것과 같음). 스크립트 파일에 쓰지 말고 `ui.password()`로 받는 것을 권합니다.

**보내기**
| 함수 | 설명 |
|---|---|
| `send(text)` | `\r`은 세션의 Enter 설정(CR / LF / CR+LF)으로 바뀜 |
| `sendLine(text)` | `send(text + Enter)` |
| `sendBytes(bytes)` | `Uint8Array`, 숫자 배열 |
| `sendKeys("^C")` | 제어 키를 정확히: `^C`, `<ESC>`, `<CR>`, `<LF>`, `<TAB>`, `<BS>`, `<DEL>`, `<UP>`…, `<F1>`–`<F12>`, `^^`(= `^`) |
| `await sendBreak()` | 250 ms Break |

**기다리기** — 모든 대기는 `{ timeout }`(밀리초, 기본 30000, `0`은 무한)을 받고, 시간이 지나면 `TimeoutError`를 던집니다.
| 함수 | 설명 |
|---|---|
| `waitFor(패턴)` | 문자열 또는 정규식. `{ match, groups, named, before }`를 돌려줌 |
| `waitForAny([패턴…])` | 먼저 나온 것. `index`로 분기 |
| `readUntil(패턴)` | 패턴까지의 출력 전체 |
| `command(줄, { prompt })` | 남은 출력을 비우고 → `sendLine` → 프롬프트까지 읽어, 에코된 명령 줄을 뺀 출력을 돌려줌. 기본 프롬프트는 `s.prompt`(처음 `/[#$>] ?$/`) |
| `waitForQuiet(ms)` | 출력이 ms 동안 없을 때까지 |
| `waitConnected()` | 연결될 때까지 |
| `discard()` | 받은 출력을 버림 |
| `onData(fn)`, `onLine(fn)` | 받는 대로 호출. 해지 함수를 돌려줌 |

**바이트로 받기** — `s.bytes`. 받은 바이트를 문자 변환 전 그대로 따로 모읍니다(HDLC 프레임 바이트 포함). 텍스트 버퍼와는 별개라서 `waitFor`로 텍스트를 읽어도 바이트는 남고, 그 반대도 같습니다. 바이너리 프로토콜에 씁니다. 2026-11-22 추가.
| 함수 | 설명 |
|---|---|
| `await bytes.read(n)` | 정확히 n바이트 (`Uint8Array`) |
| `await bytes.readUntil(패턴)` | 패턴(바이트 배열, 문자열은 UTF-8)까지 포함해서 |
| `bytes.available`, `bytes.take()`, `bytes.discard()` | 남은 개수, 남은 것 전부 꺼내기, 버리기 |
| `bytes.onData(fn)` | 받는 대로 `Uint8Array`로 호출 |

`discard()`는 텍스트와 바이트를 모두 버립니다. 시간 제한 오류에는 읽지 않은 바이트의 끝부분이 16진수로 나옵니다.

**주의:** 대기는 지금까지 받은 텍스트에 패턴이 맞는 순간 끝납니다. 데이터는 나뉘어 오므로 `(\d+)`처럼 끝이 열린 패턴은 숫자가 다 오기 전에 맞을 수 있습니다(`1048576` 대신 `1`). 값 뒤에 오는 글자까지 넣습니다: `/Bytes transferred = (\d+) /`. `\b`는 받은 텍스트의 끝에서도 맞으므로 소용없습니다. PL2303·FTDI 실기 시험에서 발견(2026-11-23).

**주의(U-Boot):** U-Boot는 빈 Enter를 받으면 **직전 명령을 다시 실행**합니다. 프롬프트를 깨우려고 `sendLine("")`을 보내면 앞의 `tftpboot` 같은 명령이 한 번 더 돕니다. 이미 프롬프트에 있다면 바로 `command()`로 시작합니다. 또 일부 U-Boot는 아무 키가 아니라 정해진 단어로만 자동 부팅이 멈춥니다(예: `Autoboot in 1 seconds, type lsp to stop`). 이때는 `const m = await s.waitFor(/type (\S+) to stop/); s.send(m.groups[0]);`처럼 안내 문구에서 단어를 읽어 보냅니다. 실제 보드 시험에서 발견(2026-11-23).

찾는 범위: 스크립트가 그 탭을 보기 시작한 뒤 받은 데이터 중, 앞의 대기에서 맞은 부분 다음부터입니다. 터미널에 보이는 그대로(색 코드 포함)라서, 필요하면 `stripAnsi()`를 씁니다. 받은 지 1 MB가 넘은 미처리 출력은 앞부분이 버려집니다.

**제어** — 탭 화면에도 그대로 반영됩니다.
| 함수 | 설명 |
|---|---|
| `setDtr(bool)`, `setRts(bool)` | 시리얼만 |
| `setEncoding("euc-kr")` | 탭의 문자 인코딩 |
| `log.start(path, { mode })`, `log.stop()` | `mode`: `"timestamp"`(기본), `"text"`, `"raw"` |
| `zmodem` / `ymodem` / `xmodem` / `xmodem1k` `.send(파일들)` | 끝날 때까지 기다림, 실패·취소면 예외. 결과 `{ files, bytes, elapsedMs }` |
| `….receive({ dir })` | ZMODEM/YMODEM은 폴더(기본: 다운로드), XMODEM은 `{ file }` 필수 |
| `hdlc.enable({ fcs: 16 })`, `hdlc.disable()` | 32, 0(없음), `addressControl`, `escapeControlChars`, `maxFrame`. 켜면 HDLC 패널도 열림 |
| `hdlc.send(bytes)`, `await hdlc.waitFrame({ filter })`, `hdlc.onFrame(fn)` | 프레임 `{ payload, fcsOk, time }`. 기다리기 전에 온 프레임은 1000개까지 보관 |
| `activate()`, `close()` | 탭을 앞으로, 닫기 |
| `name`, `state`, `connected` | |

전송 중에는 받은 데이터가 전송으로 가므로 `waitFor`에는 보이지 않습니다(터미널과 같음).

## 4. ui와 fs

| 함수 | 설명 |
|---|---|
| `ui.print(...)`, `ui.error(...)` | 출력 패널 |
| `await ui.prompt(msg, 기본값)` | 취소하면 `null` |
| `await ui.password(msg)` | 화면·로그·디스크에 남지 않음. 취소하면 `null` |
| `await ui.confirm(msg)` | `true` / `false` |
| `await ui.choose(msg, [옵션…])` | 고른 문자열 또는 `null` |
| `await ui.openFile({ folder })`, `ui.saveFile({ name })` | 파일 선택 창 |
| `ui.status(text)` | 상태 표시줄 문구 (`null`이면 지움) |
| `ui.result("PASS" \| "FAIL", detail)` | 출력 패널에 굵은 PASS/FAIL 줄 |
| `fs.readText`, `readBytes`, `writeText`, `appendText`, `writeBytes`, `exists`, `resolve` | 쓸 때 폴더가 없으면 만듦. 접근 범위 제한 없음(사용자 자신의 스크립트) |

**TFTP 서버** — `tftp`. 상태 표시줄 TFTP 패널과 같은 서버(앱에 하나)입니다. 0.3.0, 2026-11-23 추가.
| 함수 | 설명 |
|---|---|
| `tftp.start({ folder, port, upload, bind })` | `folder`(스크립트 폴더 기준, 기본 `.`)를 공개. 포트 기본 69. `upload: true`일 때만 장치가 올릴 수 있고, 있는 파일은 덮어쓰지 않음. `{ port, folder, addresses }`를 돌려줌. 이미 같은 설정으로 돌고 있으면 그대로 |
| `tftp.stop()`, `tftp.running` | 끄기, 켜져 있는지 |
| `tftp.addresses()` | 이 PC의 IPv4 주소 `{ name, ip, prefix, linkLocal }`. 쓸 수 있는 것 먼저, 169.254.x.x는 `linkLocal` |
| `tftp.transfers()` | 최근 전송 100개 `{ peer, file, upload, size, bytes, blockSize, state, error, … }` |
| `await tftp.waitFor(file, { timeout, upload })` | 지금 진행 중이거나 이후 시작되는 `file` 전송이 끝날 때까지(기본 120초). 실패하면 예외, 시간 초과는 `TimeoutError`. 이름은 대소문자·경로 무시 |

```js
const srv = tftp.start({ folder: "images" });
const pc = srv.addresses.find((a) => !a.linkLocal) ?? srv.addresses[0];
await s.command(`setenv serverip ${pc.ip}`, { prompt: /=> $/ });
s.sendLine("tftpboot ${loadaddr} fw.bin");
const t = await tftp.waitFor("fw.bin");
print(`${t.bytes} bytes in ${t.elapsedMs} ms`);
```
서버는 스크립트가 끝나도 켜진 채로 남습니다(패널에서 보임). 끄려면 `tftp.stop()`.

## 5. 예제

[examples/scripts/](../examples/scripts/): `uboot-tftp.js`(U-Boot 멈추고 TFTP), `modem-info.js`(AT 명령 결과를 TSV로), `openwrt-info.js`(OpenWrt 콘솔에서 펌웨어·커널·가동 시간·메모리·시리얼 포트를 읽어 파일로, 읽기만 함), `reboot-test.js`(SSH로 재부팅, 시리얼로 부팅 확인 반복), `binary-protocol.js`(바이너리 요청/응답을 바이트 단위로, 체크섬 확인. AN-001 7절과 같은 코드이며 `cargo test`로 가짜 장치에 실행). 첫 줄의 `/// <reference path="termworker.d.ts" />`로 VS Code에서 자동 완성과 오류 표시가 됩니다.

## 6. 구조 (개발자용)

- 스크립트마다 스레드 하나, tokio current-thread 런타임 하나, QuickJS 런타임 하나.
- 사용자 API는 `prelude.js`(JavaScript)에 있고, Rust는 작은 네이티브 함수만 제공합니다(`__tw`): 보내기, 이벤트 큐(`next`), UI 요청(`request`), 파일.
- **받기:** 탭마다 `Tap`이 있고, 백엔드 세션의 수신 처리(pump)가 터미널에 보내는 텍스트·받은 원래 바이트(U+0000–U+00FF 문자열로 1바이트당 1글자)·연결 상태·HDLC 프레임을 그 탭을 보는 스크립트들의 큐에 넣습니다. 탭 ID(`TermSession.tabId`)는 재연결해도 바뀌지 않습니다.
- **화면에 보이는 동작**(탭 열기·닫기, 대화상자, 로그, 전송, DTR/RTS, 인코딩, HDLC 켜기)은 `script:request` 이벤트로 UI에 요청하고 `script_reply`로 답을 받습니다. UI 상태와 실제 상태가 어긋나지 않게 하기 위해서입니다.
- `sessions.open`은 스크립트 쪽에서 탭 ID를 먼저 만들고 그 탭을 본 다음 UI에 요청합니다. 연결 직후의 출력(예: 부트 카운트다운)도 놓치지 않습니다.
- 타이머와 대기 시간 제한은 prelude의 이벤트 루프가 처리합니다(네이티브 `next(timeout)` 하나).
- Stop: 실행 중인 JS는 QuickJS 인터럽트로, 대기 중인 네이티브 호출은 취소 신호로 끝냅니다.

## 7. 다음 단계 (아직 없음)

- 트리거: "패턴이 보이면 스크립트 실행"을 세션마다 저장
- 키 입력 녹화 → 스크립트 생성
- 앱 안 편집기
- 외부 Python 연결(같은 API)
- 명령줄 실행(`termworker --script x.js`)과 `args`, 탭 없이 여는 세션(양산 라인용)
- 연결 중 속도 변경(`setBaud`): 백엔드에 기능이 없음

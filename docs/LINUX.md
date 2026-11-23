# Ubuntu / Debian에서 빌드하고 시험하기

Ubuntu 22.04 / 24.04 기준입니다(Debian 12도 같습니다). 아직 리눅스에서 한 번도 빌드하지 않았으므로 **처음 빌드에서 고칠 곳이 나올 수 있습니다.** 오류가 나면 그 출력을 그대로 남겨 주세요.

## 1. 필요한 패키지

```bash
sudo apt update
sudo apt install -y build-essential curl wget file pkg-config git \
  libwebkit2gtk-4.1-dev libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev \
  libudev-dev \
  fonts-dejavu fonts-noto-cjk \
  lrzsz
```

- `libwebkit2gtk-4.1-dev` 등: Tauri 화면(WebKitGTK)
- `libudev-dev`: 시리얼 포트 목록(USB 설명, VID:PID)
- `fonts-noto-cjk`: 터미널의 한글
- `lrzsz`: ZMODEM/YMODEM/XMODEM 상호운용 시험용(`sz`, `rz`, `sb`, `rb`, `sx`, `rx`)

Rust와 Node.js:

```bash
# Rust (rust-version 1.90 이상)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Node.js 20 LTS 이상 (배포판 기본 nodejs가 오래됐으면 NodeSource 사용)
curl -fsSL https://deb.nodesource.com/setup_22.x | sudo -E bash -
sudo apt install -y nodejs
```

## 2. 시리얼 포트 권한

```bash
sudo usermod -aG dialout $USER
# 로그아웃 후 다시 로그인해야 적용됩니다. 확인:
groups | grep dialout
```

권한이 없으면 TermWorker가 "permission denied. Add your user to the "dialout" group…"이라고 안내합니다.

## 3. 소스 받기

```bash
git clone https://github.com/donghyunpark-dhplab/TERMWorker.git
cd TERMWorker
```

## 4. 빌드와 실행

```bash
npm install
npm run build                    # 프런트엔드 타입 검사 + 빌드
(cd src-tauri && cargo test)     # 백엔드 시험 (하드웨어 시험은 기본 제외)
npm run tauri dev                # 앱 실행
npx tauri build                  # 설치 파일: src-tauri/target/release/bundle/{deb,appimage}/
```

`npx tauri build`로 만든 설치 파일은 **빌드한 PC의 glibc 버전 이상**에서만 실행됩니다. 예를 들어 26.04에서 빌드하면 24.04와 22.04에서는 실행되지 않습니다. 이 파일은 시험용으로만 씁니다.

### 배포용 설치 파일

```bash
scripts/release/linux/build.sh   # Docker 필요(docker 그룹). 결과: release/linux/
```

- Ubuntu 22.04 컨테이너 안에서 deb와 AppImage를 만듭니다. Ubuntu 22.04 이상에서 실행되는 것이 목표입니다.
- 실행 파일과 AppImage 안의 모든 라이브러리가 glibc 2.35 이하만 요구하는지 검사합니다. 하나라도 넘으면 실패합니다.
- 결과 폴더에 `SHA256SUMS`도 함께 만듭니다.
- 작업 트리를 그대로 복사해서 빌드합니다. 커밋하지 않은 변경도 들어갑니다. 판매용 파일은 깨끗한 트리에서 만듭니다.
- 두 번째 빌드부터는 Docker 볼륨 `termworker-*`의 캐시를 써서 빨라집니다.
- AppImage에는 WebKitGTK까지 들어 있지만, 그래픽 드라이버 계열(libgbm, EGL/GLES)과 libwayland-client는 넣지 않습니다. 데스크톱이 설치된 Ubuntu에는 항상 있습니다. 서버용(데스크톱 없는) 설치에서는 실행되지 않습니다.
- deb는 `libwebkit2gtk-4.1-0`, `libgtk-3-0`을 요구하고, `sudo apt install ./TermWorker_<버전>_amd64.deb`로 설치하면 함께 설치됩니다.

## 5. 확인할 것 (체크리스트)

**화면**
- [ ] 창이 뜨고 글꼴이 깨지지 않음(UI, 터미널). 한글 2칸 문자 정렬
- [ ] 터미널 렌더링(WebGL이 안 되면 자동으로 일반 렌더링으로 바뀜). 스크롤이 버벅이지 않는지
- [ ] 파일 열기/저장/폴더 선택 창(로그 저장, Transfer 메뉴)

**시리얼** (어댑터 이름은 `ls -l /dev/serial/by-id/`로 확인)
- [ ] 포트 콤보 박스에 `/dev/ttyUSB0` 등과 설명·VID:PID가 나옴
- [ ] 연결, 입력 에코, DTR/RTS, Break
- [ ] 다른 프로그램이 쓰는 포트 → "the port is in use by another application"
- [ ] USB를 뽑았다 꽂기 → 자동 재연결

**SSH / Telnet**: 기존 서버에 접속

## 6. 하드웨어 시험 (교차 연결된 어댑터 두 개)

Windows와 같은 시험을 포트 이름만 바꿔 실행합니다. 포트 이름은 `ls -l /dev/serial/by-id/`로 확인합니다. 아래는 두 어댑터가 `/dev/ttyUSB2`와 `/dev/ttyUSB3`인 예입니다.

```bash
cd src-tauri
export TW_HW_PORT=/dev/ttyUSB2 TW_HW_PEER=/dev/ttyUSB3 TW_HW_MAX_BAUD=460800
cargo test --lib -- --ignored --test-threads=1 --nocapture hw_ --skip hw_unplug --skip hw_ext --skip hw_file_transfer --skip hw_lrzsz
cargo test --lib -- --ignored --nocapture hw_file_transfer
# lrzsz 상호운용: 우리 쪽은 TW_HW_PORT, lrzsz(sz/rz/sb/rb/sx/rx)는 TW_HW_PEER. TW_HW_XFER_SECS=15 로 큰 파일
cargo test --lib -- --ignored --nocapture hw_lrzsz_interop
cargo test --lib -- --ignored --nocapture hw_hdlc_frames      # HDLC 프레임 양방향
TW_HW_UNPLUG=/dev/ttyUSB2 cargo test --lib -- --ignored --nocapture hw_unplug_reconnect   # 사람이 뽑고 꽂기
```

`dialout` 그룹에 막 추가해서 다시 로그인하기 전이라면, 시험 명령을 `sg dialout -c "…"`로 감싸 실행합니다.

`hw_ext_*` 시험은 Windows 전용 .NET 스크립트(`serialpeer.ps1`)가 필요하므로 리눅스에서는 건너뜁니다.

참고: 한쪽 방향만 비트 오류가 나거나 효율이 낮으면 배선(특히 GND) 문제일 가능성이 큽니다.

## 7. ZMODEM 상호운용 시험 (lrzsz, 수동)

6절의 `hw_lrzsz_interop`가 아래 시험을 자동으로 합니다(세션 4에서 8가지 모두 통과). 아래는 앱 화면에서 직접 확인할 때의 방법입니다.

같은 PC에서 어댑터 하나는 TermWorker가, 다른 하나는 lrzsz가 씁니다. 다른 구현과의 호환을 확인하는 가장 중요한 시험입니다.

```bash
# lrzsz 쪽 포트를 raw 모드로 (TermWorker는 /dev/ttyUSB0에 115200으로 연결해 둔 상태)
stty -F /dev/ttyUSB1 115200 raw -echo -ixon -ixoff -crtscts

# ① lrzsz → TermWorker (TermWorker에 "Receive files" 창이 떠야 함)
head -c 300000 /dev/urandom > /tmp/test.bin
sz /tmp/test.bin < /dev/ttyUSB1 > /dev/ttyUSB1

# ② TermWorker → lrzsz (TermWorker에 "Send files" 창이 떠야 함)
mkdir -p /tmp/rx && cd /tmp/rx && rz < /dev/ttyUSB1 > /dev/ttyUSB1

# ③ YMODEM / XMODEM: TermWorker에서 Transfer 메뉴로 반대쪽을 시작
sb /tmp/test.bin < /dev/ttyUSB1 > /dev/ttyUSB1          # TermWorker: Receive with YMODEM
cd /tmp/rx && rb < /dev/ttyUSB1 > /dev/ttyUSB1          # TermWorker: Send with YMODEM
sx /tmp/test.bin < /dev/ttyUSB1 > /dev/ttyUSB1          # TermWorker: Receive with XMODEM
cd /tmp/rx && rx out.bin < /dev/ttyUSB1 > /dev/ttyUSB1  # TermWorker: Send with XMODEM

# 결과 비교
sha256sum /tmp/test.bin /tmp/rx/* ~/Downloads/test.bin
```

SSH로도 같은 시험을 할 수 있습니다. TermWorker로 서버에 접속해 셸에서 `sz 파일` 또는 `rz`를 실행하면 됩니다.

## 8. 자동 화면 시험 (Xvfb)

Ubuntu 26.04의 GNOME(Wayland)은 다른 프로그램이 창에 키·마우스 입력을 보내는 것을 막습니다(Xwayland `-enable-ei-portal`). 캡처는 되지만 조작은 안 되므로, 가상 X 화면에서 앱을 띄워 시험합니다.

```bash
sudo apt install -y xvfb                     # 한 번만
Xvfb :99 -screen 0 1400x900x24 -nolisten tcp &
T=$(mktemp -d)                               # 시험용 설정 폴더: 사용자 설정(~/.config/com.dhplab.termworker)을 건드리지 않음
env -u WAYLAND_DISPLAY DISPLAY=:99 GDK_BACKEND=x11 \
  XDG_CONFIG_HOME=$T/config XDG_DATA_HOME=$T/data XDG_CACHE_HOME=$T/cache npm run tauri dev &

export DISPLAY=:99
python3 scripts/dev/x11.py shot /tmp/a.png        # 캡처
python3 scripts/dev/x11.py click 125 64           # 창 기준 좌표 클릭
python3 scripts/dev/x11.py type "raw 127.0.0.1:5555"
python3 scripts/dev/x11.py key enter
xwininfo -root -tree                              # 파일 선택 창 등의 창 ID → x11.py의 [title] 자리에 0x… 로 지정
```

- Xvfb에는 창 관리자가 없어 제목 표시줄이 없습니다(좌표가 실제 데스크톱과 다름). GPU가 없어 WebGL 대신 일반 렌더링입니다.
- 시험용 설정 폴더를 쓰면 수신 기본 폴더가 홈으로 나옵니다. **받기 전에 저장 위치를 꼭 확인**하고 임시 폴더를 고릅니다.
- 실제 데스크톱 화면은 `GDK_BACKEND=x11`로 띄우면 `x11.py shot`으로 캡처만 할 수 있습니다.

### Telnet 시험 (busybox telnetd)

```bash
env TERM=xterm-256color busybox telnetd -F -K -b 127.0.0.1:2323 -l /bin/bash &   # 이 PC 안에서만 접속 가능
# 앱: 빠른 연결 "telnet 127.0.0.1:2323"
cd src-tauri && cargo test --lib -- --ignored --nocapture zmodem_over_busybox_telnetd   # Telnet 위 sz/rz 자동 시험
```

busybox telnetd는 TTYPE을 협상하지 않으므로 셸의 `TERM`은 telnetd를 띄운 환경의 값이 됩니다.

## 9. 알려 줄 것

- 빌드 오류가 났다면 그 출력 전체
- 체크리스트에서 안 된 항목과 스크린샷
- 하드웨어 시험 출력(`hw_` 시험, `hw_file_transfer`)
- lrzsz 시험에서 해시가 일치했는지, 걸린 시간

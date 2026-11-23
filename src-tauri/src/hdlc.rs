//! Asynchronous HDLC-like framing (RFC 1662): `7E` flags, `7D` escapes and a
//! frame check sequence. Used to send and inspect frames on a serial link; the
//! protocol carried inside the frames is up to the device.
//!
//! Receiving: everything between two flags is a frame candidate. A candidate
//! whose FCS checks out is a frame. One that fails but is plain text (a device
//! that mixes console output with frames) goes back to the terminal; anything
//! else is reported as a damaged frame. Bytes outside frames go to the terminal.

use serde::{Deserialize, Serialize};

pub const FLAG: u8 = 0x7E;
pub const ESCAPE: u8 = 0x7D;
const XOR: u8 = 0x20;
const ADDRESS: u8 = 0xFF;
const CONTROL: u8 = 0x03;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Fcs {
    None,
    #[default]
    Fcs16,
    Fcs32,
}

impl Fcs {
    fn len(self) -> usize {
        match self {
            Fcs::None => 0,
            Fcs::Fcs16 => 2,
            Fcs::Fcs32 => 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HdlcConfig {
    pub fcs: Fcs,
    /// Prefix frames with address `FF` and control `03` (PPP style), and strip them on receive.
    pub address_control: bool,
    /// Also escape control characters `00`–`1F` (RFC 1662 default ACCM); otherwise only `7E` and `7D`.
    pub escape_control_chars: bool,
    /// Longest frame accepted on receive (after unescaping). Longer runs without a flag are treated as text.
    pub max_frame: usize,
}

impl Default for HdlcConfig {
    fn default() -> Self {
        Self { fcs: Fcs::Fcs16, address_control: false, escape_control_chars: false, max_frame: 4096 }
    }
}

/// CRC-16/X-25 as used by RFC 1662 (reflected 0x1021, init and final XOR 0xFFFF).
pub fn fcs16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x8408 } else { crc >> 1 };
        }
    }
    !crc
}

/// CRC-32 (the zlib / RFC 1662 32-bit FCS).
pub fn fcs32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn must_escape(cfg: &HdlcConfig, b: u8) -> bool {
    b == FLAG || b == ESCAPE || (cfg.escape_control_chars && b < 0x20)
}

/// One complete frame on the wire, flags included.
pub fn encode(cfg: &HdlcConfig, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(payload.len() + 6);
    if cfg.address_control {
        body.extend_from_slice(&[ADDRESS, CONTROL]);
    }
    body.extend_from_slice(payload);
    match cfg.fcs {
        Fcs::None => {}
        Fcs::Fcs16 => body.extend_from_slice(&fcs16(&body).to_le_bytes()),
        Fcs::Fcs32 => body.extend_from_slice(&fcs32(&body).to_le_bytes()),
    }
    let mut out = Vec::with_capacity(body.len() + body.len() / 16 + 2);
    out.push(FLAG);
    for b in body {
        if must_escape(cfg, b) {
            out.extend_from_slice(&[ESCAPE, b ^ XOR]);
        } else {
            out.push(b);
        }
    }
    out.push(FLAG);
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Contents without flags, escapes, address/control (if configured) and FCS.
    pub payload: Vec<u8>,
    pub fcs_ok: bool,
    /// Bytes on the wire for this frame, without its flags.
    pub wire_len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// Bytes for the terminal.
    Text(Vec<u8>),
    Frame(Frame),
}

pub struct Decoder {
    cfg: HdlcConfig,
    /// Inside a frame candidate (a flag has been seen).
    inside: bool,
    /// The flag that opened the current candidate closed a valid frame, so it is
    /// not text even if this candidate turns out to be.
    opened_by_frame: bool,
    escaped: bool,
    /// Unescaped candidate bytes.
    body: Vec<u8>,
    /// The same bytes as received, to hand back to the terminal if it is not a frame.
    raw: Vec<u8>,
    text: Vec<u8>,
}

impl Decoder {
    pub fn new(cfg: HdlcConfig) -> Self {
        Self {
            cfg,
            inside: false,
            opened_by_frame: false,
            escaped: false,
            body: Vec::new(),
            raw: Vec::new(),
            text: Vec::new(),
        }
    }

    pub fn config(&self) -> &HdlcConfig {
        &self.cfg
    }

    pub fn feed(&mut self, input: &[u8]) -> Vec<Item> {
        let mut out = Vec::new();
        for &b in input {
            if b == FLAG {
                if self.inside {
                    self.close_candidate(&mut out);
                } else {
                    self.inside = true;
                    self.opened_by_frame = false;
                }
                continue;
            }
            if !self.inside {
                self.text.push(b);
                continue;
            }
            self.raw.push(b);
            if self.escaped {
                self.body.push(b ^ XOR);
                self.escaped = false;
            } else if b == ESCAPE {
                self.escaped = true;
            } else {
                self.body.push(b);
            }
            if self.body.len() > self.cfg.max_frame {
                // No closing flag in sight: this was not a frame.
                self.candidate_to_text();
            }
        }
        self.flush_text(&mut out);
        out
    }

    /// The line went quiet with a candidate still open: hand it back as text
    /// (e.g. a shell prompt containing "~") or report it as a cut-off frame.
    pub fn flush(&mut self) -> Vec<Item> {
        let mut out = Vec::new();
        if self.inside {
            if self.raw.is_empty() || looks_like_text(&self.raw) {
                self.candidate_to_text();
            } else {
                let frame = Frame { payload: strip(&self.cfg, &self.body), fcs_ok: false, wire_len: self.raw.len() };
                self.reset_candidate();
                self.inside = false;
                self.flush_text(&mut out);
                out.push(Item::Frame(frame));
            }
        }
        self.flush_text(&mut out);
        out
    }

    /// Bytes are held back waiting for a closing flag.
    pub fn pending(&self) -> bool {
        self.inside
    }

    fn candidate_to_text(&mut self) {
        if !self.opened_by_frame {
            self.text.push(FLAG);
        }
        self.text.extend_from_slice(&self.raw);
        self.reset_candidate();
        self.inside = false;
        self.opened_by_frame = false;
    }

    fn reset_candidate(&mut self) {
        self.body.clear();
        self.raw.clear();
        self.escaped = false;
    }

    fn flush_text(&mut self, out: &mut Vec<Item>) {
        if !self.text.is_empty() {
            out.push(Item::Text(std::mem::take(&mut self.text)));
        }
    }

    /// A flag ended the current candidate; that flag also opens the next one.
    fn close_candidate(&mut self, out: &mut Vec<Item>) {
        if self.raw.is_empty() {
            // Back-to-back flags. Keep `opened_by_frame` as it was, but a lone
            // flag in text ("~~") must still reach the terminal.
            if !self.opened_by_frame {
                self.text.push(FLAG);
            }
            return;
        }
        if let Some(frame) = self.check() {
            let frame = Frame { wire_len: self.raw.len(), ..frame };
            self.reset_candidate();
            self.flush_text(out);
            out.push(Item::Frame(frame));
            self.opened_by_frame = true;
        } else if looks_like_text(&self.raw) {
            // Console output: give back the opening flag (unless it closed a frame)
            // and the bytes. The closing flag is undecided: it opens the next candidate.
            if !self.opened_by_frame {
                self.text.push(FLAG);
            }
            self.text.extend_from_slice(&self.raw);
            self.reset_candidate();
            self.opened_by_frame = false;
        } else {
            let frame = Frame { payload: strip(&self.cfg, &self.body), fcs_ok: false, wire_len: self.raw.len() };
            self.reset_candidate();
            self.flush_text(out);
            out.push(Item::Frame(frame));
            self.opened_by_frame = true;
        }
    }

    /// The candidate as a frame, if its FCS checks out.
    fn check(&self) -> Option<Frame> {
        let body = &self.body;
        let n = self.cfg.fcs.len();
        if self.escaped || body.len() <= n && n > 0 {
            return None;
        }
        let data = &body[..body.len() - n];
        let ok = match self.cfg.fcs {
            Fcs::None => true,
            Fcs::Fcs16 => fcs16(data).to_le_bytes() == body[data.len()..],
            Fcs::Fcs32 => fcs32(data).to_le_bytes() == body[data.len()..],
        };
        ok.then(|| Frame { payload: strip(&self.cfg, data), fcs_ok: true, wire_len: 0 })
    }
}

/// Removes the address/control prefix if configured and present.
fn strip(cfg: &HdlcConfig, data: &[u8]) -> Vec<u8> {
    if cfg.address_control && data.len() >= 2 && data[0] == ADDRESS && data[1] == CONTROL {
        data[2..].to_vec()
    } else {
        data.to_vec()
    }
}

/// Printable text, tabs, line ends and escape sequences only (UTF-8 allowed).
fn looks_like_text(raw: &[u8]) -> bool {
    std::str::from_utf8(raw)
        .is_ok_and(|s| s.chars().all(|c| !c.is_control() || matches!(c, '\r' | '\n' | '\t' | '\x1b' | '\x08' | '\x07')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_values() {
        // CRC catalogue check values for "123456789".
        assert_eq!(fcs16(b"123456789"), 0x906E);
        assert_eq!(fcs32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn roundtrip_with_escapes() {
        for fcs in [Fcs::None, Fcs::Fcs16, Fcs::Fcs32] {
            for address_control in [false, true] {
                for escape_control_chars in [false, true] {
                    let cfg = HdlcConfig { fcs, address_control, escape_control_chars, max_frame: 4096 };
                    let payload: Vec<u8> = (0..=255u8).chain([FLAG, ESCAPE, FLAG, 0, 0x11]).collect();
                    let wire = encode(&cfg, &payload);
                    assert_eq!(wire.iter().filter(|&&b| b == FLAG).count(), 2, "flags only at the ends");
                    let mut d = Decoder::new(cfg.clone());
                    let items = d.feed(&wire);
                    assert_eq!(items.len(), 1, "{cfg:?}: {items:?}");
                    let Item::Frame(f) = &items[0] else { panic!("{items:?}") };
                    assert!(f.fcs_ok);
                    assert_eq!(f.payload, payload, "{cfg:?}");
                }
            }
        }
    }

    #[test]
    fn frames_split_across_reads_and_shared_flags() {
        let cfg = HdlcConfig::default();
        let a = encode(&cfg, b"first");
        let b = encode(&cfg, b"second");
        // Shared flag between frames, as PPP does.
        let mut wire = a.clone();
        wire.extend_from_slice(&b[1..]);
        let mut d = Decoder::new(cfg);
        let mut items = Vec::new();
        for chunk in wire.chunks(3) {
            items.extend(d.feed(chunk));
        }
        let frames: Vec<_> =
            items.iter().filter_map(|i| if let Item::Frame(f) = i { Some(f.payload.clone()) } else { None }).collect();
        assert_eq!(frames, vec![b"first".to_vec(), b"second".to_vec()]);
    }

    #[test]
    fn console_text_around_frames_reaches_the_terminal() {
        let cfg = HdlcConfig::default();
        let mut wire = b"U-Boot> boot\r\n".to_vec();
        wire.extend(encode(&cfg, &[1, 2, 3]));
        wire.extend_from_slice(b"login: ");
        wire.extend(encode(&cfg, &[4]));
        let mut d = Decoder::new(cfg);
        let mut items = d.feed(&wire);
        items.extend(d.flush());
        let text: Vec<u8> =
            items.iter().filter_map(|i| if let Item::Text(t) = i { Some(t.clone()) } else { None }).flatten().collect();
        let frames = items.iter().filter(|i| matches!(i, Item::Frame(_))).count();
        assert_eq!(frames, 2);
        // "login: " sits between the closing flag of one frame and the opening of the
        // next, so it is a candidate that fails the FCS and is handed back as text.
        assert_eq!(String::from_utf8_lossy(&text), "U-Boot> boot\r\nlogin: ");
    }

    #[test]
    fn tildes_in_ordinary_text_survive() {
        let mut d = Decoder::new(HdlcConfig::default());
        let mut items = d.feed(b"root@host:~# cd ~/x && echo a~~b\r\n");
        items.extend(d.feed(b"root@host:~/x# "));
        items.extend(d.flush());
        let text: Vec<u8> =
            items.iter().filter_map(|i| if let Item::Text(t) = i { Some(t.clone()) } else { None }).flatten().collect();
        assert!(items.iter().all(|i| matches!(i, Item::Text(_))), "{items:?}");
        assert_eq!(String::from_utf8_lossy(&text), "root@host:~# cd ~/x && echo a~~b\r\nroot@host:~/x# ");
    }

    #[test]
    fn damaged_frame_is_reported() {
        let cfg = HdlcConfig::default();
        let mut wire = encode(&cfg, &[0x10, 0x20, 0x30, 0x80, 0x90]);
        wire[3] ^= 0x01;
        let mut d = Decoder::new(cfg);
        let items = d.feed(&wire);
        assert!(matches!(&items[..], [Item::Frame(Frame { fcs_ok: false, .. })]), "{items:?}");
    }

    #[test]
    fn unterminated_run_falls_back_to_text() {
        let cfg = HdlcConfig { max_frame: 16, ..HdlcConfig::default() };
        let mut d = Decoder::new(cfg);
        let mut items = d.feed(b"~this is not a frame at all, just long text");
        items.extend(d.flush());
        let text: Vec<u8> =
            items.iter().filter_map(|i| if let Item::Text(t) = i { Some(t.clone()) } else { None }).flatten().collect();
        assert_eq!(text, b"~this is not a frame at all, just long text");
    }
}

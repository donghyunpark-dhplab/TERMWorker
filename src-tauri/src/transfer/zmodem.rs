//! ZMODEM, following Chuck Forsberg's protocol description (zmodem.txt).
//!
//! Frames start with a header (hex, 16-bit binary or 32-bit binary) and may
//! carry data subpackets. Inside binary frames the byte ZDLE (0x18) escapes
//! control characters and marks the end of a subpacket (ZCRCE/G/Q/W). The
//! sender streams subpackets without waiting; the receiver asks for a resend
//! from a given offset (ZRPOS) when something arrives damaged.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::crc::{crc16, crc16_update, crc32, crc32_update};
use super::{
    file_err, mtime_secs, protocol_err, safe_file_name, send_name, set_mtime, unique_path, Chan, Error, Report, Result,
};

const ZPAD: u8 = b'*';
const ZDLE: u8 = 0x18;
const ZBIN: u8 = b'A';
const ZHEX: u8 = b'B';
const ZBIN32: u8 = b'C';
const XON: u8 = 0x11;
const XOFF: u8 = 0x13;

// Frame types.
const ZRQINIT: u8 = 0;
const ZRINIT: u8 = 1;
const ZSINIT: u8 = 2;
const ZACK: u8 = 3;
const ZFILE: u8 = 4;
const ZSKIP: u8 = 5;
const ZNAK: u8 = 6;
const ZABORT: u8 = 7;
const ZFIN: u8 = 8;
const ZRPOS: u8 = 9;
const ZDATA: u8 = 10;
const ZEOF: u8 = 11;
const ZFERR: u8 = 12;
const ZCRC: u8 = 13;
const ZCHALLENGE: u8 = 14;
const ZCOMPL: u8 = 15;
const ZFREECNT: u8 = 17;
const ZCOMMAND: u8 = 18;

// Subpacket ends (after ZDLE).
const ZCRCE: u8 = b'h'; // end of frame, header follows
const ZCRCG: u8 = b'i'; // more data follows, no reply
const ZCRCQ: u8 = b'j'; // more data follows, ZACK expected
const ZCRCW: u8 = b'k'; // end of frame, ZACK expected
const ZRUB0: u8 = b'l'; // escaped 0x7F
const ZRUB1: u8 = b'm'; // escaped 0xFF

// ZRINIT capability flags (ZF0).
const CANFDX: u8 = 0x01;
const CANOVIO: u8 = 0x02;
const CANFC32: u8 = 0x20;
const ESCCTL: u8 = 0x40;
// ZFILE conversion option (ZF0): binary.
const ZCBIN: u8 = 1;

const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
/// After a ZRINIT in reply to a file offer: how long to wait for the real answer.
const STALE_RINIT_WAIT: Duration = Duration::from_secs(2);
const DATA_TIMEOUT: Duration = Duration::from_secs(10);
const SUBPACKET: usize = 1024;
/// Largest subpacket accepted (some senders use 8 KiB).
const MAX_SUBPACKET: usize = 16 * 1024;
/// Garbage tolerated while looking for a header (the sender may still be streaming).
const GARBAGE_LIMIT: usize = 256 * 1024;
const MAX_ERRORS: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Header {
    kind: u8,
    /// ZP0..ZP3 (a little-endian position), or ZF3..ZF0 (flags, ZF0 last).
    d: [u8; 4],
}

impl Header {
    fn pos(kind: u8, pos: u64) -> Self {
        Header { kind, d: (pos as u32).to_le_bytes() }
    }
    fn flags(kind: u8, f0: u8) -> Self {
        Header { kind, d: [0, 0, 0, f0] }
    }
    fn position(&self) -> u64 {
        u32::from_le_bytes(self.d) as u64
    }
    fn f0(&self) -> u8 {
        self.d[3]
    }
    fn bytes(&self) -> [u8; 5] {
        [self.kind, self.d[0], self.d[1], self.d[2], self.d[3]]
    }
}

enum Got {
    Header(Header),
    /// Something header-like that failed its CRC, or too much noise.
    Garbage,
    Timeout,
}

/// One unescaped byte inside a binary frame.
enum Zb {
    Data(u8),
    End(u8),
    Bad,
    Timeout,
}

enum Sub {
    Ok(Vec<u8>, u8),
    Bad,
    Timeout,
}

struct Z<'c, 'a> {
    ch: &'c mut Chan<'a>,
    /// CRC of incoming data subpackets: follows the form of the last binary header.
    rx_crc32: bool,
    tx_crc32: bool,
    escape_ctl: bool,
    /// The link is Telnet: also escape every CR (servers rewrite "CR LF" and "CR NUL")
    /// and NUL (busybox telnetd drops a NUL that ends one of its socket reads).
    telnet_safe: bool,
    last_sent: u8,
}

impl<'c, 'a> Z<'c, 'a> {
    fn new(ch: &'c mut Chan<'a>) -> Self {
        Self { ch, rx_crc32: false, tx_crc32: false, escape_ctl: false, telnet_safe: false, last_sent: 0 }
    }

    // ---------------------------------------------------------------- output

    fn escape_into(&mut self, out: &mut Vec<u8>, data: &[u8]) {
        for &b in data {
            let escape = match b {
                ZDLE | 0x10 | 0x90 | XON | XOFF | 0x91 | 0x93 => true,
                // Telenet treats "@ CR" as a command; break it up.
                0x0d | 0x8d => self.telnet_safe || self.last_sent & 0x7f == b'@',
                0x00 => self.telnet_safe,
                _ => self.escape_ctl && b & 0x60 == 0,
            };
            if escape {
                out.push(ZDLE);
                out.push(b ^ 0x40);
                self.last_sent = b ^ 0x40;
            } else {
                out.push(b);
                self.last_sent = b;
            }
        }
    }

    fn send_hex(&mut self, h: Header) -> Result<()> {
        let bytes = h.bytes();
        let mut out = vec![ZPAD, ZPAD, ZDLE, ZHEX];
        for b in bytes.iter().chain(crc16(&bytes).to_be_bytes().iter()) {
            out.extend_from_slice(format!("{b:02x}").as_bytes());
        }
        out.extend_from_slice(&[0x0d, 0x8a]);
        if h.kind != ZFIN && h.kind != ZACK {
            out.push(XON);
        }
        self.last_sent = *out.last().unwrap();
        self.ch.write(&out)
    }

    fn send_bin(&mut self, h: Header) -> Result<()> {
        let bytes = h.bytes();
        let mut out = vec![ZPAD, ZDLE, if self.tx_crc32 { ZBIN32 } else { ZBIN }];
        self.escape_into(&mut out, &bytes);
        if self.tx_crc32 {
            self.escape_into(&mut out, &crc32(&bytes).to_le_bytes());
        } else {
            self.escape_into(&mut out, &crc16(&bytes).to_be_bytes());
        }
        self.ch.write(&out)
    }

    fn send_data(&mut self, data: &[u8], end: u8) -> Result<()> {
        let mut out = Vec::with_capacity(data.len() + data.len() / 8 + 16);
        self.escape_into(&mut out, data);
        out.push(ZDLE);
        out.push(end);
        self.last_sent = end;
        if self.tx_crc32 {
            let c = !crc32_update(crc32_update(0xFFFF_FFFF, data), &[end]);
            self.escape_into(&mut out, &c.to_le_bytes());
        } else {
            let c = crc16_update(crc16(data), &[end]);
            self.escape_into(&mut out, &c.to_be_bytes());
        }
        if end == ZCRCW {
            out.push(XON);
        }
        self.ch.write(&out)
    }

    // ----------------------------------------------------------------- input

    /// Finds and decodes the next header. Five CANs in a row mean the remote aborted.
    fn read_header(&mut self, timeout: Duration) -> Result<Got> {
        let deadline = Instant::now() + timeout;
        let mut garbage = 0usize;
        let mut cans = 0u32;
        loop {
            let Some(c) = self.next_counting(deadline, &mut cans)? else {
                return Ok(Got::Timeout);
            };
            if c & 0x7f != ZPAD {
                garbage += 1;
                if garbage > GARBAGE_LIMIT {
                    return Ok(Got::Garbage);
                }
                continue;
            }
            // One or more ZPADs, then ZDLE and the header format.
            let mut c = c;
            while c & 0x7f == ZPAD {
                let Some(n) = self.next_counting(deadline, &mut cans)? else {
                    return Ok(Got::Timeout);
                };
                c = n;
            }
            if c != ZDLE {
                garbage += 1;
                continue;
            }
            let Some(fmt) = self.next_counting(deadline, &mut cans)? else {
                return Ok(Got::Timeout);
            };
            return match fmt {
                ZHEX => self.read_hex_header(deadline),
                ZBIN => self.read_bin_header(false),
                ZBIN32 => self.read_bin_header(true),
                ZDLE => {
                    // Part of a CAN run (ZDLE == CAN): keep counting.
                    continue;
                }
                _ => {
                    garbage += 1;
                    continue;
                }
            };
        }
    }

    /// Next byte before `deadline`, counting CANs (== ZDLE) in a row.
    fn next_counting(&mut self, deadline: Instant, cans: &mut u32) -> Result<Option<u8>> {
        let b = self.ch.byte(deadline.saturating_duration_since(Instant::now()))?;
        if b == Some(ZDLE) {
            *cans += 1;
            if *cans >= 5 {
                return Err(Error::RemoteCancelled);
            }
        } else {
            *cans = 0;
        }
        Ok(b)
    }

    fn read_hex_header(&mut self, deadline: Instant) -> Result<Got> {
        let mut bytes = [0u8; 7];
        for slot in bytes.iter_mut() {
            let mut pair = [0u8; 2];
            for p in pair.iter_mut() {
                let left = deadline.saturating_duration_since(Instant::now()).max(Duration::from_secs(1));
                let Some(c) = self.ch.byte(left)? else {
                    return Ok(Got::Timeout);
                };
                let Some(v) = ((c & 0x7f) as char).to_digit(16) else {
                    return Ok(Got::Garbage);
                };
                *p = v as u8;
            }
            *slot = pair[0] << 4 | pair[1];
        }
        // Throw away the CR LF that follows a hex header.
        if let Some(c) = self.ch.byte(Duration::from_millis(100))? {
            if c & 0x7f == 0x0d {
                let _ = self.ch.byte(Duration::from_millis(100))?;
            } else {
                self.ch.unread(c);
            }
        }
        if crc16(&bytes[..5]) != u16::from_be_bytes([bytes[5], bytes[6]]) {
            return Ok(Got::Garbage);
        }
        self.rx_crc32 = false;
        Ok(Got::Header(Header { kind: bytes[0], d: [bytes[1], bytes[2], bytes[3], bytes[4]] }))
    }

    fn read_bin_header(&mut self, crc32_form: bool) -> Result<Got> {
        let n = if crc32_form { 9 } else { 7 };
        let mut bytes = [0u8; 9];
        for slot in bytes.iter_mut().take(n) {
            match self.zdl(Duration::from_secs(2))? {
                Zb::Data(b) => *slot = b,
                Zb::Timeout => return Ok(Got::Timeout),
                Zb::End(_) | Zb::Bad => return Ok(Got::Garbage),
            }
        }
        let ok = if crc32_form {
            crc32(&bytes[..5]) == u32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]])
        } else {
            crc16(&bytes[..5]) == u16::from_be_bytes([bytes[5], bytes[6]])
        };
        if !ok {
            return Ok(Got::Garbage);
        }
        self.rx_crc32 = crc32_form;
        Ok(Got::Header(Header { kind: bytes[0], d: [bytes[1], bytes[2], bytes[3], bytes[4]] }))
    }

    /// Reads one byte of a binary frame, undoing ZDLE escapes.
    fn zdl(&mut self, timeout: Duration) -> Result<Zb> {
        loop {
            let Some(c) = self.ch.byte(timeout)? else {
                return Ok(Zb::Timeout);
            };
            match c {
                ZDLE => break,
                // Unescaped flow-control characters are line noise, not data.
                XON | XOFF | 0x91 | 0x93 => continue,
                _ => return Ok(Zb::Data(c)),
            }
        }
        loop {
            let Some(c) = self.ch.byte(timeout)? else {
                return Ok(Zb::Timeout);
            };
            return Ok(match c {
                ZDLE => {
                    // ZDLE is also CAN: five in a row abort the session.
                    let mut cans = 2;
                    loop {
                        match self.ch.byte(timeout)? {
                            Some(ZDLE) => {
                                cans += 1;
                                if cans >= 5 {
                                    return Err(Error::RemoteCancelled);
                                }
                            }
                            Some(other) => {
                                self.ch.unread(other);
                                return Ok(Zb::Bad);
                            }
                            None => return Ok(Zb::Timeout),
                        }
                    }
                }
                ZCRCE | ZCRCG | ZCRCQ | ZCRCW => Zb::End(c),
                ZRUB0 => Zb::Data(0x7f),
                ZRUB1 => Zb::Data(0xff),
                XON | XOFF | 0x91 | 0x93 => continue,
                c if c & 0x60 == 0x40 => Zb::Data(c ^ 0x40),
                _ => Zb::Bad,
            });
        }
    }

    fn read_data(&mut self, max: usize) -> Result<Sub> {
        let mut buf = Vec::with_capacity(SUBPACKET);
        loop {
            match self.zdl(DATA_TIMEOUT)? {
                Zb::Data(b) => {
                    if buf.len() >= max {
                        return Ok(Sub::Bad);
                    }
                    buf.push(b);
                }
                Zb::End(end) => {
                    let n = if self.rx_crc32 { 4 } else { 2 };
                    let mut c = [0u8; 4];
                    for slot in c.iter_mut().take(n) {
                        match self.zdl(DATA_TIMEOUT)? {
                            Zb::Data(b) => *slot = b,
                            Zb::Timeout => return Ok(Sub::Timeout),
                            _ => return Ok(Sub::Bad),
                        }
                    }
                    let ok = if self.rx_crc32 {
                        !crc32_update(crc32_update(0xFFFF_FFFF, &buf), &[end]) == u32::from_le_bytes(c)
                    } else {
                        crc16_update(crc16(&buf), &[end]) == u16::from_be_bytes([c[0], c[1]])
                    };
                    return Ok(if ok { Sub::Ok(buf, end) } else { Sub::Bad });
                }
                Zb::Bad => return Ok(Sub::Bad),
                Zb::Timeout => return Ok(Sub::Timeout),
            }
        }
    }
}

// ------------------------------------------------------------------ sending

pub fn send(ch: &mut Chan, files: &[PathBuf], remote_ready: bool, report: &mut dyn FnMut(Report)) -> Result<()> {
    let mut z = Z::new(ch);
    if !remote_ready {
        // Starts `rz` when the remote side is sitting at a shell prompt.
        z.ch.write(b"rz\r")?;
    }
    z.send_hex(Header::pos(ZRQINIT, 0))?;
    let rinit = await_rinit(&mut z)?;
    z.tx_crc32 = rinit.f0() & CANFC32 != 0;
    z.escape_ctl = rinit.f0() & ESCCTL != 0;
    z.telnet_safe = z.ch.mangles_cr();
    // Receiver buffer size; 0 means it can take a continuous stream.
    let rx_window = u16::from_le_bytes([rinit.d[0], rinit.d[1]]) as usize;

    let sizes: Vec<u64> = files.iter().map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)).collect();
    for (i, path) in files.iter().enumerate() {
        let files_left = files.len() - i;
        let bytes_left: u64 = sizes[i..].iter().sum();
        send_file(&mut z, path, files_left, bytes_left, rx_window, report)?;
    }
    finish_session(&mut z)
}

fn await_rinit(z: &mut Z) -> Result<Header> {
    let mut errors = 0;
    loop {
        match z.read_header(HEADER_TIMEOUT)? {
            Got::Header(h) => match h.kind {
                ZRINIT => return Ok(h),
                ZCHALLENGE => z.send_hex(Header { kind: ZACK, d: h.d })?,
                // Our own ZRQINIT echoed by a shell, or other chatter.
                _ => {}
            },
            Got::Garbage | Got::Timeout => {
                errors += 1;
                if errors > 6 {
                    return Err(Error::Timeout);
                }
                z.send_hex(Header::pos(ZRQINIT, 0))?;
            }
        }
    }
}

enum Interrupt {
    None,
    Resend(u64),
    Skip,
}

/// Looks at what the receiver sent while we were streaming (ZRPOS after an error, a cancel …).
fn check_interrupt(z: &mut Z) -> Result<Interrupt> {
    if !z.ch.poll()? {
        return Ok(Interrupt::None);
    }
    if !z.ch.buffered().iter().any(|&b| b == ZPAD || b == ZDLE) {
        // Noise (XON/XOFF, line garbage): drop it and anything queued behind it,
        // but notice a cancel among it (CAN CAN; replies to us hold single ZDLEs).
        z.ch.discard_pending_watching_cancel()?;
        return Ok(Interrupt::None);
    }
    Ok(match z.read_header(Duration::from_secs(2))? {
        Got::Header(h) if h.kind == ZRPOS => Interrupt::Resend(h.position()),
        Got::Header(h) if h.kind == ZSKIP => Interrupt::Skip,
        Got::Header(h) if h.kind == ZFERR || h.kind == ZABORT => {
            return Err(protocol_err("the receiver aborted the file"));
        }
        _ => Interrupt::None,
    })
}

fn read_full(file: &mut File, buf: &mut [u8], path: &Path) -> Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(file_err(path, e)),
        }
    }
    Ok(n)
}

fn file_crc32(file: &mut File, path: &Path) -> Result<u32> {
    file.seek(SeekFrom::Start(0)).map_err(|e| file_err(path, e))?;
    let mut crc = 0xFFFF_FFFF;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = read_full(file, &mut buf, path)?;
        if n == 0 {
            break;
        }
        crc = crc32_update(crc, &buf[..n]);
    }
    Ok(!crc)
}

fn send_file(
    z: &mut Z,
    path: &PathBuf,
    files_left: usize,
    bytes_left: u64,
    rx_window: usize,
    report: &mut dyn FnMut(Report),
) -> Result<()> {
    let mut file = File::open(path).map_err(|e| file_err(path, e))?;
    let meta = file.metadata().map_err(|e| file_err(path, e))?;
    let size = meta.len();
    let name = send_name(path);
    report(Report::File { name: name.clone(), path: path.clone(), size: Some(size) });

    let mtime = mtime_secs(&meta);
    let mut info = format!("{name}\0{size} {mtime:o} 100644 0 {files_left} {bytes_left}").into_bytes();
    info.push(0);

    // Offer the file until the receiver says where to start.
    let mut errors = 0;
    let mut pos = 'offer: loop {
        z.send_bin(Header::flags(ZFILE, ZCBIN))?;
        z.send_data(&info, ZCRCW)?;
        let mut wait = HEADER_TIMEOUT;
        while let Got::Header(h) = z.read_header(wait)? {
            match h.kind {
                ZRPOS => break 'offer h.position(),
                ZSKIP => {
                    report(Report::FileDone { bytes: 0, skipped: true });
                    return Ok(());
                }
                ZCRC => {
                    let c = file_crc32(&mut file, path)?;
                    z.send_hex(Header { kind: ZCRC, d: c.to_le_bytes() })?;
                }
                ZFERR | ZABORT | ZFIN => return Err(protocol_err("the receiver refused the file")),
                // Either the receiver could not read the offer, or a stale one: rz
                // repeats ZRINIT while it waits and answers our ZRQINIT with another.
                // Re-offering on a stale one makes it send ZRPOS twice, and the late
                // duplicate restarts the stream from the beginning. So, like lrzsz,
                // offer again only if nothing else follows shortly.
                ZRINIT => wait = STALE_RINIT_WAIT,
                ZNAK => break, // offer again
                _ => {}
            }
        }
        errors += 1;
        if errors > MAX_ERRORS {
            return Err(Error::Timeout);
        }
    };

    let mut buf = vec![0u8; SUBPACKET];
    let mut stall = Stall::new(pos);
    'resend: loop {
        pos = pos.min(size);
        file.seek(SeekFrom::Start(pos)).map_err(|e| file_err(path, e))?;
        // Replies already queued predate this (re)start: on a noisy line the
        // receiver sends a ZRPOS for every damaged subpacket, and acting on those
        // stale ones rewinds again and again. A cancel among them still counts.
        z.ch.discard_pending_watching_cancel()?;
        if pos < size {
            z.send_bin(Header::pos(ZDATA, pos))?;
            let mut since_ack = 0usize;
            loop {
                let n = read_full(&mut file, &mut buf, path)?;
                let at_eof = n < buf.len() || pos + n as u64 >= size;
                let end = if at_eof {
                    ZCRCE
                } else if rx_window != 0 && since_ack + n >= rx_window {
                    ZCRCW
                } else {
                    ZCRCG
                };
                z.send_data(&buf[..n], end)?;
                pos += n as u64;
                since_ack += n;
                report(Report::Progress { bytes: pos });
                if at_eof {
                    break;
                }
                if end == ZCRCW {
                    // A limited receiver buffer: wait until it caught up, then open a new frame.
                    match z.read_header(HEADER_TIMEOUT)? {
                        Got::Header(h) if h.kind == ZACK => {}
                        Got::Header(h) if h.kind == ZRPOS => {
                            pos = h.position();
                            stall.reached(pos)?;
                            continue 'resend;
                        }
                        _ => stall.check()?,
                    }
                    since_ack = 0;
                    z.send_bin(Header::pos(ZDATA, pos))?;
                    continue;
                }
                match check_interrupt(z)? {
                    Interrupt::None => {}
                    Interrupt::Resend(p) => {
                        stall.reached(p)?;
                        pos = p;
                        continue 'resend;
                    }
                    Interrupt::Skip => {
                        report(Report::FileDone { bytes: pos, skipped: true });
                        return Ok(());
                    }
                }
            }
        }

        // End of file: wait until the receiver confirms it has everything.
        z.send_bin(Header::pos(ZEOF, size))?;
        stall.touch();
        loop {
            match z.read_header(HEADER_TIMEOUT)? {
                Got::Header(h) => match h.kind {
                    ZRINIT => {
                        report(Report::FileDone { bytes: size, skipped: false });
                        return Ok(());
                    }
                    ZRPOS => {
                        pos = h.position();
                        stall.reached(pos)?;
                        continue 'resend;
                    }
                    ZSKIP => {
                        report(Report::FileDone { bytes: pos, skipped: true });
                        return Ok(());
                    }
                    ZFERR | ZABORT => return Err(protocol_err("the receiver aborted the file")),
                    _ => {} // e.g. a late ZACK
                },
                Got::Garbage | Got::Timeout => {
                    stall.check()?;
                    z.send_bin(Header::pos(ZEOF, size))?;
                }
            }
        }
    }
}

fn finish_session(z: &mut Z) -> Result<()> {
    for _ in 0..5 {
        z.send_hex(Header::pos(ZFIN, 0))?;
        if let Got::Header(h) = z.read_header(HEADER_TIMEOUT)? {
            if h.kind == ZFIN {
                z.ch.write(b"OO")?;
                return z.ch.flush();
            }
        }
    }
    // Every file went through; a lost ZFIN is not worth failing for.
    Ok(())
}

// ---------------------------------------------------------------- receiving

pub fn receive(ch: &mut Chan, dir: &Path, report: &mut dyn FnMut(Report)) -> Result<()> {
    let mut z = Z::new(ch);
    let rinit = Header::flags(ZRINIT, CANFDX | CANOVIO | CANFC32);
    z.send_hex(rinit)?;
    let mut errors = 0;
    loop {
        match z.read_header(HEADER_TIMEOUT)? {
            Got::Header(h) => match h.kind {
                ZRQINIT => z.send_hex(rinit)?,
                ZSINIT => {
                    let _ = z.read_data(1024)?;
                    z.send_hex(Header::pos(ZACK, 1))?;
                }
                ZFILE => match z.read_data(4096)? {
                    Sub::Ok(info, _) => {
                        errors = 0;
                        receive_file(&mut z, &info, dir, report)?;
                        z.send_hex(rinit)?;
                    }
                    _ => z.send_hex(Header::pos(ZNAK, 0))?,
                },
                ZFIN => {
                    z.send_hex(Header::pos(ZFIN, 0))?;
                    // The sender closes with "OO"; don't leave it on the terminal.
                    for _ in 0..2 {
                        if z.ch.byte(Duration::from_millis(500))?.is_none() {
                            break;
                        }
                    }
                    return Ok(());
                }
                ZCOMMAND => {
                    // Remote command execution is never allowed.
                    let _ = z.read_data(4096)?;
                    z.send_hex(Header::pos(ZCOMPL, 1))?;
                }
                ZFREECNT => z.send_hex(Header::pos(ZACK, u32::MAX as u64))?,
                _ => z.send_hex(rinit)?,
            },
            Got::Garbage | Got::Timeout => {
                errors += 1;
                if errors > MAX_ERRORS {
                    return Err(Error::Timeout);
                }
                z.send_hex(rinit)?;
            }
        }
    }
}

fn receive_file(z: &mut Z, info: &[u8], dir: &Path, report: &mut dyn FnMut(Report)) -> Result<()> {
    let name_end = info.iter().position(|&b| b == 0).unwrap_or(info.len());
    let raw_name = String::from_utf8_lossy(&info[..name_end]).into_owned();
    let rest = info.get(name_end + 1..).unwrap_or(&[]);
    let rest = &rest[..rest.iter().position(|&b| b == 0).unwrap_or(rest.len())];
    let fields: Vec<&str> = std::str::from_utf8(rest).unwrap_or("").split_whitespace().collect();
    let size = fields.first().and_then(|s| s.parse::<u64>().ok());
    let mtime = fields.get(1).and_then(|s| u64::from_str_radix(s, 8).ok());

    let path = unique_path(dir, &safe_file_name(&raw_name));
    let file = File::create(&path).map_err(|e| file_err(&path, e))?;
    report(Report::File { name: raw_name, path: path.clone(), size });
    let mut out = BufWriter::with_capacity(64 * 1024, file);

    let mut pos = 0u64;
    let mut stall = Stall::new(0);
    z.send_hex(Header::pos(ZRPOS, pos))?;
    loop {
        match z.read_header(HEADER_TIMEOUT)? {
            Got::Header(h) => match h.kind {
                ZDATA => {
                    if h.position() != pos {
                        // Data from before our last ZRPOS; ask again.
                        stall.check()?;
                        z.send_hex(Header::pos(ZRPOS, pos))?;
                        continue;
                    }
                    loop {
                        match z.read_data(MAX_SUBPACKET)? {
                            Sub::Ok(data, end) => {
                                out.write_all(&data).map_err(|e| file_err(&path, e))?;
                                pos += data.len() as u64;
                                stall.reached(pos)?;
                                report(Report::Progress { bytes: pos });
                                match end {
                                    ZCRCW => {
                                        z.send_hex(Header::pos(ZACK, pos))?;
                                        break;
                                    }
                                    ZCRCQ => z.send_hex(Header::pos(ZACK, pos))?,
                                    ZCRCG => {}
                                    _ => break, // ZCRCE: a header follows
                                }
                            }
                            Sub::Bad | Sub::Timeout => {
                                stall.check()?;
                                z.send_hex(Header::pos(ZRPOS, pos))?;
                                break;
                            }
                        }
                    }
                }
                ZEOF => {
                    if h.position() == pos {
                        out.flush().map_err(|e| file_err(&path, e))?;
                        drop(out);
                        if let Some(t) = mtime {
                            set_mtime(&path, t);
                        }
                        report(Report::FileDone { bytes: pos, skipped: false });
                        return Ok(());
                    }
                    // Data is missing (the EOF crossed our ZRPOS): ask again.
                    stall.check()?;
                    z.send_hex(Header::pos(ZRPOS, pos))?;
                }
                ZFILE => {
                    // The sender missed our ZRPOS.
                    let _ = z.read_data(4096)?;
                    z.send_hex(Header::pos(ZRPOS, pos))?;
                }
                ZFIN => return Err(protocol_err("the sender ended the session before the file was complete")),
                _ => {}
            },
            Got::Garbage | Got::Timeout => {
                stall.check()?;
                z.send_hex(Header::pos(ZRPOS, pos))?;
            }
        }
    }
}

/// Gives up when the transfer position has not advanced for [`STALL_LIMIT`],
/// however many retries that took. Counting errors instead would fail long
/// files on a line that is merely noisy.
struct Stall {
    best: u64,
    since: Instant,
}

const STALL_LIMIT: Duration = Duration::from_secs(60);

impl Stall {
    fn new(pos: u64) -> Self {
        Self { best: pos, since: Instant::now() }
    }

    fn touch(&mut self) {
        self.since = Instant::now();
    }

    /// Notes a position reported or reached; fails if we have been stuck too long.
    fn reached(&mut self, pos: u64) -> Result<()> {
        if pos > self.best {
            self.best = pos;
            self.touch();
            return Ok(());
        }
        self.check()
    }

    fn check(&self) -> Result<()> {
        if self.since.elapsed() > STALL_LIMIT {
            Err(protocol_err("no progress for a minute: too many errors on the line"))
        } else {
            Ok(())
        }
    }
}

// ------------------------------------------------------------- detection

/// What the remote side announced with a ZMODEM greeting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detected {
    /// `sz` sent ZRQINIT: the remote wants to send files.
    RemoteSends,
    /// `rz` sent ZRINIT: the remote waits for files.
    RemoteReceives,
}

/// Spots `sz` / `rz` starting on the remote side in normal terminal output.
#[derive(Default)]
pub struct Detector {
    tail: Vec<u8>,
}

const GREETING: &[u8] = b"**\x18B0";

impl Detector {
    pub fn feed(&mut self, data: &[u8]) -> Option<Detected> {
        let scan = |h: &[u8]| {
            h.windows(GREETING.len() + 1).find_map(|w| match (&w[..GREETING.len()] == GREETING, w[GREETING.len()]) {
                (true, b'0') => Some(Detected::RemoteSends),
                (true, b'1') => Some(Detected::RemoteReceives),
                _ => None,
            })
        };
        let mut seam = self.tail.clone();
        seam.extend_from_slice(&data[..data.len().min(GREETING.len())]);
        let found = scan(&seam).or_else(|| scan(data));
        let mut all = std::mem::take(&mut self.tail);
        all.extend_from_slice(&data[data.len().saturating_sub(GREETING.len())..]);
        self.tail = all[all.len().saturating_sub(GREETING.len())..].to_vec();
        found
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn hex_headers_match_lrzsz() {
        // The greetings every ZMODEM user has seen on screen.
        let h = Header::flags(ZRINIT, CANFDX | CANOVIO | CANFC32);
        let bytes = h.bytes();
        assert_eq!(format!("{:04x}", crc16(&bytes)), "be50");
        assert_eq!(crc16(&Header::pos(ZRQINIT, 0).bytes()), 0);
    }

    #[test]
    fn detector_finds_greetings_split_across_chunks() {
        let mut d = Detector::default();
        assert_eq!(d.feed(b"rz waiting to receive.**\x18"), None);
        assert_eq!(d.feed(b"B0100000023be50\r\x8a\x11"), Some(Detected::RemoteReceives));
        let mut d = Detector::default();
        assert_eq!(d.feed(b"rz\r**\x18B00000000000000\r\x8a\x11"), Some(Detected::RemoteSends));
        assert_eq!(d.feed(b"plain text"), None);
    }
}

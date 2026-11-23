//! XMODEM (checksum, CRC-16, 1K) and YMODEM batch transfers.
//!
//! Block: `SOH|STX, n, 255-n, 128|1024 data bytes, checksum | CRC-16 (big endian)`.
//! The receiver starts the transfer by sending `C` (CRC) or NAK (checksum),
//! then ACKs or NAKs every block. YMODEM adds block 0 carrying the file name
//! and size, so several files can follow each other and padding is trimmed.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::crc::crc16;
use super::{
    file_err, mtime_secs, protocol_err, safe_file_name, send_name, set_mtime, unique_path, Chan, Error, Report, Result,
};

const SOH: u8 = 0x01;
const STX: u8 = 0x02;
const EOT: u8 = 0x04;
const ACK: u8 = 0x06;
const NAK: u8 = 0x15;
const CAN: u8 = 0x18;
const CPMEOF: u8 = 0x1A;
const CRC_START: u8 = b'C';

/// How long a sender waits for the receiver to start (the user may still be typing the command).
const START_TIMEOUT: Duration = Duration::from_secs(60);
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
const BYTE_TIMEOUT: Duration = Duration::from_secs(1);
const INVITE_INTERVAL: Duration = Duration::from_secs(3);
const MAX_RETRIES: u32 = 10;
/// Invitations sent as `C` before falling back to NAK for checksum-only senders.
const CRC_INVITES: u32 = 5;

// ------------------------------------------------------------------ sending

/// Waits for the receiver's start character. Returns whether it wants CRC-16.
fn wait_start(ch: &mut Chan, timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    let mut cans = 0;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match ch.byte(left)? {
            Some(CRC_START) => return Ok(true),
            Some(NAK) => return Ok(false),
            Some(CAN) => {
                cans += 1;
                if cans >= 2 {
                    return Err(Error::RemoteCancelled);
                }
            }
            Some(_) => cans = 0,
            None => return Err(Error::Timeout),
        }
    }
}

enum Reply {
    Ack,
    Retry,
}

fn wait_reply(ch: &mut Chan) -> Result<Reply> {
    let deadline = Instant::now() + REPLY_TIMEOUT;
    let mut cans = 0;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match ch.byte(left)? {
            Some(ACK) => return Ok(Reply::Ack),
            // `C` here means the receiver is still waiting for the first block.
            Some(NAK) | Some(CRC_START) => return Ok(Reply::Retry),
            Some(CAN) => {
                cans += 1;
                if cans >= 2 {
                    return Err(Error::RemoteCancelled);
                }
            }
            Some(_) => cans = 0,
            None => return Ok(Reply::Retry),
        }
    }
}

fn build_block(num: u8, data: &[u8], size: usize, crc: bool, pad: u8) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(size + 5);
    pkt.push(if size == 1024 { STX } else { SOH });
    pkt.push(num);
    pkt.push(!num);
    pkt.extend_from_slice(data);
    pkt.resize(3 + size, pad);
    let body = &pkt[3..];
    if crc {
        let c = crc16(body);
        pkt.extend_from_slice(&c.to_be_bytes());
    } else {
        let sum = body.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        pkt.push(sum);
    }
    pkt
}

fn send_block(ch: &mut Chan, pkt: &[u8]) -> Result<()> {
    for _ in 0..MAX_RETRIES {
        // Anything already waiting is a stale reply (e.g. repeated `C` invitations).
        ch.discard_pending_watching_cancel()?;
        ch.write(pkt)?;
        if let Reply::Ack = wait_reply(ch)? {
            return Ok(());
        }
    }
    Err(protocol_err("too many retries: the receiver keeps rejecting blocks"))
}

fn send_eot(ch: &mut Chan) -> Result<()> {
    for _ in 0..MAX_RETRIES {
        ch.discard_pending_watching_cancel()?;
        ch.write(&[EOT])?;
        if let Reply::Ack = wait_reply(ch)? {
            return Ok(());
        }
    }
    Err(protocol_err("the receiver did not acknowledge the end of the file"))
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

/// Sends the file contents as blocks 1, 2, … Returns the bytes sent.
fn send_data(
    ch: &mut Chan,
    file: &mut File,
    path: &Path,
    crc: bool,
    one_k: bool,
    report: &mut dyn FnMut(Report),
) -> Result<u64> {
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = [0u8; 1024];
    let mut num: u8 = 1;
    let mut sent = 0u64;
    loop {
        let remaining = size.saturating_sub(sent);
        let block = if one_k && crc && remaining >= 1024 { 1024 } else { 128 };
        let n = read_full(file, &mut buf[..block], path)?;
        if n == 0 {
            break;
        }
        send_block(ch, &build_block(num, &buf[..n], block, crc, CPMEOF))?;
        sent += n as u64;
        num = num.wrapping_add(1);
        report(Report::Progress { bytes: sent });
    }
    Ok(sent)
}

pub fn send_xmodem(ch: &mut Chan, path: &PathBuf, one_k: bool, report: &mut dyn FnMut(Report)) -> Result<()> {
    let mut file = File::open(path).map_err(|e| file_err(path, e))?;
    let size = file.metadata().map(|m| m.len()).ok();
    report(Report::File { name: send_name(path), path: path.clone(), size });
    let crc = wait_start(ch, START_TIMEOUT)?;
    // 1K blocks are only defined together with CRC-16.
    let sent = send_data(ch, &mut file, path, crc, one_k, report)?;
    send_eot(ch)?;
    report(Report::FileDone { bytes: sent, skipped: false });
    Ok(())
}

pub fn send_ymodem(ch: &mut Chan, files: &[PathBuf], report: &mut dyn FnMut(Report)) -> Result<()> {
    for path in files {
        let mut file = File::open(path).map_err(|e| file_err(path, e))?;
        let meta = file.metadata().map_err(|e| file_err(path, e))?;
        let name = send_name(path);
        report(Report::File { name: name.clone(), path: path.clone(), size: Some(meta.len()) });

        let crc = wait_start(ch, START_TIMEOUT)?;
        let mut header = format!("{name}\0{} {:o} 100644", meta.len(), mtime_secs(&meta)).into_bytes();
        header.push(0);
        let size = if header.len() > 128 { 1024 } else { 128 };
        if header.len() > 1024 {
            return Err(protocol_err(format!("file name too long for YMODEM: {name}")));
        }
        send_block(ch, &build_block(0, &header, size, crc, 0))?;

        // The receiver asks for the data with another `C`.
        let crc = wait_start(ch, REPLY_TIMEOUT * 3)?;
        let sent = send_data(ch, &mut file, path, crc, true, report)?;
        send_eot(ch)?;
        report(Report::FileDone { bytes: sent, skipped: false });
    }
    // An empty block 0 ends the batch.
    let crc = wait_start(ch, START_TIMEOUT)?;
    send_block(ch, &build_block(0, &[], 128, crc, 0))
}

// ---------------------------------------------------------------- receiving

enum Rx {
    Block {
        num: u8,
        data: Vec<u8>,
    },
    Eot,
    /// A damaged block (bad header, checksum or a byte timeout inside it).
    Bad,
    Timeout,
}

fn recv_block(ch: &mut Chan, crc: bool, timeout: Duration) -> Result<Rx> {
    let deadline = Instant::now() + timeout;
    let mut cans = 0;
    let size = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let Some(b) = ch.byte(left)? else {
            return Ok(Rx::Timeout);
        };
        match b {
            SOH => break 128,
            STX => break 1024,
            EOT => return Ok(Rx::Eot),
            CAN => {
                cans += 1;
                if cans >= 2 {
                    return Err(Error::RemoteCancelled);
                }
            }
            _ => cans = 0, // noise between blocks
        }
    };
    let trailer = if crc { 2 } else { 1 };
    let mut raw = Vec::with_capacity(2 + size + trailer);
    for _ in 0..2 + size + trailer {
        match ch.byte(BYTE_TIMEOUT)? {
            Some(b) => raw.push(b),
            None => return Ok(Rx::Bad),
        }
    }
    let (num, inv) = (raw[0], raw[1]);
    let data = &raw[2..2 + size];
    let good = num == !inv
        && if crc {
            crc16(data) == u16::from_be_bytes([raw[2 + size], raw[3 + size]])
        } else {
            data.iter().fold(0u8, |a, &b| a.wrapping_add(b)) == raw[2 + size]
        };
    Ok(if good { Rx::Block { num, data: data.to_vec() } } else { Rx::Bad })
}

/// Invites the sender until the first block (or EOT) arrives. Returns the CRC mode in use.
fn invite(ch: &mut Chan, crc_only: bool) -> Result<(bool, Rx)> {
    let deadline = Instant::now() + START_TIMEOUT;
    let mut tries = 0;
    while Instant::now() < deadline {
        let crc = crc_only || tries < CRC_INVITES;
        ch.write(&[if crc { CRC_START } else { NAK }])?;
        tries += 1;
        match recv_block(ch, crc, INVITE_INTERVAL)? {
            Rx::Timeout | Rx::Bad => continue,
            rx => return Ok((crc, rx)),
        }
    }
    Err(Error::Timeout)
}

/// Receives data blocks 1, 2, … until EOT. Writes at most `limit` bytes (the
/// YMODEM file size), so block padding is dropped. Returns the bytes written.
#[allow(clippy::too_many_arguments)]
fn recv_data(
    ch: &mut Chan,
    out: &mut BufWriter<File>,
    path: &Path,
    crc: bool,
    mut pending: Option<Rx>,
    limit: Option<u64>,
    ymodem: bool,
    report: &mut dyn FnMut(Report),
) -> Result<u64> {
    let mut expected: u8 = 1;
    let mut written = 0u64;
    let mut errors = 0;
    let mut eots = 0;
    loop {
        let rx = match pending.take() {
            Some(rx) => rx,
            None => recv_block(ch, crc, REPLY_TIMEOUT)?,
        };
        match rx {
            Rx::Block { num, data } if num == expected => {
                let take = match limit {
                    Some(l) => (l.saturating_sub(written) as usize).min(data.len()),
                    None => data.len(),
                };
                out.write_all(&data[..take]).map_err(|e| file_err(path, e))?;
                written += take as u64;
                expected = expected.wrapping_add(1);
                errors = 0;
                ch.write(&[ACK])?;
                report(Report::Progress { bytes: written });
            }
            Rx::Block { num, .. } if num == expected.wrapping_sub(1) => {
                // Our ACK got lost; the sender repeated the block.
                ch.write(&[ACK])?;
                if ymodem && num == 0 {
                    ch.write(&[CRC_START])?;
                }
            }
            Rx::Block { .. } => {
                return Err(protocol_err("lost block synchronization with the sender"));
            }
            Rx::Eot => {
                // YMODEM convention: NAK the first EOT to make sure it was really meant.
                if ymodem && eots == 0 {
                    eots += 1;
                    ch.write(&[NAK])?;
                    continue;
                }
                ch.write(&[ACK])?;
                out.flush().map_err(|e| file_err(path, e))?;
                return Ok(written);
            }
            Rx::Bad => {
                errors += 1;
                ch.purge(BYTE_TIMEOUT, REPLY_TIMEOUT)?;
                ch.write(&[NAK])?;
            }
            Rx::Timeout => {
                errors += 1;
                ch.write(&[NAK])?;
            }
        }
        if errors >= MAX_RETRIES {
            return Err(protocol_err("too many errors in a row"));
        }
    }
}

pub fn receive_xmodem(ch: &mut Chan, dest: &PathBuf, report: &mut dyn FnMut(Report)) -> Result<()> {
    let file = File::create(dest).map_err(|e| file_err(dest, e))?;
    let name = send_name(dest);
    report(Report::File { name, path: dest.clone(), size: None });
    let mut out = BufWriter::new(file);
    let (crc, first) = invite(ch, false)?;
    let written = recv_data(ch, &mut out, dest, crc, Some(first), None, false, report)?;
    report(Report::FileDone { bytes: written, skipped: false });
    Ok(())
}

pub fn receive_ymodem(ch: &mut Chan, dir: &Path, report: &mut dyn FnMut(Report)) -> Result<()> {
    loop {
        let (crc, first) = invite(ch, true)?;
        let header = match first {
            Rx::Block { num: 0, data } => data,
            Rx::Eot => {
                ch.write(&[ACK])?;
                continue;
            }
            _ => return Err(protocol_err("expected a YMODEM header block")),
        };
        let name_end = header.iter().position(|&b| b == 0).unwrap_or(header.len());
        if name_end == 0 {
            ch.write(&[ACK])?; // empty header: end of batch
            return Ok(());
        }
        let raw_name = String::from_utf8_lossy(&header[..name_end]).into_owned();
        let rest = header.get(name_end + 1..).unwrap_or(&[]);
        let rest = &rest[..rest.iter().position(|&b| b == 0).unwrap_or(rest.len())];
        let fields: Vec<&str> = std::str::from_utf8(rest).unwrap_or("").split_whitespace().collect();
        let size = fields.first().and_then(|s| s.parse::<u64>().ok());
        let mtime = fields.get(1).and_then(|s| u64::from_str_radix(s, 8).ok());

        let path = unique_path(dir, &safe_file_name(&raw_name));
        let file = File::create(&path).map_err(|e| file_err(&path, e))?;
        report(Report::File { name: raw_name, path: path.clone(), size });
        let mut out = BufWriter::new(file);
        ch.write(&[ACK, CRC_START])?;
        let written = recv_data(ch, &mut out, &path, crc, None, size, true, report)?;
        drop(out);
        if let Some(t) = mtime {
            set_mtime(&path, t);
        }
        report(Report::FileDone { bytes: written, skipped: false });
    }
}

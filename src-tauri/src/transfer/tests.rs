//! Sender against receiver over an in-memory pipe, with optional line noise.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use super::*;

/// Deterministic line noise: flips bits in, or drops, about one byte in `every`.
struct Noise {
    state: u64,
    every: u64,
    drops: bool,
}

impl Noise {
    fn next(&mut self) -> u64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }

    fn apply(&mut self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        for &b in data {
            if self.next().is_multiple_of(self.every) {
                if self.drops && self.next().is_multiple_of(2) {
                    continue;
                }
                out.push(b ^ (1 << (self.next() % 8)));
            } else {
                out.push(b);
            }
        }
        out
    }
}

struct End {
    rx: Receiver<Vec<u8>>,
    tx: Sender<Vec<u8>>,
    noise: Option<Noise>,
}

impl Link for End {
    fn recv(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        match self.rx.recv_timeout(timeout) {
            Ok(v) => Ok(v),
            Err(RecvTimeoutError::Timeout) => Ok(Vec::new()),
            Err(RecvTimeoutError::Disconnected) => Err(Error::LinkClosed),
        }
    }
    fn send(&mut self, data: &[u8]) -> Result<()> {
        let data = match &mut self.noise {
            Some(n) => n.apply(data),
            None => data.to_vec(),
        };
        self.tx.send(data).map_err(|_| Error::LinkClosed)
    }
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

fn pipe(noise: Option<(u64, bool)>) -> (End, End) {
    let (a_tx, b_rx) = channel();
    let (b_tx, a_rx) = channel();
    let mk = |seed: u64| noise.map(|(every, drops)| Noise { state: seed, every, drops });
    (
        End { rx: a_rx, tx: a_tx, noise: mk(0x9E37_79B9_7F4A_7C15) },
        End { rx: b_rx, tx: b_tx, noise: mk(0xD1B5_4A32_D192_ED03) },
    )
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let p = std::env::temp_dir().join(format!("tw-xfer-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn file(&self, name: &str, data: &[u8]) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }
    fn sub(&self, name: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn random(len: usize, seed: u64) -> Vec<u8> {
    let mut n = Noise { state: seed | 1, every: 1, drops: false };
    (0..len).map(|_| (n.next() >> 24) as u8).collect()
}

struct Outcome {
    send: Result<Summary>,
    recv: Result<Summary>,
    elapsed: Duration,
}

/// Runs `send` and `recv` against each other. `cancel_recv_after` cancels the
/// receiver once it has that many bytes.
fn exchange(send: Job, recv: Job, noise: Option<(u64, bool)>, cancel_recv_after: Option<u64>) -> Outcome {
    let (mut a, mut b) = pipe(noise);
    let (cancel_a, cancel_b) = (AtomicBool::new(false), AtomicBool::new(false));
    let t = Instant::now();
    let (send, recv) = std::thread::scope(|s| {
        let tx = s.spawn(|| run(&send, &mut a, &cancel_a, &mut |_| {}));
        let rx = s.spawn(|| {
            run(&recv, &mut b, &cancel_b, &mut |r| {
                if let (Some(limit), Report::Progress { bytes }) = (cancel_recv_after, &r) {
                    if *bytes >= limit {
                        cancel_b.store(true, Ordering::SeqCst);
                    }
                }
            })
        });
        (tx.join().unwrap(), rx.join().unwrap())
    });
    Outcome { send, recv, elapsed: t.elapsed() }
}

fn received(dir: &Path, name: &str) -> Vec<u8> {
    std::fs::read(dir.join(name)).unwrap_or_else(|e| panic!("{name} missing in {}: {e}", dir.display()))
}

// ------------------------------------------------------------------ XMODEM

#[test]
fn xmodem_pads_to_whole_blocks() {
    let tmp = TempDir::new("x");
    let data = random(1000, 1);
    let src = tmp.file("src.bin", &data);
    let dst = tmp.0.join("dst.bin");
    let o = exchange(
        Job::Send { protocol: Protocol::Xmodem, files: vec![src], remote_ready: true },
        Job::Receive { protocol: Protocol::Xmodem, dest: dst.clone() },
        None,
        None,
    );
    o.send.unwrap();
    o.recv.unwrap();
    let got = std::fs::read(&dst).unwrap();
    assert_eq!(got.len(), 1024, "XMODEM has no length field: the last block stays padded");
    assert_eq!(&got[..1000], &data[..]);
    assert!(got[1000..].iter().all(|&b| b == 0x1A));
}

#[test]
fn xmodem_1k_with_noise() {
    let tmp = TempDir::new("x1k");
    let data = random(40 * 1024, 2);
    let src = tmp.file("src.bin", &data);
    let dst = tmp.0.join("dst.bin");
    let o = exchange(
        Job::Send { protocol: Protocol::Xmodem1k, files: vec![src], remote_ready: true },
        Job::Receive { protocol: Protocol::Xmodem, dest: dst.clone() },
        Some((8000, false)),
        None,
    );
    o.send.unwrap();
    o.recv.unwrap();
    assert_eq!(std::fs::read(&dst).unwrap(), data, "1K blocks: 40 KiB fits exactly");
}

// ------------------------------------------------------------------ YMODEM

#[test]
fn ymodem_batch_keeps_names_and_exact_sizes() {
    let tmp = TempDir::new("y");
    let files = [("empty.txt", Vec::new()), ("one.bin", vec![0x42]), ("data.bin", random(3000, 3))];
    let paths: Vec<PathBuf> = files.iter().map(|(n, d)| tmp.file(n, d)).collect();
    let out = tmp.sub("out");
    let o = exchange(
        Job::Send { protocol: Protocol::Ymodem, files: paths, remote_ready: true },
        Job::Receive { protocol: Protocol::Ymodem, dest: out.clone() },
        None,
        None,
    );
    o.send.unwrap();
    let summary = o.recv.unwrap();
    assert_eq!(summary.files.len(), 3);
    for (name, data) in &files {
        assert_eq!(&received(&out, name), data, "{name}");
    }
}

#[test]
fn ymodem_with_noise() {
    let tmp = TempDir::new("yn");
    let data = random(60_000, 4);
    let src = tmp.file("noisy.bin", &data);
    let out = tmp.sub("out");
    let o = exchange(
        Job::Send { protocol: Protocol::Ymodem, files: vec![src], remote_ready: true },
        Job::Receive { protocol: Protocol::Ymodem, dest: out.clone() },
        Some((10_000, false)),
        None,
    );
    o.send.unwrap();
    o.recv.unwrap();
    assert_eq!(received(&out, "noisy.bin"), data);
}

// ------------------------------------------------------------------ ZMODEM

#[test]
fn zmodem_batch() {
    let tmp = TempDir::new("z");
    let files = [
        ("empty.txt", Vec::new()),
        ("펌웨어 v1.2.bin", random(200 * 1024, 5)),
        // Every byte value, including ZDLE, XON/XOFF and "@\r".
        ("bytes.bin", (0..=255u8).chain(b"@\r@\r\x18\x18\x18".iter().copied()).collect()),
    ];
    let paths: Vec<PathBuf> = files.iter().map(|(n, d)| tmp.file(n, d)).collect();
    let out = tmp.sub("out");
    let o = exchange(
        Job::Send { protocol: Protocol::Zmodem, files: paths, remote_ready: true },
        Job::Receive { protocol: Protocol::Zmodem, dest: out.clone() },
        None,
        None,
    );
    o.send.unwrap();
    let summary = o.recv.unwrap();
    assert_eq!(summary.files.len(), 3);
    for (name, data) in &files {
        assert_eq!(&received(&out, name), data, "{name}");
    }
    assert!(o.elapsed < Duration::from_secs(5), "clean line took {:?}", o.elapsed);
}

#[test]
fn zmodem_recovers_from_corruption() {
    let tmp = TempDir::new("zn");
    let data = random(300 * 1024, 6);
    let src = tmp.file("big.bin", &data);
    let out = tmp.sub("out");
    let o = exchange(
        Job::Send { protocol: Protocol::Zmodem, files: vec![src], remote_ready: true },
        Job::Receive { protocol: Protocol::Zmodem, dest: out.clone() },
        Some((40_000, false)),
        None,
    );
    o.send.unwrap();
    o.recv.unwrap();
    assert_eq!(received(&out, "big.bin"), data);
    eprintln!("zmodem with bit errors: {:?}", o.elapsed);
}

#[test]
fn zmodem_recovers_from_dropped_bytes() {
    let tmp = TempDir::new("zd");
    let data = random(120 * 1024, 7);
    let src = tmp.file("drops.bin", &data);
    let out = tmp.sub("out");
    let o = exchange(
        Job::Send { protocol: Protocol::Zmodem, files: vec![src], remote_ready: true },
        Job::Receive { protocol: Protocol::Zmodem, dest: out.clone() },
        Some((30_000, true)),
        None,
    );
    o.send.unwrap();
    o.recv.unwrap();
    assert_eq!(received(&out, "drops.bin"), data);
    eprintln!("zmodem with dropped bytes: {:?}", o.elapsed);
}

// ------------------------------------------------------------ cancellation

#[test]
fn cancelling_the_receiver_stops_the_sender() {
    for protocol in [Protocol::Zmodem, Protocol::Ymodem, Protocol::Xmodem] {
        let tmp = TempDir::new("c");
        let src = tmp.file("c.bin", &random(200 * 1024, 8));
        let dest = if protocol == Protocol::Xmodem { tmp.0.join("c.out") } else { tmp.sub("out") };
        let o = exchange(
            Job::Send { protocol, files: vec![src], remote_ready: true },
            Job::Receive { protocol, dest },
            None,
            Some(20 * 1024),
        );
        assert!(matches!(o.recv, Err(Error::Cancelled)), "{protocol:?} receiver: {:?}", o.recv.err());
        assert!(matches!(o.send, Err(Error::RemoteCancelled)), "{protocol:?} sender: {:?}", o.send.err());
        assert!(o.elapsed < Duration::from_secs(5), "{protocol:?} took {:?} to stop", o.elapsed);
    }
}

// ------------------------------------------------------------ file names

#[test]
fn received_names_cannot_escape_the_folder() {
    assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
    assert_eq!(safe_file_name(r"C:\Windows\evil.exe"), "evil.exe");
    assert_eq!(safe_file_name("a:b*c?.txt"), "a_b_c_.txt");
    assert_eq!(safe_file_name("con.txt"), "_con.txt");
    assert_eq!(safe_file_name("COM1"), "_COM1");
    assert_eq!(safe_file_name("COM10.log"), "COM10.log");
    assert_eq!(safe_file_name(".."), "received.bin");
    assert_eq!(safe_file_name(""), "received.bin");
    assert_eq!(safe_file_name("펌웨어.bin"), "펌웨어.bin");
}

#[test]
fn existing_files_are_not_overwritten() {
    let tmp = TempDir::new("u");
    tmp.file("a.bin", b"1");
    tmp.file("a (1).bin", b"2");
    assert_eq!(unique_path(&tmp.0, "a.bin"), tmp.0.join("a (2).bin"));
    assert_eq!(unique_path(&tmp.0, "b.bin"), tmp.0.join("b.bin"));
}

#[test]
fn heavy_noise_all_protocols() {
    // About one damaged or lost byte per 3 KB: hundreds of retries per run.
    for protocol in [Protocol::Zmodem, Protocol::Ymodem, Protocol::Xmodem1k] {
        let tmp = TempDir::new("heavy");
        // XMODEM and YMODEM wait out a 10 s timeout for every lost reply, so keep their file small.
        let data = random(if protocol == Protocol::Zmodem { 150 * 1024 } else { 24 * 1024 }, 9);
        let src = tmp.file("heavy.bin", &data);
        let dest = if protocol == Protocol::Xmodem1k { tmp.0.join("heavy.out") } else { tmp.sub("out") };
        let o = exchange(
            Job::Send { protocol, files: vec![src], remote_ready: true },
            Job::Receive { protocol, dest: dest.clone() },
            Some((3000, true)),
            None,
        );
        eprintln!("{protocol:?} with heavy noise: {:?}", o.elapsed);
        o.send.unwrap();
        o.recv.unwrap();
        let got =
            if protocol == Protocol::Xmodem1k { std::fs::read(&dest).unwrap() } else { received(&dest, "heavy.bin") };
        assert_eq!(got, data, "{protocol:?}");
    }
}

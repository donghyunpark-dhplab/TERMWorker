//! [`Link`] over a transport: input from the session pump, output through the
//! transport's command channel, with only a few KiB in flight so ZMODEM can
//! react to errors quickly.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc as std_mpsc, Arc};
use std::time::Duration;

use tokio::sync::mpsc;

use super::{Error, Link, Result};
use crate::transport::TransportCmd;

const SYNC_EVERY: usize = 4 * 1024;
const MAX_SYNCS_IN_FLIGHT: usize = 2;

pub struct TransportLink {
    input: std_mpsc::Receiver<Vec<u8>>,
    cmd_tx: mpsc::UnboundedSender<TransportCmd>,
    cancel: Arc<AtomicBool>,
    unsynced: usize,
    syncs: VecDeque<std_mpsc::Receiver<()>>,
    telnet: bool,
}

impl TransportLink {
    pub fn new(
        input: std_mpsc::Receiver<Vec<u8>>,
        cmd_tx: mpsc::UnboundedSender<TransportCmd>,
        cancel: Arc<AtomicBool>,
    ) -> Self {
        Self { input, cmd_tx, cancel, unsynced: 0, syncs: VecDeque::new(), telnet: false }
    }

    /// The session is Telnet: see [`Link::mangles_cr`].
    pub fn telnet(mut self, telnet: bool) -> Self {
        self.telnet = telnet;
        self
    }

    fn sync_point(&mut self) -> Result<()> {
        let (tx, rx) = std_mpsc::channel();
        self.cmd_tx.send(TransportCmd::Sync(tx)).map_err(|_| Error::LinkClosed)?;
        self.syncs.push_back(rx);
        self.unsynced = 0;
        Ok(())
    }

    /// Waits for the oldest sync point. A transport that drops the request has
    /// still processed everything before it, so that counts as done too.
    fn wait_oldest(&mut self) -> Result<()> {
        let Some(rx) = self.syncs.pop_front() else {
            return Ok(());
        };
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(()) | Err(std_mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                Err(std_mpsc::RecvTimeoutError::Timeout) => {
                    if self.cancel.load(Ordering::SeqCst) {
                        return Err(Error::Cancelled);
                    }
                }
            }
        }
    }
}

impl Link for TransportLink {
    fn recv(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let first = match self.input.recv_timeout(timeout) {
            Ok(v) => v,
            Err(std_mpsc::RecvTimeoutError::Timeout) => return Ok(Vec::new()),
            Err(std_mpsc::RecvTimeoutError::Disconnected) => return Err(Error::LinkClosed),
        };
        let mut data = first;
        while data.len() < 64 * 1024 {
            match self.input.try_recv() {
                Ok(more) => data.extend_from_slice(&more),
                Err(_) => break,
            }
        }
        Ok(data)
    }

    fn send(&mut self, data: &[u8]) -> Result<()> {
        self.cmd_tx.send(TransportCmd::WriteData(data.to_vec())).map_err(|_| Error::LinkClosed)?;
        self.unsynced += data.len();
        if self.unsynced >= SYNC_EVERY {
            self.sync_point()?;
            // After a cancel only the abort sequence is still sent; don't wait for the line.
            while self.syncs.len() > MAX_SYNCS_IN_FLIGHT && !self.cancel.load(Ordering::SeqCst) {
                self.wait_oldest()?;
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.sync_point()?;
        while !self.syncs.is_empty() {
            if self.cancel.load(Ordering::SeqCst) {
                self.syncs.clear();
                break;
            }
            self.wait_oldest()?;
        }
        Ok(())
    }

    fn mangles_cr(&self) -> bool {
        self.telnet
    }
}

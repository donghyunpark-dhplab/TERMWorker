//! Windows serial I/O on an overlapped (asynchronous) handle.
//!
//! `serialport` opens ports for synchronous I/O, and Windows serializes
//! synchronous requests on one file object. With a reader thread parked in
//! `ReadFile`, every keystroke waited for the read to time out, and a busy
//! writer could starve the reader until the driver's receive buffer overflowed
//! (measured: ~0.5 % loss in full duplex at 460800 baud). With
//! `FILE_FLAG_OVERLAPPED` reads and writes run independently, each with its
//! own `OVERLAPPED`, so one `Port` is shared by the reader and writer threads.

use std::io;
use std::mem;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows_sys::core::BOOL;
use windows_sys::Win32::Devices::Communication::{
    ClearCommBreak, EscapeCommFunction, GetCommState, SetCommBreak, SetCommState, SetCommTimeouts, SetupComm, CLRDTR,
    CLRRTS, COMMTIMEOUTS, DCB, EVENPARITY, NOPARITY, ODDPARITY, ONESTOPBIT, SETDTR, SETRTS, TWOSTOPBITS,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_IO_PENDING, ERROR_OPERATION_ABORTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, OPEN_EXISTING};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use super::WRITE_STALL_TIMEOUT;
use crate::config::{FlowControl, Parity, SerialConfig};

// DCB bit fields (winbase.h).
const F_BINARY: u32 = 1 << 0;
const F_PARITY: u32 = 1 << 1;
const F_OUTX_CTS_FLOW: u32 = 1 << 2;
const F_TX_CONTINUE_ON_XOFF: u32 = 1 << 7;
const F_OUTX: u32 = 1 << 8;
const F_INX: u32 = 1 << 9;
const DTR_CONTROL_SHIFT: u32 = 4;
const RTS_CONTROL_SHIFT: u32 = 12;
const CONTROL_DISABLE: u32 = 0;
const CONTROL_ENABLE: u32 = 1;
const CONTROL_HANDSHAKE: u32 = 2;

pub struct Port {
    handle: HANDLE,
    read_timeout: Duration,
    /// Writes go out in pieces of about 100 ms of line time, so a stall is
    /// noticed promptly and cancelling one never loses track of what was sent.
    write_chunk: usize,
    chunk_time: Duration,
}

// SAFETY: a Win32 file handle may be used from any thread, and every request
// below has its own OVERLAPPED and event that live until the request finishes.
unsafe impl Send for Port {}
unsafe impl Sync for Port {}

impl Port {
    pub fn open(cfg: &SerialConfig, read_timeout: Duration) -> io::Result<Port> {
        let name = cfg.port.trim_start_matches(r"\\.\");
        let path: Vec<u16> = format!(r"\\.\{name}").encode_utf16().chain(Some(0)).collect();
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let bits = 1 + cfg.data_bits as u32 + (cfg.parity != Parity::None) as u32 + cfg.stop_bits as u32;
        let line_rate = (cfg.baud_rate / bits).max(1) as usize; // bytes per second
        let write_chunk = (line_rate / 10).clamp(64, 16 * 1024);
        let port = Port {
            handle,
            read_timeout,
            write_chunk,
            chunk_time: Duration::from_secs_f64(write_chunk as f64 / line_rate as f64),
        };

        // Ask for roomier driver queues; drivers may ignore this.
        unsafe { SetupComm(handle, 64 * 1024, 16 * 1024) };

        let mut dcb: DCB = unsafe { mem::zeroed() };
        dcb.DCBlength = mem::size_of::<DCB>() as u32;
        if unsafe { GetCommState(handle, &mut dcb) } == 0 {
            return Err(io::Error::last_os_error());
        }
        configure(&mut dcb, cfg);
        if unsafe { SetCommState(handle, &dcb) } == 0 {
            let code = unsafe { GetLastError() };
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the adapter does not support these settings (baud rate, frame or flow control; os error {code})"
                ),
            ));
        }

        // Complete a read as soon as anything arrives, or after `read_timeout`.
        let timeouts = COMMTIMEOUTS {
            ReadIntervalTimeout: u32::MAX,
            ReadTotalTimeoutMultiplier: u32::MAX,
            ReadTotalTimeoutConstant: millis(read_timeout).max(1),
            WriteTotalTimeoutMultiplier: 0,
            WriteTotalTimeoutConstant: 0,
        };
        if unsafe { SetCommTimeouts(handle, &timeouts) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(port)
    }

    /// Returns `Ok(0)` when nothing arrived within the read timeout.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let ev = Event::new()?;
        let mut ov = ev.overlapped();
        let len = buf.len().min(u32::MAX as usize) as u32;
        let ok = unsafe { ReadFile(self.handle, buf.as_mut_ptr(), len, ptr::null_mut(), &mut ov) };
        // The comm timeouts end the read; the extra second only guards against a misbehaving driver.
        let (n, _) = self.finish(ok, &mut ov, &ev, self.read_timeout + Duration::from_secs(1), None)?;
        Ok(n)
    }

    /// Sends everything, or fails with `TimedOut` when nothing could be sent for
    /// [`WRITE_STALL_TIMEOUT`] (e.g. CTS held low, or XOFF from the device).
    /// Returns early when `abort` is set.
    pub fn write(&self, data: &[u8], abort: &AtomicBool) -> io::Result<()> {
        let mut rest = data;
        while !rest.is_empty() {
            if abort.load(Ordering::SeqCst) {
                return Ok(());
            }
            let part = &rest[..rest.len().min(self.write_chunk)];
            let ev = Event::new()?;
            let mut ov = ev.overlapped();
            let ok = unsafe { WriteFile(self.handle, part.as_ptr(), part.len() as u32, ptr::null_mut(), &mut ov) };
            let (n, _) = self.finish(ok, &mut ov, &ev, self.chunk_time + WRITE_STALL_TIMEOUT, Some(abort))?;
            if n == 0 && !abort.load(Ordering::SeqCst) {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "transmit stalled"));
            }
            rest = &rest[n..];
        }
        Ok(())
    }

    pub fn set_dtr(&self, on: bool) -> io::Result<()> {
        check(unsafe { EscapeCommFunction(self.handle, if on { SETDTR } else { CLRDTR }) })
    }

    pub fn set_rts(&self, on: bool) -> io::Result<()> {
        check(unsafe { EscapeCommFunction(self.handle, if on { SETRTS } else { CLRRTS }) })
    }

    pub fn set_break(&self, on: bool) -> io::Result<()> {
        check(unsafe {
            if on {
                SetCommBreak(self.handle)
            } else {
                ClearCommBreak(self.handle)
            }
        })
    }

    /// Wakes any thread blocked in [`read`](Self::read) or [`write`](Self::write).
    pub fn cancel_io(&self) {
        unsafe { CancelIoEx(self.handle, ptr::null()) };
    }

    /// Waits for an overlapped request started with result `ok`. If it is still
    /// pending after `limit` (or `abort` gets set) it is cancelled. Returns the
    /// bytes transferred and whether the request finished on its own.
    fn finish(
        &self,
        ok: BOOL,
        ov: &mut OVERLAPPED,
        ev: &Event,
        limit: Duration,
        abort: Option<&AtomicBool>,
    ) -> io::Result<(usize, bool)> {
        let mut finished = true;
        if ok == 0 {
            let err = unsafe { GetLastError() };
            if err != ERROR_IO_PENDING {
                return Err(io::Error::from_raw_os_error(err as i32));
            }
            let deadline = Instant::now() + limit;
            finished = loop {
                let slice = deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(50));
                if unsafe { WaitForSingleObject(ev.0, millis(slice)) } == WAIT_OBJECT_0 {
                    break true;
                }
                if Instant::now() >= deadline || abort.is_some_and(|a| a.load(Ordering::SeqCst)) {
                    break false;
                }
            };
            if !finished {
                unsafe { CancelIoEx(self.handle, ov) };
            }
        }
        // Always collect the result: the buffer and OVERLAPPED must outlive the request.
        let mut n = 0u32;
        if unsafe { GetOverlappedResult(self.handle, ov, &mut n, 1) } == 0 {
            let err = unsafe { GetLastError() };
            if err == ERROR_OPERATION_ABORTED {
                return Ok((n as usize, false));
            }
            return Err(io::Error::from_raw_os_error(err as i32));
        }
        Ok((n as usize, finished))
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        unsafe {
            CancelIoEx(self.handle, ptr::null());
            CloseHandle(self.handle);
        }
    }
}

fn configure(dcb: &mut DCB, cfg: &SerialConfig) {
    dcb.BaudRate = cfg.baud_rate;
    dcb.ByteSize = cfg.data_bits;
    dcb.Parity = match cfg.parity {
        Parity::None => NOPARITY,
        Parity::Odd => ODDPARITY,
        Parity::Even => EVENPARITY,
    };
    dcb.StopBits = if cfg.stop_bits == 2 { TWOSTOPBITS } else { ONESTOPBIT };

    let hardware = cfg.flow_control == FlowControl::Hardware;
    let software = cfg.flow_control == FlowControl::Software;
    // Start from a clean slate: in particular fAbortOnError and fNull must be off.
    let mut bits = F_BINARY | F_TX_CONTINUE_ON_XOFF;
    if cfg.parity != Parity::None {
        bits |= F_PARITY;
    }
    if hardware {
        bits |= F_OUTX_CTS_FLOW;
    }
    if software {
        bits |= F_OUTX | F_INX;
    }
    let dtr = if cfg.dtr { CONTROL_ENABLE } else { CONTROL_DISABLE };
    let rts = if hardware {
        CONTROL_HANDSHAKE
    } else if cfg.rts {
        CONTROL_ENABLE
    } else {
        CONTROL_DISABLE
    };
    dcb._bitfield = bits | dtr << DTR_CONTROL_SHIFT | rts << RTS_CONTROL_SHIFT;
    dcb.XonChar = 0x11;
    dcb.XoffChar = 0x13;
    if dcb.XonLim == 0 || dcb.XoffLim == 0 {
        dcb.XonLim = 2048;
        dcb.XoffLim = 512;
    }
}

fn check(ok: BOOL) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn millis(d: Duration) -> u32 {
    d.as_millis().min(u32::MAX as u128 - 1) as u32
}

/// Manual-reset event for one overlapped request.
struct Event(HANDLE);

impl Event {
    fn new() -> io::Result<Self> {
        let h = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if h.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(h))
        }
    }

    fn overlapped(&self) -> OVERLAPPED {
        let mut ov: OVERLAPPED = unsafe { mem::zeroed() };
        ov.hEvent = self.0;
        ov
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

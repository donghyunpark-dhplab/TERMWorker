//! Session logging.
//!
//! * `raw`         – the exact bytes received, before any decoding.
//! * `text`        – decoded text with ANSI escape sequences removed.
//! * `timestamped` – like `text`, with a `[YYYY-MM-DD HH:MM:SS.mmm] ` prefix on every line.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogMode {
    Raw,
    Text,
    Timestamped,
}

pub struct Logger {
    out: BufWriter<File>,
    path: PathBuf,
    mode: LogMode,
    strip: AnsiStripper,
    at_line_start: bool,
}

impl Logger {
    pub fn create(path: &Path, mode: LogMode) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut logger = Self {
            out: BufWriter::with_capacity(64 * 1024, file),
            path: path.to_path_buf(),
            mode,
            strip: AnsiStripper::default(),
            at_line_start: true,
        };
        if mode != LogMode::Raw {
            let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            writeln!(logger.out, "===== TermWorker log started {now} =====")?;
            logger.out.flush()?;
        }
        Ok(logger)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Record one chunk of received data. `raw` and `text` are the same chunk before/after decoding.
    pub fn write(&mut self, raw: &[u8], text: &str) -> io::Result<()> {
        match self.mode {
            LogMode::Raw => self.out.write_all(raw)?,
            LogMode::Text | LogMode::Timestamped => {
                let plain = self.strip.feed(text);
                let stamp = self.mode == LogMode::Timestamped;
                let mut buf = String::with_capacity(plain.len() + 32);
                for ch in plain.chars() {
                    if self.at_line_start && stamp {
                        buf.push_str(&chrono::Local::now().format("[%Y-%m-%d %H:%M:%S%.3f] ").to_string());
                    }
                    self.at_line_start = false;
                    buf.push(ch);
                    if ch == '\n' {
                        self.at_line_start = true;
                    }
                }
                self.out.write_all(buf.as_bytes())?;
            }
        }
        // Flush per chunk so the log survives a crash or a yanked cable.
        self.out.flush()
    }
}

/// Streaming filter that removes terminal control sequences and carriage returns,
/// leaving printable text, tabs and line feeds.
#[derive(Default)]
pub struct AnsiStripper {
    state: StripState,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum StripState {
    #[default]
    Normal,
    Esc,
    /// ESC followed by an intermediate byte such as `(` – one more byte to swallow.
    EscIntermediate,
    Csi,
    /// OSC / DCS / SOS / PM / APC – terminated by BEL or ST (ESC \).
    Str,
    StrEsc,
}

impl AnsiStripper {
    pub fn feed(&mut self, input: &str) -> String {
        use StripState::*;
        let mut out = String::with_capacity(input.len());
        for ch in input.chars() {
            self.state = match (self.state, ch) {
                (Normal, '\x1b') => Esc,
                (Normal, '\u{9b}') => Csi,
                (Normal, '\n' | '\t') => {
                    out.push(ch);
                    Normal
                }
                (Normal, c) if c.is_control() => Normal,
                (Normal, c) => {
                    out.push(c);
                    Normal
                }
                (Esc, '[') => Csi,
                (Esc, ']' | 'P' | 'X' | '^' | '_') => Str,
                (Esc, ' '..='/') => EscIntermediate,
                (Esc, _) | (EscIntermediate, _) => Normal,
                (Csi, '\u{40}'..='\u{7e}') => Normal,
                (Csi, _) => Csi,
                (Str, '\x07') => Normal,
                (Str, '\x1b') => StrEsc,
                (Str, _) => Str,
                (StrEsc, '\\') => Normal,
                (StrEsc, _) => Str,
            };
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_sgr_and_cursor_sequences() {
        let mut s = AnsiStripper::default();
        assert_eq!(s.feed("\x1b[1;32mOK\x1b[0m done\r\n"), "OK done\n");
        assert_eq!(s.feed("\x1b]0;title\x07prompt$ "), "prompt$ ");
        assert_eq!(s.feed("\x1b(Bplain\x1b=\x1b[?25h"), "plain");
    }

    #[test]
    fn strips_sequences_split_across_chunks() {
        let mut s = AnsiStripper::default();
        let a = s.feed("abc\x1b[3");
        let b = s.feed("1mdef");
        assert_eq!(format!("{a}{b}"), "abcdef");
    }

    #[test]
    fn timestamped_log_prefixes_each_line() {
        let dir = std::env::temp_dir().join(format!("termworker-test-{}", std::process::id()));
        let path = dir.join("t.log");
        let _ = fs::remove_file(&path);
        {
            let mut log = Logger::create(&path, LogMode::Timestamped).unwrap();
            log.write(b"", "U-Boot 2024.01\r\nDRAM").unwrap();
            log.write(b"", ": 512 MiB\r\n").unwrap();
        }
        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert!(lines[0].starts_with("===== TermWorker log started"));
        assert!(lines[1].starts_with('[') && lines[1].ends_with("] U-Boot 2024.01"), "{}", lines[1]);
        assert!(lines[2].ends_with("] DRAM: 512 MiB"), "{}", lines[2]);
        let _ = fs::remove_dir_all(&dir);
    }
}

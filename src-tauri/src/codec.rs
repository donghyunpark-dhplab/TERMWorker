//! Character-set conversion between the byte stream of a connection and the
//! UTF-16/UTF-8 world of the terminal widget.
//!
//! Decoding is streaming: a multi-byte character split across two reads is
//! reassembled correctly. Many embedded devices and legacy Korean
//! systems still emit CP949, so this is selectable per session.

use encoding_rs::{Decoder, Encoding, UTF_8};

use crate::config::Newline;

pub struct Codec {
    encoding: &'static Encoding,
    decoder: Decoder,
    enter: Newline,
}

impl Codec {
    pub fn new(label: &str, enter: Newline) -> Result<Self, String> {
        let encoding = lookup(label)?;
        Ok(Self { encoding, decoder: encoding.new_decoder_without_bom_handling(), enter })
    }

    pub fn set_encoding(&mut self, label: &str) -> Result<(), String> {
        let encoding = lookup(label)?;
        self.encoding = encoding;
        self.decoder = encoding.new_decoder_without_bom_handling();
        Ok(())
    }

    pub fn encoding_name(&self) -> &'static str {
        self.encoding.name()
    }

    /// Decode received bytes. Incomplete trailing sequences are buffered until the next call.
    pub fn decode(&mut self, bytes: &[u8]) -> String {
        let cap = self.decoder.max_utf8_buffer_length(bytes.len()).unwrap_or(bytes.len() * 3 + 16);
        let mut out = String::with_capacity(cap);
        let (_result, _read, _replaced) = self.decoder.decode_to_string(bytes, &mut out, false);
        out
    }

    /// Encode keyboard / paste input for transmission, applying the Enter-key mapping.
    /// The terminal widget always reports Enter (and pasted line breaks) as `\r`.
    pub fn encode_input(&self, text: &str) -> Vec<u8> {
        let mapped: std::borrow::Cow<str> = match self.enter {
            Newline::Cr => text.into(),
            other => {
                let nl = std::str::from_utf8(other.as_bytes()).unwrap();
                text.replace('\r', nl).into()
            }
        };
        self.encode(&mapped)
    }

    /// Encode text as it is (no Enter-key mapping).
    pub fn encode(&self, text: &str) -> Vec<u8> {
        if self.encoding == UTF_8 {
            return text.as_bytes().to_vec();
        }
        let (bytes, _, _) = self.encoding.encode(text);
        bytes.into_owned()
    }
}

fn lookup(label: &str) -> Result<&'static Encoding, String> {
    Encoding::for_label(label.trim().as_bytes()).ok_or_else(|| format!("Unknown encoding: {label}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_split_across_reads() {
        let mut c = Codec::new("utf-8", Newline::Cr).unwrap();
        let bytes = "한글".as_bytes();
        let a = c.decode(&bytes[..2]);
        let b = c.decode(&bytes[2..]);
        assert_eq!(format!("{a}{b}"), "한글");
    }

    #[test]
    fn cp949_roundtrip() {
        let mut c = Codec::new("euc-kr", Newline::Cr).unwrap();
        let encoded = c.encode_input("안녕");
        assert_eq!(encoded, vec![0xBE, 0xC8, 0xB3, 0xE7]);
        assert_eq!(c.decode(&encoded), "안녕");
    }

    #[test]
    fn enter_mapping() {
        let c = Codec::new("utf-8", Newline::Crlf).unwrap();
        assert_eq!(c.encode_input("ls\r"), b"ls\r\n");
        let c = Codec::new("utf-8", Newline::Lf).unwrap();
        assert_eq!(c.encode_input("a\rb\r"), b"a\nb\n");
    }

    #[test]
    fn rejects_unknown_encoding() {
        assert!(Codec::new("klingon", Newline::Cr).is_err());
    }
}

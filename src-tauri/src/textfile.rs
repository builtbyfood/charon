//! Bytes ⇄ editor text. Detects BOM / UTF-8 / Latin-1 and the line ending on
//! the way in; re-encodes with the chosen encoding and EOL on the way out.
//! Pure so it is testable; the file I/O lives behind `sftp::Fs`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoding {
    #[serde(rename = "utf-8")]
    Utf8,
    #[serde(rename = "utf-8-bom")]
    Utf8Bom,
    #[serde(rename = "latin-1")]
    Latin1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Eol {
    Lf,
    Crlf,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TextFile {
    /// Always LF-separated; the editor applies `eol` on display and save.
    pub text: String,
    pub encoding: Encoding,
    pub eol: Eol,
    pub size: u64,
}

const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// NUL in the first 8 KiB = not text.
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|&b| b == 0)
}

pub fn decode(bytes: &[u8]) -> TextFile {
    let size = bytes.len() as u64;
    let (encoding, body) = match bytes.strip_prefix(BOM) {
        Some(rest) => (Encoding::Utf8Bom, rest),
        None => (Encoding::Utf8, bytes),
    };
    let (encoding, raw) = match std::str::from_utf8(body) {
        Ok(s) => (encoding, s.to_string()),
        // Not UTF-8: every byte is one Latin-1 char, so nothing is lost on a round trip.
        Err(_) => (Encoding::Latin1, body.iter().map(|&b| b as char).collect()),
    };
    // First line break decides the EOL; lone CRs (classic Mac) are treated as LF.
    let eol = match raw.find('\n') {
        Some(i) if i > 0 && raw.as_bytes()[i - 1] == b'\r' => Eol::Crlf,
        _ => Eol::Lf,
    };
    let text = raw.replace("\r\n", "\n").replace('\r', "\n");
    TextFile { text, encoding, eol, size }
}

pub fn encode(text: &str, encoding: Encoding, eol: Eol) -> Vec<u8> {
    let normalised = text.replace("\r\n", "\n");
    let with_eol = match eol {
        Eol::Lf => normalised,
        Eol::Crlf => normalised.replace('\n', "\r\n"),
    };
    match encoding {
        Encoding::Utf8 => with_eol.into_bytes(),
        Encoding::Utf8Bom => {
            let mut v = BOM.to_vec();
            v.extend_from_slice(with_eol.as_bytes());
            v
        }
        // Characters outside Latin-1 become '?', as Notepad++ does.
        Encoding::Latin1 => with_eol.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_encoding_and_eol() {
        let t = decode(b"a\r\nb\r\n");
        assert_eq!((t.encoding, t.eol, t.text.as_str()), (Encoding::Utf8, Eol::Crlf, "a\nb\n"));
        let t = decode(b"\xEF\xBB\xBFx\ny");
        assert_eq!((t.encoding, t.eol, t.text.as_str()), (Encoding::Utf8Bom, Eol::Lf, "x\ny"));
        let t = decode(b"caf\xE9\n");
        assert_eq!((t.encoding, t.text.as_str()), (Encoding::Latin1, "café\n"));
        let t = decode(b"no newline");
        assert_eq!(t.eol, Eol::Lf);
        assert!(looks_binary(b"\x7fELF\0\0"));
        assert!(!looks_binary("plain ünïcode\n".as_bytes()));
    }

    #[test]
    fn wire_names_match_the_ui() {
        assert_eq!(serde_json::to_string(&Encoding::Utf8Bom).unwrap(), "\"utf-8-bom\"");
        assert_eq!(serde_json::to_string(&Encoding::Latin1).unwrap(), "\"latin-1\"");
        assert_eq!(serde_json::to_string(&Eol::Crlf).unwrap(), "\"crlf\"");
        assert_eq!(serde_json::from_str::<Encoding>("\"utf-8\"").unwrap(), Encoding::Utf8);
    }

    #[test]
    fn round_trips_and_converts() {
        let src = b"one\r\ntwo\r\n";
        let t = decode(src);
        assert_eq!(encode(&t.text, t.encoding, t.eol), src);
        assert_eq!(encode(&t.text, Encoding::Utf8, Eol::Lf), b"one\ntwo\n");
        assert_eq!(encode("x\n", Encoding::Utf8Bom, Eol::Lf), b"\xEF\xBB\xBFx\n");
        assert_eq!(encode("café ✓\n", Encoding::Latin1, Eol::Crlf), b"caf\xE9 ?\r\n");
        let l = decode(b"\xE9\xFF");
        assert_eq!(encode(&l.text, Encoding::Latin1, Eol::Lf), b"\xE9\xFF");
    }
}

//! The formatting and input-parsing the Reticulum client tools share.
//!
//! `rnprobe` and `rnpath` render hashes, parse a destination argument and
//! animate their wait with the same code in the reference tree (both go
//! through `RNS.prettyhexrep` and carry the identical `syms` string), so
//! `lnprobe` and `lnpath` share it here rather than each keeping a copy
//! that can drift out of parity on its own.
//!
//! Each binary uses its own subset; the module is `#[allow(dead_code)]`
//! at every include site for that reason.

use std::io::{IsTerminal, Write as _};

use leviculum_std::DestinationHash;

pub fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("hex string has odd length".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// Python `RNS.prettyhexrep`: `<hex>`.
pub fn prettyhexrep(bytes: &[u8]) -> String {
    format!("<{}>", hex_encode(bytes))
}

/// The destination argument both tools take: 32 hex characters, 16 bytes,
/// with the reference tools' two error sentences (rnprobe.py:58-64,
/// rnpath.py:439-442).
pub fn parse_destination_hash(hex: &str) -> Result<DestinationHash, String> {
    if hex.len() != 32 {
        return Err(
            "Destination length is invalid, must be 32 hexadecimal characters (16 bytes).".into(),
        );
    }
    let mut bytes = [0u8; 16];
    for (i, chunk) in bytes.iter_mut().enumerate() {
        *chunk = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| "Invalid destination entered. Check your input.".to_string())?;
    }
    Ok(DestinationHash::new(bytes))
}

/// The animated wait indicator (rnprobe.py:86, rnpath.py:453-457). Only
/// animated on a TTY so piped output stays clean; the surrounding text is
/// unchanged either way.
pub struct Spinner {
    syms: Vec<char>,
    i: usize,
    tty: bool,
}

impl Spinner {
    pub fn new() -> Self {
        Self {
            syms: "⢄⢂⢁⡁⡈⡐⡠".chars().collect(),
            i: 0,
            tty: std::io::stdout().is_terminal(),
        }
    }

    pub fn tick(&mut self) {
        if !self.tty {
            return;
        }
        print!("\u{8}\u{8}{} ", self.syms[self.i]);
        let _ = std::io::stdout().flush();
        self.i = (self.i + 1) % self.syms.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_hash_parsing_matches_the_reference_rules() {
        assert!(parse_destination_hash("a1b2c3d4e5f6a7b8a1b2c3d4e5f6a7b8").is_ok());
        // Wrong length
        assert!(parse_destination_hash("a1b2").is_err());
        // Right length, not hex
        assert!(parse_destination_hash("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz").is_err());
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(hex_encode(&[0x00, 0xff, 0x10]), "00ff10");
        assert_eq!(hex_decode("00ff10").unwrap(), vec![0x00, 0xff, 0x10]);
        assert!(hex_decode("abc").is_err());
        assert_eq!(prettyhexrep(&[0xaa, 0xbb]), "<aabb>");
    }
}

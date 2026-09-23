//! Library face of the CLI crate.
//!
//! The binaries in this crate are the product; this lib target exists so the
//! transfer logic `lncp` runs ([`cp`]) can be driven by an integration test in
//! another crate. A test that re-implements the push loop instead is a parallel
//! driver, and a parallel driver proves nothing about the tool a user runs —
//! see the reproducer in
//! `leviculum-std/tests/rnsd_interop/lncp_identify_loss_interop_tests.rs`, which
//! calls [`cp::run_send`] directly.
//!
//! Only what a test needs is public. The binaries keep their own `main`, argument
//! parsing and per-tool modules.

use std::fmt::Write;

pub mod cp;

/// Lowercase hex of `bytes`, the form every destination and identity hash is
/// printed and parsed in across the tools.
pub fn hex_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Inverse of [`hex_encode`], rejecting odd lengths and non-hex digits.
pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("hex string has odd length".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

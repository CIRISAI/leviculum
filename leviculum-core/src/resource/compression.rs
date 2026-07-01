//! Bzip2 compression/decompression wrappers for Resource transfers.
//!
//! Gated behind the `compression` cargo feature (uses `libbz2-rs-sys`).
//! Python Reticulum uses `bz2.compress()` / `bz2.decompress()`.

#[cfg(feature = "compression")]
use super::ResourceError;
#[cfg(feature = "compression")]
use alloc::vec;
#[cfg(feature = "compression")]
use alloc::vec::Vec;

/// Compress data using bzip2 (matching Python's `bz2.compress(data)`).
///
/// Uses block size 9 (900k) and work factor 0 (default = 30), matching
/// Python's default bz2 compression settings.
#[cfg(feature = "compression")]
pub(crate) fn bz2_compress(input: &[u8]) -> Result<Vec<u8>, ResourceError> {
    use core::ffi::{c_char, c_int, c_uint};

    // bzip2 worst-case expansion is ~1.01x + 600 bytes
    let max_output = input.len() + input.len() / 100 + 600 + 1;
    let mut output = vec![0u8; max_output];
    let mut dest_len: c_uint = max_output as c_uint;

    let ret = unsafe {
        libbz2_rs_sys::BZ2_bzBuffToBuffCompress(
            output.as_mut_ptr() as *mut c_char,
            &mut dest_len,
            input.as_ptr() as *mut c_char,
            input.len() as c_uint,
            9 as c_int, // blockSize100k = 9 (Python default)
            0 as c_int, // verbosity = 0
            0 as c_int, // workFactor = 0 (default = 30)
        )
    };

    if ret != libbz2_rs_sys::BZ_OK {
        return Err(ResourceError::CompressionFailed);
    }

    output.truncate(dest_len as usize);
    Ok(output)
}

/// Decompress bzip2 data (matching Python's `bz2.decompress(data)`).
///
/// `expected_size` is used as a hint for the output buffer size
/// (from the advertisement's `data_size` field).
#[cfg(feature = "compression")]
pub(crate) fn bz2_decompress(input: &[u8], expected_size: usize) -> Result<Vec<u8>, ResourceError> {
    use core::ffi::{c_char, c_int, c_uint};

    // Start with expected size + margin, retry with larger buffer if needed
    let mut buf_size = expected_size.saturating_add(expected_size / 10).max(1024);

    for _ in 0..4 {
        let mut output = vec![0u8; buf_size];
        let mut dest_len: c_uint = buf_size as c_uint;

        let ret = unsafe {
            libbz2_rs_sys::BZ2_bzBuffToBuffDecompress(
                output.as_mut_ptr() as *mut c_char,
                &mut dest_len,
                input.as_ptr() as *mut c_char,
                input.len() as c_uint,
                0 as c_int, // small = 0 (use normal algorithm)
                0 as c_int, // verbosity = 0
            )
        };

        if ret == libbz2_rs_sys::BZ_OK {
            output.truncate(dest_len as usize);
            return Ok(output);
        }

        if ret == libbz2_rs_sys::BZ_OUTBUFF_FULL {
            buf_size = buf_size.saturating_mul(2);
            continue;
        }

        return Err(ResourceError::DecompressionFailed);
    }

    Err(ResourceError::DecompressionFailed)
}

#[cfg(test)]
#[cfg(feature = "compression")]
mod tests {
    use super::*;

    #[test]
    fn test_compress_decompress_roundtrip() {
        let data = b"Hello, this is test data for bzip2 compression!";
        let compressed = bz2_compress(data).unwrap();
        let decompressed = bz2_decompress(&compressed, data.len()).unwrap();
        assert_eq!(&decompressed, data);
    }

    #[test]
    fn test_compress_actually_compresses_repetitive_data() {
        let data = vec![0x42u8; 10000];
        let compressed = bz2_compress(&data).unwrap();
        assert!(
            compressed.len() < data.len(),
            "compressed {} vs original {}",
            compressed.len(),
            data.len()
        );
    }

    #[test]
    fn test_decompress_bad_data() {
        let result = bz2_decompress(b"not valid bz2", 100);
        assert!(result.is_err());
    }

    #[test]
    fn test_compress_empty() {
        let compressed = bz2_compress(b"").unwrap();
        let decompressed = bz2_decompress(&compressed, 0).unwrap();
        assert!(decompressed.is_empty());
    }
}

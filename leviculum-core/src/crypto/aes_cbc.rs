//! AES-256-CBC encryption/decryption with PKCS7 padding

use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use cbc::{Decryptor, Encryptor};

type Aes256CbcEnc = Encryptor<aes::Aes256>;
type Aes256CbcDec = Decryptor<aes::Aes256>;

use crate::constants::{AES256_KEY_SIZE, AES_BLOCK_SIZE};

/// AES-256-CBC encryption error
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AesError {
    /// Invalid key length (must be 32 bytes)
    InvalidKeyLength,
    /// Invalid IV length (must be 16 bytes)
    InvalidIvLength,
    /// Buffer too small for encrypted output
    BufferTooSmall,
    /// Decryption failed (invalid padding or ciphertext)
    DecryptionFailed,
}

impl core::fmt::Display for AesError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AesError::InvalidKeyLength => write!(f, "invalid key length"),
            AesError::InvalidIvLength => write!(f, "invalid IV length"),
            AesError::BufferTooSmall => write!(f, "buffer too small"),
            AesError::DecryptionFailed => write!(f, "decryption failed"),
        }
    }
}

/// Apply PKCS7 padding to a buffer
fn pkcs7_pad(data: &[u8], output: &mut [u8]) -> usize {
    let padding_len = AES_BLOCK_SIZE - (data.len() % AES_BLOCK_SIZE);
    let total_len = data.len() + padding_len;
    output[..data.len()].copy_from_slice(data);
    output[data.len()..total_len].fill(padding_len as u8);
    total_len
}

/// Remove PKCS7 padding and return valid length
fn pkcs7_unpad(data: &[u8]) -> Result<usize, AesError> {
    if data.is_empty() {
        return Err(AesError::DecryptionFailed);
    }
    let padding_len = data[data.len() - 1] as usize;
    if padding_len == 0 || padding_len > AES_BLOCK_SIZE || padding_len > data.len() {
        return Err(AesError::DecryptionFailed);
    }
    // Verify all padding bytes are correct
    for &byte in &data[data.len() - padding_len..] {
        if byte as usize != padding_len {
            return Err(AesError::DecryptionFailed);
        }
    }
    Ok(data.len() - padding_len)
}

/// Encrypt data using AES-256-CBC with PKCS7 padding
///
/// Returns ciphertext length written to the output buffer.
/// The output buffer must be large enough to hold plaintext + padding (up to 16 extra bytes).
pub fn aes256_cbc_encrypt(
    key: &[u8],
    iv: &[u8],
    plaintext: &[u8],
    output: &mut [u8],
) -> Result<usize, AesError> {
    if key.len() != AES256_KEY_SIZE {
        return Err(AesError::InvalidKeyLength);
    }
    if iv.len() != AES_BLOCK_SIZE {
        return Err(AesError::InvalidIvLength);
    }

    // Calculate padded length
    let padded_len = ((plaintext.len() / AES_BLOCK_SIZE) + 1) * AES_BLOCK_SIZE;
    if output.len() < padded_len {
        return Err(AesError::BufferTooSmall);
    }

    // Apply PKCS7 padding
    let total_len = pkcs7_pad(plaintext, output);

    // Create cipher and encrypt in place
    let mut cipher =
        Aes256CbcEnc::new_from_slices(key, iv).expect("key/iv lengths validated above");

    // Encrypt block by block
    for chunk in output[..total_len].chunks_exact_mut(AES_BLOCK_SIZE) {
        cipher.encrypt_block_mut(chunk.into());
    }

    Ok(total_len)
}

/// Decrypt data using AES-256-CBC with PKCS7 padding
///
/// Returns plaintext length written to the output buffer.
pub fn aes256_cbc_decrypt(
    key: &[u8],
    iv: &[u8],
    ciphertext: &[u8],
    output: &mut [u8],
) -> Result<usize, AesError> {
    if key.len() != AES256_KEY_SIZE {
        return Err(AesError::InvalidKeyLength);
    }
    if iv.len() != AES_BLOCK_SIZE {
        return Err(AesError::InvalidIvLength);
    }
    if !ciphertext.len().is_multiple_of(AES_BLOCK_SIZE) || ciphertext.is_empty() {
        return Err(AesError::DecryptionFailed);
    }
    if output.len() < ciphertext.len() {
        return Err(AesError::BufferTooSmall);
    }

    // Copy ciphertext to output buffer
    output[..ciphertext.len()].copy_from_slice(ciphertext);

    // Create cipher and decrypt in place
    let mut cipher =
        Aes256CbcDec::new_from_slices(key, iv).expect("key/iv lengths validated above");

    // Decrypt block by block
    for chunk in output[..ciphertext.len()].chunks_exact_mut(AES_BLOCK_SIZE) {
        cipher.decrypt_block_mut(chunk.into());
    }

    // Remove PKCS7 padding
    pkcs7_unpad(&output[..ciphertext.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pkcs7_padding() {
        let data = b"hello";
        let mut output = [0u8; 16];
        let len = pkcs7_pad(data, &mut output);
        assert_eq!(len, 16);
        // "hello" is 5 bytes, padding should be 11 bytes of 0x0b
        assert_eq!(&output[5..16], &[11u8; 11]);
    }

    #[test]
    fn test_pkcs7_unpad() {
        let mut data = [0u8; 16];
        data[..5].copy_from_slice(b"hello");
        data[5..16].fill(11);
        let len = pkcs7_unpad(&data).unwrap();
        assert_eq!(len, 5);
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = [0x42u8; 32];
        let iv = [0x13u8; 16];
        let plaintext = b"Hello, Reticulum!";

        let mut encrypted = [0u8; 32]; // 17 bytes + padding = 32 bytes
        let enc_len = aes256_cbc_encrypt(&key, &iv, plaintext, &mut encrypted).unwrap();
        assert_eq!(enc_len, 32);

        let mut decrypted = [0u8; 32];
        let dec_len = aes256_cbc_decrypt(&key, &iv, &encrypted[..enc_len], &mut decrypted).unwrap();
        assert_eq!(dec_len, plaintext.len());
        assert_eq!(&decrypted[..dec_len], plaintext);
    }

    #[test]
    fn test_encrypt_block_aligned() {
        let key = [0x42u8; 32];
        let iv = [0x13u8; 16];
        let plaintext = [0xab; 16]; // Exactly one block

        let mut encrypted = [0u8; 32]; // Will need extra block for padding
        let enc_len = aes256_cbc_encrypt(&key, &iv, &plaintext, &mut encrypted).unwrap();
        assert_eq!(enc_len, 32); // 16 bytes data + 16 bytes padding block
    }

    #[test]
    fn test_invalid_key_length() {
        let key = [0x42u8; 16]; // Wrong size
        let iv = [0x13u8; 16];
        let plaintext = b"test";
        let mut output = [0u8; 32];

        let result = aes256_cbc_encrypt(&key, &iv, plaintext, &mut output);
        assert_eq!(result, Err(AesError::InvalidKeyLength));
    }

    #[test]
    fn test_invalid_iv_length() {
        let key = [0x42u8; 32];
        let iv = [0x13u8; 8]; // Wrong size
        let plaintext = b"test";
        let mut output = [0u8; 32];

        let result = aes256_cbc_encrypt(&key, &iv, plaintext, &mut output);
        assert_eq!(result, Err(AesError::InvalidIvLength));
    }

    // ==================== EDGE CASE TESTS ====================

    #[test]
    fn test_empty_plaintext() {
        // Empty plaintext should produce exactly one block (all padding)
        let key = [0x42u8; 32];
        let iv = [0x13u8; 16];
        let plaintext = b"";

        let mut encrypted = [0u8; 16];
        let enc_len = aes256_cbc_encrypt(&key, &iv, plaintext, &mut encrypted).unwrap();
        assert_eq!(enc_len, 16); // One full block of padding (16 bytes of 0x10)

        let mut decrypted = [0u8; 16];
        let dec_len = aes256_cbc_decrypt(&key, &iv, &encrypted[..enc_len], &mut decrypted).unwrap();
        assert_eq!(dec_len, 0); // Empty plaintext
    }

    #[test]
    fn test_buffer_too_small_encrypt() {
        let key = [0x42u8; 32];
        let iv = [0x13u8; 16];
        let plaintext = b"Hello, World!"; // 13 bytes -> needs 16 bytes output

        let mut output = [0u8; 8]; // Too small
        let result = aes256_cbc_encrypt(&key, &iv, plaintext, &mut output);
        assert_eq!(result, Err(AesError::BufferTooSmall));
    }

    #[test]
    fn test_buffer_too_small_decrypt() {
        let key = [0x42u8; 32];
        let iv = [0x13u8; 16];
        let plaintext = b"Hello, World!";

        let mut encrypted = [0u8; 16];
        let enc_len = aes256_cbc_encrypt(&key, &iv, plaintext, &mut encrypted).unwrap();

        let mut output = [0u8; 8]; // Too small
        let result = aes256_cbc_decrypt(&key, &iv, &encrypted[..enc_len], &mut output);
        assert_eq!(result, Err(AesError::BufferTooSmall));
    }

    #[test]
    fn test_invalid_ciphertext_length() {
        // Ciphertext must be multiple of block size
        let key = [0x42u8; 32];
        let iv = [0x13u8; 16];
        let bad_ciphertext = [0u8; 17]; // Not a multiple of 16

        let mut output = [0u8; 32];
        let result = aes256_cbc_decrypt(&key, &iv, &bad_ciphertext, &mut output);
        assert_eq!(result, Err(AesError::DecryptionFailed));
    }

    #[test]
    fn test_empty_ciphertext() {
        let key = [0x42u8; 32];
        let iv = [0x13u8; 16];
        let empty_ciphertext: [u8; 0] = [];

        let mut output = [0u8; 16];
        let result = aes256_cbc_decrypt(&key, &iv, &empty_ciphertext, &mut output);
        assert_eq!(result, Err(AesError::DecryptionFailed));
    }

    #[test]
    fn test_invalid_padding_zero() {
        // Padding byte of 0 is invalid
        // Test pkcs7_unpad directly with invalid padding
        let mut bad_block = [0u8; 16];
        bad_block[15] = 0; // Invalid padding value

        // Create "ciphertext" that would decrypt to this bad padding
        // Instead, let's test pkcs7_unpad directly
        let result = pkcs7_unpad(&bad_block);
        assert_eq!(result, Err(AesError::DecryptionFailed));
    }

    #[test]
    fn test_invalid_padding_too_large() {
        // Padding byte larger than block size is invalid
        let mut bad_block = [0u8; 16];
        bad_block[15] = 17; // Invalid: larger than block size

        let result = pkcs7_unpad(&bad_block);
        assert_eq!(result, Err(AesError::DecryptionFailed));
    }

    #[test]
    fn test_invalid_padding_inconsistent() {
        // All padding bytes must be the same value
        let mut bad_block = [0u8; 16];
        // Claim 4 bytes of padding
        bad_block[15] = 4;
        bad_block[14] = 4;
        bad_block[13] = 4;
        bad_block[12] = 3; // Inconsistent!

        let result = pkcs7_unpad(&bad_block);
        assert_eq!(result, Err(AesError::DecryptionFailed));
    }

    #[test]
    fn test_wrong_key_decrypt() {
        let key1 = [0x42u8; 32];
        let key2 = [0x43u8; 32]; // Different key
        let iv = [0x13u8; 16];
        let plaintext = b"Secret message";

        let mut encrypted = [0u8; 16];
        let enc_len = aes256_cbc_encrypt(&key1, &iv, plaintext, &mut encrypted).unwrap();

        // Decrypting with wrong key should fail (bad padding after decryption)
        let mut decrypted = [0u8; 16];
        let result = aes256_cbc_decrypt(&key2, &iv, &encrypted[..enc_len], &mut decrypted);
        // This will likely produce invalid padding
        assert!(
            result.is_err() || {
                // Or if it happens to produce valid-looking padding, the data will be garbage
                let len = result.unwrap();
                &decrypted[..len] != plaintext
            }
        );
    }

    #[test]
    fn test_wrong_iv_decrypt() {
        let key = [0x42u8; 32];
        let iv1 = [0x13u8; 16];
        let iv2 = [0x14u8; 16]; // Different IV
        let plaintext = b"Secret message";

        let mut encrypted = [0u8; 16];
        let enc_len = aes256_cbc_encrypt(&key, &iv1, plaintext, &mut encrypted).unwrap();

        // Decrypting with wrong IV - first block will be corrupted
        let mut decrypted = [0u8; 16];
        let result = aes256_cbc_decrypt(&key, &iv2, &encrypted[..enc_len], &mut decrypted);

        // With wrong IV, first block is XORed with wrong value
        // The padding in last block might still be valid, but data is corrupted
        if let Ok(len) = result {
            assert_ne!(&decrypted[..len], plaintext);
        }
        // Or it might fail due to corrupted padding
    }

    #[test]
    fn test_multi_block_roundtrip() {
        let key = [0x42u8; 32];
        let iv = [0x13u8; 16];
        // 100 bytes = 6 full blocks + 4 bytes -> 7 blocks total with padding
        let plaintext = [0xab; 100];

        let mut encrypted = [0u8; 112]; // 7 * 16 = 112
        let enc_len = aes256_cbc_encrypt(&key, &iv, &plaintext, &mut encrypted).unwrap();
        assert_eq!(enc_len, 112);

        let mut decrypted = [0u8; 112];
        let dec_len = aes256_cbc_decrypt(&key, &iv, &encrypted[..enc_len], &mut decrypted).unwrap();
        assert_eq!(dec_len, 100);
        assert_eq!(&decrypted[..dec_len], &plaintext);
    }

    #[test]
    fn test_single_byte_plaintext() {
        let key = [0x42u8; 32];
        let iv = [0x13u8; 16];
        let plaintext = [0xaa; 1];

        let mut encrypted = [0u8; 16];
        let enc_len = aes256_cbc_encrypt(&key, &iv, &plaintext, &mut encrypted).unwrap();
        assert_eq!(enc_len, 16);

        let mut decrypted = [0u8; 16];
        let dec_len = aes256_cbc_decrypt(&key, &iv, &encrypted[..enc_len], &mut decrypted).unwrap();
        assert_eq!(dec_len, 1);
        assert_eq!(decrypted[0], 0xaa);
    }

    #[test]
    fn test_deterministic_encryption() {
        // Same key + IV + plaintext should always produce same ciphertext
        let key = [0x42u8; 32];
        let iv = [0x13u8; 16];
        let plaintext = b"deterministic test";

        let mut enc1 = [0u8; 32];
        let mut enc2 = [0u8; 32];

        let len1 = aes256_cbc_encrypt(&key, &iv, plaintext, &mut enc1).unwrap();
        let len2 = aes256_cbc_encrypt(&key, &iv, plaintext, &mut enc2).unwrap();

        assert_eq!(len1, len2);
        assert_eq!(&enc1[..len1], &enc2[..len2]);
    }

    #[test]
    fn test_decrypt_invalid_key_length() {
        let key = [0x42u8; 16]; // Wrong size
        let iv = [0x13u8; 16];
        let ciphertext = [0u8; 16];
        let mut output = [0u8; 16];

        let result = aes256_cbc_decrypt(&key, &iv, &ciphertext, &mut output);
        assert_eq!(result, Err(AesError::InvalidKeyLength));
    }

    #[test]
    fn test_decrypt_invalid_iv_length() {
        let key = [0x42u8; 32];
        let iv = [0x13u8; 8]; // Wrong size
        let ciphertext = [0u8; 16];
        let mut output = [0u8; 16];

        let result = aes256_cbc_decrypt(&key, &iv, &ciphertext, &mut output);
        assert_eq!(result, Err(AesError::InvalidIvLength));
    }

    #[test]
    fn test_full_padding_block() {
        // When plaintext is exactly n*16 bytes, we need an extra full padding block
        let key = [0x42u8; 32];
        let iv = [0x13u8; 16];
        let plaintext = [0xab; 32]; // Exactly 2 blocks

        let mut encrypted = [0u8; 48]; // Need 3 blocks for output
        let enc_len = aes256_cbc_encrypt(&key, &iv, &plaintext, &mut encrypted).unwrap();
        assert_eq!(enc_len, 48); // 32 bytes data + 16 bytes padding block

        let mut decrypted = [0u8; 48];
        let dec_len = aes256_cbc_decrypt(&key, &iv, &encrypted[..enc_len], &mut decrypted).unwrap();
        assert_eq!(dec_len, 32);
        assert_eq!(&decrypted[..dec_len], &plaintext);
    }
}

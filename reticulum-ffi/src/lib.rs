//! C-API/FFI bindings for reticulum
//!
//! This crate provides a C-compatible API for using reticulum from
//! other programming languages. The API follows these conventions:
//!
//! - All functions are prefixed with `lns_` (reticulum namespace)
//! - Opaque pointers are used for complex types
//! - Error codes are returned as integers
//! - Strings are passed as null-terminated C strings
//! - Memory allocated by the library must be freed using the corresponding free function

#![allow(clippy::missing_safety_doc)]
#![warn(unreachable_pub)]

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::ptr;

use reticulum_core::constants::TRUNCATED_HASHBYTES;

// Error codes
pub const LNS_OK: c_int = 0;
pub const LNS_ERR_NULL_PTR: c_int = -1;
pub const LNS_ERR_INVALID_ARG: c_int = -2;
pub const LNS_ERR_INIT_FAILED: c_int = -3;
pub const LNS_ERR_NOT_RUNNING: c_int = -4;
pub const LNS_ERR_ALREADY_RUNNING: c_int = -5;
pub const LNS_ERR_IO: c_int = -6;
pub const LNS_ERR_CRYPTO: c_int = -7;
pub const LNS_ERR_BUFFER_TOO_SMALL: c_int = -8;

/// Opaque handle to a Reticulum instance
pub struct LnsReticulum {
    // Will hold the actual Reticulum instance and runtime
    _runtime: tokio::runtime::Runtime,
    // instance: reticulum_std::Reticulum,
}

/// Opaque handle to an Identity
pub struct LnsIdentity {
    inner: reticulum_core::Identity,
}

/// Opaque handle to a Destination
pub struct LnsDestination {
    // Will hold the actual Destination
    _placeholder: (),
}

/// Initialize the library (call once at startup)
#[no_mangle]
pub extern "C" fn lns_init() -> c_int {
    // Initialize logging, etc.
    LNS_OK
}

/// Get the library version string
#[no_mangle]
pub extern "C" fn lns_version() -> *const c_char {
    static VERSION: &[u8] = b"0.1.0\0";
    VERSION.as_ptr() as *const c_char
}

// --- Identity functions ---

/// Create a new random identity
#[no_mangle]
pub extern "C" fn lns_identity_new() -> *mut LnsIdentity {
    let identity = reticulum_core::Identity::generate(&mut rand_core::OsRng);
    Box::into_raw(Box::new(LnsIdentity { inner: identity }))
}

/// Free an identity
#[no_mangle]
pub unsafe extern "C" fn lns_identity_free(identity: *mut LnsIdentity) {
    if !identity.is_null() {
        drop(Box::from_raw(identity));
    }
}

/// Get the identity hash (16 bytes)
#[no_mangle]
pub unsafe extern "C" fn lns_identity_hash(
    identity: *const LnsIdentity,
    out_hash: *mut u8,
    out_len: *mut usize,
) -> c_int {
    if identity.is_null() || out_hash.is_null() || out_len.is_null() {
        return LNS_ERR_NULL_PTR;
    }

    let identity = &(*identity).inner;
    let hash = identity.hash();

    if *out_len < TRUNCATED_HASHBYTES {
        *out_len = TRUNCATED_HASHBYTES;
        return LNS_ERR_BUFFER_TOO_SMALL;
    }

    ptr::copy_nonoverlapping(hash.as_ptr(), out_hash, TRUNCATED_HASHBYTES);
    *out_len = TRUNCATED_HASHBYTES;

    LNS_OK
}

/// Get the public key bytes (64 bytes)
#[no_mangle]
pub unsafe extern "C" fn lns_identity_public_key(
    identity: *const LnsIdentity,
    out_key: *mut u8,
    out_len: *mut usize,
) -> c_int {
    if identity.is_null() || out_key.is_null() || out_len.is_null() {
        return LNS_ERR_NULL_PTR;
    }

    let identity = &(*identity).inner;
    let key = identity.public_key_bytes();

    if *out_len < key.len() {
        *out_len = key.len();
        return LNS_ERR_BUFFER_TOO_SMALL;
    }

    ptr::copy_nonoverlapping(key.as_ptr(), out_key, key.len());
    *out_len = key.len();

    LNS_OK
}

/// Sign a message
#[no_mangle]
pub unsafe extern "C" fn lns_identity_sign(
    identity: *const LnsIdentity,
    message: *const u8,
    message_len: usize,
    out_signature: *mut u8,
    out_len: *mut usize,
) -> c_int {
    if identity.is_null() || message.is_null() || out_signature.is_null() || out_len.is_null() {
        return LNS_ERR_NULL_PTR;
    }

    let identity = &(*identity).inner;
    let message = std::slice::from_raw_parts(message, message_len);

    match identity.sign(message) {
        Ok(sig) => {
            if *out_len < sig.len() {
                *out_len = sig.len();
                return LNS_ERR_BUFFER_TOO_SMALL;
            }
            ptr::copy_nonoverlapping(sig.as_ptr(), out_signature, sig.len());
            *out_len = sig.len();
            LNS_OK
        }
        Err(_) => LNS_ERR_CRYPTO,
    }
}

/// Verify a signature
#[no_mangle]
pub unsafe extern "C" fn lns_identity_verify(
    identity: *const LnsIdentity,
    message: *const u8,
    message_len: usize,
    signature: *const u8,
    signature_len: usize,
) -> c_int {
    if identity.is_null() || message.is_null() || signature.is_null() {
        return LNS_ERR_NULL_PTR;
    }

    let identity = &(*identity).inner;
    let message = std::slice::from_raw_parts(message, message_len);
    let signature = std::slice::from_raw_parts(signature, signature_len);

    match identity.verify(message, signature) {
        Ok(true) => LNS_OK,
        Ok(false) | Err(_) => LNS_ERR_CRYPTO,
    }
}

/// Load identity from private key bytes (64 bytes)
#[no_mangle]
pub unsafe extern "C" fn lns_identity_from_private_key(
    key: *const u8,
    key_len: usize,
) -> *mut LnsIdentity {
    if key.is_null() || key_len != 64 {
        return ptr::null_mut();
    }

    let key_bytes = std::slice::from_raw_parts(key, key_len);

    match reticulum_core::Identity::from_private_key_bytes(key_bytes) {
        Ok(identity) => Box::into_raw(Box::new(LnsIdentity { inner: identity })),
        Err(_) => ptr::null_mut(),
    }
}

/// Load identity from public key bytes (64 bytes)
#[no_mangle]
pub unsafe extern "C" fn lns_identity_from_public_key(
    key: *const u8,
    key_len: usize,
) -> *mut LnsIdentity {
    if key.is_null() || key_len != 64 {
        return ptr::null_mut();
    }

    let key_bytes = std::slice::from_raw_parts(key, key_len);

    match reticulum_core::Identity::from_public_key_bytes(key_bytes) {
        Ok(identity) => Box::into_raw(Box::new(LnsIdentity { inner: identity })),
        Err(_) => ptr::null_mut(),
    }
}

/// Get the private key bytes (64 bytes)
///
/// Returns LNS_ERR_CRYPTO if identity has no private keys (public-only).
#[no_mangle]
pub unsafe extern "C" fn lns_identity_private_key(
    identity: *const LnsIdentity,
    out_key: *mut u8,
    out_len: *mut usize,
) -> c_int {
    if identity.is_null() || out_key.is_null() || out_len.is_null() {
        return LNS_ERR_NULL_PTR;
    }

    let identity = &(*identity).inner;

    match identity.private_key_bytes() {
        Ok(key) => {
            if *out_len < key.len() {
                *out_len = key.len();
                return LNS_ERR_BUFFER_TOO_SMALL;
            }
            ptr::copy_nonoverlapping(key.as_ptr(), out_key, key.len());
            *out_len = key.len();
            LNS_OK
        }
        Err(_) => LNS_ERR_CRYPTO,
    }
}

/// Check if identity has private keys
///
/// Returns 1 if identity has private keys, 0 otherwise.
#[no_mangle]
pub unsafe extern "C" fn lns_identity_has_private_keys(identity: *const LnsIdentity) -> c_int {
    if identity.is_null() {
        return 0;
    }
    if (*identity).inner.has_private_keys() {
        1
    } else {
        0
    }
}

/// Encrypt data for an identity
///
/// The ciphertext can only be decrypted by the holder of the identity's private key.
/// Output format: [ephemeral_pub (32)] [token (variable)]
///
/// Returns the ciphertext length, or negative error code.
#[no_mangle]
pub unsafe extern "C" fn lns_identity_encrypt(
    identity: *const LnsIdentity,
    plaintext: *const u8,
    plaintext_len: usize,
    out_ciphertext: *mut u8,
    out_len: *mut usize,
) -> c_int {
    if identity.is_null() || plaintext.is_null() || out_ciphertext.is_null() || out_len.is_null() {
        return LNS_ERR_NULL_PTR;
    }

    let identity = &(*identity).inner;
    let plaintext = std::slice::from_raw_parts(plaintext, plaintext_len);

    let ciphertext = match identity.encrypt(plaintext, &mut rand_core::OsRng) {
        Ok(c) => c,
        Err(_) => return LNS_ERR_CRYPTO,
    };

    if *out_len < ciphertext.len() {
        *out_len = ciphertext.len();
        return LNS_ERR_BUFFER_TOO_SMALL;
    }

    ptr::copy_nonoverlapping(ciphertext.as_ptr(), out_ciphertext, ciphertext.len());
    *out_len = ciphertext.len();
    LNS_OK
}

/// Decrypt data encrypted for this identity
///
/// Requires the identity to have private keys.
///
/// Returns the plaintext length, or negative error code.
#[no_mangle]
pub unsafe extern "C" fn lns_identity_decrypt(
    identity: *const LnsIdentity,
    ciphertext: *const u8,
    ciphertext_len: usize,
    out_plaintext: *mut u8,
    out_len: *mut usize,
) -> c_int {
    if identity.is_null() || ciphertext.is_null() || out_plaintext.is_null() || out_len.is_null() {
        return LNS_ERR_NULL_PTR;
    }

    let identity = &(*identity).inner;
    let ciphertext = std::slice::from_raw_parts(ciphertext, ciphertext_len);

    match identity.decrypt(ciphertext) {
        Ok(plaintext) => {
            if *out_len < plaintext.len() {
                *out_len = plaintext.len();
                return LNS_ERR_BUFFER_TOO_SMALL;
            }
            ptr::copy_nonoverlapping(plaintext.as_ptr(), out_plaintext, plaintext.len());
            *out_len = plaintext.len();
            LNS_OK
        }
        Err(_) => LNS_ERR_CRYPTO,
    }
}

// --- Utility functions ---

/// Free a string allocated by the library
#[no_mangle]
pub unsafe extern "C" fn lns_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

/// Get the error message for an error code
#[no_mangle]
pub extern "C" fn lns_error_string(code: c_int) -> *const c_char {
    let msg: &'static [u8] = match code {
        LNS_OK => b"Success\0",
        LNS_ERR_NULL_PTR => b"Null pointer\0",
        LNS_ERR_INVALID_ARG => b"Invalid argument\0",
        LNS_ERR_INIT_FAILED => b"Initialization failed\0",
        LNS_ERR_NOT_RUNNING => b"Not running\0",
        LNS_ERR_ALREADY_RUNNING => b"Already running\0",
        LNS_ERR_IO => b"I/O error\0",
        LNS_ERR_CRYPTO => b"Cryptographic error\0",
        LNS_ERR_BUFFER_TOO_SMALL => b"Buffer too small\0",
        _ => b"Unknown error\0",
    };
    msg.as_ptr() as *const c_char
}

// TODO(#29): Add more FFI functions for:
// - Reticulum instance management
// - Destination creation and management
// - Link establishment
// - Packet sending/receiving
// - Path requests
// - Resource transfers

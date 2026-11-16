//! What `lnstatus -R` and `lnmgmt` share on the client side of remote
//! management: reading the `-R` transport identity hash and the `-i`
//! management identity, and attaching to the local shared instance for
//! transport. The protocol flow itself is `leviculum_std::remote_status`.

use std::path::Path;

use leviculum_std::driver::{EventReceiver, ReticulumNode, ReticulumNodeBuilder};
use leviculum_std::Identity;

/// Parse a 32-hex-char transport identity hash (the `-R` argument) into 16
/// bytes. The messages are `rnstatus`'s.
pub fn parse_identity_hash(hex: &str) -> Result<[u8; 16], String> {
    if hex.len() != 32 {
        return Err(format!(
            "Destination length is invalid, must be 32 hexadecimal characters (16 bytes), got {}.",
            hex.len()
        ));
    }
    let mut out = [0u8; 16];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let s =
            std::str::from_utf8(chunk).map_err(|_| "Invalid destination entered.".to_string())?;
        out[i] =
            u8::from_str_radix(s, 16).map_err(|_| "Invalid destination entered.".to_string())?;
    }
    Ok(out)
}

/// Load the management identity from its RNS identity file (64 raw private
/// key bytes), matching `RNS.Identity.from_file`.
pub fn load_management_identity(path: &Path) -> Result<Identity, String> {
    std::fs::read(path)
        .map_err(|e| e.to_string())
        .and_then(|b| Identity::from_private_key_bytes(&b).map_err(|e| e.to_string()))
        .map_err(|e| {
            format!(
                "Could not load management identity from {}: {e}",
                path.display()
            )
        })
}

/// Attach to the local shared instance for transport (no authkey needed;
/// this is the shared-instance IPC socket, as `lncp` uses it), start the
/// node and take its event receiver. The error is the cause only; each tool
/// puts its own first line above it.
pub async fn attach_to_shared_instance(
    instance_name: &str,
    storage_path: std::path::PathBuf,
) -> Result<(ReticulumNode, EventReceiver), String> {
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(instance_name)
        // The daemon's storage directory, which the config may place off the
        // config directory (Codeberg #241).
        .storage_path(storage_path)
        .build_sync()
        .map_err(|e| e.to_string())?;
    node.start().await.map_err(|e| e.to_string())?;
    let events = node
        .take_event_receiver()
        .ok_or_else(|| "Internal error: event receiver unavailable".to_string())?;
    Ok((node, events))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_hash_parsing() {
        let h = parse_identity_hash("000102030405060708090a0b0c0d0e0f").unwrap();
        assert_eq!(h, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
        // Wrong length and non-hex are rejected.
        assert!(parse_identity_hash("abcd").is_err());
        assert!(parse_identity_hash(&"z".repeat(32)).is_err());
    }

    #[test]
    fn a_missing_identity_file_names_the_path() {
        let Err(err) = load_management_identity(Path::new("/nonexistent/lnmgmt-id")) else {
            panic!("a missing file must not load");
        };
        assert!(err.contains("/nonexistent/lnmgmt-id"), "{err}");
    }
}

//! The propagation node's operator lists (audit 2026-10-12 D4, D18):
//! `--static-peer`, `--auth-allowed` and `--control-allowed`, each
//! repeatable, and the `--clear-*` flag of each.
//!
//! One [`leviculum_core::envelope::TYPE_PN_LIST`] frame per list named on
//! the command line, each replacing that list whole. The board stores the
//! list as a record of its message store and the role reads it at boot, so a
//! list set here takes effect at the next reset; the boot line
//! `PN_LISTS static_peers=<n> auth_allowed=<n> control_allowed=<n>` on the
//! debug port (if00) proves what came up.
//!
//! What the lists mean on the board, lxmd's keys:
//!
//! * `--static-peer <dest>`: a propagation node's destination hash
//!   (`lxmf.propagation`, the hash `lxmd --status` lists a peer by). Always
//!   peered, never culled, path requested at boot — lxmd's `static_peers`.
//! * `--auth-allowed <identity>`: an identity hash (the form `rnid -i`
//!   prints) that may drain its mailbox over `/get`. A non-empty list refuses
//!   everyone else with `ERROR_NO_ACCESS`; empty is open — lxmd's
//!   `auth_required` with its `allowed` file.
//! * `--control-allowed <identity>`: lxmd's `control_allowed`. Stored and
//!   counted; the board registers no control destination yet, so nothing
//!   consults it until it does.

use leviculum_core::envelope::{
    PnListWire, PN_LIST_AUTH_ALLOWED, PN_LIST_CONTROL_ALLOWED, PN_LIST_MAX_HASHES,
    PN_LIST_STATIC_PEERS,
};

/// One 16-byte hash in the hex form RNS prints, `<...>` tolerated.
pub fn parse_hash(flag: &str, text: &str) -> Result<[u8; 16], String> {
    let trimmed = text.trim();
    let hex = trimmed
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .unwrap_or(trimmed);
    if hex.len() != 32 || !hex.is_ascii() {
        return Err(format!(
            "{flag} {text:?}: a hash is 32 hex digits ({} given)",
            hex.len()
        ));
    }
    let mut hash = [0u8; 16];
    for (i, byte) in hash.iter_mut().enumerate() {
        let pair = &hex[i * 2..i * 2 + 2];
        *byte = u8::from_str_radix(pair, 16)
            .map_err(|_| format!("{flag} {text:?}: {pair:?} is not two hex digits"))?;
    }
    Ok(hash)
}

/// The frame one list's flags ask for: `None` when neither the set nor the
/// clear flag was given, the empty list for the clear. More hashes than the
/// board holds stop the run here, before any board is touched.
pub fn plan(
    flag: &str,
    clear_flag: &str,
    list: u8,
    values: &[String],
    clear: bool,
) -> Result<Option<PnListWire>, String> {
    if clear && !values.is_empty() {
        return Err(format!(
            "{clear_flag} empties the list and {flag} fills it; pick one"
        ));
    }
    if !clear && values.is_empty() {
        return Ok(None);
    }
    if values.len() > PN_LIST_MAX_HASHES {
        return Err(format!(
            "{flag} given {} times: a board holds at most {PN_LIST_MAX_HASHES}",
            values.len()
        ));
    }
    let hashes = values
        .iter()
        .map(|text| parse_hash(flag, text))
        .collect::<Result<Vec<_>, _>>()?;
    PnListWire::new(list, &hashes)
        .map(Some)
        .ok_or_else(|| format!("{flag}: not a list the board holds"))
}

/// The three lists' plans in one go, in the order the frames are sent.
pub fn plans(
    static_peers: (&[String], bool),
    auth_allowed: (&[String], bool),
    control_allowed: (&[String], bool),
) -> Result<Vec<PnListWire>, String> {
    let mut out = Vec::new();
    for (flag, clear_flag, list, (values, clear)) in [
        (
            "--static-peer",
            "--clear-static-peers",
            PN_LIST_STATIC_PEERS,
            static_peers,
        ),
        (
            "--auth-allowed",
            "--clear-auth-allowed",
            PN_LIST_AUTH_ALLOWED,
            auth_allowed,
        ),
        (
            "--control-allowed",
            "--clear-control-allowed",
            PN_LIST_CONTROL_ALLOWED,
            control_allowed,
        ),
    ] {
        if let Some(wire) = plan(flag, clear_flag, list, values, clear)? {
            out.push(wire);
        }
    }
    Ok(out)
}

/// The list's name in the transcript, lxmd's key.
pub fn list_name(list: u8) -> &'static str {
    match list {
        PN_LIST_STATIC_PEERS => "static_peers",
        PN_LIST_AUTH_ALLOWED => "auth_allowed",
        PN_LIST_CONTROL_ALLOWED => "control_allowed",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "7d7e542829b40f32364499b27438dba8";

    #[test]
    fn a_static_peer_becomes_one_frame_of_its_list() {
        let wires = plans((&[HOME.to_string()], false), (&[], false), (&[], false)).unwrap();
        assert_eq!(wires.len(), 1);
        assert_eq!(wires[0].list(), PN_LIST_STATIC_PEERS);
        assert_eq!(
            wires[0].hashes(),
            &[[
                0x7d, 0x7e, 0x54, 0x28, 0x29, 0xb4, 0x0f, 0x32, 0x36, 0x44, 0x99, 0xb2, 0x74, 0x38,
                0xdb, 0xa8
            ]]
        );
    }

    #[test]
    fn a_clear_is_an_empty_list_and_nothing_given_sends_nothing() {
        let wires = plans((&[], false), (&[], true), (&[], false)).unwrap();
        assert_eq!(wires.len(), 1);
        assert_eq!(wires[0].list(), PN_LIST_AUTH_ALLOWED);
        assert!(wires[0].hashes().is_empty());
        assert!(plans((&[], false), (&[], false), (&[], false))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_bad_hash_too_many_or_set_with_clear_stops_the_run() {
        assert!(plans((&["abc".to_string()], false), (&[], false), (&[], false)).is_err());
        let many: Vec<String> = (0..=PN_LIST_MAX_HASHES).map(|_| HOME.to_string()).collect();
        assert!(plans((&[], false), (&many, false), (&[], false)).is_err());
        assert!(plans((&[], false), (&[], false), (&[HOME.to_string()], true)).is_err());
        assert_eq!(
            parse_hash("--auth-allowed", &format!("<{HOME}>")).unwrap()[0],
            0x7d
        );
    }
}

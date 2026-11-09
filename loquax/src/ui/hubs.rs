//! The public rooms' hubs Loquax offers (1577, #232): the `[[hub]]`
//! entries of `loquax/assets/hubs.toml`, compiled in. A hub is there only
//! because it answered when measured (the file's head says how); the
//! `[[unreached]]` entries are the file's record, never offered. Nothing
//! connects to a hub of this list on its own: the settings offer it, and
//! the reader chooses ([`super::rooms`]).
//!
//! The file is read by the few lines this needs, not by a TOML crate: the
//! core is `no_std`, and the file's shape is held by the tests below.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// The file, as shipped.
pub const SHIPPED: &str = include_str!("../../assets/hubs.toml");

/// A hub the settings offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hub {
    /// What the settings show.
    pub name: String,
    /// Its `rrc.hub` destination.
    pub hash: [u8; 16],
    /// The rooms its own welcome named.
    pub rooms: Vec<String>,
}

/// A `[[hub]]` entry while its lines are read: name, hash, rooms.
type Partial = (Option<String>, Option<[u8; 16]>, Vec<String>);

/// The `[[hub]]` entries of `text`, in its order. An entry without a name
/// or a hash of 32 hex digits is left out (the tests hold the shipped
/// file to having none such).
pub fn parse(text: &str) -> Vec<Hub> {
    let mut hubs = Vec::new();
    let mut current: Option<Partial> = None;
    let mut finish = |entry: Option<Partial>| {
        if let Some((Some(name), Some(hash), rooms)) = entry {
            hubs.push(Hub { name, hash, rooms });
        }
    };
    for raw in text.lines() {
        // Only whole-line comments: a room's name starts with `#`.
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with("[[") {
            finish(current.take());
            current = (line == "[[hub]]").then(|| (None, None, Vec::new()));
            continue;
        }
        let (Some(entry), Some((key, value))) = (current.as_mut(), line.split_once('=')) else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "name" => entry.0 = Some(value.trim_matches('"').to_string()),
            "hash" => entry.1 = hash(value.trim_matches('"')),
            "rooms" => {
                entry.2 = value
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .split(',')
                    .map(|room| room.trim().trim_matches('"'))
                    .filter(|room| !room.is_empty())
                    .map(ToString::to_string)
                    .collect();
            }
            _ => {}
        }
    }
    finish(current.take());
    hubs
}

fn hash(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (byte, pair) in out.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
        let digit = |c: u8| char::from(c).to_digit(16);
        *byte = u8::try_from(digit(pair[0])? * 16 + digit(pair[1])?).ok()?;
    }
    Some(out)
}

/// The hubs Loquax offers.
pub fn shipped() -> Vec<Hub> {
    parse(SHIPPED)
}

/// The shipped hub with this hash.
pub fn named(hash: &[u8; 16]) -> Option<Hub> {
    shipped().into_iter().find(|hub| &hub.hash == hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped file: five hubs that answered, each named, the two
    /// that did not left out, no hash twice.
    #[test]
    fn the_shipped_list_is_the_hubs_that_answered() {
        let hubs = shipped();
        let names: Vec<&str> = hubs.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "RNS Community",
                "ShadowLink",
                "HADARC",
                "RNS Moscow",
                "Radio Oxford"
            ]
        );
        let mut hashes: Vec<[u8; 16]> = hubs.iter().map(|h| h.hash).collect();
        hashes.sort_unstable();
        hashes.dedup();
        assert_eq!(hashes.len(), hubs.len(), "no hash twice");
        assert_eq!(hubs[0].rooms, ["general"]);
        let heads = |head: &str| SHIPPED.lines().filter(|l| l.trim() == head).count();
        let unreached = heads("[[unreached]]");
        assert_eq!(unreached, 2, "MichMesh and Amber are recorded");
        assert_eq!(
            heads("[[hub]]"),
            hubs.len(),
            "every [[hub]] entry parses: a name and 32 hex digits"
        );
        assert!(named(&hubs[2].hash).is_some_and(|h| h.name == "HADARC"));
        assert!(named(&[0; 16]).is_none());
    }

    /// The control: a broken hash and an unreached entry are no hubs.
    #[test]
    fn a_broken_entry_is_no_hub() {
        let text = "[[hub]]\nname = \"a\"\nhash = \"28c7\"\n\
                    [[unreached]]\nname = \"b\"\nhash = \"28c7c1a68c735693aa8e6b8193ed44b2\"\n\
                    [[hub]]\nname = \"c\"\nhash = \"28C7C1A68C735693AA8E6B8193ED44B2\"\nrooms = [\"#lab\"]\n";
        let hubs = parse(text);
        assert_eq!(hubs.len(), 1);
        assert_eq!(hubs[0].name, "c");
        assert_eq!(hubs[0].hash[0], 0x28);
        assert_eq!(hubs[0].rooms, ["#lab"], "a room's # is no comment");
    }

    /// A user never reads the protocol's name (1577): no hub the settings
    /// offer is called by it.
    #[test]
    fn no_shipped_name_says_the_protocol() {
        for hub in shipped() {
            assert!(
                !hub.name.to_lowercase().contains("rrc")
                    && !hub.name.to_lowercase().contains("relay chat"),
                "{}",
                hub.name
            );
        }
    }
}

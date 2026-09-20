//! The `lnpnd` configuration file — `lxmd`'s format, `lxmd`'s keys
//! (Codeberg #384 part 4, deliverable 3).
//!
//! The file is the ini dialect `lxmd` reads through `ConfigObj`
//! (`reference/LXMF/LXMF/Utilities/lxmd.py:352`): `[section]` headers,
//! `key = value` pairs, `#` comment lines, comma-separated lists. The
//! sections and key names are the reference's own (`apply_config`,
//! `lxmd.py:71-291`), so one config file drives either daemon. Command
//! line flags override file values, exactly as documented in the manual
//! page, which also records the keys `lnpnd` accepts but does not act on
//! and why.
//!
//! The config directory holds the same files `lxmd`'s does
//! (`program_setup`, `lxmd.py:334-339`): `config`, `identity`,
//! `allowed` (identity hashes for `auth_required`), `ignored`
//! (destination hashes to drop), and `storage/` for the daemon's state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One parsed config file: sections of key → value strings. Unknown keys
/// are kept (and warned about by the caller) rather than refused — the
/// reference ignores keys it does not know, and a shared config file may
/// legitimately carry keys only the other daemon reads.
#[derive(Debug, Default, Clone)]
pub struct RawConfig {
    sections: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug)]
pub enum ConfigError {
    Io(PathBuf, std::io::Error),
    /// A line that is neither a section, a comment, nor `key = value`.
    Malformed {
        line_number: usize,
        line: String,
    },
    /// A value that does not parse as its key's type.
    BadValue {
        section: &'static str,
        key: &'static str,
        value: String,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Malformed { line_number, line } => {
                write!(f, "config line {line_number} is not key = value: {line}")
            }
            Self::BadValue {
                section,
                key,
                value,
            } => write!(f, "config [{section}] {key} = {value}: invalid value"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl RawConfig {
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut current = String::new();
        for (index, raw_line) in text.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[') {
                let Some(name) = name.strip_suffix(']') else {
                    return Err(ConfigError::Malformed {
                        line_number: index + 1,
                        line: line.to_string(),
                    });
                };
                current = name.trim().to_string();
                sections.entry(current.clone()).or_default();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(ConfigError::Malformed {
                    line_number: index + 1,
                    line: line.to_string(),
                });
            };
            sections
                .entry(current.clone())
                .or_default()
                .insert(key.trim().to_string(), value.trim().to_string());
        }
        Ok(Self { sections })
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text =
            std::fs::read_to_string(path).map_err(|e| ConfigError::Io(path.to_path_buf(), e))?;
        Self::parse(&text)
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.sections.get(section)?.get(key).map(String::as_str)
    }

    /// The truthy spellings `ConfigObj.as_bool` accepts
    /// (`reference/Reticulum/RNS/vendor/configobj.py`, `_bools`).
    pub fn get_bool(
        &self,
        section: &'static str,
        key: &'static str,
    ) -> Result<Option<bool>, ConfigError> {
        let Some(value) = self.get(section, key) else {
            return Ok(None);
        };
        match value.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Ok(Some(true)),
            "false" | "no" | "off" | "0" => Ok(Some(false)),
            _ => Err(ConfigError::BadValue {
                section,
                key,
                value: value.to_string(),
            }),
        }
    }

    pub fn get_u64(
        &self,
        section: &'static str,
        key: &'static str,
    ) -> Result<Option<u64>, ConfigError> {
        let Some(value) = self.get(section, key) else {
            return Ok(None);
        };
        // The reference reads sizes as floats (`as_float`); whole-number
        // spellings like `500.0` are accepted, fractions are not — our
        // stores account in bytes and a fractional kilobyte would be a
        // silent truncation.
        let parsed = value
            .parse::<u64>()
            .ok()
            .or_else(|| match value.parse::<f64>() {
                Ok(float) if float >= 0.0 && float.fract() == 0.0 => Some(float as u64),
                _ => None,
            });
        match parsed {
            Some(number) => Ok(Some(number)),
            None => Err(ConfigError::BadValue {
                section,
                key,
                value: value.to_string(),
            }),
        }
    }

    pub fn get_u8(
        &self,
        section: &'static str,
        key: &'static str,
    ) -> Result<Option<u8>, ConfigError> {
        match self.get_u64(section, key)? {
            None => Ok(None),
            Some(number) => u8::try_from(number)
                .map(Some)
                .map_err(|_| ConfigError::BadValue {
                    section,
                    key,
                    value: number.to_string(),
                }),
        }
    }

    /// A comma-separated list of 16-byte hex hashes (`ConfigObj.as_list`
    /// splits on commas; `apply_config` hex-decodes each,
    /// `reference/LXMF/LXMF/Utilities/lxmd.py:224-230`).
    pub fn get_hash_list(
        &self,
        section: &'static str,
        key: &'static str,
    ) -> Result<Option<Vec<[u8; 16]>>, ConfigError> {
        let Some(value) = self.get(section, key) else {
            return Ok(None);
        };
        let mut hashes = Vec::new();
        for raw in value.split(',') {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            match parse_hash(raw) {
                Some(hash) => hashes.push(hash),
                None => {
                    return Err(ConfigError::BadValue {
                        section,
                        key,
                        value: raw.to_string(),
                    })
                }
            }
        }
        Ok(Some(hashes))
    }

    /// Keys present in the file that `lnpnd` knows it does not act on,
    /// for a start-up warning: accepted silently they would look
    /// honoured, refused they would break a config file shared with
    /// `lxmd`.
    pub fn inert_keys(&self) -> Vec<(&'static str, &'static str, &'static str)> {
        const INERT: &[(&str, &str, &str)] = &[
            (
                "propagation",
                "announce_at_start",
                "lnpnd always announces the node shortly after start; see lnpnd(1)",
            ),
            (
                "propagation",
                "prioritise_destinations",
                "eviction is size- and age-driven only; see lnpnd(1)",
            ),
            (
                "propagation",
                "static_peers_bypass_sequential",
                "one validation worker serves all peers in arrival order; see lnpnd(1)",
            ),
            (
                "propagation",
                "sequential_pn_stamp_validation",
                "validation is always sequential in lnpnd; see lnpnd(1)",
            ),
        ];
        INERT
            .iter()
            .filter(|(section, key, _)| self.get(section, key).is_some())
            .map(|entry| (entry.0, entry.1, entry.2))
            .collect()
    }
}

/// Parse one 32-hex-character destination or identity hash.
pub fn parse_hash(raw: &str) -> Option<[u8; 16]> {
    if raw.len() != 32 || !raw.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut hash = [0u8; 16];
    for (index, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&raw[2 * index..2 * index + 2], 16).ok()?;
    }
    Some(hash)
}

/// One hash per line, the `allowed` / `ignored` file format
/// (`apply_config`, `reference/LXMF/LXMF/Utilities/lxmd.py:246-288`:
/// lines of `TRUNCATED_HASHLENGTH//8*2` hex characters, others logged
/// and skipped).
pub fn load_hash_file(path: &Path) -> Vec<[u8; 16]> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| parse_hash(line.trim()))
        .collect()
}

/// The default config directory, `lxmd`'s own search order with `lnpnd`
/// names (`program_setup`, `reference/LXMF/LXMF/Utilities/lxmd.py:326-332`):
/// `/etc/lnpnd` when it holds a config, else `~/.config/lnpnd` when it
/// does, else `~/.lnpnd`.
pub fn default_config_dir() -> PathBuf {
    let etc = PathBuf::from("/etc/lnpnd");
    if etc.join("config").is_file() {
        return etc;
    }
    if let Some(home) = std::env::var_os("HOME") {
        let xdg = PathBuf::from(&home).join(".config/lnpnd");
        if xdg.join("config").is_file() {
            return xdg;
        }
        return PathBuf::from(home).join(".lnpnd");
    }
    PathBuf::from(".lnpnd")
}

/// Our example config: `lxmd --exampleconfig`'s keys
/// (`__default_lxmd_config__`, `reference/LXMF/LXMF/Utilities/lxmd.py:957-1178`)
/// with this daemon's defaults and the differences called out where they
/// are.
pub const EXAMPLE_CONFIG: &str = r#"# This is an example lnpnd config file.
# lnpnd reads lxmd's config format and key names, so a config written
# for lxmd works here. Keys lnpnd accepts but does not act on are noted
# below and in lnpnd(1).

[propagation]

# The propagation node is what lnpnd is; this key exists for lxmd
# config compatibility and only "yes" is accepted.

enable_node = yes

# You can specify identity hashes for remotes
# that are allowed to control and query status
# for this propagation node.

# The node's own identity is always allowed, so `lnpnd --status` on this
# host needs no entry here. This key only adds OTHER people. Their hash
# is what `lnpnd --status` prints for them, or the first 32 hex digits of
# an identity file's hash; it is not a destination hash.

# control_allowed = 7d7e542829b40f32364499b27438dba8, 437229f8e29598b2282b88bad5e44698

# An optional name for this node, included
# in announces.

# node_name = Anonymous Propagation Node

# Automatic announce interval in minutes.
# 6 hours by default.

announce_interval = 360

# Whether to announce when the node starts. lnpnd always announces
# shortly after start (the router's own behaviour); this key is
# accepted for lxmd compatibility.

announce_at_start = yes

# Whether to automatically peer with other
# propagation nodes on the network.

autopeer = yes

# The maximum peering depth (in hops) for
# automatically peered nodes.

autopeer_maxdepth = 4

# The maximum amount of storage to use for
# the propagation node message store,
# specified in megabytes. Old and oversized
# messages are removed first when the store
# fills. Defaults to 500 megabytes.

# message_storage_limit = 500

# The maximum accepted transfer size per in-
# coming propagation message, in kilobytes.

# propagation_message_max_accepted_size = 256

# The maximum accepted transfer size per in-
# coming propagation node sync.

# propagation_sync_max_accepted_size = 10240

# The stamp cost required to deliver messages via this node. lnpnd's
# default is 0 (no proof-of-work required); lxmd never announces
# below 13. Announcing 0 is wire-legal and honoured by reference
# clients.

# propagation_stamp_cost_target = 0

# How far below the target a stamp from another propagation node may
# fall and still be accepted.

# propagation_stamp_cost_flexibility = 3

# The peering key value required of nodes that want to deliver
# messages to this one. lnpnd's default is 0; note that a stock lxmd
# peer can never sync TOWARD a node announcing peering cost 0 (its
# peering_key_ready short-circuits on a falsy cost), so set at least
# 1 if stock nodes should push messages here.

# peering_cost = 0

# The maximum peering cost of remote nodes this node will mine a
# peering key for.

# remote_peering_cost_max = 26

# The maximum number of propagation nodes this node will peer with
# automatically. The default is 20.

# max_peers = 20

# A list of static propagation node peers this node is always peered
# with.

# static_peers = e17f833c4ddf8890dd3a79a6fea8161d, 5a2d0029b6e5ec87020abaea0d746da4

# Only accept incoming propagation messages from configured static
# peers.

# from_static_only = False

# How many concurrent inbound propagation sync transfers are
# accepted before nodes offering messages receive a throttle
# response.

# max_inbound_syncs = 3

# By default, any destination is allowed to connect and download
# messages. If you enable authentication, list the allowed identity
# hashes in a file named "allowed" in the lnpnd config directory,
# one hash per line.

auth_required = no


[lxmf]

# lnpnd creates an LXMF delivery destination it can receive messages
# on. This option sets the announced display name for this
# destination.

display_name = Anonymous Peer

# Whether to announce the delivery destination when lnpnd starts.

announce_at_start = no

# You can also announce the delivery destination at a specified
# interval in minutes. This is not enabled by default.

# announce_interval = 360

# The required stamp cost for incoming messages.

# stamp_cost = 12

# The maximum accepted transfer size for messages received directly
# from other peers, specified in kilobytes.

delivery_transfer_max_accepted_size = 1000

# An external program to run every time a message is received. It
# receives the full path to the message file as its argument.

# on_inbound = rm


[logging]
# Valid log levels are 0 through 7:
#   0: Log only critical information
#   1: Log errors and lower log levels
#   2: Log warnings and lower log levels
#   3: Log notices and lower log levels
#   4: Log info and lower (this is the default)
#   5: Verbose logging
#   6: Debug logging
#   7: Extreme logging

loglevel = 4
"#;

/// Map the reference's numeric log level (`lxmd.py:1166-1176`) to a
/// tracing filter directive.
pub fn loglevel_filter(level: u64) -> &'static str {
    match level {
        0 | 1 => "error",
        2 => "warn",
        3 | 4 => "info",
        5 | 6 => "debug",
        _ => "trace",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference's own example config must parse: the drop-in claim
    /// is that a config written for lxmd drives lnpnd.
    ///
    /// The text is vendored rather than sliced out of
    /// `reference/LXMF/LXMF/Utilities/lxmd.py` at compile time (Codeberg
    /// #300): an `include_str!` into a submodule is a compile-time
    /// dependency, and the forge gate clones with `submodules: false`, so
    /// this whole crate's lib tests failed to build there. It is the
    /// `__default_lxmd_config__` literal verbatim, at submodule pin
    /// 795fdaa2 (LXMF 1.1.0); re-extract it with
    ///
    /// ```text
    /// python3 -c 'import sys,re; t=open(sys.argv[1]).read(); m="__default_lxmd_config__ = \"\"\""; \
    ///   s=t.index(m)+len(m); sys.stdout.write(t[s:s+t[s:].index("\"\"\"")])' \
    ///   reference/LXMF/LXMF/Utilities/lxmd.py > lnpnd/tests_data/lxmd_example_config.conf
    /// ```
    #[test]
    fn the_reference_example_config_parses() {
        let text = include_str!("../tests_data/lxmd_example_config.conf");
        let config = RawConfig::parse(text).expect("parses");
        assert_eq!(config.get("propagation", "enable_node"), Some("no"));
        assert_eq!(config.get("propagation", "announce_interval"), Some("360"));
        assert_eq!(config.get("lxmf", "display_name"), Some("Anonymous Peer"));
        assert_eq!(config.get("logging", "loglevel"), Some("4"));
    }

    #[test]
    fn our_example_config_parses_and_holds_the_defaults() {
        let config = RawConfig::parse(EXAMPLE_CONFIG).expect("parses");
        assert_eq!(
            config.get_bool("propagation", "enable_node").unwrap(),
            Some(true)
        );
        assert_eq!(
            config.get_u64("propagation", "announce_interval").unwrap(),
            Some(360)
        );
        assert_eq!(
            config
                .get_u64("lxmf", "delivery_transfer_max_accepted_size")
                .unwrap(),
            Some(1000)
        );
        assert_eq!(
            config.get_bool("propagation", "auth_required").unwrap(),
            Some(false)
        );
    }

    #[test]
    fn lists_bools_and_numbers_parse_like_configobj() {
        let config = RawConfig::parse(
            "[propagation]\n\
             control_allowed = 7d7e542829b40f32364499b27438dba8, 437229f8e29598b2282b88bad5e44698\n\
             autopeer = Yes\n\
             message_storage_limit = 500.0\n",
        )
        .expect("parses");
        assert_eq!(
            config
                .get_hash_list("propagation", "control_allowed")
                .expect("list")
                .expect("present")
                .len(),
            2
        );
        assert_eq!(
            config.get_bool("propagation", "autopeer").unwrap(),
            Some(true)
        );
        assert_eq!(
            config
                .get_u64("propagation", "message_storage_limit")
                .unwrap(),
            Some(500)
        );
        assert!(config
            .get_u64("propagation", "missing")
            .expect("ok")
            .is_none());
    }

    #[test]
    fn a_fractional_size_is_refused_not_truncated() {
        let config = RawConfig::parse("[propagation]\nmessage_storage_limit = 0.5\n").unwrap();
        assert!(config
            .get_u64("propagation", "message_storage_limit")
            .is_err());
    }
}

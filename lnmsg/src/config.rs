//! `lnmsg`'s own tiny config file.
//!
//! `${LNMSG_HOME}/config` (the same directory as the identity — see
//! [`crate::identity::home_dir`]) holds `key = value` lines. The one key this
//! slice reads is `propagation_node`, the persisted default for `--pn`:
//!
//! ```text
//! # the mailbox this machine uses when a direct delivery fails
//! propagation_node = 76cff79ab2f43bebcb7398e08e56ee0f
//! ```
//!
//! Deliberately not the Reticulum config file: that one belongs to the
//! daemon and is shared with Python's `rnsd`, and a foreign key in it would
//! be a compatibility hazard. And deliberately hand-parsed rather than a
//! TOML dependency: two keys a decade from now do not justify a parser
//! crate in a messenger's supply chain.
//!
//! A missing file means defaults. A present-but-wrong value is an error
//! that names the file and line, never silently ignored: a typo in a
//! mailbox hash would otherwise turn into "my mail goes nowhere" with no
//! trace.

use std::path::{Path, PathBuf};

use crate::address;

/// The config file's name under the lnmsg home directory.
pub const CONFIG_FILE: &str = "config";

/// Why the config could not be used.
#[derive(Debug)]
pub enum ConfigError {
    Io(PathBuf, std::io::Error),
    /// A line that should carry a value does not parse. 1-based line number.
    BadValue {
        path: PathBuf,
        line: usize,
        key: &'static str,
        detail: String,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::BadValue {
                path,
                line,
                key,
                detail,
            } => write!(f, "{}:{line}: {key}: {detail}", path.display()),
        }
    }
}

impl std::error::Error for ConfigError {}

/// What the config file contributes to a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// The persisted default propagation node, `--pn`'s fallback.
    pub propagation_node: Option<[u8; 16]>,
}

/// Load `home/config`. A missing file is an empty config, not an error.
pub fn load(home: &Path) -> Result<Config, ConfigError> {
    let path = home.join(CONFIG_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(error) => return Err(ConfigError::Io(path, error)),
    };
    parse(&text, &path)
}

fn parse(text: &str, path: &Path) -> Result<Config, ConfigError> {
    let mut config = Config::default();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // Unknown keys pass silently: a config written by a newer lnmsg
        // must not stop an older one from sending at all.
        if key.trim() == "propagation_node" {
            let value = value.trim().trim_matches('"');
            match address::parse(value) {
                Ok(hash) => config.propagation_node = Some(hash),
                Err(error) => {
                    return Err(ConfigError::BadValue {
                        path: path.to_owned(),
                        line: index + 1,
                        key: "propagation_node",
                        detail: error.to_string(),
                    });
                }
            }
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(text: &str) -> Result<Config, ConfigError> {
        parse(text, Path::new("/test/config"))
    }

    #[test]
    fn a_missing_file_is_an_empty_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = load(dir.path()).expect("no file is fine");
        assert_eq!(config, Config::default());
    }

    #[test]
    fn the_propagation_node_key_parses_with_comments_and_quotes() {
        let config = parse_str(
            "# the mailbox\npropagation_node = \"76cff79ab2f43bebcb7398e08e56ee0f\"  # note\n",
        )
        .expect("well-formed config");
        assert_eq!(
            config.propagation_node.map(|h| crate::address::to_hex(&h)),
            Some("76cff79ab2f43bebcb7398e08e56ee0f".to_string())
        );
    }

    /// The failure mode this module's header names: a typo in the mailbox
    /// hash must stop the run and say where, not quietly select nothing.
    #[test]
    fn a_malformed_node_hash_is_an_error_naming_the_line() {
        let error = parse_str("\npropagation_node = not-a-hash\n")
            .expect_err("a bad hash must not be ignored");
        let text = error.to_string();
        assert!(
            text.contains(":2:") && text.contains("propagation_node"),
            "the error must name file, line and key: {text}"
        );
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let config = parse_str("future_key = whatever\n").expect("unknown keys pass");
        assert_eq!(config, Config::default());
    }
}

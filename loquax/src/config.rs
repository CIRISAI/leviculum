//! `loquax`'s own tiny config file.
//!
//! `${LNMSG_HOME}/config` (the same directory as the identity — see
//! [`crate::identity::home_dir`]) holds `key = value` lines. Seven keys:
//! `propagation_node`, the persisted default for `--pn`; `send_on_enter`,
//! whether `Enter` in the compose pane sends (`true`, the default) or breaks
//! the line (`false`, and the modifier chord sends); `theme`, `auto`
//! (the default: ask the terminal), `dark` or `light`, which `--theme`
//! overrides; and `sync_interval`, the longest the messenger leaves the
//! mailbox unsynced (`15m` by default, `off` for start and the key only),
//! which `--sync-interval` overrides. Three keys for the map's autoload
//! (1089): `map_autoload`, `on` (the default) or `off`, whether going to a
//! place loads its region by itself; `map_source`, the URL of the PMTiles
//! archive regions are cut from (unset: the newest Protomaps daily build);
//! and `map_cache_mb`, how many megabytes the autoloaded regions may take
//! (500 by default). `map_theme`, `light` (the default), `dark` or `auto`
//! (1099): the map's colour table, `auto` following the theme the screen
//! is drawn in where the frontend has one (the terminal) and light where
//! it has none (the window). `map_fling`, `on` (the default) or `off`
//! (1188): whether the phone's map coasts on after a finger that panned
//! lifts. `read_receipts`, `on` (the default) or `off` (1231): whether
//! this messenger says it takes read receipts and sends them for what the
//! reader saw (`docs/src/concepts/loquax-read-receipts.md`); off, it does
//! neither. And one key a terminal context for the map's
//! drawing (1093), `map_drawing.<context> = <drawing>`, which the terminal
//! writes itself when the reader leaves the map or quits: the way of
//! drawing the map the reader stayed on in that context, a `--graphics`
//! value (`docs/src/man/loquax.1.md`, MAP). The context is
//! `<mosh|tmux|screen|local>:<TERM_PROGRAM, else TERM>`, so a desktop
//! terminal and a mosh session from the phone each keep their own:
//!
//! ```text
//! # the mailbox this machine uses when a direct delivery fails
//! propagation_node = 76cff79ab2f43bebcb7398e08e56ee0f
//! # Enter breaks the line; Alt-Enter (Ctrl-Enter where the terminal can) sends
//! send_on_enter = false
//! # a light terminal that does not answer the background query
//! theme = light
//! # sync the mailbox at least every half hour, not every fifteen minutes
//! sync_interval = 30m
//! # the club's own copy of the map, and no more than 200 MB of it here
//! map_source = https://maps.example.org/planet.pmtiles
//! map_cache_mb = 200
//! # the map in its dark colours whenever the terminal is dark
//! map_theme = auto
//! # written by the terminal: octants in foot, braille over mosh
//! map_drawing.local:foot = octant
//! map_drawing.mosh:xterm-256color = braille-thin
//! ```
//!
//! On a home node (`docs/src/concepts/loquax-home-node.md`) five keys more:
//! `archive_cap_mb`, how many megabytes of messages the archive keeps before
//! the oldest go (1024 by default, `crate::home::DEFAULT_ARCHIVE_CAP_MB`);
//! `home_token_hours`, how long an admission code stays good (24 by
//! default); `outbox_retry_secs`, how often a message sent through the home
//! node is tried direct again besides on every announce of its recipient
//! (3600 by default); `outbox_days`, for how many days (7 by default); and
//! `home_wake`, `on` (the default) or `off`, whether clients that keep a
//! link are woken when mail arrives.
//!
//! A drawing's name is not checked here: the set is the frontend's, and a
//! name an older loquax does not know falls back to its own choice, said
//! once, rather than stop it from starting.
//!
//! Deliberately not the Reticulum config file: that one belongs to the
//! daemon and is shared with Python's `rnsd`, and a foreign key in it would
//! be a compatibility hazard. And deliberately hand-parsed rather than a
//! TOML dependency: three keys a decade from now do not justify a parser
//! crate in a messenger's supply chain.
//!
//! A missing file means defaults. A present-but-wrong value is an error
//! that names the file and line, never silently ignored: a typo in a
//! mailbox hash would otherwise turn into "my mail goes nowhere" with no
//! trace.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use leviculum_ui::theme::ThemeFlag;

use crate::address;

/// The config file's name under the loquax home directory.
pub const CONFIG_FILE: &str = "config";

/// Why the config text could not be used. It names the line; the file it came
/// from is the shell's to add ([`LoadError`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// A line that should carry a value does not parse. 1-based line number.
    BadValue {
        line: usize,
        key: &'static str,
        detail: String,
    },
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadValue { line, key, detail } => write!(f, "line {line}: {key}: {detail}"),
        }
    }
}

impl core::error::Error for ConfigError {}

/// What the config file contributes to a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// The persisted default propagation node, `--pn`'s fallback.
    pub propagation_node: Option<[u8; 16]>,
    /// `send_on_enter`, when the file sets it; unset is on.
    pub send_on_enter: Option<bool>,
    /// `home_wake`, when the file sets it; unset is on.
    pub home_wake: Option<bool>,
    /// `theme`, when the file sets it; unset is `auto`.
    pub theme: Option<ThemeFlag>,
    /// `sync_interval`, when the file sets it: the mailbox schedule's bound
    /// in milliseconds, `None` inside for the timer off
    /// ([`crate::sync::parse_interval`]); unset is fifteen minutes.
    pub sync_interval: Option<Option<u64>>,
    /// `map_autoload`, when the file sets it; unset is on.
    pub map_autoload: Option<bool>,
    /// `map_source`, when the file sets it; unset is the Protomaps daily
    /// build.
    pub map_source: Option<String>,
    /// `map_cache_mb`, when the file sets it.
    pub map_cache_mb: Option<u64>,
    /// `archive_cap_mb`, when the file sets it: the home node's archive cap.
    pub archive_cap_mb: Option<u64>,
    /// `home_token_hours`, when the file sets it: an admission token's life.
    pub home_token_hours: Option<u64>,
    /// `outbox_retry_secs`, when the file sets it: the home node's slow
    /// direct retry cadence.
    pub outbox_retry_secs: Option<u64>,
    /// `outbox_days`, when the file sets it: how long the home node retries.
    pub outbox_days: Option<u64>,
    /// `map_theme`, when the file sets it; unset is light.
    pub map_theme: Option<ThemeFlag>,
    /// `map_fling`, when the file sets it; unset is on.
    pub map_fling: Option<bool>,
    /// `read_receipts`, when the file sets it; unset is on.
    pub read_receipts: Option<bool>,
    /// `map_drawing.<context>` lines: each context and the drawing
    /// remembered for it, in file order, a later line for a context
    /// replacing an earlier one.
    pub map_drawing: Vec<(String, String)>,
}

/// The prefix of a remembered map drawing's key; the context follows it.
pub const MAP_DRAWING: &str = "map_drawing.";

impl Config {
    /// The drawing remembered for `context`.
    pub fn map_drawing(&self, context: &str) -> Option<&str> {
        self.map_drawing
            .iter()
            .find(|(c, _)| c == context)
            .map(|(_, d)| d.as_str())
    }
}

/// `text` with `key` set to `value`: the first line that sets `key`
/// rewritten, any later one dropped, or the line appended; every other line,
/// comments and unknown keys included, kept as it was.
pub fn set(text: &str, key: &str, value: &str) -> String {
    let mut out = String::with_capacity(text.len() + key.len() + value.len() + 4);
    let mut done = false;
    for raw in text.lines() {
        let code = raw.split('#').next().unwrap_or("");
        let sets = code.split_once('=').is_some_and(|(k, _)| k.trim() == key);
        match (sets, done) {
            (true, false) => {
                out.push_str(&alloc::format!("{key} = {value}\n"));
                done = true;
            }
            (true, true) => {}
            (false, _) => {
                out.push_str(raw);
                out.push('\n');
            }
        }
    }
    if !done {
        out.push_str(&alloc::format!("{key} = {value}\n"));
    }
    out
}

/// Read the config file's text.
pub fn parse(text: &str) -> Result<Config, ConfigError> {
    let mut config = Config::default();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // Unknown keys pass silently: a config written by a newer loquax
        // must not stop an older one from sending at all.
        for (name, slot) in [
            ("send_on_enter", &mut config.send_on_enter),
            ("home_wake", &mut config.home_wake),
            ("map_fling", &mut config.map_fling),
            ("read_receipts", &mut config.read_receipts),
        ] {
            if key.trim() == name {
                *slot = Some(match value.trim().trim_matches('"') {
                    "true" | "yes" | "on" => true,
                    "false" | "no" | "off" => false,
                    other => {
                        return Err(ConfigError::BadValue {
                            line: index + 1,
                            key: name,
                            detail: alloc::format!("{other:?} is not true or false"),
                        });
                    }
                });
            }
        }
        for (name, slot) in [
            ("theme", &mut config.theme),
            ("map_theme", &mut config.map_theme),
        ] {
            if key.trim() == name {
                *slot = Some(match value.trim().trim_matches('"') {
                    "auto" => ThemeFlag::Auto,
                    "dark" => ThemeFlag::Dark,
                    "light" => ThemeFlag::Light,
                    other => {
                        return Err(ConfigError::BadValue {
                            line: index + 1,
                            key: name,
                            detail: alloc::format!("{other:?} is not auto, dark or light"),
                        });
                    }
                });
            }
        }
        if key.trim() == "sync_interval" {
            match crate::sync::parse_interval(value.trim().trim_matches('"')) {
                Ok(bound) => config.sync_interval = Some(bound),
                Err(detail) => {
                    return Err(ConfigError::BadValue {
                        line: index + 1,
                        key: "sync_interval",
                        detail,
                    });
                }
            }
        }
        if key.trim() == "map_autoload" {
            config.map_autoload = Some(match value.trim().trim_matches('"') {
                "true" | "yes" | "on" => true,
                "false" | "no" | "off" => false,
                other => {
                    return Err(ConfigError::BadValue {
                        line: index + 1,
                        key: "map_autoload",
                        detail: alloc::format!("{other:?} is not on or off"),
                    });
                }
            });
        }
        if key.trim() == "map_source" {
            let url = value.trim().trim_matches('"');
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(ConfigError::BadValue {
                    line: index + 1,
                    key: "map_source",
                    detail: alloc::format!("{url:?} is not an http or https URL"),
                });
            }
            config.map_source = Some(url.to_string());
        }
        if key.trim() == "map_cache_mb" {
            let text = value.trim().trim_matches('"');
            match text.parse::<u64>() {
                Ok(mb) => config.map_cache_mb = Some(mb),
                Err(_) => {
                    return Err(ConfigError::BadValue {
                        line: index + 1,
                        key: "map_cache_mb",
                        detail: alloc::format!("{text:?} is not a number of megabytes"),
                    });
                }
            }
        }
        if key.trim() == "archive_cap_mb" {
            let text = value.trim().trim_matches('"');
            match text.parse::<u64>() {
                Ok(mb) if mb > 0 => config.archive_cap_mb = Some(mb),
                _ => {
                    return Err(ConfigError::BadValue {
                        line: index + 1,
                        key: "archive_cap_mb",
                        detail: alloc::format!("{text:?} is not a number of megabytes above 0"),
                    });
                }
            }
        }
        for (name, slot, unit) in [
            ("home_token_hours", &mut config.home_token_hours, "hours"),
            (
                "outbox_retry_secs",
                &mut config.outbox_retry_secs,
                "seconds",
            ),
            ("outbox_days", &mut config.outbox_days, "days"),
        ] {
            if key.trim() == name {
                let text = value.trim().trim_matches('"');
                match text.parse::<u64>() {
                    Ok(n) if n > 0 => *slot = Some(n),
                    _ => {
                        return Err(ConfigError::BadValue {
                            line: index + 1,
                            key: name,
                            detail: alloc::format!("{text:?} is not a number of {unit} above 0"),
                        });
                    }
                }
            }
        }
        if let Some(context) = key.trim().strip_prefix(MAP_DRAWING) {
            let drawing = value.trim().trim_matches('"').to_string();
            match config.map_drawing.iter_mut().find(|(c, _)| c == context) {
                Some((_, held)) => *held = drawing,
                None => config.map_drawing.push((context.to_string(), drawing)),
            }
        }
        if key.trim() == "propagation_node" {
            let value = value.trim().trim_matches('"');
            match address::parse(value) {
                Ok(hash) => config.propagation_node = Some(hash),
                Err(error) => {
                    return Err(ConfigError::BadValue {
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

/// Why `home/config` could not be used, with the file named.
#[cfg(feature = "std")]
#[derive(Debug)]
pub enum LoadError {
    Io(std::path::PathBuf, std::io::Error),
    Bad(std::path::PathBuf, ConfigError),
}

#[cfg(feature = "std")]
impl core::fmt::Display for LoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Bad(path, ConfigError::BadValue { line, key, detail }) => {
                write!(f, "{}:{line}: {key}: {detail}", path.display())
            }
        }
    }
}

#[cfg(feature = "std")]
impl core::error::Error for LoadError {}

/// Set `key` to `value` in `home/config` ([`set`]), the file written whole
/// under another name and moved over the old one, so a crash leaves the old
/// file or the new, never half of one. A missing file is created.
#[cfg(feature = "std")]
pub fn save(home: &std::path::Path, key: &str, value: &str) -> std::io::Result<()> {
    let path = home.join(CONFIG_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    leviculum_std::create_secret_dir_all(home)?;
    let next = home.join(alloc::format!(".{CONFIG_FILE}.new"));
    std::fs::write(&next, set(&text, key, value))?;
    std::fs::rename(&next, &path)
}

/// Load `home/config`. A missing file is an empty config, not an error.
#[cfg(feature = "std")]
pub fn load(home: &std::path::Path) -> Result<Config, LoadError> {
    let path = home.join(CONFIG_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(error) => return Err(LoadError::Io(path, error)),
    };
    parse(&text).map_err(|error| LoadError::Bad(path, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(text: &str) -> Result<Config, ConfigError> {
        parse(text)
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
        assert!(
            matches!(
                error,
                ConfigError::BadValue {
                    line: 2,
                    key: "propagation_node",
                    ..
                }
            ),
            "the error must name line and key: {error:?}"
        );
    }

    /// The file half of the same rule: the shell adds the path, in the
    /// `file:line:` shape an editor jumps to.
    #[test]
    fn a_malformed_file_is_an_error_naming_file_line_and_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CONFIG_FILE);
        std::fs::write(&path, "\npropagation_node = not-a-hash\n").expect("write");
        let text = load(dir.path())
            .expect_err("a bad hash must not be ignored")
            .to_string();
        assert!(
            text.starts_with(&format!("{}:2: propagation_node: ", path.display())),
            "the error must name file, line and key: {text}"
        );
    }

    #[test]
    fn send_on_enter_parses_and_a_typo_is_an_error() {
        assert_eq!(parse_str("").expect("empty").send_on_enter, None);
        let off = parse_str("send_on_enter = false\n").expect("off");
        assert_eq!(off.send_on_enter, Some(false));
        let on = parse_str("send_on_enter = yes\n").expect("on");
        assert_eq!(on.send_on_enter, Some(true));
        assert!(matches!(
            parse_str("send_on_enter = maybe\n"),
            Err(ConfigError::BadValue {
                line: 1,
                key: "send_on_enter",
                ..
            })
        ));
    }

    #[test]
    fn read_receipts_parses_and_a_typo_is_an_error() {
        assert_eq!(parse_str("").expect("empty").read_receipts, None);
        let off = parse_str("read_receipts = off\n").expect("off");
        assert_eq!(off.read_receipts, Some(false));
        let on = parse_str("read_receipts = true\n").expect("on");
        assert_eq!(on.read_receipts, Some(true));
        assert!(matches!(
            parse_str("read_receipts = sometimes\n"),
            Err(ConfigError::BadValue {
                line: 1,
                key: "read_receipts",
                ..
            })
        ));
    }

    #[test]
    fn theme_parses_and_a_typo_is_an_error() {
        assert_eq!(parse_str("").expect("empty").theme, None);
        for (text, flag) in [
            ("theme = auto\n", ThemeFlag::Auto),
            ("theme = dark\n", ThemeFlag::Dark),
            ("theme = \"light\"\n", ThemeFlag::Light),
        ] {
            assert_eq!(parse_str(text).expect(text).theme, Some(flag), "{text}");
        }
        assert!(matches!(
            parse_str("\ntheme = solarized\n"),
            Err(ConfigError::BadValue {
                line: 2,
                key: "theme",
                ..
            })
        ));
    }

    #[test]
    fn sync_interval_parses_and_a_typo_is_an_error() {
        assert_eq!(parse_str("").expect("empty").sync_interval, None);
        let half = parse_str("sync_interval = 30m\n").expect("30m");
        assert_eq!(half.sync_interval, Some(Some(30 * 60_000)));
        let off = parse_str("sync_interval = \"off\"\n").expect("off");
        assert_eq!(off.sync_interval, Some(None));
        assert!(matches!(
            parse_str("\nsync_interval = often\n"),
            Err(ConfigError::BadValue {
                line: 2,
                key: "sync_interval",
                ..
            })
        ));
    }

    #[test]
    fn the_archive_cap_parses_and_zero_is_an_error() {
        assert_eq!(parse_str("").expect("empty").archive_cap_mb, None);
        let home = parse_str("home_token_hours = 2\noutbox_retry_secs = 30\noutbox_days = 1\n")
            .expect("home keys");
        assert_eq!(home.home_token_hours, Some(2));
        assert_eq!(home.home_wake, None);
        assert_eq!(
            parse_str("home_wake = off\n").expect("off").home_wake,
            Some(false)
        );
        assert!(parse_str("home_wake = sometimes\n").is_err());
        assert_eq!(home.map_fling, None);
        assert_eq!(
            parse_str("map_fling = off\n").expect("off").map_fling,
            Some(false)
        );
        assert!(parse_str("map_fling = sometimes\n").is_err());
        assert_eq!(home.outbox_retry_secs, Some(30));
        assert_eq!(home.outbox_days, Some(1));
        for (text, key) in [
            ("home_token_hours = 0\n", "home_token_hours"),
            ("outbox_retry_secs = soon\n", "outbox_retry_secs"),
            ("outbox_days = -1\n", "outbox_days"),
        ] {
            assert!(
                matches!(parse_str(text), Err(ConfigError::BadValue { key: k, .. }) if k == key),
                "{text}"
            );
        }
        assert_eq!(
            parse_str("archive_cap_mb = 300\n")
                .expect("set")
                .archive_cap_mb,
            Some(300)
        );
        for text in ["archive_cap_mb = 0\n", "archive_cap_mb = lots\n"] {
            assert!(matches!(
                parse_str(text),
                Err(ConfigError::BadValue {
                    key: "archive_cap_mb",
                    ..
                })
            ));
        }
    }

    #[test]
    fn the_map_keys_parse_and_a_typo_is_an_error() {
        let empty = parse_str("").expect("empty");
        assert_eq!(
            (empty.map_autoload, empty.map_source, empty.map_cache_mb),
            (None, None, None)
        );
        let set = parse_str(
            "map_autoload = off\nmap_source = \"https://maps.example.org/p.pmtiles\"\nmap_cache_mb = 200\n",
        )
        .expect("set");
        assert_eq!(set.map_autoload, Some(false));
        assert_eq!(
            set.map_source.as_deref(),
            Some("https://maps.example.org/p.pmtiles")
        );
        assert_eq!(set.map_cache_mb, Some(200));
        assert_eq!(empty.map_theme, None);
        assert_eq!(
            parse_str("map_theme = auto\n").expect("auto").map_theme,
            Some(ThemeFlag::Auto)
        );
        assert_eq!(
            parse_str("map_theme = \"dark\"\n").expect("dark").map_theme,
            Some(ThemeFlag::Dark)
        );
        for (text, key) in [
            ("map_theme = sepia\n", "map_theme"),
            ("map_autoload = sometimes\n", "map_autoload"),
            ("map_source = ftp://x/p.pmtiles\n", "map_source"),
            ("map_cache_mb = lots\n", "map_cache_mb"),
        ] {
            match parse_str(text) {
                Err(ConfigError::BadValue {
                    line: 1, key: k, ..
                }) => assert_eq!(k, key),
                other => panic!("{text}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_remembered_drawing_is_read_per_context_the_last_line_winning() {
        let config = parse_str(
            "map_drawing.local:foot = octant\n\
             map_drawing.mosh:xterm-256color = braille-thin # the phone\n\
             map_drawing.local:foot = blocks\n\
             map_drawing.local:WezTerm = \"no-such-drawing\"\n",
        )
        .expect("parses");
        assert_eq!(config.map_drawing("local:foot"), Some("blocks"));
        assert_eq!(
            config.map_drawing("mosh:xterm-256color"),
            Some("braille-thin")
        );
        // Not checked here: the frontend knows its drawings.
        assert_eq!(config.map_drawing("local:WezTerm"), Some("no-such-drawing"));
        assert_eq!(config.map_drawing("tmux:tmux"), None);
        assert_eq!(config.map_drawing.len(), 3);
    }

    /// Setting a key rewrites its line in place, drops a second one, and
    /// keeps every other line, comment and unknown key as it was; a key not
    /// there is appended.
    #[test]
    fn setting_a_key_keeps_the_rest_of_the_file() {
        let text = "# mine\ntheme = light\nmap_drawing.local:foot = octant # chosen\n\
                    future_key = 1\nmap_drawing.local:foot = blocks\n";
        let set_once = set(text, "map_drawing.local:foot", "braille");
        assert_eq!(
            set_once,
            "# mine\ntheme = light\nmap_drawing.local:foot = braille\nfuture_key = 1\n"
        );
        let appended = set(&set_once, "map_drawing.mosh:foot", "braille-thin");
        assert!(appended.ends_with("future_key = 1\nmap_drawing.mosh:foot = braille-thin\n"));
        let config = parse_str(&appended).expect("parses");
        assert_eq!(config.theme, Some(ThemeFlag::Light));
        assert_eq!(config.map_drawing("local:foot"), Some("braille"));
        assert_eq!(config.map_drawing("mosh:foot"), Some("braille-thin"));
        assert_eq!(set("", "k", "v"), "k = v\n");
        // A comment that names the key sets nothing.
        assert_eq!(set("# k = old\n", "k", "v"), "# k = old\nk = v\n");
    }

    #[test]
    fn saving_a_key_writes_the_file_and_load_reads_it_back() {
        let dir = tempfile::tempdir().expect("tmp");
        save(dir.path(), "map_drawing.local:foot", "octant").expect("save");
        save(dir.path(), "map_drawing.local:foot", "blocks").expect("save again");
        let config = load(dir.path()).expect("load");
        assert_eq!(config.map_drawing("local:foot"), Some("blocks"));
        let text = std::fs::read_to_string(dir.path().join(CONFIG_FILE)).expect("read");
        assert_eq!(text, "map_drawing.local:foot = blocks\n");
        assert!(!dir.path().join(".config.new").exists());
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let config = parse_str("future_key = whatever\n").expect("unknown keys pass");
        assert_eq!(config, Config::default());
    }
}

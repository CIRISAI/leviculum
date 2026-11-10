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
//! neither and shows none received. The settings' switch and the
//! palette's `read receipts` write it (1233). `photo_size`, `small`,
//! `phone` (the default) or `original` (1220): how far a photo is scaled
//! before it goes (`crate::photo`), which the settings' Chats page
//! writes. `browse_tab`, `on` (the default since 1618) or `off` (1337,
//! 1369): whether the phone, the window and the terminal open the pages
//! Explore lists; off is the opt-out.
//! `explore_az`, `off` (the default) or `on` (1578): whether the Explore
//! tab lists the pages A-Z instead of last seen first; its toggle writes it.
//! `rooms_hub`, a hub's `rrc.hub` destination hash (1379): the Reticulum
//! Relay Chat hub the Rooms tab links to; unset, there is no Rooms tab.
//! The settings' Advanced section writes it (1577), from the shipped hubs
//! (`loquax/assets/hubs.toml`) or an address pasted. `rooms_told`, the
//! hub the reader was last told that their name is visible in its rooms,
//! before the first link to it (1577). `rooms_suggested`, the hub
//! "suggested" marked last, held across a restart (1632).
//! And one key a terminal context for the map's
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
//! Three keys say how a phone and its mailbox reach each other
//! (`docs/src/concepts/loquax-reachability.md`, 1196), all for power users:
//! the defaults decide. `rendezvous`, the ordered list of public nodes both
//! ends dial, one at a time with failover, `host:port` each, separated by
//! commas and optionally in brackets (`rendezvous = ["a.example:4343",
//! "b.example:4343"]`; unset is `leviculum.network:4343` with `:4242`
//! behind it, `crate::home::rendezvous::DEFAULT`); the port is the phones'
//! access point port, and a mailbox dials the ordinary one beside it.
//! `rendezvous_dial`, `on` (the default) or `off`: off is "mesh only", no
//! rendezvous dialled at all. And `home_address`, a fixed `host:port` the
//! phone dials instead of any rendezvous (a mailbox with a public address
//! or a router pinhole, "Advanced"); on a mailbox, the address its codes
//! carry, which `loquax mailbox setup` writes with `rendezvous_dial = off`
//! (`crate::home::setup`, 1266).
//!
//! Two keys say how a mailbox on a computer reaches the network (1354),
//! for power users too: `mailbox_upstream`, `shared` or `mesh`. Unset, the
//! mailbox runs beside the Reticulum daemon when one answers on its shared
//! instance and on a node of its own when none does, as the phone's mailbox
//! always does (`crate::home::upstream`). `shared` waits for the daemon;
//! `mesh` runs on its own node even beside one. And `mailbox_servers`, the
//! entry servers that node dials, one at a time with failover, written as
//! `rendezvous` is (unset: the rendezvous list, `rendezvous` or the shipped
//! one).
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
    /// `rooms_hub` (1379): the `rrc.hub` destination of the Reticulum
    /// Relay Chat hub the Rooms tab links to; unset, there is no tab.
    pub rooms_hub: Option<[u8; 16]>,
    /// `rooms_told` (1577): the hub the reader was told, before the first
    /// link to it, that their name is visible in its rooms.
    pub rooms_told: Option<[u8; 16]>,
    /// `rooms_suggested` (1632): the hub "suggested" held, so a restart
    /// marks the same one while it still fits.
    pub rooms_suggested: Option<[u8; 16]>,
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
    /// `browse_tab`, when the file sets it; unset is on (1618; off before):
    /// whether Explore opens the pages it lists, on the phone, the window
    /// and the terminal.
    pub browse_tab: Option<bool>,
    /// `explore_az`, when the file sets it; unset is off (1578): the
    /// Explore tab's pages by last seen, the newest first.
    pub explore_az: Option<bool>,
    /// `photo_size`, when the file sets it; unset is `phone`.
    pub photo_size: Option<crate::ui::photos::PhotoSize>,
    /// `map_drawing.<context>` lines: each context and the drawing
    /// remembered for it, in file order, a later line for a context
    /// replacing an earlier one.
    pub map_drawing: Vec<(String, String)>,
    /// The `tcp_*` keys, when the file sets them: the TCP keepalive of
    /// the device's connection to its mailbox (1176).
    pub tcp: TcpLiveness,
    /// `rendezvous`, when the file sets it: the ordered list of public
    /// nodes, `host:port` each (1196).
    pub rendezvous: Option<Vec<String>>,
    /// `rendezvous_dial`, when the file sets it; unset is on, off is
    /// "mesh only".
    pub rendezvous_dial: Option<bool>,
    /// `home_address`, when the file sets it: the fixed `host:port` a phone
    /// dials instead of a rendezvous.
    pub home_address: Option<String>,
    /// `mailbox_upstream`, when the file sets it; unset decides by whether
    /// a daemon answers (`crate::home::upstream`, 1354).
    pub mailbox_upstream: Option<MailboxUpstream>,
    /// `mailbox_servers`, when the file sets it: the entry servers a
    /// mailbox on a node of its own dials, `host:port` each.
    pub mailbox_servers: Option<Vec<String>>,
}

/// The `mailbox_upstream` key's two values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxUpstream {
    /// Beside the device's Reticulum daemon, on its shared instance.
    Shared,
    /// On a node of its own that dials the entry servers.
    Mesh,
}

/// A TCP connection's keepalive and user timeout, as the file sets them
/// (1176): the same four keys, with the same names and meaning, as an
/// lnsd TCP interface section (`docs/src/guide/configuration.md`, "TCP
/// server"). Unset is the reference's value; the range is checked when the
/// node is built, which refuses a value the kernel would not take.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TcpLiveness {
    /// `tcp_keepalive_idle_secs`: idle seconds before the first probe.
    pub idle_secs: Option<u64>,
    /// `tcp_keepalive_interval_secs`: seconds between unanswered probes.
    pub interval_secs: Option<u64>,
    /// `tcp_keepalive_probes`: unanswered probes before the connection
    /// is dropped.
    pub probes: Option<u32>,
    /// `tcp_user_timeout_secs`: seconds sent data may stay unacknowledged;
    /// `0` is the kernel default.
    pub user_timeout_secs: Option<u64>,
}

impl TcpLiveness {
    /// These keys on a device's connection to its mailbox (1346): the idle
    /// [`crate::home::keepalive::TCP_IDLE_SECS`] unless the file sets one,
    /// the rest as the file says or the reference's.
    pub fn for_device(self) -> Self {
        Self {
            idle_secs: self
                .idle_secs
                .or(Some(crate::home::keepalive::TCP_IDLE_SECS)),
            ..self
        }
    }
}

/// The prefix of a remembered map drawing's key; the context follows it.
pub const MAP_DRAWING: &str = "map_drawing.";

impl Config {
    /// Whether Explore opens the pages it lists (1618): on unless the file
    /// says `browse_tab = off`.
    pub fn browse_on(&self) -> bool {
        self.browse_tab.unwrap_or(true)
    }

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

/// `text` without any line that sets `key`; every other line kept as it was.
pub fn unset(text: &str, key: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for raw in text.lines() {
        let code = raw.split('#').next().unwrap_or("");
        if code.split_once('=').is_some_and(|(k, _)| k.trim() == key) {
            continue;
        }
        out.push_str(raw);
        out.push('\n');
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
            ("browse_tab", &mut config.browse_tab),
            ("explore_az", &mut config.explore_az),
            ("rendezvous_dial", &mut config.rendezvous_dial),
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
        if key.trim() == "photo_size" {
            let word = value.trim().trim_matches('"');
            config.photo_size = Some(crate::ui::photos::PhotoSize::from_word(word).ok_or_else(
                || ConfigError::BadValue {
                    line: index + 1,
                    key: "photo_size",
                    detail: alloc::format!("{word:?} is not small, phone or original"),
                },
            )?);
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
        for name in [
            "tcp_keepalive_idle_secs",
            "tcp_keepalive_interval_secs",
            "tcp_keepalive_probes",
            "tcp_user_timeout_secs",
        ] {
            if key.trim() != name {
                continue;
            }
            let text = value.trim().trim_matches('"');
            let Ok(n) = text.parse::<u64>() else {
                return Err(ConfigError::BadValue {
                    line: index + 1,
                    key: name,
                    detail: alloc::format!("{text:?} is not a whole number"),
                });
            };
            match name {
                "tcp_keepalive_idle_secs" => config.tcp.idle_secs = Some(n),
                "tcp_keepalive_interval_secs" => config.tcp.interval_secs = Some(n),
                "tcp_keepalive_probes" => {
                    config.tcp.probes = Some(u32::try_from(n).unwrap_or(u32::MAX))
                }
                _ => config.tcp.user_timeout_secs = Some(n),
            }
        }
        if let Some(context) = key.trim().strip_prefix(MAP_DRAWING) {
            let drawing = value.trim().trim_matches('"').to_string();
            match config.map_drawing.iter_mut().find(|(c, _)| c == context) {
                Some((_, held)) => *held = drawing,
                None => config.map_drawing.push((context.to_string(), drawing)),
            }
        }
        for (name, slot) in [
            ("rendezvous", &mut config.rendezvous),
            ("mailbox_servers", &mut config.mailbox_servers),
        ] {
            if key.trim() == name {
                *slot = Some(host_port_list(value, index + 1, name)?);
            }
        }
        if key.trim() == "mailbox_upstream" {
            config.mailbox_upstream = Some(match value.trim().trim_matches('"') {
                "shared" => MailboxUpstream::Shared,
                "mesh" => MailboxUpstream::Mesh,
                other => {
                    return Err(ConfigError::BadValue {
                        line: index + 1,
                        key: "mailbox_upstream",
                        detail: alloc::format!("{other:?} is not shared or mesh"),
                    });
                }
            });
        }
        if key.trim() == "home_address" {
            let entry = value.trim().trim_matches('"').trim();
            if !is_host_port(entry) {
                return Err(ConfigError::BadValue {
                    line: index + 1,
                    key: "home_address",
                    detail: alloc::format!("{entry:?} is not host:port"),
                });
            }
            config.home_address = Some(entry.to_string());
        }
        for (name, slot) in [
            ("propagation_node", &mut config.propagation_node),
            ("rooms_hub", &mut config.rooms_hub),
            ("rooms_told", &mut config.rooms_told),
            ("rooms_suggested", &mut config.rooms_suggested),
        ] {
            if key.trim() != name {
                continue;
            }
            let value = value.trim().trim_matches('"');
            match address::parse(value) {
                Ok(hash) => *slot = Some(hash),
                Err(error) => {
                    return Err(ConfigError::BadValue {
                        line: index + 1,
                        key: name,
                        detail: error.to_string(),
                    });
                }
            }
        }
    }
    Ok(config)
}

/// A list of `host:port` entries, separated by commas and optionally in
/// brackets, each optionally quoted; an empty one is an empty list.
fn host_port_list(value: &str, line: usize, key: &'static str) -> Result<Vec<String>, ConfigError> {
    let inner = value.trim();
    let inner = inner
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .unwrap_or(inner);
    let mut list = Vec::new();
    for entry in inner.split(',') {
        let entry = entry.trim().trim_matches('"').trim();
        if entry.is_empty() {
            continue;
        }
        if !is_host_port(entry) {
            return Err(ConfigError::BadValue {
                line,
                key,
                detail: alloc::format!("{entry:?} is not host:port"),
            });
        }
        list.push(entry.to_string());
    }
    Ok(list)
}

/// Whether `text` reads as `host:port`, a port from 1 to 65535 behind the
/// last colon and a host before it; the dialler says what else is wrong.
fn is_host_port(text: &str) -> bool {
    text.rsplit_once(':').is_some_and(|(host, port)| {
        !host.is_empty() && port.parse::<u16>().is_ok_and(|port| port > 0)
    })
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
    let text = read_text(home)?;
    save_text(home, &set(&text, key, value))
}

/// Write `key = <hash in hex>` into `home/config`, or take `key` out with
/// `None` (1577, `UiEffect::RememberHash`); the other lines kept.
#[cfg(feature = "std")]
pub fn save_hash(home: &std::path::Path, key: &str, hash: Option<[u8; 16]>) -> std::io::Result<()> {
    let text = read_text(home)?;
    let next = match hash {
        Some(hash) => {
            let hex: String = hash.iter().map(|b| alloc::format!("{b:02x}")).collect();
            set(&text, key, &hex)
        }
        None => unset(&text, key),
    };
    save_text(home, &next)
}

/// `home/config`'s text, empty when there is no file.
#[cfg(feature = "std")]
pub fn read_text(home: &std::path::Path) -> std::io::Result<String> {
    match std::fs::read_to_string(home.join(CONFIG_FILE)) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error),
    }
}

/// Write `text` as `home/config`, whole, as [`save`] does.
#[cfg(feature = "std")]
pub fn save_text(home: &std::path::Path, text: &str) -> std::io::Result<()> {
    leviculum_std::create_secret_dir_all(home)?;
    let next = home.join(alloc::format!(".{CONFIG_FILE}.new"));
    std::fs::write(&next, text)?;
    std::fs::rename(&next, home.join(CONFIG_FILE))
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
    fn photo_size_parses_and_a_typo_is_an_error() {
        use crate::ui::photos::PhotoSize;
        assert_eq!(parse_str("").expect("empty").photo_size, None);
        let small = parse_str("photo_size = small\n").expect("small");
        assert_eq!(small.photo_size, Some(PhotoSize::Small));
        let original = parse_str("photo_size = \"original\"\n").expect("original");
        assert_eq!(original.photo_size, Some(PhotoSize::Original));
        assert!(matches!(
            parse_str("photo_size = huge\n"),
            Err(ConfigError::BadValue {
                line: 1,
                key: "photo_size",
                ..
            })
        ));
    }

    /// 1577: the settings' hub written and taken out again, the other
    /// lines kept, and read back as the next start reads it.
    #[test]
    fn a_hub_is_saved_and_taken_out() {
        let dir = tempfile::tempdir().expect("tempdir");
        save(dir.path(), "read_receipts", "off").expect("a line before");
        let hub = [0x28u8; 16];
        save_hash(dir.path(), "rooms_hub", Some(hub)).expect("save");
        save_hash(dir.path(), "rooms_told", Some(hub)).expect("save");
        let config = load(dir.path()).expect("load");
        assert_eq!(config.rooms_hub, Some(hub));
        assert_eq!(config.rooms_told, Some(hub));
        save_hash(dir.path(), "rooms_hub", None).expect("unset");
        let config = load(dir.path()).expect("load");
        assert_eq!(config.rooms_hub, None);
        assert_eq!(config.rooms_told, Some(hub), "the other hash kept");
        assert_eq!(config.read_receipts, Some(false), "the other line kept");
    }

    #[test]
    fn rooms_hub_is_a_hash_and_unset_is_none() {
        assert_eq!(parse_str("").expect("empty").rooms_hub, None);
        assert_eq!(
            parse_str("rooms_hub = 076b76c29219f0cac81fb511039a6ad5\n")
                .expect("a hub")
                .rooms_hub,
            Some([
                0x07, 0x6b, 0x76, 0xc2, 0x92, 0x19, 0xf0, 0xca, 0xc8, 0x1f, 0xb5, 0x11, 0x03, 0x9a,
                0x6a, 0xd5
            ])
        );
        assert!(parse_str("rooms_hub = lab\n").is_err(), "the control");
    }

    /// Explore opens pages unless the file says off (1618): the walk of
    /// 17024c3a found every page listed and none openable.
    #[test]
    fn browse_tab_is_on_unless_the_file_says_off() {
        assert_eq!(parse_str("").expect("empty").browse_tab, None);
        assert!(parse_str("").expect("empty").browse_on());
        assert_eq!(
            parse_str("browse_tab = on\n").expect("on").browse_tab,
            Some(true)
        );
        assert!(!parse_str("browse_tab = off\n").expect("off").browse_on());
        assert!(parse_str("browse_tab = maybe\n").is_err(), "the control");
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

    /// Unsetting drops every line that sets the key and nothing else; a
    /// comment that names it stays.
    #[test]
    fn unsetting_a_key_keeps_the_rest_of_the_file() {
        let text = "# home_address = a:1\nhome_address = b:2\ntheme = light\nhome_address = c:3\n";
        assert_eq!(
            unset(text, "home_address"),
            "# home_address = a:1\ntheme = light\n"
        );
        assert_eq!(unset("", "k"), "");
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

    /// The mailbox's two keys (1354): `shared` or `mesh`, and a list of
    /// entry servers written as `rendezvous` is.
    #[test]
    fn the_mailbox_upstream_keys_parse_and_a_bad_value_is_an_error() {
        let config = parse_str(
            "mailbox_upstream = mesh\nmailbox_servers = [\"a.example:4242\", b.example:4965]\n",
        )
        .expect("parses");
        assert_eq!(config.mailbox_upstream, Some(MailboxUpstream::Mesh));
        assert_eq!(
            config.mailbox_servers,
            Some(vec![
                "a.example:4242".to_string(),
                "b.example:4965".to_string()
            ])
        );
        assert_eq!(
            parse_str("mailbox_upstream = \"shared\"\n")
                .expect("shared")
                .mailbox_upstream,
            Some(MailboxUpstream::Shared)
        );
        let none = parse_str("").expect("empty");
        assert_eq!((none.mailbox_upstream, none.mailbox_servers), (None, None));
        assert_eq!(
            parse_str("x = 1\nmailbox_upstream = lnsd\n"),
            Err(ConfigError::BadValue {
                line: 2,
                key: "mailbox_upstream",
                detail: "\"lnsd\" is not shared or mesh".to_string(),
            })
        );
        assert!(parse_str("mailbox_servers = a.example\n").is_err());
    }

    #[test]
    fn the_rendezvous_keys_parse_and_a_bad_entry_is_an_error() {
        let config = parse_str(
            "rendezvous = [\"a.example:4343\", b.example:4343 , [fd00::1]:4343]\n\
             rendezvous_dial = off\nhome_address = 10.0.0.2:4242\n",
        )
        .expect("parses");
        assert_eq!(
            config.rendezvous,
            Some(vec![
                "a.example:4343".to_string(),
                "b.example:4343".to_string(),
                "[fd00::1]:4343".to_string()
            ])
        );
        assert_eq!(config.rendezvous_dial, Some(false));
        assert_eq!(config.home_address.as_deref(), Some("10.0.0.2:4242"));
        let none = parse_str("").expect("empty");
        assert_eq!(
            (none.rendezvous, none.rendezvous_dial, none.home_address),
            (None, None, None)
        );
        assert!(parse_str("rendezvous = a.example\n").is_err());
        assert!(parse_str("rendezvous = a.example:0\n").is_err());
        assert!(parse_str("home_address = :4242\n").is_err());
    }

    /// The four TCP keys (1176) parse under the names an lnsd section
    /// uses, and a word where a number belongs is an error naming the key.
    #[test]
    fn the_tcp_liveness_keys_parse_and_a_word_is_an_error() {
        let config = parse_str(
            "tcp_keepalive_idle_secs = 600\ntcp_keepalive_interval_secs = 30\n\
             tcp_keepalive_probes = 4\ntcp_user_timeout_secs = 0\n",
        )
        .unwrap();
        assert_eq!(
            config.tcp,
            TcpLiveness {
                idle_secs: Some(600),
                interval_secs: Some(30),
                probes: Some(4),
                user_timeout_secs: Some(0),
            }
        );
        assert_eq!(parse_str("").unwrap().tcp, TcpLiveness::default());
        // 1346: a device's connection takes the long idle unless the file
        // names one, and keeps every key the file names.
        assert_eq!(
            TcpLiveness::default().for_device(),
            TcpLiveness {
                idle_secs: Some(crate::home::keepalive::TCP_IDLE_SECS),
                ..TcpLiveness::default()
            }
        );
        assert_eq!(
            parse_str("tcp_keepalive_idle_secs = 600\n")
                .unwrap()
                .tcp
                .for_device()
                .idle_secs,
            Some(600)
        );
        assert!(matches!(
            parse_str("tcp_keepalive_idle_secs = long\n"),
            Err(ConfigError::BadValue {
                key: "tcp_keepalive_idle_secs",
                ..
            })
        ));
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let config = parse_str("future_key = whatever\n").expect("unknown keys pass");
        assert_eq!(config, Config::default());
    }
}

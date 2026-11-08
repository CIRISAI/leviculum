//! The automatic layer's server list (#471 step 3,
//! `docs/src/concepts/zero-config.md`).
//!
//! With `[auto] servers = yes` lnsd dials TCP servers nobody configured:
//! up to [`BOOTSTRAP_ENTRIES`] entries of a list, each a `bootstrap_only`
//! TCP client, so it is detached once enough discovered interfaces stand
//! ([`DISCOVERED_CEILING`], which the layer raises
//! `autoconnect_discovered_interfaces` to). The list is the shipped one
//! ([`SHIPPED_SERVERS`]), or the `[auto] server_list` key, or
//! [`ENV_SERVERS`] in the environment, the last two pinned: taken in their
//! order instead of drawn.
//!
//! Each dialled entry holds a slot of one shared [`ServerPool`]. The TCP
//! client of a slot asks the pool for the next entry after
//! [`FAILOVER_AFTER_FAILURES`] failed connects in a row, keeping its
//! backoff, so a dead entry is left and a live one taken without a user
//! stuck on the first. The pool persists the slots' entries in the storage
//! directory ([`CHOICE_FILE`]) and logs every choice as one `AUTO_SERVER`
//! line. The core never learns any of it: the slots are TCP clients, and
//! fail-over is the TCP client's own business.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::auto_layer::AutoLayer;
use crate::config::{Config, InterfaceConfig, InterfaceOrigin};
use crate::sync_ext::MutexRecover;

/// The shipped list. The first entry is Lew's own server, which every
/// fresh install dials; an entry joins only with its operator's consent
/// (the page's decision record).
pub const SHIPPED_SERVERS: &[&str] = &["leviculum.network:4242"];

/// How many entries a start dials at most, each a `bootstrap_only` TCP
/// client. Open question 3's starting value; a measurement moves it, not a
/// guess.
pub const BOOTSTRAP_ENTRIES: usize = 2;

/// The `autoconnect_discovered_interfaces` ceiling the layer raises a
/// configuration's `0` to: once this many discovered interfaces stand, the
/// bootstrap entries are detached. Open question 3's starting value (two
/// to three); a measurement moves it, not a guess.
pub const DISCOVERED_CEILING: usize = 2;

/// Failed connects in a row on one entry before its slot moves to the next.
/// The TCP client's backoff keeps running across the move, so a list of
/// dead entries is not dialled faster than one dead entry.
pub const FAILOVER_AFTER_FAILURES: u64 = 3;

/// The environment knob that pins the list: `host:port` entries separated
/// by commas, dialled in their order. Set but empty is a list of none. A
/// cell points it at a periculum node; an operator can pin with it too.
pub const ENV_SERVERS: &str = "LEVICULUM_AUTO_SERVERS";

/// The file in the storage directory that keeps the slots' entries, one
/// `host:port` per line in slot order.
pub const CHOICE_FILE: &str = "auto_servers";

/// The section name of slot `n` (1-based).
pub fn section_name(slot: usize) -> String {
    format!("Auto Server {}", slot + 1)
}

/// One list entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub host: String,
    pub port: u16,
}

impl Entry {
    /// `host:port`, `[v6]:port`. `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (host, port) = if let Some(rest) = text.strip_prefix('[') {
            let (host, rest) = rest.split_once(']')?;
            (host, rest.strip_prefix(':')?)
        } else {
            let (host, port) = text.rsplit_once(':')?;
            if host.contains(':') {
                return None;
            }
            (host, port)
        };
        if host.is_empty() {
            return None;
        }
        Some(Self {
            host: host.to_string(),
            port: port.parse().ok()?,
        })
    }
}

impl std::fmt::Display for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

/// Parse a comma-separated list; an entry that is no `host:port` refuses it.
pub fn parse_list(text: &str) -> Result<Vec<Entry>, String> {
    text.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| Entry::parse(t).ok_or_else(|| format!("'{t}' is not host:port")))
        .collect()
}

/// Where the list came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListSource {
    /// [`SHIPPED_SERVERS`]: entries are drawn at random.
    Shipped,
    /// `[auto] server_list`: pinned, in order.
    Config,
    /// [`ENV_SERVERS`]: pinned, in order.
    Env,
}

/// The list a start dials from: the environment over the file's
/// `server_list` over the shipped one.
pub fn resolve_list(
    env: Option<&str>,
    configured: Option<&[String]>,
) -> crate::error::Result<(Vec<Entry>, ListSource)> {
    let bad = |what: &str, e: String| crate::error::Error::Config(format!("{what}: {e}"));
    if let Some(text) = env {
        return Ok((
            parse_list(text).map_err(|e| bad(ENV_SERVERS, e))?,
            ListSource::Env,
        ));
    }
    if let Some(list) = configured {
        return Ok((
            parse_list(&list.join(",")).map_err(|e| bad("[auto] server_list", e))?,
            ListSource::Config,
        ));
    }
    let shipped = SHIPPED_SERVERS
        .iter()
        .filter_map(|s| Entry::parse(s))
        .collect();
    Ok((shipped, ListSource::Shipped))
}

/// Why a slot holds its entry: the `reason=` of the `AUTO_SERVER` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceReason {
    /// The storage directory's choice from an earlier start.
    Persisted,
    /// Drawn from the shipped list.
    Drawn,
    /// Taken in order from a pinned list.
    Pinned,
    /// The slot's previous entry failed to connect.
    Failover,
}

impl ChoiceReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Persisted => "persisted",
            Self::Drawn => "drawn",
            Self::Pinned => "pinned",
            Self::Failover => "failover",
        }
    }
}

/// The first choice of a start: which entry indices the slots take.
///
/// A persisted choice is kept when every entry it names is still on the
/// list and it fills as many slots as the list allows; otherwise a pinned
/// list gives its first entries and the shipped list a random draw
/// (`draw` returns a value below its argument).
pub fn choose(
    list: &[Entry],
    source: ListSource,
    persisted: &[Entry],
    mut draw: impl FnMut(usize) -> usize,
) -> (Vec<usize>, ChoiceReason) {
    let slots = list.len().min(BOOTSTRAP_ENTRIES);
    let kept: Option<Vec<usize>> = persisted
        .iter()
        .map(|p| list.iter().position(|e| e == p))
        .collect();
    if let Some(kept) = kept {
        let mut distinct = kept.clone();
        distinct.sort_unstable();
        distinct.dedup();
        if slots > 0 && kept.len() == slots && distinct.len() == slots {
            return (kept, ChoiceReason::Persisted);
        }
    }
    match source {
        ListSource::Shipped => {
            let mut order: Vec<usize> = (0..list.len()).collect();
            // Fisher-Yates, the first `slots` positions are enough.
            for i in 0..slots {
                let j = i + draw(list.len() - i);
                order.swap(i, j);
            }
            order.truncate(slots);
            (order, ChoiceReason::Drawn)
        }
        ListSource::Config | ListSource::Env => ((0..slots).collect(), ChoiceReason::Pinned),
    }
}

/// The slots' entries, shared by their TCP clients.
#[derive(Debug)]
pub struct ServerPool {
    list: Vec<Entry>,
    held: Vec<usize>,
    persist: Option<PathBuf>,
}

impl ServerPool {
    /// A pool over `list` whose slots hold `held` (indices into `list`).
    pub fn new(list: Vec<Entry>, held: Vec<usize>, persist: Option<PathBuf>) -> Self {
        Self {
            list,
            held,
            persist,
        }
    }

    /// The entry slot `slot` dials now.
    pub fn current(&self, slot: usize) -> Option<Entry> {
        self.held.get(slot).and_then(|&i| self.list.get(i)).cloned()
    }

    /// Slot `slot`'s entry failed: move it to the next entry on the list no
    /// other slot holds, persist and log the move. `None` when there is no
    /// such entry, and the slot stays where it is.
    pub fn fail_over(&mut self, slot: usize) -> Option<Entry> {
        let &from = self.held.get(slot)?;
        let len = self.list.len();
        let next = (1..len)
            .map(|step| (from + step) % len)
            .find(|i| !self.held.contains(i))?;
        self.held[slot] = next;
        let chosen = self.list[next].clone();
        log_choice(
            slot,
            &chosen,
            ChoiceReason::Failover,
            Some(&self.list[from]),
        );
        self.save();
        Some(chosen)
    }

    /// Write the slots' entries to the choice file. A failed write is a
    /// warning: the choice is still taken, only not remembered.
    pub fn save(&self) {
        let Some(path) = &self.persist else { return };
        let text: String = self
            .held
            .iter()
            .filter_map(|&i| self.list.get(i))
            .map(|e| format!("{e}\n"))
            .collect();
        if let Err(e) = std::fs::write(path, text) {
            tracing::warn!(
                "could not keep the automatic server choice in {}: {}",
                path.display(),
                e
            );
        }
    }
}

/// One slot's handle on the pool, what its TCP client carries.
#[derive(Debug, Clone)]
pub struct ServerSlot {
    pool: Arc<Mutex<ServerPool>>,
    slot: usize,
}

impl ServerSlot {
    pub fn new(pool: Arc<Mutex<ServerPool>>, slot: usize) -> Self {
        Self { pool, slot }
    }

    /// The entry this slot dials now.
    pub fn current(&self) -> Option<Entry> {
        self.pool.lock_recover().current(self.slot)
    }

    /// See [`ServerPool::fail_over`].
    pub fn fail_over(&self) -> Option<Entry> {
        self.pool.lock_recover().fail_over(self.slot)
    }
}

/// The `AUTO_SERVER` line: which entry a slot dials and why.
fn log_choice(slot: usize, chosen: &Entry, reason: ChoiceReason, from: Option<&Entry>) {
    let chosen = chosen.to_string();
    let from = from.map(|f| f.to_string());
    tracing::info!(
        event = "AUTO_SERVER",
        slot = slot + 1,
        chosen = %crate::event_log::Scalar(&chosen),
        reason = reason.as_str(),
        from = %crate::event_log::Scalar(from.as_deref().unwrap_or("none")),
    );
}

/// Read a choice file; a missing or unreadable one is no choice.
fn read_choice(path: &Path) -> Vec<Entry> {
    std::fs::read_to_string(path)
        .map(|text| text.lines().filter_map(Entry::parse).collect())
        .unwrap_or_default()
}

/// Add the server slots `layer` asks for to `config`, as `bootstrap_only`
/// TCP clients of origin `default`, and raise a `0`
/// `autoconnect_discovered_interfaces` to [`DISCOVERED_CEILING`].
///
/// `env` is [`ENV_SERVERS`]' value, `storage` the directory the choice is
/// kept in. Nothing happens with `servers` off. A list of none logs one
/// `AUTO_SERVER chosen=none reason=empty` line and adds nothing.
pub fn apply(
    config: &mut Config,
    layer: AutoLayer,
    env: Option<&str>,
    storage: Option<&Path>,
) -> crate::error::Result<()> {
    if !layer.media.servers {
        return Ok(());
    }
    let (list, source) = resolve_list(env, config.auto_server_list.as_deref())?;
    let persist = storage.map(|dir| dir.join(CHOICE_FILE));
    let persisted = persist.as_deref().map(read_choice).unwrap_or_default();
    let (held, reason) = choose(&list, source, &persisted, |n| {
        use rand_core::RngCore;
        (rand_core::OsRng.next_u64() % n as u64) as usize
    });
    if held.is_empty() {
        tracing::info!(
            event = "AUTO_SERVER",
            slot = 0,
            chosen = "none",
            reason = "empty",
            from = "none",
        );
        return Ok(());
    }
    if let Some(dir) = storage {
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!("could not create {}: {}", dir.display(), e);
        }
    }
    let pool = ServerPool::new(list, held.clone(), persist);
    for (slot, &i) in held.iter().enumerate() {
        log_choice(slot, &pool.list[i], reason, None);
    }
    pool.save();
    let pool = Arc::new(Mutex::new(pool));
    for slot in 0..held.len() {
        let name = section_name(slot);
        if config.interfaces.contains_key(&name) {
            tracing::warn!(
                "[auto] servers = yes, but the configuration has a section [[{}]] of its own; \
                 that slot is not dialled",
                name
            );
            continue;
        }
        let entry = ServerSlot::new(Arc::clone(&pool), slot).current();
        let Some(entry) = entry else { continue };
        config.interfaces.insert(
            name.clone(),
            InterfaceConfig {
                name,
                interface_type: "TCPClientInterface".to_string(),
                enabled: true,
                target_host: Some(entry.host),
                target_port: Some(entry.port),
                bootstrap_only: true,
                origin: InterfaceOrigin::Default,
                server_slot: Some(ServerSlot::new(Arc::clone(&pool), slot)),
                ..Default::default()
            },
        );
    }
    if config.reticulum.autoconnect_discovered_interfaces == 0 {
        config.reticulum.autoconnect_discovered_interfaces = DISCOVERED_CEILING;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto_layer::{AutoMedia, AutoReason};

    fn e(text: &str) -> Entry {
        Entry::parse(text).expect("entry")
    }

    #[test]
    fn entries_parse_as_host_port_and_bracketed_v6() {
        assert_eq!(
            e("leviculum.network:4242"),
            Entry {
                host: "leviculum.network".into(),
                port: 4242
            }
        );
        assert_eq!(e("[::1]:7").host, "::1");
        assert_eq!(e("[::1]:7").to_string(), "[::1]:7");
        for bad in [
            "",
            "host",
            ":1",
            "::1:7",
            "host:port",
            "[::1]7",
            "host:70000",
        ] {
            assert_eq!(Entry::parse(bad), None, "{bad}");
        }
        assert_eq!(parse_list(" a:1, ,b:2 ").expect("list").len(), 2);
        assert!(parse_list("a:1,b").is_err());
        assert_eq!(parse_list("").expect("empty"), vec![]);
    }

    #[test]
    fn the_shipped_list_starts_with_our_server() {
        let (list, source) = resolve_list(None, None).expect("shipped");
        assert_eq!(source, ListSource::Shipped);
        assert_eq!(list[0], e("leviculum.network:4242"));
        let (list, source) = resolve_list(Some("x:1"), Some(&["y:2".to_string()])).expect("env");
        assert_eq!((list, source), (vec![e("x:1")], ListSource::Env));
        let (list, source) = resolve_list(None, Some(&["y:2".to_string()])).expect("config");
        assert_eq!((list, source), (vec![e("y:2")], ListSource::Config));
        assert!(resolve_list(Some("nonsense"), None).is_err());
    }

    #[test]
    fn a_pinned_list_is_taken_in_order_and_a_shipped_one_drawn() {
        let list = vec![e("a:1"), e("b:2"), e("c:3")];
        assert_eq!(
            choose(&list, ListSource::Env, &[], |_| 0),
            (vec![0, 1], ChoiceReason::Pinned)
        );
        // A draw that always takes the last remaining index.
        assert_eq!(
            choose(&list, ListSource::Shipped, &[], |n| n - 1),
            (vec![2, 0], ChoiceReason::Drawn)
        );
        assert_eq!(
            choose(&list[..1], ListSource::Shipped, &[], |_| 0),
            (vec![0], ChoiceReason::Drawn)
        );
        assert_eq!(
            choose(&[], ListSource::Env, &[], |_| 0),
            (vec![], ChoiceReason::Pinned)
        );
    }

    #[test]
    fn a_persisted_choice_survives_only_while_the_list_names_it() {
        let list = vec![e("a:1"), e("b:2"), e("c:3")];
        assert_eq!(
            choose(&list, ListSource::Env, &[e("c:3"), e("a:1")], |_| 0),
            (vec![2, 0], ChoiceReason::Persisted)
        );
        // An entry that left the list, a short or doubled choice: drawn anew.
        for stale in [
            vec![e("c:3"), e("z:9")],
            vec![e("c:3")],
            vec![e("c:3"), e("c:3")],
        ] {
            assert_eq!(
                choose(&list, ListSource::Env, &stale, |_| 0).1,
                ChoiceReason::Pinned,
                "{stale:?}"
            );
        }
    }

    #[test]
    fn fail_over_takes_the_next_entry_no_other_slot_holds_and_persists_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CHOICE_FILE);
        let list = vec![e("a:1"), e("b:2"), e("c:3"), e("d:4")];
        let mut pool = ServerPool::new(list, vec![0, 1], Some(path.clone()));
        assert_eq!(pool.fail_over(0), Some(e("c:3")));
        assert_eq!(read_choice(&path), vec![e("c:3"), e("b:2")]);
        assert_eq!(pool.fail_over(0), Some(e("d:4")));
        // Past the end it wraps, skipping slot 1's entry.
        assert_eq!(pool.fail_over(0), Some(e("a:1")));
        assert_eq!(pool.current(1), Some(e("b:2")));

        // Two slots over two entries: nowhere to go.
        let mut pool = ServerPool::new(vec![e("a:1"), e("b:2")], vec![0, 1], None);
        assert_eq!(pool.fail_over(1), None);
        assert_eq!(pool.current(1), Some(e("b:2")));
    }

    fn on() -> AutoLayer {
        AutoLayer {
            media: AutoMedia {
                servers: true,
                ..AutoMedia::NONE
            },
            reason: AutoReason::Default,
        }
    }

    #[test]
    fn apply_adds_bootstrap_slots_and_raises_the_ceiling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = Config::default();
        apply(&mut config, on(), Some("a:1,b:2,c:3"), Some(dir.path())).expect("apply");
        assert_eq!(config.interfaces.len(), BOOTSTRAP_ENTRIES);
        let first = config.interfaces.get("Auto Server 1").expect("slot 1");
        assert_eq!(first.interface_type, "TCPClientInterface");
        assert_eq!(first.target_host.as_deref(), Some("a"));
        assert!(first.bootstrap_only);
        assert_eq!(first.origin, InterfaceOrigin::Default);
        assert_eq!(
            config.reticulum.autoconnect_discovered_interfaces,
            DISCOVERED_CEILING
        );
        assert_eq!(
            read_choice(&dir.path().join(CHOICE_FILE)),
            vec![e("a:1"), e("b:2")]
        );

        // The next start keeps the choice, even under a reordered list.
        let mut config = Config::default();
        config.reticulum.autoconnect_discovered_interfaces = 5;
        apply(&mut config, on(), Some("c:3,b:2,a:1"), Some(dir.path())).expect("apply");
        let first = config.interfaces.get("Auto Server 1").expect("slot 1");
        assert_eq!(first.target_host.as_deref(), Some("a"));
        assert_eq!(config.reticulum.autoconnect_discovered_interfaces, 5);
    }

    #[test]
    fn apply_adds_nothing_with_servers_off_or_an_empty_list() {
        let mut config = Config::default();
        let off = AutoLayer {
            media: AutoMedia::NONE,
            reason: AutoReason::Config,
        };
        apply(&mut config, off, Some("a:1"), None).expect("apply");
        assert!(config.interfaces.is_empty());
        apply(&mut config, on(), Some(""), None).expect("apply");
        assert!(config.interfaces.is_empty());
        assert_eq!(config.reticulum.autoconnect_discovered_interfaces, 0);
    }
}

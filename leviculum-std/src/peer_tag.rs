//! What the daemon writes about a peer's network address (order 1678, #232).
//!
//! lnsd stores no IP addresses. A log line or an event that would name a
//! client's or a neighbour's address names a [`PeerTag`] instead: the first
//! four bytes of HMAC-SHA256 over the address, under a key the process draws
//! from the OS at first use and redraws every [`KEY_EPOCH`], held only in
//! memory and never written. Within a day the same source carries the same
//! tag, so 412 connections from one host still read as one source; once the
//! key turns, nothing written before can be linked to anything written after,
//! nor back to the address. The port is never part of it: a client's
//! ephemeral port says nothing a debugger needs and only adds a field to
//! correlate on.
//!
//! What debugging needs beyond that is already there: a peer that announces
//! is known by its transport hash, and the live table (`lnstatus`) still
//! shows the address of a connection while it lives, next to its tag
//! (`src_tag`), so a log line can be matched to a live connection while it
//! exists. Holding an address in memory while the connection is open is
//! processing; storing it in a file is what this module ends.
//!
//! The rare operator who wants raw addresses sets
//! `[logging] log_addresses = full`; the daemon then says so at start.
//! Python's rnsd logs addresses raw, which makes this a deliberate deviation:
//! nothing on the wire changes, and a log holds nothing to leak.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rand_core::RngCore;
use serde::{Deserialize, Serialize};

use crate::sync_ext::MutexRecover;

/// How long one pseudonym key lives before the next one is drawn.
pub const KEY_EPOCH: Duration = Duration::from_secs(24 * 60 * 60);

/// `[logging] log_addresses`: what the daemon writes where a peer's address
/// would go.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogAddresses {
    /// A [`PeerTag`], the default.
    #[default]
    Pseudonym,
    /// The address itself, with the port where there is one.
    Full,
}

impl LogAddresses {
    /// Parse the config value; `None` for anything but the two names.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "pseudonym" => Some(Self::Pseudonym),
            "full" => Some(Self::Full),
            _ => None,
        }
    }
}

/// A peer's daily pseudonym: eight hex characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeerTag([u8; 4]);

impl fmt::Display for PeerTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// The bytes an address is tagged by: an IPv4 address and its IPv4-mapped
/// IPv6 form are one source, so they give one tag.
fn address_bytes(ip: IpAddr) -> Vec<u8> {
    match ip {
        IpAddr::V4(v4) => v4.octets().to_vec(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.octets().to_vec(),
            None => v6.octets().to_vec(),
        },
    }
}

fn tag_under(key: &[u8; 32], ip: IpAddr) -> PeerTag {
    let mac = leviculum_core::crypto::hmac_sha256(key, &address_bytes(ip));
    PeerTag([mac[0], mac[1], mac[2], mac[3]])
}

/// One key and when it was drawn. The process holds one in [`STATE`]; tests
/// hold their own with a clock they move.
pub(crate) struct TagKey {
    key: [u8; 32],
    drawn: Instant,
}

impl TagKey {
    /// A key drawn from the OS at `now`.
    pub(crate) fn draw(now: Instant) -> Self {
        let mut key = [0u8; 32];
        rand_core::OsRng.fill_bytes(&mut key);
        Self { key, drawn: now }
    }

    /// The tag of `ip` at `now`, drawing a fresh key first when the current
    /// one is [`KEY_EPOCH`] old.
    pub(crate) fn tag(&mut self, ip: IpAddr, now: Instant) -> PeerTag {
        if now.saturating_duration_since(self.drawn) >= KEY_EPOCH {
            *self = Self::draw(now);
        }
        tag_under(&self.key, ip)
    }
}

struct State {
    mode: LogAddresses,
    key: Option<TagKey>,
}

/// The process's mode and key. A daemon runs one node; a test binary that
/// builds several shares one key, which only makes their tags agree.
static STATE: Mutex<State> = Mutex::new(State {
    mode: LogAddresses::Pseudonym,
    key: None,
});

/// Set what the process writes for an address, from the config. Saying so
/// when it is `full` is the caller's job, once, at start.
pub fn set_mode(mode: LogAddresses) {
    STATE.lock_recover().mode = mode;
}

/// The process's current mode.
pub fn mode() -> LogAddresses {
    STATE.lock_recover().mode
}

/// The tag of `ip` under the process's current key.
pub fn tag(ip: IpAddr) -> PeerTag {
    let now = Instant::now();
    let mut state = STATE.lock_recover();
    state
        .key
        .get_or_insert_with(|| TagKey::draw(now))
        .tag(ip, now)
}

/// What a log line writes for a peer's IP address: its tag, or the address
/// under `log_addresses = full`.
pub fn show_ip(ip: IpAddr) -> String {
    render_ip(mode(), ip, tag)
}

/// What a log line writes for a peer's socket address: its tag (the port
/// dropped), or `ip:port` under `log_addresses = full`.
pub fn show_addr(addr: SocketAddr) -> String {
    render_addr(mode(), addr, tag)
}

fn render_ip(mode: LogAddresses, ip: IpAddr, tag: impl FnOnce(IpAddr) -> PeerTag) -> String {
    match mode {
        LogAddresses::Pseudonym => tag(ip).to_string(),
        LogAddresses::Full => ip.to_string(),
    }
}

fn render_addr(
    mode: LogAddresses,
    addr: SocketAddr,
    tag: impl FnOnce(IpAddr) -> PeerTag,
) -> String {
    match mode {
        LogAddresses::Pseudonym => tag(addr.ip()).to_string(),
        LogAddresses::Full => addr.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn same_address_same_tag_within_an_epoch() {
        let t0 = Instant::now();
        let mut key = TagKey::draw(t0);
        let a = key.tag(ip("192.0.2.7"), t0);
        let later = key.tag(ip("192.0.2.7"), t0 + KEY_EPOCH - Duration::from_secs(1));
        assert_eq!(a, later, "one source keeps its tag for the whole day");
        assert_eq!(a.to_string().len(), 8);
        assert!(a.to_string().bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(
            a,
            key.tag(ip("192.0.2.8"), t0),
            "two sources are told apart"
        );
    }

    #[test]
    fn the_key_turns_after_an_epoch() {
        let t0 = Instant::now();
        let mut key = TagKey::draw(t0);
        let before_key = key.key;
        let before = key.tag(ip("2001:db8::1"), t0);
        let after = key.tag(ip("2001:db8::1"), t0 + KEY_EPOCH);
        assert_ne!(key.key, before_key, "a fresh key is drawn at the epoch");
        assert_ne!(before, after, "after the key turns the tag is not linkable");
        // The new epoch is stable again.
        assert_eq!(
            after,
            key.tag(
                ip("2001:db8::1"),
                t0 + KEY_EPOCH * 2 - Duration::from_secs(1)
            )
        );
    }

    #[test]
    fn the_port_is_dropped_and_mapped_v4_is_v4() {
        let t0 = Instant::now();
        let mut key = TagKey::draw(t0);
        let v4 = key.tag(ip("198.51.100.4"), t0);
        assert_eq!(v4, key.tag(ip("::ffff:198.51.100.4"), t0));
        let mut k = |a: IpAddr| key.tag(a, t0);
        let one = render_addr(
            LogAddresses::Pseudonym,
            "198.51.100.4:1".parse().unwrap(),
            &mut k,
        );
        let two = render_addr(
            LogAddresses::Pseudonym,
            "198.51.100.4:2".parse().unwrap(),
            &mut k,
        );
        assert_eq!(one, two);
        assert_eq!(one, v4.to_string());
    }

    #[test]
    fn pseudonym_never_writes_the_address_and_full_does() {
        let addr: SocketAddr = "[2001:db8::42]:4242".parse().unwrap();
        let fixed = |_: IpAddr| PeerTag([0xab, 0xcd, 0x01, 0x23]);
        let shown = render_addr(LogAddresses::Pseudonym, addr, fixed);
        assert_eq!(shown, "abcd0123");
        assert!(!shown.contains("2001") && !shown.contains("4242"));
        assert_eq!(
            render_addr(LogAddresses::Full, addr, fixed),
            "[2001:db8::42]:4242"
        );
        assert_eq!(
            render_ip(LogAddresses::Full, addr.ip(), fixed),
            "2001:db8::42"
        );
        assert_eq!(
            render_ip(LogAddresses::Pseudonym, addr.ip(), fixed),
            "abcd0123"
        );
    }

    #[test]
    fn the_config_value_parses() {
        assert_eq!(
            LogAddresses::parse("pseudonym"),
            Some(LogAddresses::Pseudonym)
        );
        assert_eq!(LogAddresses::parse(" Full "), Some(LogAddresses::Full));
        assert_eq!(LogAddresses::parse("raw"), None);
        assert_eq!(LogAddresses::default(), LogAddresses::Pseudonym);
    }

    #[test]
    fn the_process_default_is_the_pseudonym() {
        // No test in this binary sets `full` on the process state.
        assert_eq!(mode(), LogAddresses::Pseudonym);
        let shown = show_ip(ip("203.0.113.9"));
        assert!(!shown.contains("203.0.113.9"));
        assert_eq!(shown, tag(ip("203.0.113.9")).to_string());
    }
}

//! Where a mailbox on a computer reaches the network (1354): beside the
//! device's Reticulum daemon, on its shared instance, or on a node of its
//! own that dials entry servers, as the phone's mailbox always does
//! (`DaemonUpstream::Mesh`). A computer without lnsd runs a mailbox all the
//! same, and a test cell builds the leaf mode on the host.
//!
//! Zero-config (`docs/src/concepts/zero-config.md`): nobody has to know a
//! flag. Unset, the mailbox runs beside the daemon when one answers on its
//! shared instance and on its own node when none does; the config's
//! `mailbox_upstream` key is the opt-out either way, and `mailbox_servers`
//! names the entry servers. The phone decides for itself and never reads
//! this ([`decide`] is the computer's rule alone).

use alloc::string::String;
use alloc::vec::Vec;

use crate::config::{Config, MailboxUpstream};

/// The way [`decide`] chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Way {
    /// Beside the daemon, on its shared instance.
    Shared,
    /// On a node of its own dialling `servers`, `host:port` each, one at a
    /// time with failover; `from` says where the list came from
    /// ([`mesh_servers`]).
    Mesh {
        servers: Vec<String>,
        from: &'static str,
    },
}

/// The way a mailbox runs and the word its `HOME upstream` line logs for
/// why: `config` for the `mailbox_upstream` key, `lnsd` for a daemon that
/// answered on its shared instance, `no-lnsd` for none. `daemon_answers`
/// is asked only when the key is unset, so a mailbox told its way never
/// touches the daemon's socket. The rule reads the state at each start, so
/// a restart under the same config and the same daemon keeps the way.
pub fn decide(config: &Config, daemon_answers: impl FnOnce() -> bool) -> (Way, &'static str) {
    let mesh = || {
        let (servers, from) = mesh_servers(config);
        Way::Mesh { servers, from }
    };
    match config.mailbox_upstream {
        Some(MailboxUpstream::Shared) => (Way::Shared, "config"),
        Some(MailboxUpstream::Mesh) => (mesh(), "config"),
        None if daemon_answers() => (Way::Shared, "lnsd"),
        None => (mesh(), "no-lnsd"),
    }
}

/// The entry servers of a mailbox on its own node, and where they came
/// from: `mailbox_servers` as written (`config`), else the rendezvous list
/// a mailbox beside a daemon dials as a peer
/// ([`super::rendezvous::mailbox_list`]: the `rendezvous` key, the shipped
/// list, `off` or `held`). The shipped one is `leviculum.network:4242`, the
/// list the phone's mailbox ships.
pub fn mesh_servers(config: &Config) -> (Vec<String>, &'static str) {
    match &config.mailbox_servers {
        Some(list) if !list.is_empty() => (list.clone(), "config"),
        _ => super::rendezvous::mailbox_list(config),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    fn config(text: &str) -> Config {
        crate::config::parse(text).expect("parses")
    }

    #[test]
    fn unset_runs_beside_a_daemon_that_answers_and_on_its_own_node_without() {
        assert_eq!(decide(&config(""), || true), (Way::Shared, "lnsd"));
        let (way, why) = decide(&config("mailbox_servers = a.example:4242\n"), || false);
        assert_eq!(why, "no-lnsd");
        assert_eq!(
            way,
            Way::Mesh {
                servers: vec!["a.example:4242".to_string()],
                from: "config"
            }
        );
    }

    #[test]
    fn the_key_wins_and_the_daemon_is_not_asked() {
        let asked = core::cell::Cell::new(false);
        let ask = || {
            asked.set(true);
            true
        };
        let (way, why) = decide(
            &config("mailbox_upstream = mesh\nmailbox_servers = a:1, b:2\n"),
            ask,
        );
        assert_eq!(why, "config");
        assert_eq!(
            way,
            Way::Mesh {
                servers: vec!["a:1".to_string(), "b:2".to_string()],
                from: "config"
            }
        );
        assert_eq!(
            decide(&config("mailbox_upstream = shared\n"), || false),
            (Way::Shared, "config")
        );
        assert!(!asked.get(), "a mailbox told its way asked the daemon");
    }

    /// Without `mailbox_servers` the mesh dials the rendezvous list on its
    /// ordinary ports, and `rendezvous_dial = off` leaves it none.
    #[test]
    fn without_servers_the_rendezvous_list_is_the_entry_list() {
        let (servers, from) = mesh_servers(&config("rendezvous = r.example:4343\n"));
        assert_eq!(
            (servers, from),
            (vec!["r.example:4242".to_string()], "config")
        );
        let (servers, from) = mesh_servers(&config("rendezvous_dial = off\n"));
        assert_eq!((servers, from), (Vec::new(), "off"));
        let (servers, _) = mesh_servers(&config("mailbox_servers = []\nrendezvous = r:4343\n"));
        assert_eq!(servers, vec!["r:4242".to_string()]);
    }

    /// The shipped list is the phone's: our node's ordinary port.
    #[test]
    fn the_shipped_entry_list_is_the_phones() {
        let shipped =
            super::super::rendezvous::peer_list(&super::super::rendezvous::default_list());
        assert_eq!(shipped, vec!["leviculum.network:4242".to_string()]);
    }
}

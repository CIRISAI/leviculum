//! Opening an admission: one device's single-use token in the mailbox's
//! store and the join code that carries it. `loquax mailbox admit` on a
//! computer and "Link a device" on a phone that holds the mailbox (1264)
//! both call [`open`], so the code a phone shows is the code a computer
//! prints: the same `lqxh1` body, the same token, the same life
//! (`home_token_hours`, [`super::token_ttl_secs`]), the rendezvous list of
//! 1196 behind it.

use alloc::string::String;
use alloc::vec::Vec;

use super::code::HomeCode;
use crate::store::{Store, StoreError};

/// The name a token is issued under when the device is to name itself:
/// the join's name then wins ([`Store::redeem_token_as`]). A phone that
/// links a device does not know what the other device is called.
pub const SELF_NAMED: &str = "";

/// An admission just opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    /// The mailbox's public key and the token.
    pub code: HomeCode,
    /// The rendezvous list behind the code, the first tried first.
    pub list: Vec<String>,
    /// When the token was issued, unix seconds.
    pub issued_at: i64,
}

impl Admission {
    /// The code as the new device takes it, the list behind it.
    pub fn text(&self) -> String {
        self.code.encode_with_rendezvous(&self.list)
    }

    /// The last second the code is good for, with tokens living `ttl_secs`.
    pub fn expires_at(&self, ttl_secs: i64) -> i64 {
        self.issued_at.saturating_add(ttl_secs)
    }
}

/// Issue a fresh token under `name` in the mailbox's `store` at `now` and
/// wrap it, with the mailbox's `public_key` and the rendezvous `list`, into
/// the code a new device redeems once.
pub fn open(
    store: &mut Store,
    public_key: [u8; 64],
    name: &str,
    list: Vec<String>,
    now: i64,
) -> Result<Admission, StoreError> {
    let mut token = [0u8; 16];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut token);
    store.issue_token(&token, name, now)?;
    Ok(Admission {
        code: HomeCode { public_key, token },
        list,
        issued_at: now,
    })
}

/// The rendezvous list a computer's mailbox writes into its codes (1196):
/// `server`, its own phone port, first when given, then the list the
/// mailbox dials itself as the phones dial it; none on a host that holds
/// the shipped defaults back, or that dials no rendezvous.
pub fn computer_list(config: &crate::config::Config, server: Option<&str>) -> Vec<String> {
    use super::rendezvous::{default_list, mailbox_list, with_first};
    let shared: Vec<String> = match mailbox_list(config) {
        (_, "config") => config.rendezvous.clone().unwrap_or_default(),
        (_, "default") => default_list(),
        _ => Vec::new(),
    };
    match server {
        Some(server) => with_first(server, &shared),
        None => shared,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Redeemed;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("dir");
        let store = Store::open_in(dir.path(), [9; 16]).expect("store");
        (dir, store)
    }

    /// The code reads back to the key, the token and the list, and its
    /// token is the one the store holds open.
    #[test]
    fn an_admission_reads_back_and_its_token_is_open() {
        let (_dir, mut store) = store();
        let list = vec!["a.example:4343".to_string(), "a.example:4242".to_string()];
        let admission = open(&mut store, [5; 64], "laptop", list.clone(), 100).expect("open");
        let (code, carried) = HomeCode::parse_with_rendezvous(&admission.text()).expect("parse");
        assert_eq!(code, admission.code);
        assert_eq!(carried, list);
        assert_eq!(admission.expires_at(3600), 3700);
        let open = store.open_tokens(101, 3600).expect("tokens");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].token, admission.code.token);
        assert_eq!(open[0].name, "laptop");
    }

    /// A token opened self-named admits the device under the name its
    /// join brought, "device" when it brought none; a named token keeps
    /// its name whatever the join says.
    #[test]
    fn a_self_named_token_takes_the_joins_name_a_named_one_keeps_its_own() {
        let (_dir, mut store) = store();
        let linked = open(&mut store, [5; 64], SELF_NAMED, Vec::new(), 1).expect("open");
        assert_eq!(
            store
                .redeem_token_as(&linked.code.token, &[1; 16], "Pixel 8", 2, 3600)
                .expect("redeem"),
            Redeemed::Admitted {
                name: "Pixel 8".to_string()
            }
        );
        let silent = open(&mut store, [5; 64], SELF_NAMED, Vec::new(), 1).expect("open");
        assert_eq!(
            store
                .redeem_token_as(&silent.code.token, &[2; 16], "  ", 2, 3600)
                .expect("redeem"),
            Redeemed::Admitted {
                name: "device".to_string()
            }
        );
        let named = open(&mut store, [5; 64], "laptop", Vec::new(), 1).expect("open");
        assert_eq!(
            store
                .redeem_token_as(&named.code.token, &[3; 16], "Pixel 8", 2, 3600)
                .expect("redeem"),
            Redeemed::Admitted {
                name: "laptop".to_string()
            }
        );
        let names: Vec<String> = store
            .devices()
            .expect("devices")
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(names, ["Pixel 8", "device", "laptop"]);
    }

    /// The computer's list: the server first, then the config's; the
    /// shipped list when nothing names one (and the host does not hold
    /// defaults back); none with `rendezvous_dial = off`.
    #[test]
    fn the_computers_list_puts_its_server_first() {
        let config = crate::config::Config {
            rendezvous: Some(vec!["r.example:4343".to_string()]),
            ..Default::default()
        };
        assert_eq!(
            computer_list(&config, Some("me.example:4243")),
            ["me.example:4243", "r.example:4343"]
        );
        let off = crate::config::Config {
            rendezvous_dial: Some(false),
            ..Default::default()
        };
        assert_eq!(
            computer_list(&off, Some("me.example:4243")),
            ["me.example:4243"]
        );
        assert!(computer_list(&off, None).is_empty());
    }
}

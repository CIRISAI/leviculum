//! The device walk's mailbox (1175): the 1173 ugly store in the archive of
//! a Loquax Mailbox home, a second home whose identity is the Ratspeak
//! conversation's peer, and a manifest of what the phone's screens must
//! show for them, and a friend's contact code the phone does not know.
//!
//! ```sh
//! cargo run -p loquax-android --example walk_mailbox -- MAILBOX SENDER MANIFEST
//! ```
//!
//! `MAILBOX` becomes the `LNMSG_HOME` of `loquax mailbox run`, `SENDER`
//! the `LNMSG_HOME` of the `loquax send` the walk's `receive_wake` step
//! runs: a message from a peer the phone already holds lands in the chat
//! the walk has open. `MANIFEST` is JSON, read by
//! `loquax-android/scripts/device-walk.py`; every value in it comes from
//! `loquax/tests/support/ugly_store.rs`, so the walk and the host tests
//! assert the same store.

use std::path::PathBuf;
use std::process::ExitCode;

#[path = "../../loquax/tests/support/ugly_store.rs"]
mod ugly_store;

/// The name a screen shows for conversation `i`: its name, or the address
/// form of `loquax::names` (`<` four bytes in hex `…>`) for none.
fn shown(i: usize) -> String {
    ugly_store::shown_name(i).unwrap_or_else(|| format!("<{}…>", &ugly_store::peer_hex(i)[..8]))
}

/// The names the map's Friends sheet may list (1200): every conversation
/// but the blocked one whose peer sent a position, archived ones too.
fn friends(own: &leviculum_core::Identity) -> Vec<String> {
    (0..ugly_store::CONVERSATIONS)
        .filter(|&i| i != ugly_store::BLOCKED)
        .filter(|&i| {
            ugly_store::rows(own, i).iter().any(|row| {
                !row.outbound
                    && row
                        .fields
                        .iter()
                        .any(|(key, _)| *key == leviculum_lxmf::constants::FIELD_TELEMETRY)
            })
        })
        .map(shown)
        .collect()
}

fn run(mailbox: PathBuf, sender: PathBuf, manifest: PathBuf) -> Result<(), String> {
    std::fs::create_dir_all(&mailbox).map_err(|e| format!("{}: {e}", mailbox.display()))?;
    std::fs::create_dir_all(&sender).map_err(|e| format!("{}: {e}", sender.display()))?;
    let filled = ugly_store::fill_archive(&mailbox);
    let own = loquax::identity::load_or_create(&mailbox.join("identity"))
        .map_err(|e| format!("the mailbox identity: {e}"))?;
    let address = loquax::engine::delivery_address(&own)?;
    let them = ugly_store::RATSPEAK;
    loquax::identity::store(&sender.join("identity"), &ugly_store::identity(them))
        .map_err(|e| format!("the sender identity: {e}"))?;
    let rows = ugly_store::rows(&own, them);
    let conversations: Vec<serde_json::Value> = (0..ugly_store::CONVERSATIONS)
        .map(|i| {
            serde_json::json!({
                "index": i,
                "peer": ugly_store::peer_hex(i),
                "shown": shown(i),
                "named": ugly_store::shown_name(i).is_some(),
                "archived": i == ugly_store::ARCHIVED,
                "blocked": i == ugly_store::BLOCKED,
                "imported": i == ugly_store::RATSPEAK || i == ugly_store::NOMADNET,
            })
        })
        .collect();
    let body = |j: usize| rows.get(j).map(|row| row.body.clone()).unwrap_or_default();
    // A friend the phone does not know yet (walk finding 12): their
    // contact code is what the walk shares into the app.
    let friend = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    let friend_key = friend.public_key_bytes();
    let friend_address = loquax::home::lxmf_address(&friend_key)?;
    let doc = serde_json::json!({
        "address": loquax::address::to_hex(&address),
        "messages": filled.messages,
        "imported": filled.imported,
        "conversations": conversations,
        "open": {
            "index": them,
            "shown": shown(them),
            "oldest_body": body(0),
            "second_body": body(1),
            "oldest_at": rows.first().map_or(0, |row| row.at),
        },
        "sender_peer": ugly_store::peer_hex(them),
        "friends": friends(&own),
        "friend": {
            "code": loquax::lxma::encode(&friend_key)?,
            "address": loquax::address::to_hex(&friend_address),
        },
    });
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    std::fs::write(&manifest, text).map_err(|e| format!("{}: {e}", manifest.display()))?;
    println!(
        "walk_mailbox address={} messages={} imported={}",
        loquax::address::to_hex(&address),
        filled.messages,
        filled.imported
    );
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let [mailbox, sender, manifest] = <[PathBuf; 3]>::try_from(args).unwrap_or_default();
    if mailbox.as_os_str().is_empty() {
        eprintln!("usage: walk_mailbox MAILBOX SENDER MANIFEST");
        return ExitCode::from(2);
    }
    match run(mailbox, sender, manifest) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("walk_mailbox: {error}");
            ExitCode::FAILURE
        }
    }
}

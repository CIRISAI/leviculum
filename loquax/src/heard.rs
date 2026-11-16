//! What a device heard, kept across a restart (1632, walk 4 new 15: after
//! a restart Explore listed one to four pages of the over twenty before,
//! and "suggested" sat on another server).
//!
//! The model owes what it heard ([`crate::ui::explore::take_heard`]); a
//! host with a store keeps it here, beside its other writes: a page in the
//! store's page rows (`home_pages`, the table a home keeps for its devices,
//! 1579, under the same identity), a hub in the archive's hub entries
//! (`KIND_HUB`, 1599), the suggestion in the config's `rooms_suggested`.
//! At the next start [`load`] reads them back and
//! [`crate::ui::explore::restore_heard`] lists them, each with its old
//! last seen. They age out by the rules that already bound them:
//! [`crate::home::pages::PAGES_CAP`] pages, [`crate::ui::hubs::HEARD_BOUND`]
//! hubs, the one heard longest ago going first.

use std::path::Path;

use crate::store::{Store, StoreError};
use crate::ui::explore::HeardOwed;
use crate::ui::hubs::HeardHub;
use lnomad::discovery::DiscoveredNode;

/// The config key of the held suggestion.
pub const SUGGESTED_KEY: &str = "rooms_suggested";

/// The archive's bound in bytes for the hub entries a device writes: the
/// home's default (`archive_cap_mb`), which a device never comes near.
const ARCHIVE_CAP_BYTES: u64 = crate::home::DEFAULT_ARCHIVE_CAP_MB * 1024 * 1024;

/// Keep what the model owed: pages, hubs, the suggestion. A failure is
/// said and the rest still kept.
pub fn keep(store: &mut Store, home: &Path, owed: HeardOwed) -> Result<(), String> {
    let mut failed = Vec::new();
    for page in &owed.pages {
        if let Err(error) = store.page_heard(page) {
            failed.push(format!("page: {error}"));
        }
    }
    for hub in &owed.hubs {
        if let Err(error) = store.archive_hub(hub, ARCHIVE_CAP_BYTES) {
            failed.push(format!("hub: {error}"));
        }
    }
    if let Some(held) = owed.held {
        if let Err(error) = crate::config::save_hash(home, SUGGESTED_KEY, held) {
            failed.push(format!("{SUGGESTED_KEY}: {error}"));
        }
    }
    match failed.is_empty() {
        true => Ok(()),
        false => Err(failed.join("; ")),
    }
}

/// What the store kept: the pages, heard last first, and the hubs.
pub fn load(store: &Store) -> Result<(Vec<DiscoveredNode>, Vec<HeardHub>), StoreError> {
    Ok((store.pages_heard()?, store.archived_hubs()?))
}

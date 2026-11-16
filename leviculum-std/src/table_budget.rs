//! What the transport tables may take of the host's RAM (order 1586,
//! LV-1000).
//!
//! Every table is a `BoundedMap` (Codeberg #421), but its ceiling was a
//! profile's, sized for a server that is the only stack on its host: the
//! desktop profile models 285 MB at its ceilings. miauhaus is a 1.9 GB host
//! running lnsd and two local clients beside a dozen other services. A
//! local client keeps `TableCaps::local_client()` at most (order 1585);
//! this module bounds every node by the host it runs on:
//!
//! * the ceilings are priced by the bytes each entry costs (the figures
//!   `TableCaps` documents and the census prints), the duplicate filter and
//!   the known destinations memory keeps included ([`modelled_bytes`]);
//! * where the sum exceeds an [`RAM_SHARE_DIVISOR`]th of the host's RAM,
//!   every ceiling shrinks by the same factor, none below
//!   [`MIN_SCALED_CAP`] ([`fit`]);
//! * a ceiling the config writes (`max_identities`, `path_table_cap`, ...)
//!   is honoured as written, and the builder warns when the result asks for
//!   more than the share.
//!
//! The rule is `lnpnd/src/store_limit.rs`'s for the message store: a share
//! of RAM, a warning for a config that asks for more, eviction oldest first
//! (each table is a `BoundedMap`), each bound in the census beside its
//! fill.

use leviculum_core::constants::TRUNCATED_HASHBYTES;
use leviculum_core::known_destinations::KnownDestEntry;
use leviculum_core::memory_storage::TableCaps;

/// The share of the host's RAM every transport table together may take: an
/// eighth. lnpnd's store index takes a quarter (`store_limit.rs`), and a
/// host runs a daemon and its clients beside it.
pub const RAM_SHARE_DIVISOR: u64 = 8;

/// No ceiling is scaled below this: a table of a few hundred entries costs
/// under a megabyte at the heaviest row, and below it a node that talks to
/// a handful of peers starts to forget them.
pub const MIN_SCALED_CAP: usize = 256;

/// The known destinations memory keeps for `caps`: the newest
/// `identity_cap` (order 1585, `Storage::new_with_limits`).
pub fn known_dest_cap(caps: &TableCaps) -> usize {
    caps.identity_cap
}

/// Modelled bytes an entry costs, per knob: the sum of the rows of the
/// tables the knob sizes, as `TableCaps` documents them
/// (`leviculum-core/src/memory_storage.rs`, the table in its docs).
mod per_entry {
    /// `path_table` 480 + `path_states` 123 + `path_requests` 144 +
    /// `discovery_path_requests` 168.
    pub(super) const PATH: u64 = 480 + 123 + 144 + 168;
    pub(super) const REVERSE: u64 = 192;
    pub(super) const LINK: u64 = 384;
    pub(super) const ANNOUNCE: u64 = 960;
    /// `announce_cache` 792 + `announce_rate_table` 192 + `known_ratchets`
    /// 240 + `known_dest_use` 168.
    pub(super) const DESTINATION: u64 = 792 + 192 + 240 + 168;
    /// `local_client_dest_map` 168 + `local_client_known_dests` 144 +
    /// `dest_ratchet_keys` 384.
    pub(super) const LOCAL_DEST: u64 = 168 + 144 + 384;
    pub(super) const RECEIPT: u64 = 384;
    pub(super) const IDENTITY: u64 = 1_656;
    /// One hash of the std storage's two `HashSet<[u8; 32]>` generations
    /// at the 1.5x its dump prices them at (`storage.rs`).
    pub(super) const PACKET_HASH: u64 = 32 * 3 / 2;
}

/// Modelled bytes of one known destination in memory: the key, the entry,
/// its 32-byte packet hash and a 64-byte app data, at the 1.5x the dump
/// prices the `BTreeMap` at (`storage.rs`, `diagnostic_dump`).
const fn known_dest_entry_bytes() -> u64 {
    ((TRUNCATED_HASHBYTES + core::mem::size_of::<KnownDestEntry>() + 32 + 64) as u64) * 3 / 2
}

/// Modelled bytes of every table at its ceiling, the duplicate filter's
/// `packet_hash_cap` hashes and the known destinations memory keeps included.
pub fn modelled_bytes(caps: &TableCaps, packet_hash_cap: usize) -> u64 {
    let rows = [
        (caps.path_cap, per_entry::PATH),
        (caps.reverse_cap, per_entry::REVERSE),
        (caps.link_cap, per_entry::LINK),
        (caps.announce_cap, per_entry::ANNOUNCE),
        (caps.destination_cap, per_entry::DESTINATION),
        (caps.local_dest_cap, per_entry::LOCAL_DEST),
        (caps.receipt_cap, per_entry::RECEIPT),
        (caps.identity_cap, per_entry::IDENTITY),
        (packet_hash_cap, per_entry::PACKET_HASH),
        (known_dest_cap(caps), known_dest_entry_bytes()),
    ];
    rows.iter()
        .map(|(entries, bytes)| (*entries as u64).saturating_mul(*bytes))
        .fold(0u64, u64::saturating_add)
}

/// The share of `ram_bytes` the tables may take.
pub fn budget_bytes(ram_bytes: u64) -> u64 {
    ram_bytes / RAM_SHARE_DIVISOR
}

/// A profile fitted to the host, and the figures that explain it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableBudget {
    pub caps: TableCaps,
    pub packet_hash_cap: usize,
    /// The host's RAM, `None` where it cannot be read.
    pub ram_bytes: Option<u64>,
    /// [`modelled_bytes`] of the profile before it was fitted.
    pub profile_bytes: u64,
    /// [`modelled_bytes`] of [`Self::caps`].
    pub fitted_bytes: u64,
}

impl TableBudget {
    /// The share of the host's RAM, `None` without a RAM figure.
    pub fn budget_bytes(&self) -> Option<u64> {
        self.ram_bytes.map(budget_bytes)
    }

    /// Whether the profile was shrunk to fit.
    pub fn scaled(&self) -> bool {
        self.fitted_bytes < self.profile_bytes
    }
}

fn shrink(cap: usize, budget: u64, modelled: u64) -> usize {
    let scaled = (cap as u128) * u128::from(budget) / u128::from(modelled.max(1));
    usize::try_from(scaled)
        .unwrap_or(usize::MAX)
        .max(MIN_SCALED_CAP.min(cap))
}

/// Fit `caps` and the duplicate filter's `packet_hash_cap` to the RAM
/// share of `ram_bytes`: unchanged when they fit or the RAM is unknown,
/// else every ceiling times `budget / modelled`, none below
/// [`MIN_SCALED_CAP`]. `receipt_cap` stays: it is the reference's number,
/// the same on both sides of the mesh.
pub fn fit(caps: TableCaps, packet_hash_cap: usize, ram_bytes: Option<u64>) -> TableBudget {
    let profile_bytes = modelled_bytes(&caps, packet_hash_cap);
    let unchanged = TableBudget {
        caps,
        packet_hash_cap,
        ram_bytes,
        profile_bytes,
        fitted_bytes: profile_bytes,
    };
    let Some(budget) = ram_bytes.map(budget_bytes) else {
        return unchanged;
    };
    if profile_bytes <= budget {
        return unchanged;
    }
    // The receipts stay as they are, so the others share what is left.
    let fixed = (caps.receipt_cap as u64).saturating_mul(per_entry::RECEIPT);
    let (budget, scalable) = (
        budget.saturating_sub(fixed),
        profile_bytes.saturating_sub(fixed),
    );
    let s = |cap: usize| shrink(cap, budget, scalable);
    let fitted = TableCaps {
        packet_hash_cap: s(caps.packet_hash_cap),
        identity_cap: s(caps.identity_cap),
        path_cap: s(caps.path_cap),
        reverse_cap: s(caps.reverse_cap),
        link_cap: s(caps.link_cap),
        announce_cap: s(caps.announce_cap),
        destination_cap: s(caps.destination_cap),
        local_dest_cap: s(caps.local_dest_cap),
        receipt_cap: caps.receipt_cap,
    };
    let packet_hash_cap = s(packet_hash_cap);
    TableBudget {
        caps: fitted,
        packet_hash_cap,
        ram_bytes,
        profile_bytes,
        fitted_bytes: modelled_bytes(&fitted, packet_hash_cap),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_core::memory_storage::MAX_RECEIPTS;

    const MIAUHAUS_RAM: u64 = 1_855_332 * 1024;

    /// The local client's profile (order 1585) fits any host this runs
    /// on: it models about 10 MB, inside an eighth of 512 MB.
    #[test]
    fn the_local_client_profile_fits_a_small_host() {
        let client = TableCaps::local_client();
        let bytes = modelled_bytes(&client, client.packet_hash_cap);
        assert!(bytes < 12_000_000, "{bytes}");
        assert!(!fit(client, client.packet_hash_cap, Some(512 << 20)).scaled());
    }

    /// A desktop profile on a host with room for it passes unchanged.
    #[test]
    fn a_profile_that_fits_is_unchanged() {
        let fitted = fit(TableCaps::desktop(), 100_000, Some(16 << 30));
        assert_eq!(fitted.caps, TableCaps::desktop());
        assert_eq!(fitted.packet_hash_cap, 100_000);
        assert!(!fitted.scaled());
    }

    /// Without a RAM figure nothing is scaled: the profile is the bound.
    #[test]
    fn unknown_ram_keeps_the_profile() {
        let fitted = fit(TableCaps::desktop(), 100_000, None);
        assert_eq!(fitted.caps, TableCaps::desktop());
        assert_eq!(fitted.budget_bytes(), None);
    }

    /// lnsd's desktop profile on miauhaus models more than an eighth of
    /// 1.9 GB, so every ceiling shrinks by one factor and the result fits.
    #[test]
    fn the_desktop_profile_on_miauhaus_is_fitted_to_an_eighth() {
        let fitted = fit(TableCaps::desktop(), 100_000, Some(MIAUHAUS_RAM));
        let budget = budget_bytes(MIAUHAUS_RAM);
        assert!(fitted.scaled(), "{fitted:?}");
        assert!(fitted.fitted_bytes <= budget, "{fitted:?} budget {budget}");
        assert!(fitted.caps.identity_cap < 50_000);
        assert!(fitted.caps.identity_cap > 20_000, "{fitted:?}");
        assert_eq!(fitted.caps.receipt_cap, MAX_RECEIPTS);
        // One factor for all: the path table keeps its ratio to identities.
        let ratio = |caps: &TableCaps| caps.path_cap as f64 / caps.identity_cap as f64;
        let before = ratio(&TableCaps::desktop());
        assert!((ratio(&fitted.caps) - before).abs() < 0.01);
    }

    /// A tiny host floors every ceiling instead of zeroing it.
    #[test]
    fn a_tiny_host_floors_at_the_minimum() {
        let client = TableCaps::local_client();
        let fitted = fit(client, client.packet_hash_cap, Some(1 << 20));
        assert_eq!(fitted.caps.identity_cap, MIN_SCALED_CAP);
        assert_eq!(fitted.caps.path_cap, MIN_SCALED_CAP);
        assert_eq!(fitted.packet_hash_cap, MIN_SCALED_CAP);
    }

    /// Memory keeps as many known destinations as identities.
    #[test]
    fn the_known_destinations_follow_the_identity_cap() {
        assert_eq!(known_dest_cap(&TableCaps::desktop()), 50_000);
    }
}

//! The message store's limit, and the host's RAM that bounds its default
//! (order 1548, LV-1000).
//!
//! The store keeps an in-memory index of every message it holds, so the
//! limit on disk is also a limit on RAM: [`INDEX_BYTES_PER_MESSAGE`] for
//! each message, at [`MEAN_MESSAGE_BYTES`] a message. A default that a
//! small host cannot index would let the daemon be configured into the OOM
//! killer by its own defaults, so the DEFAULT limit is bounded: a full
//! store's index takes at most a [`RAM_SHARE_DIVISOR`]th of the host's
//! RAM. A limit the operator set (`message_storage_limit` in the config,
//! or `--store-limit-kb`) is honoured as written; the start then warns
//! when that limit's index exceeds the same share.
//!
//! A board has no such index: its store is scanned on flash
//! (`docs/src/concepts/propagation-node-on-a-board.md`), so the rule is
//! the host daemon's alone.

/// Heap bytes the store index costs per message, measured by the growth
/// soak's census at 326 000 messages after bb9cb8da (order 1515).
pub const INDEX_BYTES_PER_MESSAGE: u64 = 289;

/// The field's mean stored message, in bytes (the miauhaus store).
pub const MEAN_MESSAGE_BYTES: u64 = 330;

/// The share of the host's RAM a full store's index may take: a quarter.
pub const RAM_SHARE_DIVISOR: u64 = 4;

/// The default limit before the RAM bound, lxmd's 500 megabytes
/// (`reference/LXMF/LXMF/Utilities/lxmd.py:163`).
pub const DEFAULT_STORE_LIMIT_BYTES: u64 = 500_000_000;

/// Where the store limit came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The default, bounded by the host's RAM.
    Default,
    /// The config file or a flag, honoured as written.
    Config,
}

impl Source {
    /// The word the `PN_STORE_LIMIT` line carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Default => "default",
            Source::Config => "config",
        }
    }
}

/// The resolved store limit and the figures that explain it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreLimit {
    /// The limit the store opens with, in bytes.
    pub bytes: u64,
    pub source: Source,
    /// The host's total RAM in bytes, `None` where it cannot be read.
    pub ram_bytes: Option<u64>,
    /// The largest limit whose full index fits the RAM share, `None`
    /// without a RAM figure.
    pub ram_bound_bytes: Option<u64>,
}

impl StoreLimit {
    /// Heap bytes the index of a full store at this limit takes.
    pub fn index_bytes(&self) -> u64 {
        index_bytes_for(self.bytes)
    }

    /// A configured limit whose full index exceeds the RAM share.
    pub fn exceeds_ram_rule(&self) -> bool {
        self.ram_bound_bytes.is_some_and(|bound| self.bytes > bound)
    }
}

/// Heap bytes of the index of a full store of `limit_bytes`.
pub fn index_bytes_for(limit_bytes: u64) -> u64 {
    scale(limit_bytes, INDEX_BYTES_PER_MESSAGE, MEAN_MESSAGE_BYTES)
}

/// The largest store limit whose full index takes at most the RAM share
/// of `ram_bytes`.
pub fn ram_bound_bytes(ram_bytes: u64) -> u64 {
    scale(
        ram_bytes / RAM_SHARE_DIVISOR,
        MEAN_MESSAGE_BYTES,
        INDEX_BYTES_PER_MESSAGE,
    )
}

/// `value * num / den` without overflow, saturating at `u64::MAX`.
fn scale(value: u64, num: u64, den: u64) -> u64 {
    let wide = u128::from(value) * u128::from(num) / u128::from(den.max(1));
    u64::try_from(wide).unwrap_or(u64::MAX)
}

/// The rule: a configured limit as written, else the default bounded by
/// the RAM share when the host's RAM is known.
pub fn resolve(configured_bytes: Option<u64>, ram_bytes: Option<u64>) -> StoreLimit {
    let ram_bound = ram_bytes.map(ram_bound_bytes);
    let (bytes, source) = match configured_bytes {
        Some(bytes) => (bytes, Source::Config),
        None => (
            ram_bound.map_or(DEFAULT_STORE_LIMIT_BYTES, |bound| {
                DEFAULT_STORE_LIMIT_BYTES.min(bound)
            }),
            Source::Default,
        ),
    };
    StoreLimit {
        bytes,
        source,
        ram_bytes,
        ram_bound_bytes: ram_bound,
    }
}

/// The host's RAM reader, shared with the transport tables' budget
/// (`leviculum_std::table_budget`, order 1586): one rule for what "the
/// host's RAM" means.
pub use leviculum_std::proc_memory::{host_ram_bytes, parse_mem_total};

/// Bytes as decimal megabytes, the unit the config's limit is written in.
pub fn megabytes(bytes: u64) -> u64 {
    bytes / 1_000_000
}

#[cfg(test)]
mod tests {
    use super::*;

    /// miauhaus: 1.9 GB of RAM. A full store's index may take 475 MB, which
    /// at 289 B a 330 B message is a 542 MB store, above the 500 MB
    /// default: the default holds there.
    #[test]
    fn a_1_9_gb_host_keeps_the_default() {
        let limit = resolve(None, Some(1_900_000_000));
        assert_eq!(limit.ram_bound_bytes, Some(542_387_543));
        assert_eq!(limit.bytes, DEFAULT_STORE_LIMIT_BYTES);
        assert_eq!(limit.source, Source::Default);
        assert!(!limit.exceeds_ram_rule());
        // 500 MB of messages cost 437 MB of index, under the 475 MB share.
        assert_eq!(megabytes(limit.index_bytes()), 437);
    }

    /// A 1 GB host (a Raspberry Pi 3) cannot index a full 500 MB store
    /// in a quarter of its RAM: the default comes down to what it can.
    #[test]
    fn a_1_gb_host_bounds_the_default() {
        let limit = resolve(None, Some(1_000_000_000));
        assert_eq!(limit.bytes, 285_467_128);
        assert_eq!(limit.source, Source::Default);
        assert!(limit.index_bytes() <= 1_000_000_000 / RAM_SHARE_DIVISOR);
        assert!(!limit.exceeds_ram_rule());
    }

    /// A 512 MB host (a Pi Zero 2W) gets a store of about 153 MB.
    #[test]
    fn a_512_mb_host_bounds_the_default_harder() {
        let limit = resolve(None, Some(512 * 1024 * 1024));
        assert_eq!(megabytes(limit.bytes), 153);
        assert!(limit.index_bytes() <= 512 * 1024 * 1024 / RAM_SHARE_DIVISOR);
    }

    /// A 64 GB host gets the default's cap, not a store the size of its
    /// RAM rule.
    #[test]
    fn a_64_gb_host_gets_the_default_cap() {
        let limit = resolve(None, Some(64_000_000_000));
        assert_eq!(limit.bytes, DEFAULT_STORE_LIMIT_BYTES);
        assert_eq!(limit.source, Source::Default);
        assert!(limit.ram_bound_bytes.is_some_and(|b| b > 2_000_000_000));
    }

    /// Without a RAM figure the default stands unbounded.
    #[test]
    fn an_unknown_host_keeps_the_default() {
        let limit = resolve(None, None);
        assert_eq!(limit.bytes, DEFAULT_STORE_LIMIT_BYTES);
        assert_eq!(limit.ram_bound_bytes, None);
        assert!(!limit.exceeds_ram_rule());
    }

    /// A configured 2 GB on miauhaus passes through as written, and is
    /// marked for the warning: its index needs 1751 MB of the 1900.
    #[test]
    fn a_configured_limit_passes_through_with_the_warning() {
        let limit = resolve(Some(2_000_000_000), Some(1_900_000_000));
        assert_eq!(limit.bytes, 2_000_000_000);
        assert_eq!(limit.source, Source::Config);
        assert!(limit.exceeds_ram_rule());
        assert_eq!(megabytes(limit.index_bytes()), 1751);
    }

    /// A configured limit inside the rule passes through without it.
    #[test]
    fn a_configured_limit_inside_the_rule_is_not_warned() {
        let limit = resolve(Some(100_000_000), Some(1_900_000_000));
        assert_eq!(limit.bytes, 100_000_000);
        assert_eq!(limit.source, Source::Config);
        assert!(!limit.exceeds_ram_rule());
    }

    #[test]
    fn mem_total_is_read_in_kib() {
        let meminfo = "MemTotal:        1855332 kB\nMemFree:          123456 kB\n";
        assert_eq!(parse_mem_total(meminfo), Some(1_855_332 * 1024));
        assert_eq!(parse_mem_total("MemFree: 1 kB\n"), None);
        assert_eq!(parse_mem_total("MemTotal: x kB\n"), None);
    }

    #[test]
    fn huge_figures_do_not_overflow() {
        assert!(index_bytes_for(u64::MAX) < u64::MAX);
        assert!(ram_bound_bytes(u64::MAX) > 0);
    }
}

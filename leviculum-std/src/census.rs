//! What every daemon's own census shares (order 1585): the resident set,
//! the bytes behind a hash table's spine, and the one SIGUSR1 dump for a
//! process that holds more than one node.
//!
//! lnpnd's `PN_CENSUS` (order 1515) and loquax-mailbox's `MAILBOX_CENSUS`
//! both price their own tables beside the embedded node's, and both need
//! the same three numbers; they live here so the two lines measure alike.
//! The process-wide sections (allocator, mappings, the `gap:` arithmetic)
//! are [`crate::reticulum::compose_diagnostic_dump`]'s, reached through
//! [`compose_multi_node_dump`](crate::census::compose_multi_node_dump) when a process carries several nodes.

/// `VmRSS` of this process in kB, 0 where `/proc/self/status` cannot say.
pub fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("VmRSS:")?
                    .split_whitespace()
                    .next()?
                    .parse()
                    .ok()
            })
        })
        .unwrap_or(0)
}

/// Heap bytes of a `HashMap` or `HashSet` spine: the table's buckets and
/// their control bytes, at its capacity.
pub fn hash_table_bytes(capacity: usize, entry_size: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    // hashbrown allocates a power of two of buckets at 7/8 load, one
    // control byte a bucket.
    let buckets = (capacity * 8 / 7).next_power_of_two();
    buckets * (entry_size + 1)
}

/// The marker the core dump totals itself with, and that
/// [`crate::reticulum::compose_diagnostic_dump`] reads the tracked bytes from.
const TOTAL_MARK: &str = "=== Total estimated: ";

/// The number on a dump's `=== Total estimated: N bytes ===` line.
pub fn total_estimated(dump: &str) -> Option<u64> {
    let rest = dump.split(TOTAL_MARK).nth(1)?;
    rest.split_whitespace().next()?.parse().ok()
}

/// One SIGUSR1 dump for a process holding several nodes beside tables of
/// its own (loquax-mailbox: its own node and one per hosted group).
///
/// Each node's core dump is printed under a `--- node <name> ---` header
/// with its total renamed `=== Node total estimated: N bytes ===`, then
/// `own` (the daemon's own census section, `own_bytes` its sum), then one
/// `=== Total estimated ===` over all of it, so the composed sections'
/// `gap:` line subtracts everything the process tracks and not only the
/// first node's tables.
pub fn compose_multi_node_dump(nodes: &[(String, String)], own: &str, own_bytes: u64) -> String {
    let mut dump = String::new();
    let mut total = own_bytes;
    for (name, core_dump) in nodes {
        total = total.saturating_add(total_estimated(core_dump).unwrap_or(0));
        dump.push_str(&format!("--- node {name} ---\n"));
        dump.push_str(&core_dump.replace(TOTAL_MARK, "=== Node total estimated: "));
        if !dump.ends_with('\n') {
            dump.push('\n');
        }
    }
    dump.push_str(own);
    if !dump.ends_with('\n') {
        dump.push('\n');
    }
    dump.push_str(&format!("{TOTAL_MARK}{total} bytes ===\n"));
    crate::reticulum::compose_diagnostic_dump(dump)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hash_table_spine_rounds_to_a_power_of_two_of_buckets() {
        assert_eq!(hash_table_bytes(0, 16), 0);
        // 7 entries at 7/8 load want 8 buckets of 16 bytes and a control byte.
        assert_eq!(hash_table_bytes(7, 16), 8 * 17);
        assert_eq!(hash_table_bytes(8, 16), 16 * 17);
    }

    /// Two nodes and the daemon's own section sum to one total, the one
    /// the gap line reads; each node's own total survives under its name.
    #[test]
    fn a_multi_node_dump_totals_every_node_and_the_own_tables() {
        let a = "known_identities: 3\n=== Total estimated: 1000 bytes ===\n".to_string();
        let b = "known_identities: 1\n=== Total estimated: 200 bytes ===\n".to_string();
        let dump = compose_multi_node_dump(
            &[("own".into(), a), ("group 1".into(), b)],
            "MAILBOX_CENSUS outbox=5 tables=5\n",
            5,
        );
        assert!(dump.contains("--- node own ---"), "{dump}");
        assert!(dump.contains("--- node group 1 ---"), "{dump}");
        assert!(
            dump.contains("=== Node total estimated: 1000 bytes ==="),
            "{dump}"
        );
        assert_eq!(total_estimated(&dump), Some(1205), "{dump}");
        assert!(dump.contains("MAILBOX_CENSUS outbox=5"), "{dump}");
    }
}

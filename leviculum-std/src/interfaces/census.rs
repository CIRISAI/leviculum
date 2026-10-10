//! The interface half of the SIGUSR1 census (597).
//!
//! The core census prices the node's tables; an interface task owns tables
//! of its own the core never sees (a BLE interface's link table, its
//! per-link packet queues and reassembly buffers, its dial ledgers). A soak
//! that cannot see them measures the wrong half of the daemon.
//!
//! Interfaces have no trait the driver could call into: each one is a task
//! that owns its state without a lock. So the hook is a published snapshot,
//! the way an RNode publishes its `RadioStats`: the task builds an
//! [`InterfaceCensus`] from its own state at its own cadence and stores it on
//! its [`InterfaceCounters`](super::InterfaceCounters); the dump reads the
//! latest one. Nothing here knows any medium: a row is a name, a count and
//! two byte figures, rendered in the shape the core's rows use.

use std::fmt::Write as _;
use std::mem::size_of;

/// One owned collection of an interface: the census line it prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CensusRow {
    /// Stable row name, the token a soak assertion names.
    pub name: &'static str,
    /// Entries the collection holds.
    pub entries: u64,
    /// Bytes of the entries themselves.
    pub raw: u64,
    /// Bytes the collection pins, overhead and spare capacity included.
    /// This is what the total adds.
    pub estimated: u64,
    /// How `estimated` was derived, printed in the line's parentheses.
    pub model: &'static str,
}

/// What one interface task owns, as of its last snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct InterfaceCensus {
    /// The counted rows; [`Self::total`] sums their `estimated`.
    pub rows: Vec<CensusRow>,
    /// Per-item detail lines (one per link, say), printed under the rows
    /// and NOT added: every byte they name is already in a row.
    pub detail: Vec<String>,
}

impl InterfaceCensus {
    /// The bytes this interface adds to the dump's estimated total.
    pub(crate) fn total(&self) -> u64 {
        self.rows.iter().map(|r| r.estimated).sum()
    }

    /// The `--- <kind>/<iface> ---` section.
    pub(crate) fn render(&self, kind: &str, iface: &str) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "--- {kind}/{iface} ---");
        for r in &self.rows {
            let _ = writeln!(
                s,
                "{}: {} entries, raw {} bytes, estimated {} bytes ({})",
                r.name, r.entries, r.raw, r.estimated, r.model
            );
        }
        for line in &self.detail {
            let _ = writeln!(s, "  {line}");
        }
        let _ = writeln!(s, "{kind}/{iface} total: estimated {} bytes", self.total());
        s
    }
}

/// One interface's census as the dump prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CensusSection {
    /// The medium's lowercase name (`InterfaceKind::as_str`), the section's
    /// first half.
    pub kind: &'static str,
    /// The interface's configured name, the second half.
    pub iface: String,
    pub census: InterfaceCensus,
}

/// Bytes a `HashMap<K, V>` pins: every slot its capacity holds, plus the
/// control byte per slot, the `hashbrown` layout. Capacity and not length,
/// because a map never shrinks: the memory a soak has to see is what the
/// busiest moment grew it to.
pub(crate) fn hash_map_bytes<K, V>(map: &std::collections::HashMap<K, V>) -> u64 {
    (map.capacity() * (size_of::<(K, V)>() + 1)) as u64
}

/// The row for a `HashMap<K, V>` of fixed-size entries.
pub(crate) fn hash_map_row<K, V>(
    name: &'static str,
    map: &std::collections::HashMap<K, V>,
) -> CensusRow {
    CensusRow {
        name,
        entries: map.len() as u64,
        raw: (map.len() * size_of::<(K, V)>()) as u64,
        estimated: hash_map_bytes(map),
        model: "HashMap capacity x (entry + ctrl)",
    }
}

/// Splice the interface sections into a core dump: before its
/// `=== Total estimated: N bytes ===` line, with N raised by their sum, so
/// the one total every reader parses (the gap line's `tracked`, periculum's
/// `lnsd_estimated_bytes`) counts them. A dump without that line gets the
/// sections appended and is otherwise left alone.
pub(crate) fn splice_into_dump(core_dump: String, sections: &[CensusSection]) -> String {
    const TOTAL: &str = "=== Total estimated: ";
    if sections.is_empty() {
        return core_dump;
    }
    let mut body = String::new();
    let mut added = 0u64;
    for section in sections {
        body.push_str(&section.census.render(section.kind, &section.iface));
        added += section.census.total();
    }
    let Some(at) = core_dump.find(TOTAL) else {
        let mut out = core_dump;
        out.push_str(&body);
        return out;
    };
    let (head, tail) = core_dump.split_at(at);
    let rest = &tail[TOTAL.len()..];
    let digits = rest.split_whitespace().next().unwrap_or("");
    let Ok(core_total) = digits.parse::<u64>() else {
        return format!("{head}{body}{tail}");
    };
    let after = &rest[digits.len()..];
    let mut out = String::with_capacity(core_dump.len() + body.len() + 64);
    out.push_str(head);
    out.push_str(&body);
    let _ = write!(out, "=== Interfaces estimated: {added} bytes ===\n{TOTAL}");
    let _ = write!(out, "{}", core_total + added);
    out.push_str(after);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake() -> InterfaceCensus {
        InterfaceCensus {
            rows: vec![
                CensusRow {
                    name: "links_central",
                    entries: 1,
                    raw: 100,
                    estimated: 100,
                    model: "Vec 1x",
                },
                CensusRow {
                    name: "pacer_queues",
                    entries: 2,
                    raw: 30,
                    estimated: 40,
                    model: "mpsc 1x",
                },
            ],
            detail: vec!["link aa:bb role=central".into()],
        }
    }

    fn section(iface: &str) -> CensusSection {
        CensusSection {
            kind: "ble",
            iface: iface.to_string(),
            census: fake(),
        }
    }

    /// 597: a fake interface with known contents prints its rows under its
    /// section, and the core total grows by exactly their sum.
    #[test]
    fn interface_sections_join_the_estimated_total() {
        let core =
            "=== Memory Diagnostic ===\nfoo: 1 entries, raw 8 bytes, estimated 8 bytes (x)\n\
                    === Total estimated: 1000 bytes ===\n"
                .to_string();
        let out = splice_into_dump(core, &[section("BLE0")]);
        assert_eq!(
            out,
            "=== Memory Diagnostic ===\nfoo: 1 entries, raw 8 bytes, estimated 8 bytes (x)\n\
             --- ble/BLE0 ---\n\
             links_central: 1 entries, raw 100 bytes, estimated 100 bytes (Vec 1x)\n\
             pacer_queues: 2 entries, raw 30 bytes, estimated 40 bytes (mpsc 1x)\n  \
             link aa:bb role=central\n\
             ble/BLE0 total: estimated 140 bytes\n\
             === Interfaces estimated: 140 bytes ===\n\
             === Total estimated: 1140 bytes ===\n"
        );
    }

    #[test]
    fn no_interface_census_leaves_the_dump_unchanged() {
        let core = "=== Total estimated: 7 bytes ===\n".to_string();
        assert_eq!(splice_into_dump(core.clone(), &[]), core);
    }

    #[test]
    fn a_dump_without_a_total_gets_the_sections_appended() {
        let out = splice_into_dump("x\n".into(), &[section("B")]);
        assert!(out.starts_with("x\n--- ble/B ---\n"), "{out}");
        assert!(out.ends_with("ble/B total: estimated 140 bytes\n"), "{out}");
    }

    #[test]
    fn a_hash_map_is_priced_by_its_capacity() {
        let mut m: std::collections::HashMap<[u8; 6], u64> = std::collections::HashMap::new();
        assert_eq!(hash_map_row("m", &m).estimated, 0);
        m.insert([1; 6], 2);
        let row = hash_map_row("m", &m);
        assert_eq!(row.entries, 1);
        assert_eq!(row.raw, size_of::<([u8; 6], u64)>() as u64);
        assert_eq!(
            row.estimated,
            (m.capacity() * (size_of::<([u8; 6], u64)>() + 1)) as u64
        );
    }
}

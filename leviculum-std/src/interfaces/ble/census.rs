//! The BLE interface's census of its own tables (597), published through
//! [`crate::interfaces::census`].
//!
//! Everything here is read off the orchestrator's state at its one-second
//! tick, so a dump sees the interface as it stood at most a second earlier.
//! The rows, and the arithmetic behind each:
//!
//! | row | entries | estimated bytes |
//! |---|---|---|
//! | `links_central`, `links_peripheral` | live links in that role | `n * size_of::<Link>()` |
//! | `link_slots_spare` | allocated link slots not in use | `spare * size_of::<Link>()` |
//! | `pending_handshakes` | peripheral connections not yet identified | the `Vec`'s slots |
//! | `abandon_reports` | reports the driver has not taken | the `Vec`'s slots |
//! | `defrag_partial` | links with a reassembly in progress | [`BleDefragmenter::held_bytes`] summed |
//! | `parked_tails` | links holding an out-of-order tail | the tails' capacity |
//! | `pacer_queues` | packets queued on central links | [`unit_bytes`] summed |
//! | `central_pipes` | central links' queue handles | `HashMap` capacity |
//! | `notifiers` | subscribed centrals' notify handles | the `Vec`'s slots |
//! | `dial_queue` | dials waiting to start | the `Vec`'s slots |
//! | `backoff_until` | addresses held from dialling | `HashMap` capacity |
//! | `dial_backoff` | addresses with a failure run | the fixed table |
//! | `scan_line_decided`, `_dropped`, `_backed_off` | addresses ever logged | `HashMap` capacity |
//! | `churn_ledger` | identities remembered | the fixed table |
//! | `link_addr_types` | handshaked addresses' types | `HashMap` capacity |
//!
//! The peripheral notify pipe has no queue: a notification is awaited at the
//! send, so its pacer holds no packet. Its links show `pacer=notify` in the
//! detail lines.
//!
//! [`BleDefragmenter::held_bytes`]: leviculum_core::framing::ble::BleDefragmenter::held_bytes

use std::collections::HashMap;
use std::mem::size_of;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bluer::gatt::local::CharacteristicNotifier;
use leviculum_ble_tx::{AddrType, DialBackoff};
use tokio::sync::mpsc;

use super::links::{Addr, ChurnGate, DialQueue, Link, LinkTable, Role, ScanLineGate};
use super::CentralPipe;
use crate::interfaces::census::{hash_map_row, CensusRow, InterfaceCensus};

/// Bytes one queued packet pins: its fragment list's slots plus every
/// fragment's buffer.
pub(crate) fn unit_bytes(unit: &[Vec<u8>]) -> usize {
    size_of_val(unit) + unit.iter().map(Vec::capacity).sum::<usize>()
}

/// What a central link's packet queue holds right now. The orchestrator
/// adds before it sends and the link task takes off after it receives, so
/// the two never meet in the middle of one packet.
#[derive(Debug, Default)]
pub(crate) struct QueueGauge {
    units: AtomicUsize,
    bytes: AtomicUsize,
}

impl QueueGauge {
    fn add(&self, bytes: usize) {
        self.units.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    fn take(&self, bytes: usize) {
        self.units.fetch_sub(1, Ordering::Relaxed);
        self.bytes.fetch_sub(bytes, Ordering::Relaxed);
    }

    /// `(packets queued, bytes they pin)`.
    pub(crate) fn read(&self) -> (usize, usize) {
        (
            self.units.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
        )
    }
}

/// The orchestrator's end of one central link's packet queue: one message
/// is one packet's fragments (#376).
#[derive(Debug, Clone)]
pub(crate) struct PacketTx {
    tx: mpsc::Sender<Vec<Vec<u8>>>,
    gauge: Arc<QueueGauge>,
}

impl PacketTx {
    /// Queue one packet; `false` when the queue is full or the link task
    /// is gone, and nothing was queued.
    pub(crate) fn try_send(&self, unit: Vec<Vec<u8>>) -> bool {
        let bytes = unit_bytes(&unit);
        self.gauge.add(bytes);
        if self.tx.try_send(unit).is_ok() {
            true
        } else {
            self.gauge.take(bytes);
            false
        }
    }

    pub(crate) fn gauge(&self) -> &QueueGauge {
        &self.gauge
    }
}

/// The link task's end of the queue.
#[derive(Debug)]
pub(crate) struct PacketRx {
    rx: mpsc::Receiver<Vec<Vec<u8>>>,
    gauge: Arc<QueueGauge>,
}

impl PacketRx {
    /// The next queued packet, `None` once the orchestrator dropped its end.
    pub(crate) async fn recv(&mut self) -> Option<Vec<Vec<u8>>> {
        let unit = self.rx.recv().await?;
        self.gauge.take(unit_bytes(&unit));
        Some(unit)
    }
}

/// A gauged packet queue of `depth` packets.
pub(crate) fn packet_queue(depth: usize) -> (PacketTx, PacketRx) {
    let (tx, rx) = mpsc::channel(depth);
    let gauge = Arc::new(QueueGauge::default());
    (
        PacketTx {
            tx,
            gauge: Arc::clone(&gauge),
        },
        PacketRx { rx, gauge },
    )
}

/// Everything the BLE orchestrator owns, borrowed for one census.
pub(super) struct BleState<'a, const N: usize> {
    pub table: &'a LinkTable,
    pub central_pipes: &'a HashMap<Addr, CentralPipe>,
    pub notifiers: &'a Vec<CharacteristicNotifier>,
    pub dial_queue: &'a DialQueue,
    pub backoff_until: &'a HashMap<Addr, u64>,
    pub dial_backoff: &'a DialBackoff<N>,
    pub scan_lines: &'a ScanLineGate,
    pub churn: &'a ChurnGate,
    pub link_addr_types: &'a HashMap<Addr, AddrType>,
}

fn row(
    name: &'static str,
    entries: usize,
    raw: usize,
    estimated: usize,
    model: &'static str,
) -> CensusRow {
    CensusRow {
        name,
        entries: entries as u64,
        raw: raw as u64,
        estimated: estimated as u64,
        model,
    }
}

impl<const N: usize> BleState<'_, N> {
    /// The census, rows in the order of the module table.
    pub(crate) fn census(&self) -> InterfaceCensus {
        let link_size = size_of::<Link>();
        let mut rows = Vec::new();
        let mut detail = Vec::new();

        let (mut central, mut peripheral) = (0usize, 0usize);
        let (mut defrag_links, mut defrag_bytes) = (0usize, 0usize);
        let (mut tail_links, mut tail_bytes) = (0usize, 0usize);
        for l in self.table.link_census() {
            match l.role {
                Role::Central => central += 1,
                Role::Peripheral => peripheral += 1,
            }
            if l.defrag_fragments > 0 {
                defrag_links += 1;
                defrag_bytes += l.defrag_bytes;
            }
            if l.parked_tail_bytes > 0 {
                tail_links += 1;
                tail_bytes += l.parked_tail_bytes;
            }
            let pacer = match (l.role, self.central_pipes.get(&l.addr)) {
                (Role::Central, Some(pipe)) => {
                    let (depth, bytes) = pipe.frames.gauge().read();
                    format!("pacer_depth={depth} pacer_bytes={bytes}")
                }
                (Role::Central, None) => "pacer=none".to_string(),
                (Role::Peripheral, _) => "pacer=notify".to_string(),
            };
            detail.push(format!(
                "link {} role={} peer={} {pacer} defrag_fragments={} defrag_bytes={} parked_tail_bytes={}",
                bluer::Address(l.addr),
                l.role.as_str(),
                crate::storage::hex_encode(&l.identity[..4]),
                l.defrag_fragments,
                l.defrag_bytes,
                l.parked_tail_bytes,
            ));
        }
        let spare = self.table.link_slots().saturating_sub(central + peripheral);
        rows.push(row(
            "links_central",
            central,
            central * link_size,
            central * link_size,
            "size_of Link",
        ));
        rows.push(row(
            "links_peripheral",
            peripheral,
            peripheral * link_size,
            peripheral * link_size,
            "size_of Link",
        ));
        rows.push(row(
            "link_slots_spare",
            spare,
            0,
            spare * link_size,
            "Vec spare slots",
        ));
        let (n, bytes) = self.table.pending_census();
        rows.push(row("pending_handshakes", n, bytes, bytes, "Vec slots"));
        let (n, bytes) = self.table.abandon_report_census();
        rows.push(row("abandon_reports", n, bytes, bytes, "Vec slots"));
        rows.push(row(
            "defrag_partial",
            defrag_links,
            defrag_bytes,
            defrag_bytes,
            "BleDefragmenter held_bytes",
        ));
        rows.push(row(
            "parked_tails",
            tail_links,
            tail_bytes,
            tail_bytes,
            "Vec capacity",
        ));

        let (mut queued, mut queued_bytes) = (0usize, 0usize);
        for pipe in self.central_pipes.values() {
            let (depth, bytes) = pipe.frames.gauge().read();
            queued += depth;
            queued_bytes += bytes;
        }
        rows.push(row(
            "pacer_queues",
            queued,
            queued_bytes,
            queued_bytes,
            "queued packets 1x",
        ));
        rows.push(hash_map_row("central_pipes", self.central_pipes));
        let notifier_size = size_of::<CharacteristicNotifier>();
        rows.push(row(
            "notifiers",
            self.notifiers.len(),
            self.notifiers.len() * notifier_size,
            self.notifiers.capacity() * notifier_size,
            "Vec slots",
        ));
        let (n, bytes) = self.dial_queue.census();
        rows.push(row("dial_queue", n, bytes, bytes, "Vec slots"));
        rows.push(hash_map_row("backoff_until", self.backoff_until));
        let fixed = size_of::<DialBackoff<N>>();
        rows.push(row(
            "dial_backoff",
            self.dial_backoff.occupied(),
            fixed,
            fixed,
            "fixed table",
        ));
        rows.extend(self.scan_lines.census_rows());
        let fixed = size_of::<ChurnGate>();
        rows.push(row(
            "churn_ledger",
            self.churn.remembered(),
            fixed,
            fixed,
            "fixed table",
        ));
        rows.push(hash_map_row("link_addr_types", self.link_addr_types));
        InterfaceCensus { rows, detail }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_ble_tx::DialFailure;
    use leviculum_core::framing::ble::fragment_packet;

    const OWN: [u8; 16] = [0x0E; 16];
    const PEER_C: [u8; 16] = [0xA1; 16];
    const PEER_P: [u8; 16] = [0xB2; 16];
    const ADDR_C: Addr = [0xC0, 0, 0, 0, 0, 0x01];
    const ADDR_P: Addr = [0xC0, 0, 0, 0, 0, 0x02];

    fn find<'a>(census: &'a InterfaceCensus, name: &str) -> &'a CensusRow {
        census
            .rows
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("no row {name}: {census:?}"))
    }

    /// 597: an interface with known contents (one link per role, a half
    /// reassembled packet on the central link, one packet queued behind
    /// it, ledger entries) produces the rows those contents price, and
    /// the census total is their sum.
    #[test]
    fn the_census_prices_what_the_orchestrator_holds() {
        let mut table = LinkTable::new(OWN, 4);
        table.admit(PEER_C, ADDR_C, Role::Central, 185, 0);
        table.admit(PEER_P, ADDR_P, Role::Peripheral, 185, 0);
        // The first fragment of a two-fragment packet: a reassembly in
        // progress on the central link.
        let frags = fragment_packet(&[0x55; 300], 185);
        assert_eq!(frags.len(), 2);
        table.central_frame(ADDR_C, &frags[0], 1);

        let (tx, _rx) = packet_queue(4);
        let unit = fragment_packet(&[0x66; 300], 185);
        let queued = unit_bytes(&unit);
        assert!(tx.try_send(unit));
        let mut central_pipes = HashMap::new();
        central_pipes.insert(ADDR_C, CentralPipe { frames: tx });

        let mut backoff_until = HashMap::new();
        backoff_until.insert([1u8; 6], 10u64);
        let mut dial_backoff: DialBackoff<8> = DialBackoff::new();
        dial_backoff.note_failure(7, DialFailure::NotColumba, 0);
        let mut scan_lines = ScanLineGate::default();
        scan_lines.backed_off([2u8; 6], 10);
        scan_lines.backed_off([3u8; 6], 10);
        let notifiers = Vec::new();
        let dial_queue = DialQueue::new(1);
        let churn = ChurnGate::new(crate::interfaces::ble::links::CHURN_POLICY);
        let link_addr_types = HashMap::new();

        let census = BleState {
            table: &table,
            central_pipes: &central_pipes,
            notifiers: &notifiers,
            dial_queue: &dial_queue,
            backoff_until: &backoff_until,
            dial_backoff: &dial_backoff,
            scan_lines: &scan_lines,
            churn: &churn,
            link_addr_types: &link_addr_types,
        }
        .census();

        let link = size_of::<Link>() as u64;
        let c = find(&census, "links_central");
        assert_eq!((c.entries, c.estimated), (1, link));
        let p = find(&census, "links_peripheral");
        assert_eq!((p.entries, p.estimated), (1, link));
        let d = find(&census, "defrag_partial");
        assert_eq!(d.entries, 1);
        assert!(d.estimated >= frags[0].len() as u64 - 5, "{d:?}");
        let q = find(&census, "pacer_queues");
        assert_eq!((q.entries, q.estimated), (1, queued as u64));
        let b = find(&census, "backoff_until");
        assert_eq!(b.entries, 1);
        assert_eq!(find(&census, "dial_backoff").entries, 1);
        assert_eq!(find(&census, "scan_line_backed_off").entries, 2);
        assert_eq!(find(&census, "scan_line_decided").entries, 0);
        assert_eq!(find(&census, "churn_ledger").entries, 0);

        let sum: u64 = census.rows.iter().map(|r| r.estimated).sum();
        assert_eq!(census.total(), sum);
        let section = census.render("ble", "BLE0");
        assert!(section.starts_with("--- ble/BLE0 ---\n"), "{section}");
        assert!(
            section.contains(&format!(
                "pacer_queues: 1 entries, raw {queued} bytes, estimated {queued} bytes (queued packets 1x)\n"
            )),
            "{section}"
        );
        assert!(
            section.contains("role=central peer=a1a1a1a1 pacer_depth=1 pacer_bytes="),
            "{section}"
        );
        assert!(
            section.contains("role=peripheral peer=b2b2b2b2 pacer=notify"),
            "{section}"
        );
        assert!(
            section.ends_with(&format!("ble/BLE0 total: estimated {sum} bytes\n")),
            "{section}"
        );
    }

    /// The gauge follows the queue: a received packet leaves it, a refused
    /// one never entered it.
    #[tokio::test]
    async fn the_queue_gauge_counts_what_the_link_task_has_not_taken() {
        let (tx, mut rx) = packet_queue(1);
        let unit = vec![vec![0u8; 20], vec![0u8; 10]];
        let bytes = unit_bytes(&unit);
        assert!(tx.try_send(unit.clone()));
        assert!(!tx.try_send(unit), "depth 1 refuses the second packet");
        assert_eq!(tx.gauge().read(), (1, bytes));
        assert!(rx.recv().await.is_some());
        assert_eq!(tx.gauge().read(), (0, 0));
    }
}

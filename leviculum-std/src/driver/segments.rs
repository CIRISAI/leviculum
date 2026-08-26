//! Multi-segment resource assembly — leviculum#62.
//!
//! A transfer larger than [`RESOURCE_MAX_EFFICIENT_SIZE`] is split by the
//! sender into segments, each separately advertised, and core delivers one
//! `ResourceCompleted` per segment carrying that segment's slice. That is the
//! right primitive for `leviculum-core`, which is `no_std` and runs on boards
//! where holding a whole multi-megabyte transfer in RAM is not an option.
//!
//! It is the wrong shape for a consumer. The reference implementation fires
//! its resource callback **once per transfer**, at the final segment
//! (`Resource.py:725/738/1130`), having spooled earlier segments to disk — so
//! anyone writing to reference semantics decodes our *first* event as the
//! whole message and gets a body cut at `RESOURCE_MAX_EFFICIENT_SIZE` minus
//! the metadata size. That cost a downstream consumer 765 messages, surfacing
//! as decode errors blamed on the sender (leviculum#61).
//!
//! So `leviculum-std` — where consumers live and an allocator already exists —
//! reassembles: intermediate segments are absorbed, and the final segment is
//! replaced by one `ResourceCompleted` carrying the whole payload and the
//! metadata from segment 1. Single-segment transfers, and every sender-side
//! event, pass through untouched at zero cost.
//!
//! **Bounded, because the segment count is peer-supplied.** A peer that
//! advertises a thousand segments must not make us hold a gigabyte. Two
//! ceilings apply, both configurable
//! ([`ReticulumNodeBuilder::max_assembled_resource_size`]):
//! per transfer, and in aggregate across links. A transfer that cannot be
//! assembled within them is **not dropped and not truncated** — it degrades
//! to the documented per-segment delivery with a loud error naming the
//! ceiling, so data is never lost, only the convenience.

use std::collections::HashMap;

use leviculum_core::link::LinkId;
use leviculum_core::node::NodeEvent;
use leviculum_core::resource::RESOURCE_MAX_EFFICIENT_SIZE;

/// Default per-transfer assembly ceiling: 64 MiB. Comfortably above the
/// payloads a mesh consumer sends today (replication frames run ~2 MiB) while
/// staying a size a server can hold without thinking about it.
pub const DEFAULT_MAX_ASSEMBLED_RESOURCE_SIZE: usize = 64 * 1024 * 1024;

/// Aggregate ceiling as a multiple of the per-transfer one: several links may
/// each be mid-transfer, and the sum is what actually bounds memory.
const AGGREGATE_MULTIPLE: usize = 4;

enum Partial {
    /// Accumulating segments for one transfer on this link.
    Assembling {
        data: Vec<u8>,
        metadata: Option<Vec<u8>>,
        total_segments: u32,
        next_expected: u32,
    },
    /// This transfer exceeds a ceiling (or arrived out of order): its
    /// remaining segments are forwarded as-is, exactly as before #62.
    PassThrough { remaining: u32 },
}

/// Per-link assembly state. A link carries at most one incoming resource at a
/// time (`Link::incoming_resource` is a single slot), so `LinkId` is an exact
/// correlation key and no `original_hash` plumbing is needed.
pub(crate) struct SegmentAssembler {
    partials: HashMap<LinkId, Partial>,
    buffered_total: usize,
    per_transfer_cap: usize,
}

impl SegmentAssembler {
    pub(crate) fn new(per_transfer_cap: usize) -> Self {
        Self {
            partials: HashMap::new(),
            buffered_total: 0,
            per_transfer_cap,
        }
    }

    fn aggregate_cap(&self) -> usize {
        self.per_transfer_cap.saturating_mul(AGGREGATE_MULTIPLE)
    }

    /// Currently buffered bytes across all in-flight assemblies.
    #[cfg(test)]
    pub(crate) fn buffered(&self) -> usize {
        self.buffered_total
    }

    /// Rewrite one batch of events: absorb intermediate receiver-side
    /// segments, and replace a final segment with the assembled whole.
    pub(crate) fn process(&mut self, events: Vec<NodeEvent>) -> Vec<NodeEvent> {
        // Fast path: nothing segmented in this batch (the overwhelming
        // majority), so no allocation and no per-event work beyond the match.
        if !events.iter().any(Self::is_segmented_receiver_completion) {
            for ev in &events {
                if let NodeEvent::LinkClosed { link_id, .. } = ev {
                    self.forget(link_id);
                }
            }
            return events;
        }

        let mut out = Vec::with_capacity(events.len());
        for ev in events {
            match ev {
                NodeEvent::LinkClosed { .. } => {
                    if let NodeEvent::LinkClosed { link_id, .. } = &ev {
                        self.forget(link_id);
                    }
                    out.push(ev);
                }
                NodeEvent::ResourceCompleted {
                    is_sender: false,
                    total_segments,
                    ..
                } if total_segments > 1 => {
                    if let Some(assembled) = self.absorb(ev) {
                        out.push(assembled);
                    }
                }
                other => out.push(other),
            }
        }
        out
    }

    fn is_segmented_receiver_completion(ev: &NodeEvent) -> bool {
        matches!(
            ev,
            NodeEvent::ResourceCompleted {
                is_sender: false,
                total_segments,
                ..
            } if *total_segments > 1
        )
    }

    fn forget(&mut self, link_id: &LinkId) {
        if let Some(Partial::Assembling { data, .. }) = self.partials.remove(link_id) {
            self.buffered_total = self.buffered_total.saturating_sub(data.len());
        }
    }

    /// Absorb one segment. Returns `Some` for an event the consumer should
    /// see — the assembled whole, or a passed-through segment — and `None`
    /// when the segment was buffered.
    fn absorb(&mut self, ev: NodeEvent) -> Option<NodeEvent> {
        let NodeEvent::ResourceCompleted {
            link_id,
            resource_hash,
            data,
            metadata,
            segment_index,
            total_segments,
            ..
        } = ev
        else {
            return Some(ev);
        };

        // First segment: decide whether this transfer can be assembled.
        if segment_index == 1 {
            let projected = (total_segments as usize).saturating_mul(RESOURCE_MAX_EFFICIENT_SIZE);
            let over_transfer = projected > self.per_transfer_cap;
            let over_aggregate =
                self.buffered_total.saturating_add(projected) > self.aggregate_cap();
            if over_transfer || over_aggregate {
                tracing::error!(
                    event = "RESOURCE_ASSEMBLY_DECLINED",
                    segments = total_segments,
                    projected_bytes = projected,
                    per_transfer_cap = self.per_transfer_cap,
                    aggregate_buffered = self.buffered_total,
                    "a {total_segments}-segment transfer (up to {projected} bytes) exceeds the \
                     assembly ceiling; delivering it per segment instead. Concatenate \
                     ResourceCompleted.data in segment_index order, or raise \
                     ReticulumNodeBuilder::max_assembled_resource_size",
                );
                self.partials.insert(
                    link_id,
                    Partial::PassThrough {
                        remaining: total_segments.saturating_sub(1),
                    },
                );
                return Some(NodeEvent::ResourceCompleted {
                    link_id,
                    resource_hash,
                    data,
                    metadata,
                    is_sender: false,
                    segment_index,
                    total_segments,
                });
            }
            self.buffered_total = self.buffered_total.saturating_add(data.len());
            self.partials.insert(
                link_id,
                Partial::Assembling {
                    data,
                    metadata,
                    total_segments,
                    next_expected: 2,
                },
            );
            return None;
        }

        match self.partials.remove(&link_id) {
            Some(Partial::PassThrough { remaining }) => {
                if remaining > 1 {
                    self.partials.insert(
                        link_id,
                        Partial::PassThrough {
                            remaining: remaining - 1,
                        },
                    );
                }
                Some(NodeEvent::ResourceCompleted {
                    link_id,
                    resource_hash,
                    data,
                    metadata,
                    is_sender: false,
                    segment_index,
                    total_segments,
                })
            }
            Some(Partial::Assembling {
                data: mut buffered,
                metadata: first_metadata,
                total_segments: expected_total,
                next_expected,
            }) => {
                if segment_index != next_expected || total_segments != expected_total {
                    // Core delivers segments in order; anything else means a
                    // transfer we cannot reason about. Do not guess — hand the
                    // consumer what arrived and say so.
                    tracing::warn!(
                        event = "RESOURCE_ASSEMBLY_OUT_OF_ORDER",
                        expected = next_expected,
                        got = segment_index,
                        "segment arrived out of order; delivering this transfer per segment"
                    );
                    self.buffered_total = self.buffered_total.saturating_sub(buffered.len());
                    return Some(NodeEvent::ResourceCompleted {
                        link_id,
                        resource_hash,
                        data,
                        metadata,
                        is_sender: false,
                        segment_index,
                        total_segments,
                    });
                }
                self.buffered_total = self.buffered_total.saturating_add(data.len());
                buffered.extend_from_slice(&data);

                if segment_index == expected_total {
                    // The transfer is whole: one event, the reference's shape.
                    self.buffered_total = self.buffered_total.saturating_sub(buffered.len());
                    Some(NodeEvent::ResourceCompleted {
                        link_id,
                        resource_hash,
                        data: buffered,
                        metadata: first_metadata,
                        is_sender: false,
                        segment_index,
                        total_segments,
                    })
                } else {
                    self.partials.insert(
                        link_id,
                        Partial::Assembling {
                            data: buffered,
                            metadata: first_metadata,
                            total_segments: expected_total,
                            next_expected: segment_index + 1,
                        },
                    );
                    None
                }
            }
            None => {
                // A later segment with no first: the transfer began before
                // this node started assembling, or segment 1 was declined.
                Some(NodeEvent::ResourceCompleted {
                    link_id,
                    resource_hash,
                    data,
                    metadata,
                    is_sender: false,
                    segment_index,
                    total_segments,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_core::link::LinkCloseReason;

    fn link(n: u8) -> LinkId {
        LinkId::new([n; 16])
    }

    fn seg(link_id: LinkId, idx: u32, total: u32, len: usize) -> NodeEvent {
        NodeEvent::ResourceCompleted {
            link_id,
            resource_hash: [idx as u8; 32],
            data: vec![idx as u8; len],
            metadata: (idx == 1).then(|| b"meta".to_vec()),
            is_sender: false,
            segment_index: idx,
            total_segments: total,
        }
    }

    fn data_of(ev: &NodeEvent) -> &[u8] {
        match ev {
            NodeEvent::ResourceCompleted { data, .. } => data,
            _ => panic!("not a completion"),
        }
    }

    #[test]
    fn segments_are_assembled_into_one_completion_with_segment_one_metadata() {
        let mut a = SegmentAssembler::new(DEFAULT_MAX_ASSEMBLED_RESOURCE_SIZE);
        assert!(
            a.process(vec![seg(link(1), 1, 3, 10)]).is_empty(),
            "absorbed"
        );
        assert!(
            a.process(vec![seg(link(1), 2, 3, 10)]).is_empty(),
            "absorbed"
        );
        let out = a.process(vec![seg(link(1), 3, 3, 5)]);
        assert_eq!(out.len(), 1, "one delivery per transfer");
        assert_eq!(data_of(&out[0]).len(), 25, "whole payload");
        match &out[0] {
            NodeEvent::ResourceCompleted {
                metadata,
                segment_index,
                total_segments,
                ..
            } => {
                assert_eq!(
                    metadata.as_deref(),
                    Some(&b"meta"[..]),
                    "segment 1's metadata"
                );
                assert_eq!((*segment_index, *total_segments), (3, 3));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(a.buffered(), 0, "buffer released on delivery");
    }

    /// Single-segment transfers and sender-side events are untouched.
    #[test]
    fn unsegmented_traffic_passes_through() {
        let mut a = SegmentAssembler::new(DEFAULT_MAX_ASSEMBLED_RESOURCE_SIZE);
        let out = a.process(vec![seg(link(2), 1, 1, 7)]);
        assert_eq!(out.len(), 1);
        assert_eq!(data_of(&out[0]).len(), 7);
        assert_eq!(a.buffered(), 0);
    }

    /// The segment count is peer-supplied, so a transfer that would exceed the
    /// ceiling must not be buffered — and must not be lost either: it degrades
    /// to per-segment delivery.
    #[test]
    fn a_transfer_past_the_ceiling_degrades_to_per_segment_delivery() {
        // Ceiling of one segment: a 3-segment transfer cannot be assembled.
        let mut a = SegmentAssembler::new(RESOURCE_MAX_EFFICIENT_SIZE);
        let first = a.process(vec![seg(link(3), 1, 3, 10)]);
        assert_eq!(first.len(), 1, "segment 1 delivered as-is");
        assert_eq!(a.buffered(), 0, "nothing buffered for a declined transfer");
        let second = a.process(vec![seg(link(3), 2, 3, 10)]);
        assert_eq!(second.len(), 1, "segment 2 delivered as-is");
        let third = a.process(vec![seg(link(3), 3, 3, 10)]);
        assert_eq!(third.len(), 1, "segment 3 delivered as-is");
        // Every byte still reached the consumer, just in three pieces.
        let total: usize = [first, second, third]
            .iter()
            .flatten()
            .map(|e| data_of(e).len())
            .sum();
        assert_eq!(total, 30);
    }

    /// A link that dies mid-transfer must not leak its partial buffer.
    #[test]
    fn a_closed_link_releases_its_partial() {
        let mut a = SegmentAssembler::new(DEFAULT_MAX_ASSEMBLED_RESOURCE_SIZE);
        a.process(vec![seg(link(4), 1, 4, 1000)]);
        assert_eq!(a.buffered(), 1000);
        let out = a.process(vec![NodeEvent::LinkClosed {
            link_id: link(4),
            reason: LinkCloseReason::Timeout,
            is_initiator: false,
            destination_hash: leviculum_core::DestinationHash::new([9u8; 16]),
        }]);
        assert_eq!(out.len(), 1, "the close itself still reaches the consumer");
        assert_eq!(a.buffered(), 0, "partial released with the link");
    }
}

//! What a transport node forwards for other nodes' links, metered and capped
//! (+ciris, leviculum#93).
//!
//! A relay deciding whether it can take another forwarded link (CIRIS: usable
//! = min(advertised, measured) - committed - reserve) needs what it is
//! actually forwarding, per link, and a way to say no or to cap. This keeps,
//! for every link this node relays, bytes and packets each way and when, and
//! applies the host's admission decision to each forwarded link request:
//! refuse it (the request is not forwarded, so the link never establishes),
//! or admit it with optional byte and packet rate caps (link packets over a
//! cap are dropped, and counted).
//!
//! No admission hook means every request is admitted uncapped, as before.
//! The hook runs inside packet processing with the node's lock held: it must
//! decide from what it is given and its own state, and never call back into
//! the node.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::constants::TRUNCATED_HASHBYTES;

type Hash = [u8; TRUNCATED_HASHBYTES];

/// A link request this node is about to forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForwardRequest {
    /// The link the request opens.
    pub link_id: Hash,
    /// The destination it is for.
    pub destination_hash: Hash,
    /// The interface it came in on (the initiator's side).
    pub from_interface: usize,
    /// The interface it would go out on (the destination's side).
    pub to_interface: usize,
    /// Hops the request has taken so far.
    pub hops: u8,
    /// Hops from here to the destination.
    pub remaining_hops: u8,
}

/// The host's answer to a [`ForwardRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardDecision {
    /// Do not forward: the link never establishes through this node.
    Refuse,
    /// Forward, capping the link's traffic through this node in each
    /// direction (`None`: no cap on that measure).
    Admit {
        /// Payload bytes per second, each direction.
        max_bytes_per_s: Option<u32>,
        /// Packets per second, each direction.
        max_packets_per_s: Option<u32>,
    },
}

impl ForwardDecision {
    /// Forward, uncapped.
    pub const ADMIT: ForwardDecision = ForwardDecision::Admit {
        max_bytes_per_s: None,
        max_packets_per_s: None,
    };
}

/// The host's admission rule.
pub type AdmissionHook = Box<dyn FnMut(&ForwardRequest) -> ForwardDecision + Send>;

/// One direction of a forwarded link.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirectionCount {
    /// Payload bytes forwarded.
    pub bytes: u64,
    /// Packets forwarded.
    pub packets: u64,
    /// Packets dropped over a cap.
    pub dropped_over_cap: u64,
}

/// A link this node forwards, as the host reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForwardedLink {
    /// The link.
    pub link_id: Hash,
    /// The destination it is for.
    pub destination_hash: Hash,
    /// The initiator's side.
    pub initiator_interface: usize,
    /// The destination's side.
    pub destination_interface: usize,
    /// Initiator to destination.
    pub toward_destination: DirectionCount,
    /// Destination to initiator.
    pub toward_initiator: DirectionCount,
    /// When the request was admitted.
    pub admitted_at_ms: u64,
    /// When the last packet was forwarded, either way.
    pub last_forwarded_ms: u64,
    /// The caps the host set.
    pub decision: ForwardDecision,
}

/// A token bucket holding up to one second of its rate.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    rate: u32,
    tokens: u64,
    at_ms: u64,
}

impl Bucket {
    fn new(rate: u32, now_ms: u64) -> Self {
        Bucket {
            rate,
            tokens: u64::from(rate),
            at_ms: now_ms,
        }
    }

    fn take(&mut self, n: u64, now_ms: u64) -> bool {
        let elapsed = now_ms.saturating_sub(self.at_ms);
        let refill = elapsed.saturating_mul(u64::from(self.rate)) / 1_000;
        if refill > 0 {
            self.tokens = (self.tokens + refill).min(u64::from(self.rate));
            self.at_ms = now_ms;
        }
        if self.tokens >= n {
            self.tokens -= n;
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Meter {
    link: ForwardedLink,
    /// Byte and packet buckets, per direction (0: toward destination).
    bytes: [Option<Bucket>; 2],
    packets: [Option<Bucket>; 2],
}

/// Every link this node forwards, and the admission rule.
#[derive(Default)]
pub struct ForwardMeters {
    hook: Option<AdmissionHook>,
    links: BTreeMap<Hash, Meter>,
    refused: u64,
}

impl core::fmt::Debug for ForwardMeters {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ForwardMeters")
            .field("hook", &self.hook.is_some())
            .field("links", &self.links.len())
            .field("refused", &self.refused)
            .finish()
    }
}

impl ForwardMeters {
    /// Set (or clear) the host's admission rule.
    pub fn set_hook(&mut self, hook: Option<AdmissionHook>) {
        self.hook = hook;
    }

    /// Decide on a request about to be forwarded; true to forward it.
    pub fn admit(&mut self, request: &ForwardRequest, now_ms: u64) -> bool {
        let decision = match &mut self.hook {
            Some(hook) => hook(request),
            None => ForwardDecision::ADMIT,
        };
        let ForwardDecision::Admit {
            max_bytes_per_s,
            max_packets_per_s,
        } = decision
        else {
            self.refused = self.refused.saturating_add(1);
            return false;
        };
        let bucket = |rate: Option<u32>| rate.map(|r| Bucket::new(r, now_ms));
        self.links.insert(
            request.link_id,
            Meter {
                link: ForwardedLink {
                    link_id: request.link_id,
                    destination_hash: request.destination_hash,
                    initiator_interface: request.from_interface,
                    destination_interface: request.to_interface,
                    toward_destination: DirectionCount::default(),
                    toward_initiator: DirectionCount::default(),
                    admitted_at_ms: now_ms,
                    last_forwarded_ms: now_ms,
                    decision,
                },
                bytes: [bucket(max_bytes_per_s), bucket(max_bytes_per_s)],
                packets: [bucket(max_packets_per_s), bucket(max_packets_per_s)],
            },
        );
        true
    }

    /// A packet of `bytes` payload on `link_id` is about to be forwarded,
    /// toward the destination or back: count it, or drop it over a cap
    /// (false). A link not on file (admitted before the meter existed) is
    /// forwarded uncounted.
    pub fn forward(
        &mut self,
        link_id: &Hash,
        toward_destination: bool,
        bytes: usize,
        now_ms: u64,
    ) -> bool {
        let Some(meter) = self.links.get_mut(link_id) else {
            return true;
        };
        let dir = usize::from(!toward_destination);
        let count = if toward_destination {
            &mut meter.link.toward_destination
        } else {
            &mut meter.link.toward_initiator
        };
        let bytes = bytes as u64;
        let within = meter.bytes[dir]
            .as_mut()
            .is_none_or(|b| b.take(bytes, now_ms))
            && meter.packets[dir]
                .as_mut()
                .is_none_or(|b| b.take(1, now_ms));
        if !within {
            count.dropped_over_cap = count.dropped_over_cap.saturating_add(1);
            return false;
        }
        count.bytes = count.bytes.saturating_add(bytes);
        count.packets = count.packets.saturating_add(1);
        meter.link.last_forwarded_ms = now_ms;
        true
    }

    /// Every forwarded link on file.
    #[must_use]
    pub fn links(&self) -> Vec<ForwardedLink> {
        self.links.values().map(|m| m.link).collect()
    }

    /// Requests refused so far.
    #[must_use]
    pub fn refused(&self) -> u64 {
        self.refused
    }

    /// Keep only the links `live` says are still in the link table.
    pub fn retain(&mut self, mut live: impl FnMut(&Hash) -> bool) {
        self.links.retain(|id, _| live(id));
    }

    /// Heap held, for the census.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        crate::heap_census::btree_map_bytes(&self.links)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: u8) -> ForwardRequest {
        ForwardRequest {
            link_id: [id; 16],
            destination_hash: [9; 16],
            from_interface: 0,
            to_interface: 1,
            hops: 1,
            remaining_hops: 1,
        }
    }

    #[test]
    fn without_a_hook_everything_is_admitted_and_counted_each_way() {
        let mut m = ForwardMeters::default();
        assert!(m.admit(&request(1), 0));
        assert!(m.forward(&[1; 16], true, 100, 10));
        assert!(m.forward(&[1; 16], false, 40, 20));
        let l = m.links()[0];
        assert_eq!(
            (l.toward_destination.bytes, l.toward_initiator.bytes),
            (100, 40)
        );
        assert_eq!(l.last_forwarded_ms, 20);
    }

    #[test]
    fn a_refused_request_is_not_on_file_and_counted() {
        let mut m = ForwardMeters::default();
        m.set_hook(Some(Box::new(|r: &ForwardRequest| {
            if r.link_id[0] == 2 {
                ForwardDecision::Refuse
            } else {
                ForwardDecision::ADMIT
            }
        })));
        assert!(!m.admit(&request(2), 0));
        assert!(m.admit(&request(3), 0));
        assert_eq!(m.refused(), 1);
        assert_eq!(m.links().len(), 1);
    }

    #[test]
    fn a_byte_cap_drops_above_one_second_of_its_rate_and_refills() {
        let mut m = ForwardMeters::default();
        m.set_hook(Some(Box::new(|_: &ForwardRequest| {
            ForwardDecision::Admit {
                max_bytes_per_s: Some(1_000),
                max_packets_per_s: None,
            }
        })));
        m.admit(&request(1), 0);
        let sent = (0..20)
            .filter(|_| m.forward(&[1; 16], true, 100, 0))
            .count();
        assert_eq!(sent, 10, "one second's worth");
        assert!(
            m.forward(&[1; 16], true, 100, 100),
            "100 ms refills 100 bytes"
        );
        assert_eq!(m.links()[0].toward_destination.dropped_over_cap, 10);
        // The other direction has its own bucket.
        assert!(m.forward(&[1; 16], false, 900, 100));
    }
}

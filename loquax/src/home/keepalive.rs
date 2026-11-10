//! What keeps a device's connection to its mailbox alive while nothing
//! happens on it (1346; the Android background study, option O2): how
//! often each end looks, when a device gives up on a long interval its
//! mailbox does not take, and the hourly count of what the link cost.
//!
//! Two mechanisms wake an idle phone. The TCP keepalive probes the
//! connection when it carried nothing for its idle time (the reference
//! probes after 5 s, TCPInterface.py:85, on both ends); the link keepalive
//! is a Reticulum packet the link's initiator sends, and its responder
//! answers, every interval the RTT gives (rtt * 360 / 1.75 s, at most
//! 360 s, Link.py:83-94, 845): 10 s on a 50 ms TCP path. Both are local
//! timing, nothing on the wire changes, so the device link sets both long
//! at both ends, and every other connection keeps the reference's values.
//!
//! The bound is the NAT between phone and mailbox: a mapping that times out
//! silently kills the connection. RFC 4787 REQ-5 asks a NAT to keep an idle
//! UDP mapping at least 2 min and recommends 5 min; RFC 5382 REQ-5 asks
//! 2 h 4 min for an established TCP connection, which carrier NATs are
//! known to cut short. 240 s for the TCP idle stays under the recommended
//! 5 min. The link keepalive comes 10 s before it, so on a live link its
//! traffic resets the TCP idle and no TCP probe ever goes out: the phone
//! wakes once per [`LINK_KEEPALIVE_SECS`], and the TCP probe is only the
//! backstop for a connection without a link. A relay between the two keeps
//! the link while it carries something every 900 s (`LINK_TIMEOUT`,
//! Transport.py:89, :688), well above the interval.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The TCP keepalive idle of the device's connection, at both ends: the
/// device's dial to its mailbox or rendezvous, and the mailbox's phone
/// port (`super::phone_server`). The `tcp_keepalive_idle_secs` key still
/// overrides it on either end.
pub const TCP_IDLE_SECS: u64 = 240;

/// The link keepalive of the device's link to its mailbox, at both ends.
/// A responder goes stale at twice its own interval (Link.py:797), so the
/// mailbox has to take the long interval too, or it drops a device link
/// after 20 s of silence; [`Lengthen`] is the device's way out when it
/// does.
pub const LINK_KEEPALIVE_SECS: u64 = 230;

// The link keepalive's traffic comes before the TCP probe would, so a live
// link costs one exchange per interval and no probe; both stay under the
// 5 min RFC 4787 recommends, and the stale time far under the 900 s a relay
// keeps an idle link. Checked when the crate builds.
const _: () = assert!(LINK_KEEPALIVE_SECS < TCP_IDLE_SECS);
const _: () = assert!(TCP_IDLE_SECS < 300);
const _: () = assert!(LINK_KEEPALIVE_SECS * 2 < 900);

/// How many watching links in a row may end before their first keepalive
/// was due before the device stops asking for the long interval: a mailbox
/// from before 1346 keeps the RTT-derived one, stale at 20 s on a fast
/// path, and drops every link the device keeps quiet for longer.
pub const SHORT_HOLDS: u8 = 2;

/// Whether the device asks the long link keepalive of the next watching
/// link. A mailbox restart ends one link early and is forgiven; two in a
/// row say the mailbox does not take the interval, and the device keeps
/// the reference's for the rest of the session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lengthen {
    short: u8,
}

impl Lengthen {
    /// Whether the next link gets [`LINK_KEEPALIVE_SECS`].
    pub fn on(&self) -> bool {
        self.short < SHORT_HOLDS
    }

    /// A watching link ended after `held_secs`. `true` when this end turned
    /// the long interval off.
    pub fn ended(&mut self, held_secs: u64) -> bool {
        if !self.on() {
            return false;
        }
        if held_secs >= LINK_KEEPALIVE_SECS {
            self.short = 0;
            return false;
        }
        self.short += 1;
        !self.on()
    }
}

/// What a device link cost in one window: the keepalive round trips, the
/// links that came up, the pump's turns (the device; the mailbox counts
/// the wakes it sent instead) and the bytes on the connection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Costs {
    pub keepalives: u64,
    pub links: u64,
    pub pumps: u64,
    pub bytes: u64,
}

/// The hour the counters are summed over, in milliseconds.
pub const SUMMARY_EVERY_MS: u64 = 3_600_000;

/// The device's line, once per [`SUMMARY_EVERY_MS`], for the bench to read
/// beside the battery's mAh.
pub fn device_line(costs: &Costs) -> String {
    format!(
        "WAKE_SUMMARY keepalives={} links={} pumps={} bytes={}",
        costs.keepalives, costs.links, costs.pumps, costs.bytes
    )
}

/// The mailbox's line for one device: its `pumps` are the wakes the
/// mailbox sent that device, the only thing of the mailbox's that makes
/// the device's pump turn.
pub fn mailbox_line(device: &str, costs: &Costs) -> String {
    format!(
        "WAKE_SUMMARY device={device} keepalives={} links={} wakes={} bytes={}",
        costs.keepalives, costs.links, costs.pumps, costs.bytes
    )
}

/// A counter that is read as a running total (an interface's bytes, a
/// link's keepalives) turned into the growth since the last reading. A
/// total that went down started over (a new connection, a new link) and
/// counts from zero.
pub fn growth(last: &mut u64, now: u64) -> u64 {
    let grown = if now >= *last { now - *last } else { now };
    *last = now;
    grown
}

/// The mailbox's book of what each device's links cost (1346): a link is
/// the device's once it asked something on it, which every device link
/// does at once (`/watch`, `/sync`, ...). Pure; the daemon hands it the
/// requests, the wakes and the links' keepalive totals, and takes the
/// window's costs once an hour.
#[derive(Debug, Clone)]
pub struct Ledger<K> {
    /// Each device link: whose it is, and its keepalive total at the last
    /// reading.
    links: BTreeMap<K, ([u8; 16], u64)>,
    costs: BTreeMap<[u8; 16], Costs>,
}

impl<K> Default for Ledger<K> {
    fn default() -> Self {
        Self {
            links: BTreeMap::new(),
            costs: BTreeMap::new(),
        }
    }
}

impl<K: Ord + Clone> Ledger<K> {
    /// Device links and devices held, and their estimated heap bytes, for
    /// the mailbox's census (order 1585).
    pub fn census(&self) -> (usize, usize) {
        use leviculum_core::heap_census::btree_map_bytes;
        (
            self.links.len() + self.costs.len(),
            btree_map_bytes(&self.links) + btree_map_bytes(&self.costs),
        )
    }

    /// `device` asked on `link`, `bytes` of request and answer. `true` the
    /// first time for the link: the caller gives it the long keepalive.
    pub fn asked(&mut self, link: K, device: [u8; 16], bytes: u64) -> bool {
        let costs = self.costs.entry(device).or_default();
        costs.bytes = costs.bytes.saturating_add(bytes);
        if self.links.contains_key(&link) {
            return false;
        }
        costs.links += 1;
        self.links.insert(link, (device, 0));
        true
    }

    /// A wake of `bytes` went to `device`.
    pub fn woke(&mut self, device: [u8; 16], bytes: u64) {
        let costs = self.costs.entry(device).or_default();
        costs.pumps += 1;
        costs.bytes = costs.bytes.saturating_add(bytes);
    }

    /// The links to read the keepalive totals of.
    pub fn links(&self) -> Vec<K> {
        self.links.keys().cloned().collect()
    }

    /// `link`'s keepalive total now, `None` once the link is gone, which
    /// forgets it.
    pub fn keepalives(&mut self, link: &K, total: Option<u64>) {
        let Some(total) = total else {
            self.links.remove(link);
            return;
        };
        if let Some((device, last)) = self.links.get_mut(link) {
            let grown = growth(last, total);
            let costs = self.costs.entry(*device).or_default();
            costs.keepalives = costs.keepalives.saturating_add(grown);
        }
    }

    /// The window's costs by device, and a fresh window; the links stay.
    pub fn take(&mut self) -> Vec<([u8; 16], Costs)> {
        core::mem::take(&mut self.costs).into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One early end is a mailbox restart; two in a row turn the long
    /// interval off for good, and a link that held resets the count.
    #[test]
    fn two_links_ended_early_in_a_row_turn_the_long_interval_off() {
        let mut lengthen = Lengthen::default();
        assert!(lengthen.on());
        assert!(!lengthen.ended(25));
        assert!(lengthen.on(), "one early end is forgiven");
        assert!(!lengthen.ended(LINK_KEEPALIVE_SECS));
        assert!(!lengthen.ended(25));
        assert!(lengthen.on(), "a link that held reset the count");
        assert!(lengthen.ended(25), "the second in a row turns it off");
        assert!(!lengthen.on());
        assert!(!lengthen.ended(3_600), "off stays off");
        assert!(!lengthen.on());
    }

    #[test]
    fn the_lines_name_every_counter() {
        let costs = Costs {
            keepalives: 15,
            links: 1,
            pumps: 12,
            bytes: 4_210,
        };
        assert_eq!(
            device_line(&costs),
            "WAKE_SUMMARY keepalives=15 links=1 pumps=12 bytes=4210"
        );
        assert_eq!(
            mailbox_line("00ff", &costs),
            "WAKE_SUMMARY device=00ff keepalives=15 links=1 wakes=12 bytes=4210"
        );
    }

    /// Each device's links, keepalives, wakes and bytes, by the link it
    /// asked on; a gone link is forgotten and the window starts over.
    #[test]
    fn the_ledger_books_each_device_by_its_links() {
        let (phone, laptop) = ([1; 16], [2; 16]);
        let mut ledger = Ledger::<u32>::default();
        assert!(ledger.asked(7, phone, 100), "a new link");
        assert!(!ledger.asked(7, phone, 50), "the same link again");
        assert!(ledger.asked(8, laptop, 10));
        ledger.woke(phone, 20);
        ledger.keepalives(&7, Some(3));
        ledger.keepalives(&8, Some(1));
        ledger.keepalives(&7, Some(5));
        let window = ledger.take();
        assert_eq!(
            window,
            [
                (
                    phone,
                    Costs {
                        keepalives: 5,
                        links: 1,
                        pumps: 1,
                        bytes: 170
                    }
                ),
                (
                    laptop,
                    Costs {
                        keepalives: 1,
                        links: 1,
                        pumps: 0,
                        bytes: 10
                    }
                ),
            ]
        );
        assert!(ledger.take().is_empty(), "the window started over");
        ledger.keepalives(&7, None);
        assert_eq!(ledger.links(), [8], "the gone link is forgotten");
        ledger.keepalives(&8, Some(2));
        assert_eq!(
            ledger.take()[0].1.keepalives,
            1,
            "counted from the last reading"
        );
    }

    #[test]
    fn a_total_that_started_over_counts_from_zero() {
        let mut last = 0;
        assert_eq!(growth(&mut last, 100), 100);
        assert_eq!(growth(&mut last, 160), 60);
        assert_eq!(growth(&mut last, 40), 40, "a new connection");
        assert_eq!(last, 40);
    }
}

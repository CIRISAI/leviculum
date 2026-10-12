//! Structured event-log sink.
//!
//! This is a PRODUCTION component: `lnsd` installs it via
//! [`install_global_subscriber`] and routes events to an append-only
//! file when `LEVICULUM_EVENT_LOG=<path>` is set.  The same sink also
//! backs the mvr / integration test capture harness in
//! [`crate::test_support::event_log`], which layers per-test buffer
//! isolation on top of the process-global layer here.
//!
//! # Format
//!
//! Each emitted event renders to a single line in canonical form:
//!
//! ```text
//! EVENT_NAME node=<n> key1=val1 key2=val2 ... t=<rel-ms>
//! ```
//!
//! - `EVENT_NAME` = string value of the `event` field passed to
//!   `tracing::debug!(event = "FOO", ...)`.
//! - `node=` is reserved as the first field, sourced from
//!   `LEVICULUM_EVENT_NODE` (default `local`).
//! - All other fields are alphabetically sorted.
//! - `t=` is always last; relative milliseconds since the layer was
//!   registered (process-global init time).
//!
//! Records that do not carry an `event = "..."` field are ignored — the
//! legacy printf-style `tracing::debug!("[FOO] ...")` sites stay
//! compatible.  They are ignored at the CALLSITE, once, by
//! [`EventFieldFilter`]: the field names a site can emit are fixed at
//! compile time, so a site with no `event` field is answered
//! `Interest::never()` and never dispatched here again.  Roughly 800
//! `tracing` sites exist across `leviculum-core` and `leviculum-std` and
//! 127 of them carry `event =`; before the filter, the other ~670 each
//! built a `BTreeMap<String, String>` with a `String` per field name and
//! per value, on every emission, to be dropped one line later.
//!
//! # Architecture: the file sink does not run on your thread (#418)
//!
//! `LEVICULUM_EVENT_LOG` is written by ONE dedicated thread. The
//! thread that emits an event formats its line, hands it to a bounded
//! queue and returns; it never touches the file.
//!
//! This is not a throughput optimisation, it is a Priority 1 fix. The
//! miauhaus soak node went completely silent — no packet, no announce,
//! not even the 10 s `PATH_TABLE` heartbeat — 2 928 times in 49 days,
//! with a tail to 37.0 s, because the layer used to `write(2)` on the
//! emitting thread and the driver's event loop emits while holding the
//! core mutex. See `docs/src/concepts/core-lock-budget.md` for the
//! measurement and the rule it produced.
//!
//! Consequences a caller should know:
//!
//! - **Loss is possible and is never silent.** A full queue drops the
//!   line and the writer reports the running count as
//!   `EVENT_LOG_DROPPED node=… n=… t=…`.
//! - **A slow disk is reported, not suffered.** A batch write taking
//!   50 ms or more emits `EVENT_LOG_WRITE_SLOW node=… lines=… ms=…
//!   t=…`, at most once a second.
//! - **`t=` is the emission time, not the write time.** It always was;
//!   it matters more now that the two can differ.
//! - **A process that asserts on the file must flush first.**
//!   [`flush_event_log`] is that point; `atexit` runs it for a normal
//!   exit and for `std::process::exit`.
//! - **`LEVICULUM_EVENT_LOG_SYNC=1`** restores the pre-#418 blocking
//!   write for anyone who would rather block than lose a line.
//!
//! Neither synthetic line goes through `tracing`: a sink that reported
//! on itself through itself would recurse.
//!
//! # Cost: an event allocates nothing in steady state (509)
//!
//! The event log has to stay on in the field, so writing a line must not
//! cost the node memory. Every value is rendered into a per-thread
//! buffer that is cleared, not freed, between events; keys are the
//! callsite's `&'static str`; the line is built in a second reused
//! buffer; the queue is one text buffer the writer swaps out whole. 508
//! measured the layer before this at about three quarters of a field-
//! loaded node's ~13 700 allocations a second, which musl's allocator
//! turned into hundreds of retained megabytes (leviculum#437). The bytes
//! written did not change: `tests::every_catalogued_event_renders_byte_identical_to_the_pre_509_path`
//! holds the new path to the old one.
//!
//! # Architecture: process-global layer + active-handles list
//!
//! A single [`EventLogLayer`] is registered once in the process (by
//! [`install_global_subscriber`] in production, or by
//! [`crate::test_support::tracing_setup::init_tracing_with_event_log`]
//! under test).  All threads, including `tokio::test(multi_thread)`
//! workers, route events through it.
//!
//! Per-test buffer isolation is built on top of the global layer: an
//! [`EventLogHandle`] (created via the helpers in
//! [`crate::test_support::event_log`]) registers an `ActiveHandle` in
//! the layer's shared list.  `on_event` iterates the active list and
//! pushes the formatted line to every active buffer.  When the handle
//! drops, it removes itself from the list.
//!
//! Concurrency consequence: every active buffer receives every event,
//! regardless of which test emitted it.  Tests that assert on buffer
//! contents must filter by event name to avoid cross-test pollution.
//!
//! # Validation
//!
//! Two violation classes, both non-blocking — original event lines
//! are never suppressed.
//!
//! ## Schema validation (per-handle)
//!
//! [`EVENT_CATALOG`] declares required keys per event name.  A name
//! may appear under several entries when its emitters use per-reason
//! shapes (Codeberg #320); a record passes if any declared shape is
//! fully present.  Per consumed event, the layer iterates each active
//! handle's catalogue (production + the handle's `extra_schemas`).
//! If the event is catalogued and no declared shape is satisfied, a
//! synthetic line naming the nearest shape's missing keys is
//! appended to that handle's buffer:
//!
//! ```text
//! EVENT_SCHEMA_VIOLATION event=PKT_RX missing=[hops,len] caller=transport.rs:1074 t=<rel-ms>
//! ```
//!
//! ## Field-value validation (per-event, per-handle)
//!
//! Token-based parsers (the `jl`/`jldiff` tools) split lines on
//! whitespace.  Field values containing whitespace, `=`, or
//! non-printable characters break that contract.  The visitor detects
//! them and the layer emits one synthetic line per offending field,
//! into every active buffer:
//!
//! ```text
//! EVENT_FIELD_VIOLATION event=PKT_RX field=note value_problem=whitespace caller=transport.rs:1074 t=<rel-ms>
//! ```
//!
//! `value_problem` ∈ {`whitespace`, `equals`, `non_printable`}.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};

use crate::counter64::Counter64;
use crate::sync_ext::MutexRecover;
use std::time::{Duration, Instant};

use tracing::field::{Field, Visit};
use tracing::subscriber::Interest;
use tracing::{Event, Metadata, Subscriber};
use tracing_subscriber::filter::Filtered;
use tracing_subscriber::layer::{Context, Filter, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter, Registry};

const NODE_ENV_VAR: &str = "LEVICULUM_EVENT_NODE";
const LOG_FILE_ENV_VAR: &str = "LEVICULUM_EVENT_LOG";
/// Opt back in to the pre-#418 blocking write on the emitting thread.
/// Set to anything but `0`.
const LOG_SYNC_ENV_VAR: &str = "LEVICULUM_EVENT_LOG_SYNC";

/// Schema for one structured event.  Declares the keys that MUST be
/// present on every emission of this event name.
pub struct EventSchema {
    pub name: &'static str,
    pub required_keys: &'static [&'static str],
}

/// Production catalogue — every structured event name any workspace
/// member emits, with the keys that MUST be present on every emission.
///
/// Both directions of the correspondence are rules, and only one of
/// them is checked:
///
/// - **Every emitted name appears here.**  The `#[cfg(test)]` module
///   `crate::event_catalog_completeness` enumerates the `tracing::*!(event =
///   "...")` sites of every workspace member's `src/` and fails on a
///   name this list is missing.  It has to be a check and not a habit:
///   the layer validates an event's SHAPE only for a name it finds
///   here, so an undeclared event can quietly lose a required field
///   forever.
/// - **Every entry here has a live emitter.**  Adding one without is
///   the "stale catalogue" failure mode (Variant 3 can't detect it),
///   so every entry MUST have a corresponding `tracing::debug!(event
///   = "FOO", ...)` in production code.
///
/// A name may appear in several entries when its emitters use
/// per-reason shapes (Codeberg #320); a record passes if any one shape
/// is fully present.
pub const EVENT_CATALOG: &[EventSchema] = &[
    // Per-packet journey contract (Periculum phase 6): PKT_RX / PKT_TX /
    // PKT_FORWARD / PKT_DROP / DEDUP_DROP live on the dedicated
    // `leviculum_core::pkt` tracing target and carry `ph`, the first
    // 16 hex chars of the dedup packet hash, as the cross-node journey
    // correlator. This layer sees every record regardless of target.
    EventSchema {
        name: "PKT_RX",
        required_keys: &["iface", "type", "dst", "hops", "len", "ph"],
    },
    EventSchema {
        name: "PKT_TX",
        required_keys: &["iface", "hops", "len", "ph"],
    },
    EventSchema {
        name: "ANN_RX",
        required_keys: &["dst", "hops", "iface", "path_response"],
    },
    EventSchema {
        name: "PATH_ADD",
        required_keys: &[
            "dst",
            "hops",
            "iface",
            "next_hop",
            "ok",
            "source",
            "table_len",
        ],
    },
    EventSchema {
        // A link-request proof that travelled the route corrected the path
        // entry's LENGTH in place (#330). Only `hops` moved, which is why the
        // shape carries the count before and after and nothing else: the
        // interface and the next hop are still the announce's.
        name: "PATH_REBALANCE",
        required_keys: &["dst", "from", "to"],
    },
    EventSchema {
        // The rebalance above, REFUSED (#332): the proof's count would have
        // made `PathEntry::needs_relay()` false while `next_hop` still named a
        // transport peer, which deletes the only known route instead of
        // shortening it. The entry is untouched, so the shape names the count
        // it keeps (`from`), the one it refused (`refused`) and the relay the
        // refusal protects.
        name: "PATH_REBALANCE_HELD",
        required_keys: &["dst", "from", "refused", "next_hop", "iface"],
    },
    // A data packet handed to local delivery. `matched=true`: a registered
    // local destination (a destination, or a link this node holds) claims
    // it. `matched=false`: a link-addressed packet no local destination
    // claims, passed to the node layer, which drops it as
    // `link-data-no-link` (#407: on a shared medium that is every overheard
    // packet of another pair's link).
    EventSchema {
        name: "PKT_LOCAL",
        required_keys: &["dst", "iface", "matched"],
    },
    // An announce this node learned from and relayed nowhere. Not a drop
    // and deliberately not in the PKT_DROP taxonomy: the packet arrived,
    // was validated, and the path it carried is installed — what is
    // reported is that no route onward was open for it. `closed` names the
    // announce-table term that closed, `discovery` the state of the
    // destination's discovery path request. See
    // docs/src/structured-event-logs.md.
    EventSchema {
        name: "ANNOUNCE_LEARNED_NOT_RELAYED",
        required_keys: &["dst", "closed", "discovery"],
    },
    // Codeberg #365: the routing decision for an ORIGINATED packet, one
    // line per `send_to_destination`, so a log distinguishes "sent to a
    // live carrier" from "withheld because the path's interface is
    // offline" — the firmware's `[TELEMETRY] send` line is the same
    // statement on the debug port (docs/src/structured-event-logs.md).
    EventSchema {
        name: "OUTBOUND_ROUTE",
        required_keys: &["dst", "iface", "next_hop", "online"],
    },
    EventSchema {
        name: "OUTBOUND_WITHHELD",
        required_keys: &["dst", "iface", "next_hop", "reason"],
    },
    EventSchema {
        name: "PKT_DROP",
        required_keys: &["dst", "hops", "iface_in", "ph", "reason", "type"],
    },
    // Second PKT_DROP shape, for the one drop on the OUTBOUND path
    // (`dispatch_actions`, Codeberg #344). The action never reached an
    // interface, so there is no `iface_in`; it was never a parsed `Packet`
    // at this layer, so there is no `dst` or `type`. What it does carry is
    // the interface it was ADDRESSED to, which is the whole diagnosis.
    EventSchema {
        name: "PKT_DROP",
        required_keys: &["hops", "iface_out", "len", "ph", "reason"],
    },
    EventSchema {
        name: "PKT_FORWARD",
        required_keys: &[
            "dst",
            "hops",
            "iface_in",
            "iface_out",
            "next_hop",
            "ph",
            "type",
        ],
    },
    // Codeberg #66 observability: duplicate-hash drop in
    // process_incoming. Deliberately its own event name (not
    // PKT_DROP reason=duplicate): PKT_DROP's schema is
    // forwarding-specific (iface_in/iface_out pairing), and the
    // #66 failure class must be greppable by name alone.
    EventSchema {
        name: "DEDUP_DROP",
        required_keys: &["dst", "iface", "ph", "type", "context"],
    },
    // Codeberg #50 Bug-A forensic instrumentation.  Emitted by
    // periculum's `src/runner.rs::silence_unused_lnode` at function
    // entry and at every exit branch; lets jl/jldiff diff between
    // RNode-only and T114-involved scenarios for any future hang.
    EventSchema {
        name: "SILENCE_LNODE_ENTER",
        required_keys: &["usb_serial", "port_path"],
    },
    EventSchema {
        name: "SILENCE_LNODE_EXIT",
        required_keys: &["usb_serial", "port_path", "result"],
    },
    // Stage-6 catalogue expansion (Codeberg #39 piece 5 follow-up).
    // Promotes the remaining `[TAG] k=v ...` printf-style sites to
    // structured events.  See `docs/src/structured-event-logs.md`
    // for the convention.
    EventSchema {
        name: "EMB_EVICT",
        required_keys: &["cap", "len_before", "map"],
    },
    EventSchema {
        name: "EMB_INSERT_FAIL",
        required_keys: &["cap", "len_after_evict", "map"],
    },
    EventSchema {
        name: "IDENTITY",
        required_keys: &["node"],
    },
    EventSchema {
        name: "PATH_LOOKUP",
        // `hops` and `iface` are present only on the `found=true`
        // branch; only `dst` and `found` are required across both
        // call sites.
        required_keys: &["dst", "found"],
    },
    EventSchema {
        name: "PATH_TABLE",
        required_keys: &["size"],
    },
    EventSchema {
        name: "PATH_TABLE_ENTRY",
        required_keys: &["dst", "expires_in_ms", "hops", "iface", "next_hop"],
    },
    EventSchema {
        name: "PROOF_GEN",
        required_keys: &["for_pkt", "to_dst"],
    },
    EventSchema {
        name: "PROOF_SEND",
        required_keys: &["iface", "pkt"],
    },
    EventSchema {
        name: "REVERSE_ADD",
        required_keys: &["in_iface", "out_iface", "pkt_hash"],
    },
    // Drop observability for the NodeEvent application channels (Codeberg
    // #71). Emitted by `EventSink` in leviculum-std/src/driver/mod.rs.
    // EVENT_CHANNEL_FULL fires only for the control plane (a dropped event
    // whose loss the EventReceiver reports to the consumer as
    // ControlPlaneOverflow); data drops are silent backpressure. Note the
    // asymmetry the field run of Codeberg #419 ran into: this line is
    // written by the driver whether or not anyone reads the event stream,
    // while the marker only reaches a consumer that does. dropped_event_type carries
    // NodeEvent::variant_name() so saturation is greppable per-event-type.
    EventSchema {
        name: "EVENT_CHANNEL_FULL",
        required_keys: &["queue_capacity", "dropped_event_type"],
    },
    EventSchema {
        name: "EVENT_CHANNEL_CLOSED",
        required_keys: &["dropped_event_type"],
    },
    // Control-plane overflow marker delivery: how many control events were
    // dropped since the last marker. Pairs with NodeEvent::ControlPlaneOverflow.
    EventSchema {
        name: "CONTROL_PLANE_OVERFLOW",
        required_keys: &["dropped_count"],
    },
    // OBS-1: announce rebroadcast made observable. ANN_TX fires when the node
    // actually (re)transmits a stored announce on an interface (pairs with
    // ANN_RX); ANN_TX_SUPPRESSED fires when an airtime cap held the rebroadcast
    // back, so a suppressed announce is also visible without claiming a TX.
    // `occasion` (Codeberg #405) says which of the five kinds of announce
    // transmission it was — see `AnnounceTxOccasion` and
    // docs/src/structured-event-logs.md. Without it a transit announce the
    // airtime cap paced, one that bypassed the cap because it originated
    // here, a requested path response and a peer-up re-offer (#383,
    // `occasion=reoffer`) are one indistinguishable line.
    EventSchema {
        name: "ANN_TX",
        required_keys: &["dst", "hops", "iface", "occasion"],
    },
    EventSchema {
        name: "ANN_TX_SUPPRESSED",
        required_keys: &["dst", "hops", "iface", "suppressed", "reason"],
    },
    // leviculum#493: the RNode TX gate's duty hold as interface state, one
    // line per edge (`state=held|lifted`, never per turn). The serial
    // interface emits the same line from an LNode's `TYPE_DUTY_HOLD` notice
    // (leviculum#501), plus `cause=iface_down` on the clear it makes itself
    // when the port goes away. While held, the
    // transport relays no announce and no path response on the interface —
    // those refusals are `ANN_TX_SUPPRESSED reason="duty_hold"
    // closed="duty_hold"` lines — and `lifted` is the visible guarantee the
    // hold ends when the rolling airtime window frees budget. `lt_ms` is
    // the modem's long-term airtime from its latest CMD_STAT reading (0
    // before the first), so the edge can be checked against the ledger
    // that caused it.
    EventSchema {
        name: "DUTY_HOLD",
        required_keys: &["iface", "state", "lt_ms"],
    },
    // The ingress burst limiter holding an announce for an unknown
    // destination (Codeberg #87 hold-and-release, emitted by
    // `transport.rs::handle_announce`). Not a drop: the announce is queued
    // per-interface and released by `process_held_announces` — `held=false`
    // is the narrow case where the queue was already at MAX_HELD_ANNOUNCES
    // and the announce really was lost (that one also counts
    // `ingress_burst_announce` in PKT_DROP_SUMMARY).
    //
    // `hops` is required for the reason the line exists at all: a burst
    // holds one copy per arrival of the same announce and the kept copy is
    // decided by hop count, so a log without it names the destination that
    // was held but not which of its copies.
    EventSchema {
        name: "ANN_HELD",
        required_keys: &["dst", "hops", "iface", "held"],
    },
    // An announce the per-interface cap queue held past its life
    // (`QUEUED_ANNOUNCE_LIFE_MS`, 24 h) and dropped at release instead of
    // keying, as Python prunes its announce queue by `QUEUED_ANNOUNCE_LIFE`.
    // `age_ms` is how long it waited, so a log shows the backlog horizon
    // that dropped it.
    EventSchema {
        name: "ANN_QUEUE_STALE",
        required_keys: &["dst", "hops", "iface", "age_ms"],
    },
    // An operator emptying the announce-cap queues (`rnpath -D`, the
    // `drop: announce_queues` RPC verb), one line per interface that held
    // any, as Python logs "Dropped N announces on <interface>"
    // (Transport.py:3165-3175). `count` is how many waited there.
    EventSchema {
        name: "ANN_QUEUE_DROPPED",
        required_keys: &["iface", "count"],
    },
    // OBS-3 (Codeberg #114): endpoint observability. A node acting as the
    // ENDPOINT of a link (accepting an inbound link, delivering locally,
    // generating the establishment proof, answering a remote-management
    // request) previously emitted no structured events -- only the RELAY path
    // (PKT_FORWARD, relay-side LINK_ENTRY_SET) was instrumented. LINK_LOCAL is
    // the endpoint counterpart to LINK_ENTRY_SET: it fires when we accept an
    // inbound link for one of our own destinations (never for a relayed link).
    // The establishment proof reuses PROOF_GEN/PROOF_SEND, and local delivery
    // reuses PKT_LOCAL (all already catalogued above).
    // A packet for one of our own links refused because it arrived on
    // another interface than the link's attached one (#658, audit D7). The
    // PKT_DROP line with reason=link-iface-mismatch carries the journey
    // hash; this one names the interface the link expected.
    EventSchema {
        name: "LINK_IFACE_MISMATCH",
        required_keys: &["link", "iface", "attached", "ctx"],
    },
    EventSchema {
        name: "LINK_LOCAL",
        required_keys: &["dst", "iface", "link"],
    },
    // OBS-3: the remote-management (and any request/response) responder path.
    // REQUEST_RX fires when an authorized request is dispatched to a handler
    // (pairs with the RequestReceived NodeEvent); RESPONSE_TX fires when the
    // responder sends the single-packet reply back over the link.
    EventSchema {
        name: "REQUEST_RX",
        required_keys: &["link", "path_hash", "request_id"],
    },
    EventSchema {
        name: "RESPONSE_TX",
        required_keys: &["len", "link", "request_id"],
    },
    // OBS-2: periodic per-reason drop summary at the PATH_TABLE cadence (~10s).
    // Surfaces the always-on drop counters (including the high-volume overheard
    // path) without per-packet flooding.
    EventSchema {
        name: "PKT_DROP_SUMMARY",
        required_keys: &[
            "overheard_transport_id",
            "invalid_announce",
            "plain_group_multihop",
            "no_path",
            "ifac",
            "duplicate",
            "announce_over_max_hops",
            "announce_replay",
            "announce_rate_limited",
            "ingress_burst_announce",
            "lrproof_invalid",
            "lrproof_no_link",
            "link_data_no_link",
            "link_repeat_echo",
            "forward_max_hops",
            "blackholed_announce",
            "single_decrypt_fail",
            "group_decrypt_fail",
            "unknown_context",
            "no_such_interface",
            "next_hop_is_requestor",
            "link_iface_mismatch",
            "total",
            // Not a drop reason and outside `total` (#374): link requests
            // for a local client's destination redirected to the clients.
            "lr_local_client_redirect",
            // Not a drop reason and outside `total` (#433): path requests
            // batched onto a discovery already pending for the same
            // destination instead of being re-originated.
            "path_request_pending_suppressed",
            // Not a drop reason and outside `total` (#433): retry ticks of
            // a pending discovery withheld because the window's requesting
            // interface is not in a DISCOVER_PATHS_FOR mode.
            "path_request_retry_withheld",
        ],
    },
    // RNode CMD_READY flow control under the firmware duty lock.
    // Emitted by `interfaces/rnode.rs::rnode_io_task`. GATED fires once the
    // gate has held queued frames past one CHTM cadence and repeats at a
    // bounded rate; RELEASED closes the pair when the gate reopens;
    // QUEUE_DROP names frames the host-side queue loses, and `reason` says
    // how:
    //   reason=queue_full  — the bounded queue shed its oldest frame;
    //                        `len` = that frame's payload bytes, `depth` =
    //                        what stays queued. One event per frame.
    //   any other reason   — a return path of the io task abandoned its
    //                        task-local queue on disconnect (serial_eof,
    //                        device_reset, error_*, …); `len` = frames
    //                        abandoned, `depth` = 0. One event per return
    //                        path, so a reconnect cannot emit 64 lines at
    //                        once.
    // The Columba BLE interface (`interfaces::ble`). BLE_SCAN_DECISION
    // carries the exact fields the firmware's line of the same name does
    // (leviculum-nrf ble/columba.rs), so a merged rig timeline correlates
    // the two sides of one decision; the link lifecycle events carry the
    // same `peer=<hex8>` the firmware logs. Emitted once per
    // (address, decision) change, not per advertising PDU. `rule` is the
    // verdict that made the peer eligible, `pref` (#412 part 1) the tier
    // the window then orders it by — a peer can be permitted and still
    // lose every window, and only the second key says why.
    EventSchema {
        name: "BLE_SCAN_DECISION",
        required_keys: &["addr", "caps", "caps_record", "initiate", "pref", "rule"],
    },
    // The dial backoff's hold on an address the verdict would dial
    // (579): `rule="backoff"`, once per hold, with the deadline on the
    // interface's clock, the time left, and the peer failures in a row.
    // A shape of its own: the hold is not a verdict and carries none of
    // the record's fields. The firmware prints the same keys.
    EventSchema {
        name: "BLE_SCAN_DECISION",
        required_keys: &[
            "addr", "hint", "initiate", "rule", "until_ms", "in_ms", "strikes",
        ],
    },
    // A dial that gave up, booked in the shared per-address backoff
    // (579): `failure` is `transport` (flat 5 s); `not_columba`,
    // `no_handshake`, `no_object` (595) double from 10 s to the 120 s cap;
    // `strikes` the peer failures in a row, `backoff_ms` the pause booked.
    EventSchema {
        name: "BLE_DIAL_BACKOFF",
        required_keys: &[
            "iface",
            "addr",
            "failure",
            "strikes",
            "backoff_ms",
            "until_ms",
        ],
    },
    // A sighting the scan decision discarded as no Columba peer (577):
    // `reason` is `no_service_uuid` or `record_without_service_uuid`,
    // the second the #443 signature (our record, but a stored device
    // object whose `UUIDs` lost the service). DEBUG, once per (address,
    // drop) change, `links::ScanLineGate`.
    EventSchema {
        name: "BLE_SCAN_DROPPED",
        required_keys: &["iface", "addr", "reason", "hint", "rssi"],
    },
    EventSchema {
        name: "BLE_LINK_UP",
        required_keys: &["iface", "peer", "addr", "role", "mtu"],
    },
    EventSchema {
        name: "BLE_LINK_DOWN",
        required_keys: &["iface", "peer", "role", "reason"],
    },
    // The duplicate rule's refusal branch (#360 round 2): the identity's
    // existing link kept the peer and this connection is dropped.
    // `rule` names the branch that decided — `abandoned`, `same_role`,
    // `columba_mtu` or `columba_identity` — without which a capture
    // shows the outcome and not the reasoning.
    EventSchema {
        name: "BLE_LINK_DUP",
        required_keys: &["peer", "addr", "action", "rule"],
    },
    // The duplicate rule's replacement branch (#360 round 2): a newer
    // connection of an identity we already hold took the peer over, in
    // either role, and the old link was disconnected at the decision.
    // `old_silence_ms` is the abandonment test's input (any frame,
    // keepalives included); `old_data_silence_ms` was round 1's input
    // and is reported only, `never` for a link that carried no payload.
    EventSchema {
        name: "BLE_LINK_REPLACED",
        required_keys: &[
            "peer",
            "addr",
            "rule",
            "origin",
            "old_silence_ms",
            "old_data_silence_ms",
        ],
    },
    // `accept_only` turned an incoming link away: the peer is not one
    // this interface serves, and the connection is dropped at the
    // identity handshake, before it becomes a link. `identity` carries
    // the whole hash beside `peer`'s four bytes because both are
    // spellings the key accepts, and the operator reading this line is
    // the one deciding whether to add it. `listed` is how many peers
    // the list names, so "a list is in force" is legible without the
    // config.
    EventSchema {
        name: "BLE_LINK_NOT_ADMITTED",
        required_keys: &[
            "iface", "peer", "identity", "addr", "role", "listed", "action",
        ],
    },
    EventSchema {
        name: "BLE_LINK_SELF",
        required_keys: &["addr", "action"],
    },
    EventSchema {
        name: "BLE_TX_FANOUT_DROP",
        required_keys: &["iface", "peer", "len", "depth"],
    },
    // What the core's #376 delivery hint made of one outbound packet.
    // BLE_TX_ROUTE names the one peer the packet was addressed to and
    // the role of the link it went on; BLE_TX_FLOOD is the broadcast
    // case with the number of live links it reached; BLE_TX_ROUTE_MISS
    // is a routed packet dropped because the addressed peer holds no
    // live link here. Exactly one of the three per outbound packet, so
    // a capture accounts for every packet the interface was handed.
    EventSchema {
        name: "BLE_TX_ROUTE",
        required_keys: &["iface", "peer", "conn", "len"],
    },
    EventSchema {
        name: "BLE_TX_FLOOD",
        required_keys: &["iface", "links", "len"],
    },
    EventSchema {
        name: "BLE_TX_ROUTE_MISS",
        required_keys: &["iface", "peer", "len"],
    },
    // A link's reassembly was discarded before completion (#373): a
    // torn or interleaved fragment stream from the peer cost `lost`
    // whole Reticulum packets, `total` is the link's running count.
    // Same name as the firmware's line (slot-keyed there), so a merged
    // bench timeline carries both receivers.
    EventSchema {
        name: "BLE_RX_ABANDON",
        required_keys: &["iface", "peer", "lost", "total"],
    },
    EventSchema {
        name: "RNODE_TX_GATED",
        required_keys: &["iface", "held_ms", "depth"],
    },
    EventSchema {
        name: "RNODE_TX_RELEASED",
        required_keys: &["iface", "held_ms", "depth"],
    },
    // RNODE_TX_QUEUE_DROP carries two shapes keyed by `reason`
    // (Codeberg #320): `queue_full` sheds one frame and reports its
    // payload size as `len` bytes, while the abandon reasons (a dying
    // io task, #316) report how many whole frames were lost as
    // `frames`. Both entries share the name; a record passes if either
    // shape is fully present (see the any-shape rule in `on_event`).
    EventSchema {
        name: "RNODE_TX_QUEUE_DROP",
        required_keys: &["iface", "len", "depth", "reason"],
    },
    EventSchema {
        name: "RNODE_TX_QUEUE_DROP",
        required_keys: &["iface", "frames", "depth", "reason"],
    },
    // `lnmsg`, the LXMF messenger. Its emitting sites are in `lnmsg/src/events.rs`
    // rather than in this workspace member: the catalogue is one global list by
    // design (the layer looks a name up here whatever crate raised it), and a
    // second per-crate catalogue would mean two places to keep a name's required
    // keys. Every entry below has a live emitter, as this file's "How to add an
    // event" rule requires.
    EventSchema {
        name: "LNMSG_SENDER",
        required_keys: &["from", "source"],
    },
    EventSchema {
        name: "LNMSG_ATTACHED",
        required_keys: &["instance", "address"],
    },
    EventSchema {
        name: "LNMSG_RESOLVED",
        required_keys: &["dst", "waited_ms"],
    },
    EventSchema {
        name: "LNMSG_ENQUEUED",
        required_keys: &["id", "dst", "bytes", "via"],
    },
    EventSchema {
        name: "LNMSG_STATE",
        required_keys: &["id", "state"],
    },
    EventSchema {
        name: "LNMSG_DONE",
        required_keys: &["id", "outcome", "code"],
    },
    EventSchema {
        name: "LNMSG_VIA",
        required_keys: &["method", "reason"],
    },
    EventSchema {
        name: "LNMSG_PN",
        required_keys: &["node", "source", "cost"],
    },
    EventSchema {
        name: "LNMSG_FETCHED",
        required_keys: &["id", "src", "bytes", "dup"],
    },
    EventSchema {
        name: "LNMSG_SYNC_DONE",
        required_keys: &["received", "duplicates", "new"],
    },
    // lnpnd propagation-node events (Codeberg #384 part 1), one line per
    // accepted upload, per `/get`, per eviction. `tid` is the first 16 hex
    // chars of the transient ID, the same shortening the journey contract
    // uses for `ph`; `dst` is the mailbox destination hash.
    EventSchema {
        name: "PN_ACCEPT",
        required_keys: &["tid", "dst", "bytes", "value", "dup", "via"],
    },
    EventSchema {
        name: "PN_GET",
        required_keys: &["dst", "form", "count", "bytes", "purged"],
    },
    EventSchema {
        name: "PN_EVICT",
        required_keys: &["tid", "bytes", "age_s", "reason"],
    },
    // Peering events (Codeberg #384 part 2): one line per peer-table
    // change, per `/offer` round (either direction), per concluded or
    // failed sync round. `peer` is the remote propagation destination
    // hash.
    EventSchema {
        name: "PN_PEER",
        required_keys: &["peer", "action", "reason"],
    },
    EventSchema {
        name: "PN_OFFER",
        required_keys: &["peer", "dir", "offered", "wanted"],
    },
    EventSchema {
        name: "PN_SYNC",
        required_keys: &["peer", "dir", "transferred", "bytes", "result"],
    },
    EventSchema {
        name: "PN_STORE",
        required_keys: &["used", "limit", "count"],
    },
    EventSchema {
        name: "PN_REJECT",
        required_keys: &["reason", "via"],
    },
    EventSchema {
        name: "PN_MAILBOX",
        required_keys: &["src", "bytes"],
    },
    // ---------------------------------------------------------------
    // Catalogue completeness sweep (2026-09-21).
    //
    // The miauhaus soak of 2026-09-18 (397 023 881 events) ended with a
    // schema-health phase that mirrors this list, and named events it had
    // seen emitted that were not declared here. Declaring them is not
    // cosmetic: `on_event` validates the shape of an event only if the name
    // is catalogued, so an undeclared event can lose a required field
    // forever without a single EVENT_SCHEMA_VIOLATION. `LINK_ENTRY_SET` is
    // the proof — 612 639 emissions in that log, and what caught its broken
    // `next_hop` was the field-VALUE check, which runs regardless of this
    // catalogue, not the schema check, which could not run at all.
    //
    // The soak could only name what that one deployment emits. The sweep
    // below came from the tree instead: `src/event_catalog_completeness.rs`
    // enumerates every `tracing::*!(event = "...")` site in every workspace
    // member's `src/` and fails on any name missing here, so the next
    // addition cannot slip past the same way.
    //
    // `required_keys` is the INTERSECTION of the keys a name's sites emit —
    // the contract is "present on every emission". Where sites differ in a
    // way worth checking, the name gets several entries (#320) and a record
    // passes if any shape is satisfied. Note the limit of that rule: a
    // shorter shape dominates a longer one that extends it, so a second
    // entry only earns its place when neither shape contains the other.
    // ---------------------------------------------------------------

    // Tunnels (Codeberg #64), `leviculum-core/src/transport.rs`. A tunnel is
    // keyed by `tunnel` (the tunnel id) and lives on one interface at a
    // time; SYNTHESIZE_SENT is the initiator side, ESTABLISHED/REAPPEARED/
    // VOIDED the responder's lifecycle, and PATH_ASSOCIATED/PATH_RESTORED
    // the path bookkeeping that makes a tunnel worth having.
    EventSchema {
        name: "TUNNEL_SYNTHESIZE_SENT",
        required_keys: &["tunnel", "iface"],
    },
    EventSchema {
        name: "TUNNEL_ESTABLISHED",
        required_keys: &["tunnel", "iface"],
    },
    EventSchema {
        name: "TUNNEL_REAPPEARED",
        required_keys: &["tunnel", "iface", "paths"],
    },
    EventSchema {
        name: "TUNNEL_VOIDED",
        required_keys: &["tunnel", "iface"],
    },
    EventSchema {
        name: "TUNNEL_PATH_ASSOCIATED",
        required_keys: &["dst", "tunnel"],
    },
    EventSchema {
        name: "PATH_RESTORED",
        required_keys: &["dst", "hops", "iface", "next_hop", "source"],
    },
    // The relay side of a link request: the hop count frozen into the link
    // entry at forward time. `recv` and `next_hop` are INTERFACE NAMES, not
    // hashes -- the soak's 252 669 field violations on `next_hop` were
    // interface names carrying whitespace, which is why every emitter wraps
    // a name in `Scalar`/`iface_name` before it reaches a field.
    EventSchema {
        name: "LINK_ENTRY_SET",
        required_keys: &["dst", "remaining_hops", "packet_hops", "recv", "next_hop"],
    },
    // The end of that entry's life: the sweep removed it (#482). For
    // `validated=0` the proof never arrived and `deadline_ms` is the proof
    // deadline frozen at forward time; for `validated=1` it is the close of
    // the inactivity window since the entry's last traffic. 480's run
    // dropped a returning LRPROOF against a removal no line had recorded.
    EventSchema {
        name: "LINK_ENTRY_EXPIRED",
        required_keys: &["link", "validated", "age_ms", "deadline_ms"],
    },
    // A link request for a local client's destination handed to the
    // connected clients instead of the mesh (#374): the daemon's path table
    // held nothing, `local_client_known_dests` still named the destination.
    // `iface_in` and `client` are interface names.
    EventSchema {
        name: "LR_LOCAL_REDIRECT",
        required_keys: &["dst", "iface_in", "client"],
    },
    // Remaining `leviculum-core` transport events.
    EventSchema {
        name: "ANN_SLOW",
        required_keys: &["ms", "iface", "from_held"],
    },
    EventSchema {
        name: "PATH_SOLICIT",
        required_keys: &["dst", "iface_in", "reason"],
    },
    EventSchema {
        name: "PKT_LOCAL_DROP",
        required_keys: &[
            "dst",
            "iface",
            "type",
            "hops",
            "transport_id",
            "expected",
            "reason",
        ],
    },
    EventSchema {
        name: "PR_REORIG",
        required_keys: &["dst", "iface_in", "iface_out", "peers"],
    },
    // A path request batched onto a discovery this node already had pending
    // for the same destination, instead of being re-originated (Codeberg
    // #433, emitted by `transport.rs::handle_path_request`). Not a drop: the
    // pending discovery's answer is owed to `requesters` interfaces, this
    // one included. `dst` is the destination prefix the requester asked for,
    // `iface_in` the interface it asked on. A count rising fast against
    // `PR_REORIG` naming the same `dst` is a requester retrying faster than
    // the mesh can answer — the 2026-09-27 field day's amplifier.
    EventSchema {
        name: "PR_PENDING_SUPPRESSED",
        required_keys: &["dst", "iface_in", "requesters"],
    },
    // A retry tick of a pending discovery withheld because the window's
    // requesting interface (`iface_req`) is not in a DISCOVER_PATHS_FOR
    // mode (`transport.rs::retry_pending_discoveries`, #433 order 385).
    // Sustained repetition for one `dst` says a requester is holding a
    // Full-mode window open for a destination the mesh cannot answer.
    EventSchema {
        name: "PR_RETRY_WITHHELD",
        required_keys: &["dst", "iface_req"],
    },
    // The management destination's own announce (`node/mod.rs`); `iface` is
    // the literal `all`, since it goes out on every interface at once.
    EventSchema {
        name: "MGMT_ANN_TX",
        required_keys: &["dst", "iface"],
    },
    // Resource transfer (`leviculum-core/src/resource/`, and the two link
    // guards in `node/link_management.rs`). `rh` is the first 4 bytes of the
    // resource hash, the correlator across a whole transfer.
    EventSchema {
        name: "RESOURCE_TX_STATE",
        // `adv_retries` rides along on the advertisement-retry sites only.
        required_keys: &["rh", "status", "retries"],
    },
    EventSchema {
        name: "RESOURCE_REQ_RX",
        required_keys: &[
            "rh",
            "n_req",
            "matched",
            "first_req_idx",
            "distinct_sent",
            "num_parts",
            "status",
        ],
    },
    EventSchema {
        name: "RESOURCE_REQ_NO_MATCH",
        required_keys: &["rh", "n_req", "search_start", "search_end"],
    },
    EventSchema {
        name: "RESOURCE_REQ_ERR",
        // Six sites, one per `reason`; only `rh` and `reason` are common to
        // all of them, the rest (`len`, `req_rh`, `idx`) are per-reason
        // detail. Same shape of contract as PATH_LOOKUP above.
        required_keys: &["rh", "reason"],
    },
    EventSchema {
        name: "RESOURCE_REQ_DROP",
        required_keys: &["reason", "link", "state"],
    },
    // The receiver re-sending its request for a resource the sender
    // advertised again before any part arrived (Codeberg #404).
    EventSchema {
        name: "RESOURCE_REQ_AGAIN",
        required_keys: &["rh", "link"],
    },
    // An advertisement refused with an RCL because its link already
    // carries `max_incoming_resources_per_link` incoming resources
    // (Codeberg #657).
    EventSchema {
        name: "RESOURCE_ADV_REFUSED",
        required_keys: &["rh", "link", "reason", "held"],
    },
    EventSchema {
        name: "RESOURCE_PART_RX",
        required_keys: &["rh", "idx", "outstanding", "consecutive"],
    },
    // Two genuinely disjoint shapes, so both are declared: the
    // `not_transferring` refusal reports the resource's `status`, the
    // `no_matching_hash` refusal reports the part hash and the window it
    // searched. Neither contains the other.
    EventSchema {
        name: "RESOURCE_PART_REJECT",
        required_keys: &["rh", "reason", "status"],
    },
    EventSchema {
        name: "RESOURCE_PART_REJECT",
        required_keys: &["rh", "reason", "mh", "consecutive", "win_start", "win_end"],
    },
    EventSchema {
        name: "RESOURCE_PART_DROP",
        required_keys: &["reason", "link", "state"],
    },
    // The receive window's per-round sample. NOTE: this site emits its own
    // `t` field, and the layer appends the relative timestamp as `t=` too,
    // so the line carries two. A line's own stamp is its LAST `t=` (the rule
    // `merge_event_logs` already applies), so the parsers agree -- but the
    // round's elapsed milliseconds are shadowed and only the second `t` is
    // readable. Declared as emitted; renaming the field is a wire change for
    // whoever greps these lines.
    EventSchema {
        name: "RESOURCE_RW",
        required_keys: &["rh", "round", "window", "wmax", "rate", "outst", "t"],
    },
    // `leviculum-std` driver. ANNOUNCE_TX/ANNOUNCE_WITHHELD are the
    // peer-up announce burst (distinct from the transport's ANN_TX, which
    // is a rebroadcast of a stored announce); CORE_STALL and the two
    // CORE_PROCESSOR_* lines are the #198/#418 core-lock observability.
    EventSchema {
        name: "ANNOUNCE_TX",
        required_keys: &["reason", "peer", "iface", "count"],
    },
    EventSchema {
        name: "ANNOUNCE_WITHHELD",
        required_keys: &["reason", "peer", "iface"],
    },
    EventSchema {
        name: "CORE_STALL",
        required_keys: &["ms"],
    },
    // One per storage flush: the core-lock holds of its three phases in
    // CPU time of the holder (#607).
    EventSchema {
        name: "FLUSH_LOCK_HOLD",
        required_keys: &["holds", "longest_cpu_us", "total_cpu_us"],
    },
    EventSchema {
        name: "CORE_PROCESSOR_OVER_BUDGET",
        required_keys: &["hook", "elapsed_us", "budget_us", "events"],
    },
    EventSchema {
        name: "CORE_PROCESSOR_PANICKED",
        required_keys: &["hook"],
    },
    // The self-deadlock tripwire (`sync_ext.rs`, Codeberg #198).
    // LOCK_DEPTH_OVERFLOW says the tripwire went inactive on a thread;
    // REENTRANT_LOCK is the tripwire firing, emitted immediately before the
    // panic so the line survives whatever the panic machinery does next.
    EventSchema {
        name: "LOCK_DEPTH_OVERFLOW",
        required_keys: &["max_depth", "tripwire"],
    },
    EventSchema {
        name: "REENTRANT_LOCK",
        required_keys: &["mutex", "held_depth"],
    },
    // The RNode interface's vport teardown (#316 family): one line per
    // sub-interface whose queue was abandoned when its incoming side closed.
    EventSchema {
        name: "RNODE_VPORT_DEREGISTERED",
        required_keys: &["iface", "vport_iface", "vport", "frames", "reason"],
    },
    // Columba BLE interface events the catalogue had not caught up with.
    // BLE_ADV_GATE's `state=off` branch also carries `reason`; it cannot be
    // required, because the `state=on` branch has no reason to give and a
    // shorter shape dominates (see the note at the head of this sweep).
    EventSchema {
        name: "BLE_ADV_GATE",
        required_keys: &["iface", "state"],
    },
    EventSchema {
        name: "BLE_DIAL_NOT_ALLOWED",
        required_keys: &["iface", "addr", "hint", "listed"],
    },
    // The churn penalty (#417): the firmware's `BLE_CHURN_LEDGER armed`
    // and `held` lines, the bare token spelled as `state=`. Two disjoint
    // shapes: the arming names the identity whose run closed the class,
    // the hold names the deadline a fallback candidate waits for.
    EventSchema {
        name: "BLE_CHURN_LEDGER",
        required_keys: &[
            "iface",
            "state",
            "identity",
            "run",
            "reason",
            "window_ms",
            "cooldown_ms",
        ],
    },
    EventSchema {
        name: "BLE_CHURN_LEDGER",
        required_keys: &["iface", "state", "run", "until_ms", "now_ms"],
    },
    EventSchema {
        name: "BLE_DIAL_QUEUE",
        required_keys: &["iface", "depth", "wait_ms"],
    },
    EventSchema {
        name: "BLE_PEER_UP",
        required_keys: &["iface", "peer"],
    },
    EventSchema {
        name: "BLE_PEER_LOST",
        required_keys: &["iface", "peer"],
    },
    // The adapter's D-Bus object vanished under a running BLE session
    // (600): bluetoothd restarted or the controller re-enumerated, seen as
    // its `InterfacesRemoved` or as a discovery failing on the missing
    // object (`error` says which). WARN, once per loss; `links` is how many connections the table held, each
    // released as `BLE_LINK_DOWN reason=adapter_lost`. `BLE_ADAPTER_BACK`
    // is the session that resolved it again, `down_ms` after the loss;
    // `forgot` is how many stored peer device objects it removed first.
    EventSchema {
        name: "BLE_ADAPTER_LOST",
        required_keys: &["iface", "hci", "links", "error"],
    },
    EventSchema {
        name: "BLE_ADAPTER_BACK",
        required_keys: &["iface", "hci", "down_ms", "forgot"],
    },
    EventSchema {
        name: "BLE_SCAN_FALLBACK",
        required_keys: &["iface", "after_ms"],
    },
    EventSchema {
        name: "BLE_SCAN_WINDOW",
        required_keys: &["iface", "seen", "chosen", "rule"],
    },
    // Two disjoint shapes: the GATT-notify writer names the `link` it
    // waited on, the BlueZ writer names the peer `addr`. Neither contains
    // the other, so both are declared.
    EventSchema {
        name: "BLE_TX_GAP",
        required_keys: &["iface", "link", "waited_ms"],
    },
    EventSchema {
        name: "BLE_TX_GAP",
        required_keys: &["iface", "addr", "waited_ms"],
    },
    // `leviculum-std/src/bin/event-log-helper.rs`, the fixture binary the
    // multi-process event-log tests drive. It emits through the production
    // subscriber like anything else, so it is declared like anything else:
    // an "it is only a test helper" exception list is precisely the kind of
    // hole that let five names go undeclared for months.
    EventSchema {
        name: "HELPER_TICK",
        required_keys: &["i"],
    },
];

/// Where the buffer is dumped on a panicking drop.
///
/// `pub(crate)` so the test-capture helpers in
/// [`crate::test_support::event_log`] can select stderr vs file when
/// building a handle via [`new_handle`].
pub(crate) enum DumpTarget {
    Stderr,
    File(PathBuf),
}

/// Per-handle bookkeeping kept in the layer's active list.
struct ActiveHandle {
    buffer: Arc<Mutex<Vec<String>>>,
    extra_schemas: &'static [EventSchema],
}

/// Handle returned from the test-capture helpers in
/// [`crate::test_support::event_log`].  While alive, every event the
/// global layer sees is appended to `buffer`.  On drop, the handle
/// removes itself from the layer's active list and — if the thread is
/// panicking — dumps the buffer to the configured target.
pub struct EventLogHandle {
    buffer: Arc<Mutex<Vec<String>>>,
    dump_target: DumpTarget,
    /// Reference to the layer's shared active-handles list, used by
    /// `Drop` to remove this handle's entry.
    active: Arc<Mutex<Vec<ActiveHandle>>>,
}

impl EventLogHandle {
    /// Snapshot the current buffer.  Useful for assertions in
    /// non-panicking tests.  Other parallel tests may have
    /// contributed lines — filter by event name.
    pub fn dump(&self) -> Vec<String> {
        self.buffer.lock_recover().clone()
    }
}

impl Drop for EventLogHandle {
    fn drop(&mut self) {
        // Remove our active entry first so subsequent events don't
        // race against a partly-torn-down handle.
        if let Ok(mut active) = self.active.lock() {
            active.retain(|h| !Arc::ptr_eq(&h.buffer, &self.buffer));
        }

        if !std::thread::panicking() {
            return;
        }

        let buffer = self.buffer.lock_recover();
        let body = buffer.join("\n");
        let dump = format!(
            "=== EVENT LOG DUMP (test panicked, {} lines) ===\n{}\n=== END EVENT LOG DUMP ===\n",
            buffer.len(),
            body,
        );
        match &self.dump_target {
            DumpTarget::Stderr => eprintln!("{dump}"),
            DumpTarget::File(p) => {
                // Best-effort — failure to write the dump must not
                // shadow the original panic.
                let _ = std::fs::write(p, dump);
            }
        }
    }
}

/// Register a fresh capture handle in the global layer's active list
/// and return it.  The caller is responsible for having installed the
/// global subscriber first (production via [`install_global_subscriber`],
/// tests via `tracing_setup::init_tracing_with_event_log`).
///
/// `pub(crate)`: the public test-capture entry points
/// (`init_event_log`, …) live in [`crate::test_support::event_log`] and
/// delegate here after ensuring the subscriber is installed.
pub(crate) fn new_handle(
    dump_target: DumpTarget,
    extra_schemas: &'static [EventSchema],
) -> EventLogHandle {
    let buffer: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let active = active_list();
    {
        let mut list = active.lock_recover();
        list.push(ActiveHandle {
            buffer: Arc::clone(&buffer),
            extra_schemas,
        });
    }
    EventLogHandle {
        buffer,
        dump_target,
        active: Arc::clone(active),
    }
}

/// Build the layer used by the global subscriber installers.  One
/// global layer per process; the active-handles list it owns is shared
/// with every [`EventLogHandle`] via `active_list`.
///
/// The layer comes wrapped in [`EventFieldFilter`], and it is handed out
/// no other way on purpose: an unwrapped `EventLogLayer` declares
/// interest in every callsite in the process, which is how ~800 `tracing`
/// sites came to be visited so that 127 of them could be kept.
pub fn layer<S>() -> Filtered<EventLogLayer, EventFieldFilter, S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    EventLogLayer {
        active: Arc::clone(active_list()),
        init_time: Instant::now(),
    }
    .with_filter(EventFieldFilter)
}

/// Answers, once per callsite, whether a record from it can carry an
/// `event = "..."` field — the only records [`EventLogLayer`] keeps.
///
/// A callsite's field names are fixed at compile time and live in its
/// `Metadata`, so `Interest::never()` for a site without an `event` field
/// is a permanent, correct answer: `tracing` caches it and stops
/// dispatching that site to this layer entirely.
///
/// # Why this is a per-layer `Filter` and not `Layer::enabled`
///
/// `Layer::register_callsite` is a filter on the WHOLE subscriber.
/// `Layered::pick_interest` returns an outer layer's `never` immediately,
/// without asking the layers beneath it
/// (`tracing-subscriber/src/layer/layered.rs`), so implementing this on
/// `EventLogLayer` itself would delete the fmt layer's `RUST_LOG` output
/// for every callsite that does not carry an `event` field — which is
/// nearly all of them, including every plain `warn!` an operator reads a
/// daemon's journal for.
///
/// A per-layer `Filter` is scoped to its own layer: `Filtered` adds this
/// interest to the per-callsite sum and returns `Interest::always()`
/// upward so the other layers keep their say. `tests/
/// event_log_callsite_filter.rs` holds that distinction as a test, not
/// only as this comment.
///
/// No `max_level_hint` is declared, deliberately: `event =` sites exist at
/// every level from TRACE to WARN, and a hint here would cap the process
/// global level and silence them.
pub struct EventFieldFilter;

impl EventFieldFilter {
    /// Whether this callsite declares an `event` field. Spans are always
    /// accepted: this layer ignores them, but a filter that refuses them
    /// would be making a claim about span storage it has no reason to
    /// make.
    fn wants(meta: &Metadata<'_>) -> bool {
        !meta.is_event() || meta.fields().field("event").is_some()
    }
}

impl<S: Subscriber> Filter<S> for EventFieldFilter {
    fn callsite_enabled(&self, meta: &'static Metadata<'static>) -> Interest {
        if Self::wants(meta) {
            Interest::always()
        } else {
            Interest::never()
        }
    }

    fn enabled(&self, meta: &Metadata<'_>, _cx: &Context<'_, S>) -> bool {
        Self::wants(meta)
    }
}

/// Global active-handles list.  Lazily allocated on first access so
/// the order of subscriber install and handle registration doesn't
/// matter.
fn active_list() -> &'static Arc<Mutex<Vec<ActiveHandle>>> {
    static ACTIVE: OnceLock<Arc<Mutex<Vec<ActiveHandle>>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Arc::new(Mutex::new(Vec::new())))
}

/// Process-wide node identifier.  Read from `LEVICULUM_EVENT_NODE`
/// at first access; defaults to `local`.  Cached via OnceLock so
/// every emitted event gets a consistent prefix even if the env
/// changes mid-run.
fn node_name() -> &'static str {
    static NODE: OnceLock<String> = OnceLock::new();
    NODE.get_or_init(|| {
        std::env::var(NODE_ENV_VAR)
            .map(|name| sanitize_scalar(&name))
            .unwrap_or_else(|_| "local".to_string())
    })
}

/// Process-wide append-only event log sink.  Returns `Some` only when
/// `LEVICULUM_EVENT_LOG=<path>` is set in the environment at first
/// access AND the file opens successfully.  Cached via OnceLock so
/// the env-var lookup + file-open happens exactly once per process.
///
/// An open failure is reported ONCE, on stderr, then cached as `None`
/// (L-0022). stderr rather than `tracing` is deliberate twice over:
/// this init runs inside the layer's own `on_event` (see the caller),
/// where emitting a tracing event would re-enter the dispatcher while
/// the OnceLock is mid-init; and a broken `LEVICULUM_EVENT_LOG` must
/// be visible even in a process that installs no subscriber. No retry:
/// the path is fixed by the environment for the process lifetime, its
/// failure modes (permissions, missing directory) do not self-heal,
/// and a retry would put a failing `open(2)` on every event emission
/// inside the tracing hot path.
///
/// `init_time` is the layer's epoch, so the writer thread's own
/// synthetic lines (`EVENT_LOG_DROPPED`, `EVENT_LOG_WRITE_SLOW`) carry
/// `t=` on the same timebase as every other line in the file.
fn file_sink(init_time: Instant) -> Option<&'static FileSink> {
    FILE_SINK
        .get_or_init(|| {
            let path = std::env::var(LOG_FILE_ENV_VAR).ok()?;
            let file = match OpenOptions::new().create(true).append(true).open(&path) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!(
                        "{LOG_FILE_ENV_VAR}={path}: cannot open event log: {e} — \
                         event logging disabled for this process"
                    );
                    return None;
                }
            };
            Some(FileSink::new(file, init_time))
        })
        .as_ref()
}

static FILE_SINK: OnceLock<Option<FileSink>> = OnceLock::new();

/// Bounded hand-off queue between the emitting threads and the writer
/// thread, in lines.
///
/// Sized from the field measurement in Codeberg #418: the longest stall
/// the miauhaus soak recorded in 49 days was 37.0 s, and the node emits
/// 30–150 events/s.  8192 lines absorbs ~55 s at the top of that rate,
/// so the stall distribution that motivated the queue fits inside it
/// with margin; at ~80 bytes/line the worst case is ~650 KiB of
/// buffered text, which is the price of not going deaf.
const SINK_QUEUE_CAPACITY: usize = 8192;

/// A batch write to the log file slower than this is reported as
/// `EVENT_LOG_WRITE_SLOW`.
///
/// The threshold is a measurement decision, not a taste one.  Appending
/// a few hundred bytes to a warm page cache is tens of microseconds;
/// 50 ms is three orders of magnitude above that and two orders below
/// the shortest stall the field saw (11 s), so it cannot miss a stall
/// of the reported shape and it cannot fire on a healthy write.
const SLOW_WRITE_MS: u128 = 50;

/// Minimum spacing between `EVENT_LOG_WRITE_SLOW` lines.  A disk that
/// is slow is slow for many consecutive writes; without this the
/// instrumentation becomes the next volume problem it is meant to
/// diagnose.  One line per second names the condition without
/// describing every instance of it.
const SLOW_REPORT_MIN_GAP_MS: u128 = 1_000;

/// Shared counters between the emitting threads and the writer thread.
struct SinkCounters {
    /// Lines accepted into the queue.
    enqueued: Counter64,
    /// Lines the writer has handed to `write(2)`.
    flushed: Counter64,
    /// Lines refused because the queue was full.
    dropped: Counter64,
}

/// Lines accepted but not yet taken by the writer thread, as one text
/// buffer rather than one `String` per line (509).
///
/// The emitting thread appends its line, newline included, under the
/// mutex; the writer swaps the whole buffer for its own empty one and
/// writes it outside the lock. Both buffers keep their capacity, so in
/// steady state accepting a line is a `memcpy` and no allocation, where
/// the channel of owned lines this replaced cost one allocation a line on
/// the emitting side and a batch `Vec` plus a batch `String` on the
/// writer's (508's census: `writer_loop` was the largest site by bytes).
struct Pending {
    /// Newline-terminated lines, in acceptance order.
    text: String,
    /// How many lines `text` holds; bounded by `SINK_QUEUE_CAPACITY`.
    lines: usize,
    /// `t=` of the first line in `text`, which stamps an
    /// `EVENT_LOG_DROPPED` line written ahead of the batch.
    first_t: u128,
}

/// What the emitting threads and the writer thread share.
struct SinkQueue {
    pending: Mutex<Pending>,
    /// Signalled when `pending` goes from empty to holding a line.
    wake: Condvar,
    counters: SinkCounters,
}

impl SinkQueue {
    /// Plain `lock` with poison recovery rather than `lock_recover`: the
    /// writer has to hand this guard to a `Condvar`, which takes a
    /// `MutexGuard`. A poisoned queue holds whole lines only (a line is
    /// pushed in one `push_str`), so recovering it loses nothing.
    fn lock(&self) -> MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// How the sink gets a line into the file.
enum SinkMode {
    /// Pre-#418 behaviour: `write(2)` on the emitting thread, under a
    /// process-global mutex.  Kept behind `LEVICULUM_EVENT_LOG_SYNC=1`
    /// as the positive control for the mvr — a measurement that cannot
    /// show the failure it claims to fix proves nothing — and as the
    /// escape hatch for anyone who would rather block than lose a line.
    Blocking(Mutex<File>),
    /// Default: hand the line to the writer thread and return.
    Queued(Arc<SinkQueue>),
}

/// The process-wide event-log file sink.
///
/// # Why the caller does not write the file (Codeberg #418)
///
/// The miauhaus soak node went silent — no packet, no announce, not even
/// the 10 s `PATH_TABLE` heartbeat — 2 928 times in 49 days, with a tail
/// to 37.0 s.  The hole always sat between two emission sites a few
/// hundred lines apart in `handle_announce`, and nothing between them
/// can take seconds.  What can is the emission itself: the driver's
/// event loop calls `tracing::debug!` while it holds the core mutex
/// (`apply_inbound`, `leviculum-std/src/driver/mod.rs:4726`), the layer
/// wrote the line with a blocking `write(2)` on that very thread, and a
/// `write(2)` to a USB disk under writeback throttling blocks for
/// seconds.  Every other task then queued behind the core mutex, so the
/// daemon emitted nothing at all — which is exactly the shape the field
/// reported.
///
/// A diagnostic write that can stop the transport is a worse bug than
/// the missing diagnostics, so the default inverts the trade: the
/// emitting thread does a bounded enqueue, a dedicated writer thread
/// owns the file, and an overrun drops lines and says so
/// (`EVENT_LOG_DROPPED`) instead of blocking the mesh.
struct FileSink {
    mode: SinkMode,
}

impl FileSink {
    fn new(file: File, init_time: Instant) -> Self {
        let sync = std::env::var(LOG_SYNC_ENV_VAR).is_ok_and(|v| v != "0");
        if sync {
            return FileSink {
                mode: SinkMode::Blocking(Mutex::new(file)),
            };
        }
        let queue = Arc::new(SinkQueue {
            pending: Mutex::new(Pending {
                text: String::new(),
                lines: 0,
                first_t: 0,
            }),
            wake: Condvar::new(),
            counters: SinkCounters {
                enqueued: Counter64::new(0),
                flushed: Counter64::new(0),
                dropped: Counter64::new(0),
            },
        });
        let writer_queue = Arc::clone(&queue);
        // A named thread so `top -H` / a stack dump can attribute the
        // one thread in the process that is allowed to block on the log.
        let spawned = std::thread::Builder::new()
            .name("leviculum-eventlog".to_string())
            .spawn(move || writer_loop(file, &writer_queue, init_time));
        match spawned {
            Ok(_) => {
                // The queue is drained by a thread, so a process that
                // exits while lines are still in flight would truncate
                // its own log. `atexit` covers both a `main` return and
                // `std::process::exit`; a signal death is not covered
                // and cannot be, which is why the drain is bounded and
                // the writer holds nothing back once it has a batch.
                unsafe { libc::atexit(flush_event_log_at_exit) };
                FileSink {
                    mode: SinkMode::Queued(queue),
                }
            }
            Err(e) => {
                // No thread, no queue: fall back to the blocking write
                // rather than silently logging nothing. Recovering the
                // file out of the moved closure is not possible, so
                // reopen it by path.
                eprintln!(
                    "{LOG_FILE_ENV_VAR}: cannot spawn event-log writer thread: {e} — \
                     falling back to blocking writes"
                );
                let path = std::env::var(LOG_FILE_ENV_VAR).unwrap_or_default();
                match OpenOptions::new().create(true).append(true).open(&path) {
                    Ok(f) => FileSink {
                        mode: SinkMode::Blocking(Mutex::new(f)),
                    },
                    Err(e) => {
                        eprintln!("{LOG_FILE_ENV_VAR}={path}: reopen failed: {e}");
                        FileSink {
                            mode: SinkMode::Blocking(Mutex::new(
                                File::open("/dev/null").expect("/dev/null"),
                            )),
                        }
                    }
                }
            }
        }
    }

    /// Hand one already-formatted line (no newline) to the file.  Never
    /// blocks on the file in the default mode: the only wait is the
    /// queue's mutex, which the writer holds for a buffer swap and never
    /// across a `write(2)`.
    fn write(&self, t_ms: u128, line: &str) {
        match &self.mode {
            SinkMode::Blocking(file) => {
                if let Ok(mut f) = file.lock() {
                    let _ = writeln!(f, "{line}");
                    let _ = f.flush();
                }
            }
            SinkMode::Queued(queue) => {
                let mut pending = queue.lock();
                if pending.lines >= SINK_QUEUE_CAPACITY {
                    drop(pending);
                    queue.counters.dropped.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                if pending.lines == 0 {
                    pending.first_t = t_ms;
                }
                pending.text.push_str(line);
                pending.text.push('\n');
                pending.lines += 1;
                let was_empty = pending.lines == 1;
                queue.counters.enqueued.fetch_add(1, Ordering::Release);
                drop(pending);
                // Only the first line of a batch can find the writer
                // asleep; signalling every line would be a futex call per
                // event for nothing.
                if was_empty {
                    queue.wake.notify_one();
                }
            }
        }
    }
}

/// The one thread in the process allowed to block on the event log.
///
/// Takes whatever is queued as one batch and hands it to a single
/// `write(2)`: under load that is both fewer syscalls than a
/// line-at-a-time path and the thing that lets the queue drain faster
/// than it fills. The batch buffer is swapped with the queue's, not
/// copied, and both keep their capacity across batches.
fn writer_loop(mut file: File, queue: &SinkQueue, init_time: Instant) {
    let node = node_name();
    let mut written: u64 = 0;
    let mut reported_drops: u64 = 0;
    let mut last_slow_report_ms: u128 = 0;
    let mut batch = String::new();
    // Synthetic lines (drop and slow-write reports), formatted here
    // rather than through `format!` and reused.
    let mut note = String::new();
    loop {
        let mut pending = queue.lock();
        while pending.lines == 0 {
            pending = queue
                .wake
                .wait_timeout(pending, Duration::from_millis(200))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        std::mem::swap(&mut pending.text, &mut batch);
        let lines = pending.lines;
        let first_t = pending.first_t;
        pending.lines = 0;
        drop(pending);

        let started = Instant::now();
        // A drop is only knowable here, and it is the loss the old code
        // could not have: say so in the file, in the canonical format,
        // before the lines that survived it.
        let dropped = queue.counters.dropped.load(Ordering::Relaxed);
        if dropped > reported_drops {
            note.clear();
            let _ = writeln!(
                note,
                "EVENT_LOG_DROPPED node={node} n={dropped} t={first_t}"
            );
            let _ = file.write_all(note.as_bytes());
            reported_drops = dropped;
        }
        let _ = file.write_all(batch.as_bytes());
        let took_ms = started.elapsed().as_millis();
        batch.clear();
        // A burst can grow the buffers to the queue's full 8192 lines
        // (about a megabyte); keep a working size, not the high-water mark.
        if batch.capacity() > BATCH_KEEP_BYTES {
            batch.shrink_to(BATCH_KEEP_BYTES);
        }

        written += lines as u64;
        queue.counters.flushed.store(written, Ordering::Release);

        // The measurement #418 asks for, from the only place that can
        // take it: how long the disk actually held the write. Emitted
        // straight into the file rather than through `tracing`, because
        // a sink that reports on itself through itself recurses.
        if took_ms >= SLOW_WRITE_MS {
            let now_ms = init_time.elapsed().as_millis();
            if now_ms.saturating_sub(last_slow_report_ms) >= SLOW_REPORT_MIN_GAP_MS {
                last_slow_report_ms = now_ms;
                note.clear();
                let _ = writeln!(
                    note,
                    "EVENT_LOG_WRITE_SLOW node={node} lines={lines} ms={took_ms} t={now_ms}"
                );
                let _ = file.write_all(note.as_bytes());
            }
        }
    }
}

/// Capacity the writer keeps on its batch buffer after a burst: 64 KiB,
/// several hundred lines, far above a normal batch at the 30-150 events a
/// second the field runs at.
const BATCH_KEEP_BYTES: usize = 64 * 1024;

/// `atexit` shim: bounded drain so a normal exit does not truncate the
/// log the process just wrote.
extern "C" fn flush_event_log_at_exit() {
    flush_event_log(Duration::from_secs(2));
}

/// Block until every line accepted by the sink has reached `write(2)`,
/// or until `timeout` expires.
///
/// Public because a process that writes an event log and then asserts on
/// the file (the multi-process event-log tests, `lnmsg`'s CLI tests)
/// needs a point where "written" is a fact rather than a race.  A no-op
/// when no event log is configured or when the blocking mode is in
/// force, because in both cases the write already happened on the
/// caller's thread.
pub fn flush_event_log(timeout: Duration) {
    let Some(Some(sink)) = FILE_SINK.get() else {
        return;
    };
    let SinkMode::Queued(queue) = &sink.mode else {
        return;
    };
    let counters = &queue.counters;
    let deadline = Instant::now() + timeout;
    while counters.flushed.load(Ordering::Acquire) < counters.enqueued.load(Ordering::Acquire) {
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Queue depth and loss of the file sink, for the memory census
/// (Codeberg #399): `(pending_lines, dropped_lines)`.
///
/// `None` when no event log is configured, or when the blocking mode is
/// in force — nothing is ever pending there, the write happened on the
/// emitting thread. The pending count is bounded by
/// `SINK_QUEUE_CAPACITY` (8192 lines); a census that reads thousands
/// here has caught the writer mid-stall, not a leak.
pub fn sink_status() -> Option<(u64, u64)> {
    let Some(Some(sink)) = FILE_SINK.get() else {
        return None;
    };
    let SinkMode::Queued(queue) = &sink.mode else {
        return None;
    };
    let counters = &queue.counters;
    let enqueued = counters.enqueued.load(Ordering::Acquire);
    let flushed = counters.flushed.load(Ordering::Acquire);
    Some((
        enqueued.saturating_sub(flushed),
        counters.dropped.load(Ordering::Relaxed),
    ))
}

/// Read every input file as text, parse the trailing `t=<n>` token of
/// each non-empty line, and return all lines sorted by parsed `n`
/// (stable on tie).  Lines that fail `t=` parsing sort to the end
/// with a synthetic timestamp of `u128::MAX`, preserving their
/// relative order.
pub fn merge_event_logs(paths: &[PathBuf]) -> Vec<String> {
    let mut lines: Vec<(u128, usize, String)> = Vec::new();
    let mut tie_breaker: usize = 0;
    for path in paths {
        let Ok(file) = File::open(path) else { continue };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            let t = parse_t(&line).unwrap_or(u128::MAX);
            lines.push((t, tie_breaker, line));
            tie_breaker += 1;
        }
    }
    lines.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    lines.into_iter().map(|(_, _, l)| l).collect()
}

fn parse_t(line: &str) -> Option<u128> {
    line.split_whitespace()
        .rev()
        .find_map(|tok| tok.strip_prefix("t="))
        .and_then(|n| n.parse::<u128>().ok())
}

/// Install a global tracing subscriber for production daemons (`lnsd`,
/// the helper bin, etc.) that combines a standard fmt layer with the
/// event-log layer when `LEVICULUM_EVENT_LOG` is set.  Unset → only
/// fmt installed; runtime overhead matches the previous standalone
/// `tracing_subscriber::fmt().init()` call.
///
/// `default_filter` is the env-filter directive used when `RUST_LOG`
/// is unset (e.g. `"info"`, `"debug"`, …).
///
/// # The fmt layer writes to stderr, not stdout
///
/// `tracing_subscriber`'s default writer is stdout, and stdout is the
/// data channel of every binary that installs this: `lnmsg` prints
/// message bodies there, `lncp` its progress, `lnprobe` its reply
/// lines.  A WARN on stdout splices a log line into that payload the
/// moment something goes wrong — which is exactly when a caller is
/// least able to cope with it.  `lnmsg`'s interop suite asserts a
/// byte-empty stdout for a successful send and went red under load for
/// precisely this reason (`CORE_PROCESSOR_OVER_BUDGET` from
/// `driver::processor::report_budget`).
///
/// Diagnostics therefore go to stderr; every consumer we have captures
/// both streams (periculum merges them into the node's daemon log,
/// systemd into the journal), so nothing downstream sees a difference.
/// Structured events are unaffected: they ride the [`EventLogLayer`]
/// into `LEVICULUM_EVENT_LOG`'s file, not either terminal stream.
pub fn install_global_subscriber(default_filter: &str) {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    let fmt_layer = fmt::layer()
        .compact()
        .with_writer(std::io::stderr)
        .with_filter(env_filter);
    if std::env::var(LOG_FILE_ENV_VAR).is_ok() {
        let _ = Registry::default().with(fmt_layer).with(layer()).try_init();
    } else {
        let _ = Registry::default().with(fmt_layer).try_init();
    }
}

/// [`install_global_subscriber`], but appending the fmt output to a file
/// instead of the terminal — service mode for a daemon whose reference
/// counterpart logs to `<configdir>/logfile` when run with `-s`
/// (`lxmd --service`, `reference/LXMF/LXMF/Utilities/lxmd.py:319-321`).
/// Falls back to [`install_global_subscriber`], i.e. to stderr, when the
/// file cannot be opened, because a daemon that silences itself over a
/// log-file permission error is undiagnosable.  The `eprintln!` below
/// says "logging to stderr" and that is now literally where the
/// fallback lands; keep the two in step if either moves.
///
/// The file writer itself is deliberately untouched by the stderr rule
/// in [`install_global_subscriber`]: a caller that named a path asked
/// for a file, and no data channel is involved.
pub fn install_global_subscriber_to_file(default_filter: &str, path: &std::path::Path) {
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(file) => file,
        Err(error) => {
            eprintln!(
                "event_log: could not open log file {} ({error}); logging to stderr",
                path.display()
            );
            install_global_subscriber(default_filter);
            return;
        }
    };
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    let writer = Mutex::new(file);
    let fmt_layer = fmt::layer()
        .compact()
        .with_ansi(false)
        .with_writer(writer)
        .with_filter(env_filter);
    if std::env::var(LOG_FILE_ENV_VAR).is_ok() {
        let _ = Registry::default().with(fmt_layer).with(layer()).try_init();
    } else {
        let _ = Registry::default().with(fmt_layer).try_init();
    }
}

/// The layer registered into the global subscriber chain.  Driven by
/// the active-handles list above.
pub struct EventLogLayer {
    active: Arc<Mutex<Vec<ActiveHandle>>>,
    init_time: Instant,
}

/// The `caller=` value of a violation line, `<basename>:<line>` or `?`.
///
/// Rendered where it is written, not built per event: every part of it is
/// in the callsite's `'static` metadata, and only a violation line, which a
/// correct emission site never produces, reads it at all. Until 509 every
/// event paid a `format!` for it (508's census, `EventLogLayer::caller`).
struct Caller<'a>(&'a Metadata<'a>);

impl std::fmt::Display for Caller<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.0.file(), self.0.line()) {
            (Some(file), Some(line)) => {
                let basename = file.rsplit('/').next().unwrap_or(file);
                write!(f, "{basename}:{line}")
            }
            _ => f.write_str("?"),
        }
    }
}

/// How many records this layer has visited, i.e. how many times it has
/// let a record walk its field visitor.
///
/// This is an instrument, not a statistic. A visit renders every field of
/// the record (into reused per-thread buffers since 509; until then a
/// `BTreeMap<String, String>` plus a `String` per field name and per
/// value), and the layer discards every record that turns out to carry no
/// `event = "..."` — so "how many visits produced nothing" is the number a
/// memory experiment on this sink has to be able to read. It is what
/// `heap-gap-bench` prints as `visits=` and what
/// `tests/event_log_callsite_filter.rs` asserts on.
///
/// Relaxed: nothing orders against it, and a reader wants the count, not a
/// position in anyone's history.
static VISITS: Counter64 = Counter64::new(0);

/// Records this layer has visited since process start.
///
/// A visit is one record walked by the field visitor (see the private
/// `VISITS` counter for what that costs). It is an instrument, not a statistic — `heap-gap-bench` prints
/// it as `visits=` and `tests/event_log_callsite_filter.rs` asserts on
/// it, because "how many visits produced nothing" is the number a churn
/// experiment on this sink has to be able to read.
pub fn visit_count() -> u64 {
    VISITS.load(Ordering::Relaxed)
}

thread_local! {
    /// Per-thread rendering buffers, reused across events (509). Every
    /// event used to build a `BTreeMap<String, String>`, a `String` per
    /// field name and value, a `String` for the caller and an owned line;
    /// 508 measured that at about three quarters of a field-loaded node's
    /// allocations. Per thread rather than under a lock because the
    /// emitting threads run concurrently and the buffers are only needed
    /// for the length of one `on_event`.
    static SCRATCH: RefCell<EventScratch> = RefCell::new(EventScratch::default());
}

impl EventLogLayer {
    /// Render one record and hand the line to every consumer.
    fn emit(&self, event: &Event<'_>, scratch: &mut EventScratch) {
        let EventScratch { fields, line } = scratch;
        fields.clear();
        event.record(fields);
        let Some(name_range) = fields.event_name.clone() else {
            return;
        };
        fields.sort();
        let t_ms = self.init_time.elapsed().as_millis();
        render_line(fields, node_name(), t_ms, line);
        let event_name = &fields.values[name_range];
        let caller = Caller(event.metadata());

        // Built once, only when a field broke the token contract, which a
        // correct emission site never does; they have no per-handle
        // component, so the file and every active buffer get the same text.
        let field_violation_lines: Vec<String> = fields
            .violations
            .iter()
            .map(|(field, problem)| field_violation_line(event_name, field, problem, &caller, t_ms))
            .collect();

        // Distribute to every active handle (test capture only; a
        // production daemon has none).  Per-handle:
        //   1. push the canonical line
        //   2. push one EVENT_FIELD_VIOLATION per offending field
        //   3. push one EVENT_SCHEMA_VIOLATION if the handle's
        //      catalogue (production + extra_schemas) declares this
        //      event with required keys absent from the record.
        let active = self.active.lock_recover();
        for handle in active.iter() {
            let mut buf = handle.buffer.lock_recover();
            buf.push(line.clone());

            for v in &field_violation_lines {
                buf.push(v.clone());
            }

            // A name may be catalogued under several shapes (e.g. the
            // per-reason field split of RNODE_TX_QUEUE_DROP, Codeberg
            // #320): the record passes if ANY declared shape is fully
            // present. The violation reports the nearest shape — the
            // one with the fewest missing keys — which for the common
            // single-shape event is just that shape's missing list.
            let mut satisfied = false;
            let mut nearest_missing: Option<Vec<&str>> = None;
            for s in EVENT_CATALOG
                .iter()
                .chain(handle.extra_schemas.iter())
                .filter(|s| s.name == event_name)
            {
                let missing: Vec<&str> = s
                    .required_keys
                    .iter()
                    .filter(|k| !fields.contains_key(k))
                    .copied()
                    .collect();
                if missing.is_empty() {
                    satisfied = true;
                    break;
                }
                match &nearest_missing {
                    Some(prev) if prev.len() <= missing.len() => {}
                    _ => nearest_missing = Some(missing),
                }
            }
            if !satisfied {
                if let Some(missing) = nearest_missing {
                    let v = format!(
                        "EVENT_SCHEMA_VIOLATION event={} missing=[{}] caller={} t={}",
                        event_name,
                        missing.join(","),
                        caller,
                        t_ms,
                    );
                    buf.push(v);
                }
            }
        }
        drop(active);

        // Process-wide append-only file (when LEVICULUM_EVENT_LOG is
        // set).  Production daemons + helper bin write here.  Schema
        // violations are per-handle so they don't appear in the file.
        // The sink copies the line into its queue; `line` stays this
        // thread's buffer for the next event.
        if let Some(sink) = file_sink(self.init_time) {
            for v in &field_violation_lines {
                sink.write(t_ms, v);
            }
            sink.write(t_ms, line);
        }
    }
}

impl<S: Subscriber> Layer<S> for EventLogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        VISITS.fetch_add(1, Ordering::Relaxed);
        // `try_with` and `try_borrow_mut`: a thread being torn down has
        // no thread-locals left, and a field whose `Debug` emits an event
        // of its own re-enters here with the buffers borrowed. Both get
        // fresh buffers rather than a panic inside the subscriber.
        let reused = SCRATCH.try_with(|cell| match cell.try_borrow_mut() {
            Ok(mut scratch) => {
                self.emit(event, &mut scratch);
                true
            }
            Err(_) => false,
        });
        if reused != Ok(true) {
            self.emit(event, &mut EventScratch::default());
        }
    }
}

/// The canonical line: `EVENT_NAME node=<n> k1=v1 ... t=<ms>`, `node=`
/// reserved first (from `LEVICULUM_EVENT_NODE`), the other fields in
/// byte order of their key, `t=` last. Written into `line`, which is
/// cleared first and keeps its capacity. `fields` must be sorted.
fn render_line(fields: &EventFields, node: &str, t_ms: u128, line: &mut String) {
    line.clear();
    if let Some(name) = fields.event_name.clone() {
        line.push_str(&fields.values[name]);
    }
    line.push(' ');
    line.push_str("node=");
    // The reserved prefix bypasses the visitor, so nothing else would
    // ever rescue it: a `LEVICULUM_EVENT_NODE` with a space in it
    // (an operator naming a node after the room it stands in) would
    // split EVERY line this process writes.
    line.push_str(node);
    for (key, value) in &fields.fields {
        // `node` from a tracing call would conflict with the
        // reserved prefix — env-var wins, user-supplied skipped.
        if *key == "node" {
            continue;
        }
        line.push(' ');
        line.push_str(key);
        line.push('=');
        line.push_str(&fields.values[value.clone()]);
    }
    // Writing to a String cannot fail.
    let _ = write!(line, " t={t_ms}");
}

fn field_violation_line(
    event_name: &str,
    field: &str,
    problem: &str,
    caller: &Caller<'_>,
    t_ms: u128,
) -> String {
    format!(
        "EVENT_FIELD_VIOLATION event={event_name} field={field} value_problem={problem} caller={caller} t={t_ms}",
    )
}

/// Fields whose value is a human/discovery-provided *name* rather than a
/// structured token.  Interface names legitimately carry whitespace
/// (auto-connect names a discovered node's interface after it, e.g.
/// `autoconnect/Dark Doodad 23`), so a space in one of these is expected
/// input, not a source-bug symptom.  Their values are still coerced into a
/// tokenizable scalar for the line, but the advisory
/// `EVENT_FIELD_VIOLATION` is suppressed so the legitimate case does not
/// flood the log or poison the self-alarm metric (Codeberg #113).  Genuinely
/// structured fields keep the detector (still catches freetext-leak bugs
/// like BUG-1).
fn is_name_field(field: &str) -> bool {
    matches!(
        field,
        "iface" | "iface_in" | "iface_out" | "in_iface" | "out_iface"
    )
}

/// Detect non-scalar values that would break a whitespace/`=`-token
/// parser.  Returns the kind of problem, or `None` if safe.
fn field_value_problem(value: &str) -> Option<&'static str> {
    for c in value.chars() {
        if c.is_ascii_whitespace() {
            return Some("whitespace");
        }
        if c == '=' {
            return Some("equals");
        }
        if !c.is_ascii_graphic() {
            return Some("non_printable");
        }
    }
    None
}

/// Display wrapper an EMISSION SITE uses for a value it knows can carry
/// text a user chose: an interface name from the config file
/// (`[[TCP Uplink]]`), a discovery-provided name
/// (`autoconnect/Dark Doodad 23`), a filesystem path with a space in it.
///
/// `iface = %Scalar(&self.name)` renders the name as the single token the
/// whitespace `key=value` parser needs, which is where that belongs: the
/// site knows the value is a name, the sink can only guess from the field
/// name (see `is_name_field`, and note that a field like `next_hop`
/// carries an interface name at one site and a hash at another, so the
/// guess cannot be made complete).
///
/// Substitution, not quoting: every consumer of this format — `jl`,
/// `jldiff`, the field-violation detector, and the `awk`/`grep` one-liners
/// the format exists for — splits on whitespace, so a quoted value with a
/// space would still be several tokens to all of them.  The mapping is the
/// same one `sanitize_scalar` applies as the sink's last-resort rescue,
/// so a value reads identically whichever produced it.
pub struct Scalar<'a>(pub &'a str);

impl std::fmt::Display for Scalar<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Character by character into the formatter rather than through
        // `sanitize_scalar`'s `String`: the formatter is the event layer's
        // reused buffer, so this costs nothing per event.
        for c in self.0.chars() {
            f.write_char(sanitized_char(c))?;
        }
        Ok(())
    }
}

/// Coerce a field value into a whitespace-free scalar so the canonical
/// line is ALWAYS tokenizable by the documented whitespace `key=val`
/// parser, no matter what an emission site passed.  Internal whitespace,
/// embedded `=`, and non-graphic bytes all become `_`.
///
/// This is the by-construction safety net behind the advisory
/// `EVENT_FIELD_VIOLATION`: the violation still fires (so the source bug
/// gets surfaced and fixed), but the emitted line is parseable regardless
/// (no stray bare tokens, no key collision from an embedded `=`).
fn sanitize_scalar(value: &str) -> String {
    value.chars().map(sanitized_char).collect()
}

/// One character of [`sanitize_scalar`]'s mapping.
fn sanitized_char(c: char) -> char {
    if c.is_ascii_whitespace() || c == '=' || !c.is_ascii_graphic() {
        '_'
    } else {
        c
    }
}

/// Reduce a `Debug` rendering to a scalar token: unwrap a single
/// `Some(...)` wrapper and strip the surrounding string-quote pair that
/// `Debug` adds to string-like values, so a value like `Some("373e…")`
/// renders as the bare `373e…` instead of leaking Rust Debug syntax into
/// the line.  `None` and other enum variants pass through unchanged
/// (they are already whitespace-free scalars; `None` is NOT collapsed to
/// empty because legitimate enum variants are also named `None`, e.g.
/// `PacketContext::None`).  Any residual whitespace/`=` is handled by
/// [`sanitize_scalar`] at record time.
///
/// Returns the scalar as a byte range of `raw`, so the caller can keep it
/// where it was rendered instead of copying it out.
fn normalize_debug(raw: &str) -> Range<usize> {
    let s = raw.trim();
    let s = s
        .strip_prefix("Some(")
        .and_then(|inner| inner.strip_suffix(')'))
        .unwrap_or(s);
    let s = s
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(s);
    // `s` is a subslice of `raw`, so its offset is the pointer difference.
    let start = s.as_ptr() as usize - raw.as_ptr() as usize;
    start..start + s.len()
}

/// One record's fields, rendered into reused storage: every value is a
/// range of `values`, every key the callsite's `&'static str`.
#[derive(Default)]
struct EventFields {
    /// Every value of the record, back to back. Ranges stay valid until
    /// the next `clear`.
    values: String,
    event_name: Option<Range<usize>>,
    /// Key and value range, in visit order until [`Self::sort`]. A key
    /// recorded twice keeps its last value, as the `BTreeMap` this
    /// replaced did.
    fields: Vec<(&'static str, Range<usize>)>,
    /// Field name and problem, in visit order.
    violations: Vec<(&'static str, &'static str)>,
}

impl EventFields {
    fn clear(&mut self) {
        self.values.clear();
        self.event_name = None;
        self.fields.clear();
        self.violations.clear();
    }

    /// Keys in byte order, the order the canonical line writes them in.
    /// Keys are unique after `record`, so an unstable sort is exact.
    fn sort(&mut self) {
        self.fields.sort_unstable_by_key(|(key, _)| *key);
    }

    fn contains_key(&self, key: &str) -> bool {
        self.fields.iter().any(|(k, _)| *k == key)
    }

    /// File the value that occupies `values[value]` under `field`.
    fn record(&mut self, field: &Field, mut value: Range<usize>) {
        if field.name() == "event" {
            self.event_name = Some(value);
            return;
        }
        // BUG-3: a non-scalar value is reported AND sanitized, so the
        // canonical line stays well-formed by construction even when an
        // emission site passes whitespace or an embedded `=`.
        //
        // #113: name-type fields (interface names) legitimately carry
        // whitespace, so they are coerced SILENTLY -- sanitized for the
        // line, but no violation, to keep the false-positive flood out of
        // the log.  Structured fields keep the detector.
        if let Some(problem) = field_value_problem(&self.values[value.clone()]) {
            if !is_name_field(field.name()) {
                self.violations.push((field.name(), problem));
            }
            value = self.sanitize_in_place(value);
        }
        match self.fields.iter_mut().find(|(k, _)| *k == field.name()) {
            Some(slot) => slot.1 = value,
            None => self.fields.push((field.name(), value)),
        }
    }

    /// Append the [`sanitize_scalar`] form of `values[value]` to `values`
    /// and return its range. Only reached for a value that broke the token
    /// contract.
    fn sanitize_in_place(&mut self, value: Range<usize>) -> Range<usize> {
        let start = self.values.len();
        let mut i = value.start;
        while i < value.end {
            let Some(c) = self.values[i..value.end].chars().next() else {
                break;
            };
            i += c.len_utf8();
            self.values.push(sanitized_char(c));
        }
        start..self.values.len()
    }

    /// Render `value` with `Display` into `values` and file it.
    fn record_display(&mut self, field: &Field, value: impl std::fmt::Display) {
        let start = self.values.len();
        let _ = write!(self.values, "{value}");
        let end = self.values.len();
        self.record(field, start..end);
    }
}

impl Visit for EventFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        let start = self.values.len();
        self.values.push_str(value);
        let end = self.values.len();
        self.record(field, start..end);
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record_display(field, value);
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record_display(field, value);
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record_display(field, value);
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.record_display(field, value);
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // BUG-2: emit a bare scalar (e.g. `373e…`), not Rust Debug
        // wrapper syntax (`Some("373e…")`). `trim_matches('"')` used to
        // strip the quotes that kept a Debug string parseable, leaking
        // its spaces into the line; `normalize_debug` unwraps the
        // wrapper instead. The rendering stays where it was written;
        // the stripped wrapper is dead bytes until the next `clear`.
        let start = self.values.len();
        let _ = write!(self.values, "{value:?}");
        let inner = normalize_debug(&self.values[start..]);
        self.record(field, start + inner.start..start + inner.end);
    }
}

/// The two reused buffers one thread renders events with.
#[derive(Default)]
struct EventScratch {
    fields: EventFields,
    line: String,
}

#[cfg(test)]
mod tests {
    //! Byte identity of the 509 rendering path against the one it replaced.
    //!
    //! `legacy` is the pre-509 visitor and line builder, verbatim apart from
    //! taking `Metadata` for the caller. Every catalogued event is rendered
    //! through both with the same recorded values, across every value kind
    //! the visitor treats differently, and the lines must be equal to the
    //! byte. Readers of the format (periculum's `FrameTally`, the trace
    //! tool, `jl`) are untouched by 509 because of this test.

    use super::*;
    use tracing::callsite::{Callsite, Identifier};
    use tracing::field::FieldSet;
    use tracing::Level;

    mod legacy {
        use super::super::{field_value_problem, is_name_field, sanitize_scalar};
        use std::collections::BTreeMap;
        use tracing::field::{Field, Visit};
        use tracing::Metadata;

        pub(super) fn caller(meta: &Metadata<'_>) -> String {
            match (meta.file(), meta.line()) {
                (Some(f), Some(l)) => {
                    let basename = f.rsplit('/').next().unwrap_or(f);
                    format!("{basename}:{l}")
                }
                _ => "?".to_string(),
            }
        }

        fn normalize_debug(raw: &str) -> String {
            let s = raw.trim();
            let s = s
                .strip_prefix("Some(")
                .and_then(|inner| inner.strip_suffix(')'))
                .unwrap_or(s);
            s.strip_prefix('"')
                .and_then(|inner| inner.strip_suffix('"'))
                .unwrap_or(s)
                .to_string()
        }

        #[derive(Default)]
        pub(super) struct EventVisitor {
            pub(super) event_name: Option<String>,
            pub(super) fields: BTreeMap<String, String>,
            pub(super) field_violations: Vec<(String, &'static str)>,
        }

        impl EventVisitor {
            fn record(&mut self, field: &Field, value: String) {
                if field.name() == "event" {
                    self.event_name = Some(value);
                    return;
                }
                let value = match field_value_problem(&value) {
                    Some(problem) => {
                        if !is_name_field(field.name()) {
                            self.field_violations
                                .push((field.name().to_string(), problem));
                        }
                        sanitize_scalar(&value)
                    }
                    None => value,
                };
                self.fields.insert(field.name().to_string(), value);
            }
        }

        impl Visit for EventVisitor {
            fn record_str(&mut self, field: &Field, value: &str) {
                self.record(field, value.to_string());
            }
            fn record_bool(&mut self, field: &Field, value: bool) {
                self.record(field, value.to_string());
            }
            fn record_i64(&mut self, field: &Field, value: i64) {
                self.record(field, value.to_string());
            }
            fn record_u64(&mut self, field: &Field, value: u64) {
                self.record(field, value.to_string());
            }
            fn record_f64(&mut self, field: &Field, value: f64) {
                self.record(field, value.to_string());
            }
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                self.record(field, normalize_debug(&format!("{value:?}")));
            }
        }

        /// The pre-509 `on_event` body up to the line and the field
        /// violation lines.
        pub(super) fn render(
            visitor: &EventVisitor,
            node: &str,
            t_ms: u128,
            caller: &str,
        ) -> Option<(String, Vec<String>)> {
            let event_name = visitor.event_name.as_ref()?;
            let mut line = String::with_capacity(64);
            line.push_str(event_name);
            line.push(' ');
            line.push_str("node=");
            line.push_str(node);
            for (k, v) in &visitor.fields {
                if k == "node" {
                    continue;
                }
                line.push(' ');
                line.push_str(k);
                line.push('=');
                line.push_str(v);
            }
            line.push_str(&format!(" t={t_ms}"));
            let violations = visitor
                .field_violations
                .iter()
                .map(|(field, problem)| {
                    format!(
                        "EVENT_FIELD_VIOLATION event={} field={} value_problem={} caller={} t={}",
                        event_name, field, problem, caller, t_ms,
                    )
                })
                .collect();
            Some((line, violations))
        }
    }

    struct TestCallsite;
    static TEST_CALLSITE: TestCallsite = TestCallsite;
    impl Callsite for TestCallsite {
        fn set_interest(&self, _: Interest) {}
        fn metadata(&self) -> &Metadata<'_> {
            unreachable!("the test callsite is never registered")
        }
    }

    /// A hash as the emission sites print one: `%HexShort(&hash)`.
    struct Hash(&'static [u8]);
    impl std::fmt::Display for Hash {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            leviculum_core::hex_fmt::write_hex(f, self.0)
        }
    }

    const HASH: &[u8] = &[
        0x37, 0x3e, 0x00, 0xff, 0x12, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0x9a, 0xbc,
        0xde,
    ];

    /// Number of value kinds `record_kind` knows.
    const KINDS: usize = 18;

    /// Record one value of kind `kind` under `field` into `v`: every path
    /// the visitor distinguishes, the clean ones and the ones that trip
    /// normalisation or the token-contract rescue.
    fn record_kind(v: &mut dyn Visit, field: &Field, kind: usize) {
        match kind {
            0 => v.record_debug(field, &tracing::field::display(Hash(HASH))),
            1 => v.record_u64(field, 42),
            2 => v.record_i64(field, -7),
            3 => v.record_bool(field, true),
            4 => v.record_str(field, "lora0"),
            5 => v.record_debug(field, &Some("373e")),
            6 => v.record_debug(field, &None::<u8>),
            7 => v.record_f64(field, 1.5),
            8 => v.record_str(field, "autoconnect/Dark Doodad 23"),
            9 => v.record_debug(field, &"a=b"),
            10 => v.record_str(field, "Café"),
            11 => v.record_debug(field, &tracing::field::display("x y")),
            12 => v.record_u64(field, u64::MAX),
            13 => v.record_i64(field, i64::MIN),
            14 => v.record_debug(field, &format_args!("  padded  ")),
            15 => v.record_debug(field, &Some(Some("x"))),
            16 => v.record_str(field, ""),
            _ => v.record_debug(
                field,
                &tracing::field::display(leviculum_core::constants::MTU),
            ),
        }
    }

    fn leak_names(names: Vec<&'static str>) -> &'static [&'static str] {
        Box::leak(names.into_boxed_slice())
    }

    fn metadata(fields: FieldSet) -> &'static Metadata<'static> {
        Box::leak(Box::new(Metadata::new(
            "test event",
            "leviculum_std::event_log::tests",
            Level::DEBUG,
            Some("leviculum-std/src/event_log.rs"),
            Some(4242),
            None,
            fields,
            tracing::metadata::Kind::EVENT,
        )))
    }

    /// Render `names` (with `event` first) through both paths, every key
    /// getting kind `(i + variant) % KINDS`, and assert identity.
    fn assert_identical(event: &'static str, names: &'static [&'static str], variant: usize) {
        let set = FieldSet::new(names, Identifier(&TEST_CALLSITE));
        let meta = metadata(FieldSet::new(names, Identifier(&TEST_CALLSITE)));
        let mut old = legacy::EventVisitor::default();
        let mut new = EventFields::default();
        // Dirty the reused buffers first: identity has to hold for the
        // n-th event a thread renders, not only for its first.
        new.values
            .push_str("stale \"junk\" from the previous event");
        new.fields.push(("zzz", 0..5));
        new.violations.push(("zzz", "whitespace"));
        new.event_name = Some(0..5);
        new.clear();
        for (i, name) in names.iter().enumerate() {
            let field = set.field(name).expect("field in its own set");
            if *name == "event" {
                old.record_str(&field, event);
                new.record_str(&field, event);
                continue;
            }
            let kind = (i + variant) % KINDS;
            record_kind(&mut old, &field, kind);
            record_kind(&mut new, &field, kind);
        }
        let t_ms = 123_456_789u128;
        let (old_line, old_violations) =
            legacy::render(&old, "probe", t_ms, &legacy::caller(meta)).expect("event name");
        new.sort();
        let mut line = String::from("left over from an earlier, longer line");
        render_line(&new, "probe", t_ms, &mut line);
        let caller = Caller(meta);
        let name = &new.values[new.event_name.clone().expect("event name")];
        let new_violations: Vec<String> = new
            .violations
            .iter()
            .map(|(f, p)| field_violation_line(name, f, p, &caller, t_ms))
            .collect();
        assert_eq!(line, old_line, "{event} variant {variant}");
        assert_eq!(new_violations, old_violations, "{event} variant {variant}");
        // The schema check reads the key set; it must be the same set.
        let old_keys: Vec<&str> = old.fields.keys().map(String::as_str).collect();
        let new_keys: Vec<&str> = new.fields.iter().map(|(k, _)| *k).collect();
        assert_eq!(new_keys, old_keys, "{event} variant {variant}");
    }

    #[test]
    fn every_catalogued_event_renders_byte_identical_to_the_pre_509_path() {
        let mut lines = 0;
        for schema in EVENT_CATALOG {
            let mut names = vec!["event"];
            names.extend(schema.required_keys.iter().copied());
            let names = leak_names(names);
            for variant in 0..KINDS {
                assert_identical(schema.name, names, variant);
                lines += 1;
            }
        }
        assert_eq!(lines, EVENT_CATALOG.len() * KINDS);
    }

    /// The shapes the catalogue does not exercise: a `node` field from the
    /// call site, a name field with a space, an extra key past the required
    /// ones, keys out of order, and fields recorded before `event`.
    #[test]
    fn reserved_name_and_unordered_fields_render_byte_identical() {
        let shapes: [&[&'static str]; 4] = [
            &["event", "node", "dst", "hops"],
            &["zeta", "iface", "event", "alpha", "next_hop"],
            &["event", "iface_in", "iface_out", "note", "a", "B", "_x"],
            &["event"],
        ];
        for names in shapes {
            let names = leak_names(names.to_vec());
            for variant in 0..KINDS {
                assert_identical("SHAPE_PROBE", names, variant);
            }
        }
    }

    /// A key recorded twice keeps its last value, as the map did.
    #[test]
    fn a_key_recorded_twice_keeps_the_last_value() {
        let names = leak_names(vec!["event", "k"]);
        let set = FieldSet::new(names, Identifier(&TEST_CALLSITE));
        let ev = set.field("event").expect("event");
        let k = set.field("k").expect("k");
        let mut old = legacy::EventVisitor::default();
        let mut new = EventFields::default();
        for v in [&mut old as &mut dyn Visit, &mut new] {
            v.record_str(&ev, "DUP");
            v.record_str(&k, "first value");
            v.record_u64(&k, 2);
        }
        new.sort();
        let mut line = String::new();
        render_line(&new, "n", 1, &mut line);
        let (old_line, _) = legacy::render(&old, "n", 1, "?").expect("name");
        assert_eq!(line, old_line);
        assert_eq!(line, "DUP node=n k=2 t=1");
        // Both recorded the violation of the first value.
        assert_eq!(new.violations, vec![("k", "whitespace")]);
    }

    #[test]
    fn scalar_display_matches_sanitize_scalar() {
        for s in [
            "lora0",
            "autoconnect/Dark Doodad 23",
            "a=b",
            "Café",
            "a\tb\nc",
            "",
        ] {
            assert_eq!(format!("{}", Scalar(s)), sanitize_scalar(s));
        }
    }

    #[test]
    fn a_metadata_without_file_or_line_renders_a_question_mark() {
        let names = leak_names(vec!["event"]);
        let meta = Box::leak(Box::new(Metadata::new(
            "test event",
            "t",
            Level::DEBUG,
            None,
            None,
            None,
            FieldSet::new(names, Identifier(&TEST_CALLSITE)),
            tracing::metadata::Kind::EVENT,
        )));
        assert_eq!(Caller(meta).to_string(), legacy::caller(meta));
        assert_eq!(Caller(meta).to_string(), "?");
    }
}

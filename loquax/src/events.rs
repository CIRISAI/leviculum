//! The structured event log.
//!
//! `docs/src/concepts/loquax-architecture.md` §7 asks for one line per protocol
//! transition in the `EVENT_NAME key=val t=<ms>` form of
//! `docs/src/structured-event-logs.md`, and calls it cheap rather than
//! speculative. It is: the emit sites are `tracing::debug!` calls carrying an
//! `event` field, and `leviculum_std::event_log::install_global_subscriber`
//! turns `LEVICULUM_EVENT_LOG=<path>` into an append-only file of exactly that
//! format — the same switch `lnsd` honours, so one merged timeline can hold
//! the daemon's packets and the messenger's message states side by side.
//!
//! Each event name below has a matching entry in
//! `leviculum_std::event_log::EVENT_CATALOG`, which is what makes a missing
//! key a loud `EVENT_SCHEMA_VIOLATION` line rather than a quiet gap.

use alloc::format;
use alloc::string::{String, ToString};

use crate::address::to_hex;

/// Make a value safe for the whitespace-tokenised parser the log format
/// assumes: no space, no `=`, nothing non-printable.
///
/// The fields that can carry text a user chose are the instance name, an
/// outcome word and the sender's display name — and an instance name comes
/// from a config file we do not control, while a display name is whatever an
/// operator typed after `--from`. Substituting rather than dropping keeps the
/// field present, which is what the schema check wants to see.
///
/// The mapping itself lives in `leviculum_core::event_scalar::Scalar`, which
/// `leviculum_std::event_log` re-exports beside the sink that would otherwise
/// have to rescue the value: one definition, so the two cannot drift into
/// rendering the same name differently, and one that needs no `std`. The
/// tests below stay here because they state what THIS crate needs from it.
fn scalar(text: &str) -> String {
    leviculum_core::event_scalar::Scalar(text).to_string()
}

/// The name this run will announce itself under, and which resolution step
/// produced it.
///
/// Emitted before anything can fail, because "the recipient saw the wrong
/// sender" is a complaint that arrives long after the run: the source word
/// separates a cron job that fell through to the last resort from an
/// interactive run that read `$USER`, which the message itself cannot.
pub fn sender(name: &str, source: &str) {
    tracing::debug!(
        event = "LNMSG_SENDER",
        from = %scalar(name),
        source = %scalar(source),
    );
}

/// Attached to a shared instance and registered an LXMF delivery destination.
/// `address` is our own address, i.e. what a reply would be sent to.
pub fn attached(instance: &str, address: &[u8; 16]) {
    tracing::debug!(
        event = "LNMSG_ATTACHED",
        instance = %scalar(instance),
        address = %to_hex(address),
    );
}

/// The destination became reachable: its identity is known and a path exists.
pub fn resolved(destination: &[u8; 16], waited_ms: u64) {
    tracing::debug!(
        event = "LNMSG_RESOLVED",
        dst = %to_hex(destination),
        waited_ms = waited_ms,
    );
}

/// The router accepted the message. This is the event the brief asks for as a
/// minimum, and since 2026-08-21 its `id` field is the only place the message
/// id appears: a successful run prints nothing, so `loquax status <id>` is
/// reachable only for someone who ran with `LEVICULUM_EVENT_LOG` set.
pub fn enqueued(message_id: &[u8; 32], destination: &[u8; 16], bytes: usize, via: &str) {
    tracing::debug!(
        event = "LNMSG_ENQUEUED",
        id = %to_hex(message_id),
        dst = %to_hex(destination),
        bytes = bytes,
        via = %scalar(via),
    );
}

/// One outbound state transition, as the router reported it.
pub fn state(message_id: &[u8; 32], state: &str) {
    tracing::debug!(
        event = "LNMSG_STATE",
        id = %to_hex(message_id),
        state = %scalar(state),
    );
}

/// The `--via auto` decision, exactly once per send: which method carried
/// the message in the end, and why. `reason` is `link-delivery` when the
/// direct leg won, and the fallback reason word from
/// [`crate::send::fallback_reason`] when the mailbox leg ran.
pub fn via(method: &str, reason: &str) {
    tracing::debug!(
        event = "LNMSG_VIA",
        method = %scalar(method),
        reason = %scalar(reason),
    );
}

/// The propagation node this run settled on. `source` is where the choice
/// came from (`flag`, `config` or `adopted`, the adoption rule; `mailbox` before 1153); `cost` is the stamp cost the
/// node announced, `-` while it has not been heard yet.
pub fn pn(node: &[u8; 16], source: &str, cost: Option<u64>) {
    tracing::debug!(
        event = "LNMSG_PN",
        pn = %to_hex(node),
        source = %scalar(source),
        cost = %cost.map(|c| c.to_string()).unwrap_or_else(|| "-".to_string()),
    );
}

/// One inbound message during a fetch. `dup` says it was suppressed because
/// an earlier run already delivered it (the cross-run seen store, not the
/// router's own in-run de-duplication).
pub fn fetched(message_id: &[u8; 32], source: &[u8; 16], bytes: usize, dup: bool) {
    tracing::debug!(
        event = "LNMSG_FETCHED",
        id = %to_hex(message_id),
        src = %to_hex(source),
        bytes = bytes,
        dup = dup,
    );
}

/// A fetch's closing line: what the node handed over, how much of it the
/// router had already seen this run, and how many messages were new to the
/// user across runs.
pub fn sync_done(received: usize, duplicates: usize, new: usize) {
    tracing::debug!(
        event = "LNMSG_SYNC_DONE",
        received = received,
        duplicates = duplicates,
        new = new,
    );
}

/// The last line of a run: what happened, and the exit code that follows from
/// it. `id` is `-` when the run ended before a message existed.
pub fn done(message_id: Option<&[u8; 32]>, outcome: &str, code: u8) {
    tracing::debug!(
        event = "LNMSG_DONE",
        id = %message_id.map(|id| to_hex(id)).unwrap_or_else(|| "-".to_string()),
        outcome = %scalar(outcome),
        code = code,
    );
}

/// The last line of a fetch that found no path to its node: [`done`]'s keys,
/// plus how many path requests the drain sent and over how long, so the
/// reader of a red cell does not have to count `PKT_TX` lines
/// (leviculum#719).
pub fn done_without_path(code: u8, path_requests: u32, path_wait_ms: u64) {
    tracing::debug!(
        event = "LNMSG_DONE",
        id = "-",
        outcome = "failed",
        code = code,
        path_requests = path_requests,
        path_wait_ms = path_wait_ms,
    );
}

/// The history store refused a write or a read (`loquax::store`): what is on
/// screen is not on disk. The screen says so too; this is the line a reader of
/// the log greps for.
pub fn store_failed(detail: &str) {
    tracing::warn!(event = "LNMSG_STORE_FAILED", detail = %scalar(detail));
}

// The home's own record (1305, study `docs/drafts/mailbox-for-friends-study-
// 2026-11.md` section 6, "structured events on the home"): what a home
// accepted, proved, synced and announced, so the N-home cells count per home
// what today's free-text `HOME ...` lines only describe. Identities and
// message ids are their first eight hex characters, as `jl` shortens them.

/// A home kept a received message in its archive, new or held already:
/// `msg` its id, `from` the sender, `via` how it came (`direct` over a link,
/// `opportunistic` as one packet, `propagated` from a propagation node),
/// `home` the identity the home answers for.
pub fn home_accept(msg: &[u8; 32], from: &[u8; 16], via: &str, home: &[u8; 16]) {
    tracing::info!(
        event = "HOME_ACCEPT",
        msg = %id8(msg),
        from = %short_hex(from),
        via = %via,
        home = %short_hex(home),
    );
}

/// A home released the delivery proof of an archived message (1239: only
/// once it is archived). Not said for a propagated one, which has no proof.
pub fn home_prove(msg: &[u8; 32], home: &[u8; 16]) {
    tracing::info!(
        event = "HOME_PROVE",
        msg = %id8(msg),
        home = %short_hex(home),
    );
}

/// One exchange of archive entries with another home of the identity:
/// `pulled` the entries taken in, `pushed` the ones handed over, `ratchets`
/// whether both hold the same ring after it (`same`) or not (`differ`).
/// Today's only such exchange is the move (1281); the N-home sync of study
/// section 2.5 says the same line.
pub fn home_sync(peer_home: &[u8; 16], pulled: usize, pushed: usize, ratchets: &str) {
    tracing::info!(
        event = "HOME_SYNC",
        peer_home = %short_hex(peer_home),
        pulled = pulled,
        pushed = pushed,
        ratchets = %ratchets,
    );
}

/// A home announced the identity: `kind` the destination (`delivery`, the
/// LXMF address; `position`, `lev.position`), `iface` where it went
/// (`all` for a broadcast on every interface, the only form today).
pub fn home_announce(home: &[u8; 16], iface: &str, kind: &str) {
    tracing::info!(
        event = "HOME_ANNOUNCE",
        home = %short_hex(home),
        iface = %iface,
        kind = %kind,
    );
}

fn id8(id: &[u8; 32]) -> String {
    let mut hex = to_hex(&id[..4]);
    hex.truncate(8);
    hex
}

// The debug log (leviculum 1002). Not structured events: plain `info` lines
// for `${LNMSG_HOME}/tui.log`, which the TUI writes at `info` for loquax's own
// targets, so a send that waits can be read back as what each leg did. No
// `event` field, so they never reach the event-log file and need no catalog
// entry; the `LNMSG_*` lines above stay the machine-readable record.

/// One step of one send leg: `SEND leg=direct|propagated to=<8 hex>
/// state=<word> elapsed_ms=<since the leg began>`. The state words are
/// `path_wait`, `queued`, `sending`, `sent`, `proof`, `no_proof` and
/// `failed`. Returns the line it logged, for the runner's
/// [`crate::effects::Effects::step`].
pub fn send_leg(
    leg: crate::send::Leg,
    to: &[u8; 16],
    state: &str,
    elapsed: core::time::Duration,
) -> String {
    let line = format!(
        "SEND leg={} to={} state={} elapsed_ms={}",
        leg.as_str(),
        short_hex(to),
        state,
        elapsed.as_millis()
    );
    tracing::info!(target: "loquax::send", "{line}");
    line
}

/// A destination's path arrived: `PATH found to=<8 hex> hops=<n|?>
/// requests=<path requests sent> wait_ms=<since the first>`.
pub fn path_found(to: &[u8; 16], hops: Option<u8>, requests: u32, wait_ms: u64) {
    tracing::info!(
        target: "loquax::send",
        "PATH found to={} hops={} requests={} wait_ms={}",
        short_hex(to),
        hops.map(|h| h.to_string()).unwrap_or_else(|| "?".to_string()),
        requests,
        wait_ms
    );
}

/// A send's budget ended with no path to its destination.
pub fn path_not_found(to: &[u8; 16], waited: core::time::Duration) {
    tracing::info!(
        target: "loquax::send",
        "PATH not_found to={} wait_ms={}",
        short_hex(to),
        waited.as_millis()
    );
}

/// A received message's sender is not known, so its path is asked for:
/// `VERIFY path_request src=<8 hex> held=<messages waiting on its key>
/// requests=<asked so far>`.
pub fn verify_path_request(source: &[u8; 16], held: usize, requests: u32) {
    tracing::info!(
        target: "loquax::verify",
        "VERIFY path_request src={} held={} requests={}",
        short_hex(source),
        held,
        requests
    );
}

/// A held message checked against its sender's key, now known: `VERIFY
/// result src=<8 hex> id=<8 hex> verdict=valid|invalid`.
pub fn verify_result(source: &[u8; 16], message_id: &[u8; 32], valid: bool) {
    let mut id = to_hex(message_id);
    id.truncate(8);
    tracing::info!(
        target: "loquax::verify",
        "VERIFY result src={} id={} verdict={}",
        short_hex(source),
        id,
        if valid { "valid" } else { "invalid" }
    );
}

/// How the link to a hub of the Rooms stands (1652): `linking`, `ready`,
/// or why it is down (`no_path`, `no_answer`, `lost`, `node_gone`,
/// `left`). Before 1652 the rooms' code logged nothing, and a phone's
/// trace could not tell "never asked" from "connecting".
pub fn rooms_link(hub: &[u8; 16], state: &str) {
    tracing::debug!(
        event = "ROOMS_LINK",
        hub = %to_hex(hub),
        state = %scalar(state),
    );
}

/// The hub's WELCOME (1652), with the version it gave (`-` for none).
pub fn rooms_welcome(hub: &[u8; 16], version: Option<&str>) {
    tracing::debug!(
        event = "ROOMS_WELCOME",
        hub = %to_hex(hub),
        version = %scalar(version.unwrap_or("-")),
    );
}

/// How many rooms a hub named (1652): `via` is `link` for the chosen
/// hub's `/list` after its WELCOME, `peek` for a server sheet's question;
/// `answered` is false when the hub gave no list in time and only its
/// greeting was read.
pub fn rooms_list(hub: &[u8; 16], rooms: usize, via: &str, answered: bool) {
    tracing::debug!(
        event = "ROOMS_LIST",
        hub = %to_hex(hub),
        rooms = rooms,
        via = %scalar(via),
        answered = answered,
    );
}

/// A join asked of the hub and how it went (1652): `ok` when the hub said
/// we are in, not when it refused or the session would not send it.
pub fn rooms_join(room: &str, ok: bool) {
    tracing::debug!(
        event = "ROOMS_JOIN",
        room = %scalar(room),
        ok = ok,
    );
}

/// The server "suggested" marks (1653, walk 8, 41), at a start and
/// whenever it moves: `hub` the one or `-`, `why` `marked`, `in_use` (a
/// hub in force marks none) or `none_fits` (no named shipped hub heard
/// that answers), `fits` how many could carry the mark.
pub fn rooms_suggest(hub: Option<&[u8; 16]>, why: &str, fits: usize) {
    tracing::debug!(
        event = "ROOMS_SUGGEST",
        hub = %hub.map_or_else(|| String::from("-"), |h| to_hex(h)),
        why = %scalar(why),
        fits = fits,
    );
}

/// A page the browser asked for and how it went (1653, walk 8, 40: the
/// places sheet closed once with "Link fehlgeschlagen" and nobody could
/// tell a tap from a link dropped while idle): `state` `asked`, `ok`,
/// `failed` or `aborted` (1667: another ask took its place, `why`
/// `superseded`, or the browser stopped, `stopped`), `why` the failure in
/// English or `-`.
pub fn browse_fetch(dest: &[u8; 16], path: &str, state: &str, why: &str) {
    tracing::debug!(
        event = "BROWSE_FETCH",
        dest = %to_hex(dest),
        path = %scalar(path),
        state = %scalar(state),
        why = %scalar(why),
    );
}

/// Explore came to the front and whether it asked the mailbox for its list
/// (1667, walk 11, finding 63: a walk could not tell an ask that did not
/// fire from one that fired unseen): `ask` `yes` or `no`, `reason`
/// `asked`, `recent` (an ask began `since_ms` ago, under the minute) or
/// `no_mailbox` (the device told of no mailbox link).
pub fn explore_open(ask: bool, reason: &str, since_ms: Option<u64>) {
    tracing::debug!(
        event = "EXPLORE_OPEN",
        ask = if ask { "yes" } else { "no" },
        reason = %scalar(reason),
        since_ms = %since_ms.map_or_else(|| String::from("-"), |ms| ms.to_string()),
    );
}

/// The first eight hex characters of an address, as the log lines carry it.
fn short_hex(address: &[u8; 16]) -> String {
    let mut hex = to_hex(address);
    hex.truncate(8);
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_replaces_everything_the_parser_would_choke_on() {
        assert_eq!(scalar("my instance"), "my_instance");
        assert_eq!(scalar("a=b"), "a_b");
        assert_eq!(scalar("tab\there"), "tab_here");
        assert_eq!(scalar("nul\0"), "nul_");
        assert_eq!(scalar("plain-name_1"), "plain-name_1");
    }

    /// Non-ASCII is one `_` per character rather than per byte: the log is a
    /// diagnostic, and a field of fixed width per input character is easier to
    /// eyeball than one whose length depends on the encoding.
    #[test]
    fn non_ascii_is_substituted_per_character() {
        assert_eq!(scalar("Grüße"), "Gr__e");
    }
}

//! The home node and its clients (`docs/src/concepts/loquax-home-node.md`).
//!
//! One always-on loquax, the home node, holds the LXMF identity and keeps
//! every message it receives in an archive with a sequence. The user's other
//! devices are its clients: each has an RNS identity of its own, is admitted
//! once with a single-use token, and pulls the archive by cursor over a link
//! to the home node's own destination, `loquax.home`. Fetching never deletes.
//!
//! - [`code`]: the admission code `loquax mailbox admit` prints.
//! - [`ask`]: a client's ask without the waiting, the path ladder and the
//!   join's steps, which `client` and the M9 drive alike.
//! - [`board`]: the M9's executor of an ask over a core node, the session
//!   that backs off between asks, and the pull batch.
//! - [`record`]: what makes a board a client, one record in flash.
//! - `admit`: opening one, on a computer's mailbox and a phone's alike.
//! - [`wire`]: the five requests, `/join`, `/sync`, `/send`, `/mark` and
//!   `/watch`, their answers, and the wake.
//! - [`wake`]: which watching link is owed a wake, on the home node.
//! - [`relation`]: which announced names the home node archives.
//! - [`about`]: what a mailbox says of itself, `/about` (1642).
//! - [`pages`]: the NomadNet pages the home heard and the identity's
//!   bookmarks, `/pages` and `/bookmark` (1579).
//! - `watch`: a client's watching link, kept up while it runs.
//! - [`outgoing`]: the home node's outbox policy, direct at once, a
//!   propagated copy on failure, direct retries for seven days.
//! - `server`: how the home node answers them, from the store.
//! - `client`: how a client asks, over the driver's link API.
//! - `daemon`: `loquax --daemon`, the home node itself.
//! - `lock`: one registrar of the delivery destination per home.
//! - `rendezvous`: the public node both ends dial, one at a time, with
//!   failover (`docs/src/concepts/loquax-reachability.md`).
//! - `setup`: `loquax mailbox setup`, the five steps of the wizard.

use alloc::string::String;

use leviculum_core::{Destination, DestinationType, Direction, Identity};

pub mod about;
#[cfg(feature = "history")]
pub mod admit;
pub mod ask;
pub mod backup;
pub mod board;
pub mod code;
pub mod keepalive;
pub mod outgoing;
pub mod pages;
pub mod record;
pub mod relation;
pub mod wake;
pub mod wire;

#[cfg(feature = "history")]
pub mod census;
#[cfg(feature = "std")]
pub mod client;
#[cfg(feature = "history")]
pub mod daemon;
#[cfg(feature = "history")]
pub mod group;
#[cfg(feature = "std")]
pub mod handover;
#[cfg(feature = "history")]
pub mod hub_rooms;
#[cfg(feature = "std")]
pub mod lock;
#[cfg(feature = "std")]
pub mod phone_server;
#[cfg(feature = "qr")]
pub mod qr;
#[cfg(feature = "std")]
pub mod rendezvous;
#[cfg(feature = "history")]
pub mod runner;
#[cfg(feature = "history")]
pub mod server;
#[cfg(feature = "std")]
pub mod setup;
#[cfg(feature = "std")]
pub mod upstream;
#[cfg(feature = "std")]
pub mod watch;

/// The network the phone has now, as the platform names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkKind {
    /// None at all: flight mode, or out of reach of any.
    None,
    /// A WLAN.
    Wifi,
    /// Mobile data.
    Mobile,
    /// Something else that carries IP (Ethernet, a VPN, Waydroid's bridge).
    #[default]
    Other,
}

impl NetworkKind {
    /// The word the platform hands in, `none`, `wifi`, `mobile` or anything
    /// else for [`NetworkKind::Other`].
    pub fn from_word(word: &str) -> Self {
        match word {
            "none" => Self::None,
            "wifi" => Self::Wifi,
            "mobile" => Self::Mobile,
            _ => Self::Other,
        }
    }

    /// The word for logs, the inverse of [`NetworkKind::from_word`].
    pub fn word(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Wifi => "wifi",
            Self::Mobile => "mobile",
            Self::Other => "other",
        }
    }
}

/// The home destination's app name and aspect: `loquax.home`, a SINGLE
/// destination of the home identity. A client learns its key from the
/// admission code and asks for its path on every connect.
///
/// It is announced, on the delivery destination's cadence (1196). Until
/// then it was not, and that cost reachability: a relay searches for a path
/// it does not know only when the request came in on an access point,
/// gateway or roaming interface (`reference/Reticulum/RNS/Transport.py:2918`),
/// so a mailbox two relays behind the phone's entry node was out of reach.
/// With the announce every relay on the way holds the path. It tells the
/// network no more than the identity's public key, which the delivery
/// announce publishes already, and that this identity runs a Loquax
/// mailbox.
pub const APP_NAME: &str = "loquax";
/// See [`APP_NAME`].
pub const ASPECT: &str = "home";

/// Whether the home compresses an answer that travels as a resource.
/// Never: a board client (the M9, an LNode) links no bz2 decompressor and
/// refuses a compressed resource with an RCL the moment it is advertised,
/// so a page past one packet never reached it (1459, the M9 on the bench
/// of 2026-11-06: 17 asks, 17 advertisements, 17 cancels). Every client
/// takes an uncompressed one. The cost is the serial page's airtime at
/// most: a radio page is one packet and never a resource.
pub const ANSWERS_COMPRESSED: bool = false;

/// The admission request: token and device name in, a status out.
pub const JOIN_PATH: &str = "/join";
/// The pull: the client's cursor in, the archive entries after it out.
pub const SYNC_PATH: &str = "/sync";
/// A message to send: the client's own id and the message in, the message
/// id out. The home node signs it, keeps it in its outbox and delivers.
pub const SEND_PATH: &str = "/send";

/// A client's word on messages, read or deleted: the flag and the message
/// ids in, a status out. The home node keeps it as an archive entry, which
/// every client applies on its next pull.
pub const MARK_PATH: &str = "/mark";

/// A client's petname for a peer (1139): the peer and the name in, an
/// empty name clearing it, a status out. The home node keeps it as an
/// archive entry ([`KIND_PETNAME`]), which every client applies on its next
/// pull, so the reader's name for a peer is the same on each device.
pub const PETNAME_PATH: &str = "/petname";

/// A client's word on a peer and its conversation (1146): the peer and its
/// [`wire::PeerState`] in, a status out. Kept as an archive entry
/// ([`KIND_PEERSTATE`]) as a petname is, so every device of the home blocks,
/// pins, archives and mutes the same people.
pub const PEERSTATE_PATH: &str = "/peerstate";

/// A client keeps its link: the cursor in, the newest sequence out, and a
/// wake over the same link whenever the archive grows ([`wire::wake`]).
/// A home node from before it answers nothing, and its clients pull on
/// their schedule as they always did.
pub const WATCH_PATH: &str = "/watch";

/// A contact's public key the reader took from its contact code (1187,
/// [`crate::lxma`]): the peer's address and the key in, a status out. The
/// home node checks the address against the key and remembers the
/// identity, so its first message to the peer goes out before any announce
/// was heard. Nothing is archived: the transport's known destinations keep
/// it. A home node from before it refuses the path, and the message waits
/// for the peer's announce as it always did.
pub const KEY_PATH: &str = "/key";

/// A move to another device (1281, [`handover`]): the move token and the
/// cursor in, a page of the archive with each entry's id out, beside the
/// identity's private key and its delivery ratchets. Answered only to an
/// admitted device that joined with the token of the move the owner
/// opened on the mailbox itself.
pub const MOVE_PATH: &str = "/move";

/// The new device holds everything up to the cursor (1281): the token and
/// the cursor in, a status out. `Ok` ends the mailbox here, for good until
/// its owner takes it back; [`wire::Status::Behind`] when the archive grew
/// past the cursor, which the new device takes with `/move` first.
pub const MOVED_PATH: &str = "/moved";

/// 1307: make a group the home hosts and invite its members.
pub const GROUP_PATH: &str = "/group";

/// 1307 G2: add members to a group the home hosts (study 4.5).
pub const GROUP_ADD_PATH: &str = "/groupadd";

/// 1307 pass four: the admin deletes a group the home hosts, for everyone.
pub const GROUP_DELETE_PATH: &str = "/groupdelete";

/// The NomadNet pages the home heard and the identity's bookmarks that
/// changed after a stamp (1579, [`pages`]): the device's stamp in, the
/// rows after it out. Not archive entries: a page row changes on every
/// hour of announces and a mesh holds thousands, so they keep a table and
/// a stamp of their own and never move a message cursor.
pub const PAGES_PATH: &str = "/pages";

/// A bookmark a device set or cleared (1579, [`pages::BookmarkRow`]): the
/// row in, a status out. The home keeps the newer by stamp, and every
/// device of the identity gets it with its next `/pages`.
pub const BOOKMARK_PATH: &str = "/bookmark";

/// What the mailbox says of itself (1642, [`about`]): nothing in, its
/// name, its machine, its version and its devices out, to an admitted
/// device. A home from before it has no handler and stays silent.
pub const ABOUT_PATH: &str = "/about";

/// The archive entry kind of a received message, packed as it came.
pub const KIND_RECEIVED: u8 = 0;
/// A message a client handed in with `/send`, packed as the home node
/// signed it.
pub const KIND_SENT: u8 = 1;
/// A propagation node took a copy of a sent message. This and the kinds
/// below carry the message id as their bytes, nothing else.
pub const KIND_PROPAGATED: u8 = 2;
/// A direct delivery proof came for a sent message.
pub const KIND_DELIVERED: u8 = 3;
/// The retries of a sent message ended with no propagation node ever
/// taking it and no proof: it never left the home node.
pub const KIND_FAILED: u8 = 4;
/// The direct retries of a propagated message ended without a proof; the
/// copy on the propagation node may still reach the recipient.
pub const KIND_RETRIES_ENDED: u8 = 5;
/// A client's `/mark`: its bytes are the request as the client sent it, a
/// flag and the message ids ([`wire::Mark`]).
pub const KIND_MARK: u8 = 6;
/// A display name the home node heard a peer announce, appended when it
/// differs from the last one it appended for that peer: a client is a leaf
/// that hears no announces itself. Its bytes are the peer's address and
/// the name as announced ([`wire::name_entry`]); the entry's time is when
/// the home node heard it.
pub const KIND_NAME: u8 = 7;
/// A client's `/petname`: its bytes are the request as the client sent it,
/// the peer's address and the reader's name for it, empty for none
/// ([`wire::petname_entry`]). Only a peer's newest is kept, and like a name
/// it wakes nobody: it rides along with the next pull.
pub const KIND_PETNAME: u8 = 8;
/// A message an import brought onto the home node from another app's
/// history (1150): its bytes are the row as the store keeps it, origin and
/// peer first (`crate::import::archive`), never a signed message. A client
/// writes it into its store as imported history.
pub const KIND_IMPORTED: u8 = 9;
/// A client's `/peerstate`: its bytes are the request as the client sent
/// it ([`wire::peer_state_entry`]). Only a peer's newest is kept, and it
/// wakes nobody, as a petname does.
pub const KIND_PEERSTATE: u8 = 10;
/// A leg of a sent message went [`outgoing::STALL_SECS`] without a sign
/// and was given up (1293); the message is tried again. Its bytes are the
/// message id, and a message keeps one such entry, the first. A client from
/// before it skips it as a kind it cannot show.
pub const KIND_STALLED: u8 = 11;
/// A member's message to a group this home hosts (1307,
/// `group`), packed as it came: the group's history and the source of
/// its copies. The owner's devices get the owner's own copy as a received
/// message, so a client skips this kind.
pub const KIND_GROUP: u8 = 12;
/// What a propagation node answered about messages this home sent through
/// it (1409, `docs/src/concepts/loquax-propagation-receipts.md` section
/// B): the home asked `/lev/status` and keeps the node's word per message
/// as a map of message id to word ([`wire::pn_status_entry`]). Every client
/// folds it into its marks as a device that asked itself does. Like a seen
/// mark it carries only what moved: `collected`, `delivered`, `expired`
/// and `evicted`. A client from before it skips it as a kind it cannot
/// show.
pub const KIND_PN_STATUS: u8 = 13;
/// A hub the home heard announcing (1599, `crate::ui::hubs`): its bytes
/// are [`wire::hub_entry`], the hub's address, name, hops and first and
/// last hearing. Only a hub's newest is kept, at most
/// [`crate::ui::hubs::HEARD_BOUND`] hubs, the one heard longest ago going
/// first, and a hub heard again is appended again only when its name or
/// hops changed or [`HUB_REFRESH_SECS`] passed. Like a name it wakes
/// nobody. A client from before it skips it as a kind it cannot show.
pub const KIND_HUB: u8 = 14;

/// How long a hub heard again goes before its entry is appended again with
/// the later hearing (1599): a hub announces about every few minutes, and
/// a device needs its last seen to the hour, not each announce.
pub const HUB_REFRESH_SECS: i64 = 3600;

/// The word `loquax mailbox archive` prints for an entry kind (1305); a
/// kind this program does not know is its number.
pub fn kind_word(kind: u8) -> alloc::borrow::Cow<'static, str> {
    alloc::borrow::Cow::Borrowed(match kind {
        KIND_RECEIVED => "received",
        KIND_SENT => "sent",
        KIND_PROPAGATED => "propagated",
        KIND_DELIVERED => "delivered",
        KIND_FAILED => "failed",
        KIND_RETRIES_ENDED => "retries-ended",
        KIND_MARK => "mark",
        KIND_NAME => "name",
        KIND_PETNAME => "petname",
        KIND_IMPORTED => "imported",
        KIND_PEERSTATE => "peerstate",
        KIND_STALLED => "stalled",
        KIND_GROUP => "group",
        KIND_PN_STATUS => "pn-status",
        KIND_HUB => "hub",
        other => return alloc::borrow::Cow::Owned(alloc::format!("{other}")),
    })
}

/// What a client shows for a state entry of a message it sent through the
/// home node: the router state its bubble takes, `None` for an entry that
/// changes nothing on screen (direct retries that ended while a propagated
/// copy waits). The sent entry is `Sent`: the message left the device and
/// the home node has it, as a router's `Sent` says a hop took it.
pub fn shown_state(kind: u8) -> Option<leviculum_lxmf::router::MessageState> {
    use leviculum_lxmf::router::MessageState;
    match kind {
        KIND_SENT => Some(MessageState::Sent),
        KIND_PROPAGATED => Some(MessageState::AwaitingCollection),
        KIND_DELIVERED => Some(MessageState::Delivered),
        KIND_FAILED => Some(MessageState::Failed),
        _ => None,
    }
}

/// The word `loquax fetch` prints for an archive entry of a message sent
/// through the home node; `None` for a received message.
pub fn sent_word(kind: u8) -> Option<&'static str> {
    Some(match kind {
        KIND_SENT => "accepted",
        KIND_PROPAGATED => "propagated",
        KIND_DELIVERED => "delivered",
        KIND_FAILED => "failed",
        KIND_RETRIES_ENDED => "retries-ended",
        KIND_STALLED => "stalled",
        _ => return None,
    })
}

/// How long an admission token stays good, by default: a day, time to
/// carry the code to the other device, short enough that a code left in a
/// chat log or a screenshot is dead by the next. `home_token_hours` in the
/// config changes it.
pub const DEFAULT_TOKEN_HOURS: u64 = 24;

/// The archive's default cap, in megabytes: 1 GiB. Twice the 500 MB a stock
/// lxmd propagation node keeps by default
/// (`reference/LXMF/LXMF/Utilities/lxmd.py:159-163`), which stores everyone's
/// mail; this archive is one person's. A text message packs to well under a
/// kilobyte, so the default is about a million messages, and an always-on
/// home host (a small board, a VPS) has the gigabyte to spare. A machine
/// with less sets `archive_cap_mb` in loquax's config.
pub const DEFAULT_ARCHIVE_CAP_MB: u64 = 1024;

/// The most packed bytes one `/sync` answer carries before it says that more
/// follow. One entry larger than this still goes, alone. 64 KiB is a
/// resource of a few hundred parts at the TCP MDU, quick on a LAN or the
/// internet, which are the media a home node's clients use in this slice.
pub const SYNC_PAGE_BYTES: usize = 64 * 1024;

/// An admission token's life in seconds, as the config sets it.
pub fn token_ttl_secs(config: &crate::config::Config) -> i64 {
    let hours = config.home_token_hours.unwrap_or(DEFAULT_TOKEN_HOURS);
    i64::try_from(hours.saturating_mul(3600)).unwrap_or(i64::MAX)
}

/// The outbox's timing, as the config sets it.
pub fn policy(config: &crate::config::Config) -> outgoing::Policy {
    let secs = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
    outgoing::Policy {
        retry_secs: secs(
            config
                .outbox_retry_secs
                .unwrap_or(outgoing::DEFAULT_RETRY_SECS),
        ),
        horizon_secs: secs(
            config
                .outbox_days
                .unwrap_or(outgoing::DEFAULT_DAYS)
                .saturating_mul(24 * 3600),
        ),
        stall_secs: secs(outgoing::STALL_SECS),
        answer_secs: secs(outgoing::ANSWER_SECS),
    }
}

/// The home destination of `identity`: `loquax.home`, inbound when the
/// identity holds its private key (the home node), outbound otherwise.
pub fn destination(identity: Identity) -> Result<Destination, String> {
    let direction = match identity.has_private_keys() {
        true => Direction::In,
        false => Direction::Out,
    };
    Destination::new(
        Some(identity),
        direction,
        DestinationType::Single,
        APP_NAME,
        &[ASPECT],
    )
    .map_err(|e| alloc::format!("home destination: {e:?}"))
}

/// The LXMF delivery address of the identity whose public key is
/// `public_key`: what the client shows as its own address, and what every
/// message in the archive is addressed to.
pub fn lxmf_address(public_key: &[u8; 64]) -> Result<[u8; 16], String> {
    let identity = Identity::from_public_key_bytes(public_key)
        .map_err(|e| alloc::format!("home identity: {e:?}"))?;
    Destination::new(
        Some(identity),
        Direction::Out,
        DestinationType::Single,
        leviculum_lxmf::node::APP_NAME,
        &[leviculum_lxmf::node::DELIVERY_ASPECT],
    )
    .map(|destination| *destination.hash().as_bytes())
    .map_err(|e| alloc::format!("delivery destination: {e:?}"))
}

/// The hash of the home destination of the identity whose public key is
/// `public_key`.
pub fn destination_hash(public_key: &[u8; 64]) -> Result<[u8; 16], String> {
    let identity = Identity::from_public_key_bytes(public_key)
        .map_err(|e| alloc::format!("home identity: {e:?}"))?;
    destination(identity).map(|destination| *destination.hash().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lxmf_address_matches_the_one_the_engine_registers() {
        let identity = Identity::generate(&mut rand_core::OsRng);
        let registered = crate::engine::delivery_address(&identity).expect("address");
        assert_eq!(
            lxmf_address(&identity.public_key_bytes()).expect("address"),
            registered
        );
    }

    #[test]
    fn the_home_destination_is_the_same_from_either_side() {
        let identity = Identity::generate(&mut rand_core::OsRng);
        let public = identity.public_key_bytes();
        let inbound = destination(identity).expect("in");
        assert_eq!(
            *inbound.hash().as_bytes(),
            destination_hash(&public).expect("out")
        );
    }
}

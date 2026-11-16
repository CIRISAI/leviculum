//! The reader's word on a conversation besides its messages (1146): pin,
//! mute, archive, block, and the peer's identity verified in person.
//!
//! Pin, mute, archive and block are one [`PeerState`], set whole: on a
//! client of a home node every change goes to the home node as well, which
//! hands it to the home's other devices on their next pull, so the laptop
//! and the phone hide, pin and silence the same people. Verified stays on
//! the device that compared the key.
//!
//! A muted conversation counts its unread and raises nothing: no toast
//! here, no notification on the phone. An archived one leaves the list for
//! the archived view; a new message brings it back unless it is muted, as
//! Signal does. A blocked one is hidden as the palette's `block` hides it
//! ([`super::contacts`]).

use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;

use leviculum_ui::toast::ToastKind;

use leviculum_ui::delivery::DeliveryState;

use super::{UiEffect, UiModel};
use crate::home::wire::PeerState;
use crate::names::Trust;
use crate::when::WallTime;

/// A mute that does not end: [`PeerState::muted_until`] for "always".
pub const MUTED_ALWAYS: i64 = i64::MAX;

/// What this device knows of how it reaches a peer, for a contact screen
/// (1146). A client of a home node holds no path of its own: the home node
/// routes, so hops and interface are not here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContactFacts {
    /// When the newest message from the peer arrived, as its bubble says.
    pub last_received: Option<WallTime>,
    /// When the newest message to the peer was written, and how it went:
    /// delivered directly, left at a propagation node, or its state.
    pub last_sent: Option<(Option<WallTime>, &'static str)>,
    /// The reader's word as the home's devices share it, and verified.
    pub state: PeerState,
    pub verified: bool,
}

/// How a sent message went, in the words of a contact screen.
fn route_words(state: DeliveryState) -> &'static str {
    match state {
        DeliveryState::Arrived => "delivered directly",
        DeliveryState::InMailbox => "left at a propagation node",
        other => super::state_words(other),
    }
}

impl<B> UiModel<B> {
    /// Whether `peer`'s conversation is muted now. Before the clock is
    /// known a timed mute still holds.
    pub fn muted(&self, peer: &[u8; 16]) -> bool {
        let Some(until) = self
            .conversations
            .iter()
            .find(|c| c.peer == *peer)
            .and_then(|c| c.flags.muted_until)
        else {
            return false;
        };
        self.now.is_none_or(|now| now.unix_secs < until)
    }

    /// Whether the reader marked `peer`'s identity as verified.
    pub fn verified(&self, peer: &[u8; 16]) -> bool {
        self.conversations
            .iter()
            .any(|c| c.peer == *peer && c.flags.verified)
    }

    /// The reader's word on `peer` as the home's devices share it.
    pub fn peer_state(&self, peer: &[u8; 16]) -> PeerState {
        let conversation = self.conversations.iter().find(|c| c.peer == *peer);
        PeerState {
            blocked: self.names.trust(peer) == Trust::Blocked,
            pinned: conversation.is_some_and(|c| c.pinned),
            archived: conversation.is_some_and(|c| c.flags.archived),
            muted_until: conversation.and_then(|c| c.flags.muted_until),
        }
    }

    /// What this device knows of `peer` for a contact screen.
    pub fn contact_facts(&self, peer: &[u8; 16]) -> ContactFacts {
        let messages = self
            .conversations
            .iter()
            .find(|c| c.peer == *peer)
            .map_or(&[][..], |c| c.messages.as_slice());
        let last_received = messages
            .iter()
            .rev()
            .find(|m| !m.outbound)
            .and_then(|m| m.at);
        let last_sent = messages
            .iter()
            .rev()
            .find(|m| m.outbound && !m.is_aside())
            .and_then(|m| m.delivery.map(|state| (m.at, route_words(state))));
        ContactFacts {
            last_received,
            last_sent,
            state: self.peer_state(peer),
            verified: self.verified(peer),
        }
    }

    /// The archived conversations the list leaves out, in the list's order:
    /// those the archived view shows.
    pub fn archived_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.conversations.len())
            .filter(|&i| {
                let c = &self.conversations[i];
                c.flags.archived && self.names.trust(&c.peer) != Trust::Blocked
            })
            .collect();
        self.count(super::counters::Op::Sorts, 1);
        self.count(super::counters::Op::Sorted, order.len());
        order.sort_by_key(|&i| core::cmp::Reverse(self.conversations[i].activity));
        order
    }

    /// Make `state` the reader's word on `peer`, set here: kept, and on a
    /// client of a home node handed to the home node too.
    pub(super) fn set_peer_state(&mut self, peer: [u8; 16], state: PeerState) -> Vec<UiEffect> {
        if self.peer_state(&peer) == state {
            return Vec::new();
        }
        let effects = self.apply_peer_state(peer, state);
        if self.home_client {
            // Owed until the home node takes it (1640), as a petname is:
            // kept in the store, handed again after a restart.
            self.owed_peer_states.set(peer, state);
        }
        effects
    }

    /// A peer state one of the home's devices set, pulled, and when the
    /// home took it (`at`, the home's clock): taken here, never handed
    /// back.
    ///
    /// Not over a newer word of this device's (1640): the rules of a
    /// petname ([`super::owed`]). While this device owes the home a state
    /// for the peer, a pulled one is older, a pin the reader loosened among
    /// them; once the home took it, until the next pull ends, a different
    /// one counts only if the home took it after the answer came.
    pub(super) fn home_peer_state(
        &mut self,
        peer: [u8; 16],
        state: PeerState,
        at: i64,
    ) -> Vec<UiEffect> {
        if !self.owed_peer_states.pulled(&peer, &state, at) {
            return Vec::new();
        }
        if self.peer_state(&peer) == state {
            return Vec::new();
        }
        self.apply_peer_state(peer, state)
    }

    fn apply_peer_state(&mut self, peer: [u8; 16], state: PeerState) -> Vec<UiEffect> {
        let mut effects = Vec::new();
        let mut contact = self.names.contacts.of(&peer);
        if contact.blocked != state.blocked {
            contact.blocked = state.blocked;
            self.names.contacts.set(&contact);
            effects.push(UiEffect::Contact(contact));
        }
        let exists = self.conversations.iter().any(|c| c.peer == peer);
        let kept = state.pinned || state.archived || state.muted_until.is_some();
        if exists || kept {
            let index = self.conversation(peer);
            let conversation = &mut self.conversations[index];
            conversation.pinned = state.pinned;
            conversation.flags.archived = state.archived;
            conversation.flags.muted_until = state.muted_until;
            self.changed_peers.insert(peer);
        }
        // A conversation that left the list does not stay open.
        self.settle();
        effects
    }

    /// The reader verified `peer`'s identity in person, or takes it back.
    /// Kept on this device only.
    pub(super) fn set_verified(&mut self, peer: [u8; 16], verified: bool) {
        let index = self.conversation(peer);
        if self.conversations[index].flags.verified == verified {
            return;
        }
        self.conversations[index].flags.verified = verified;
        self.changed_peers.insert(peer);
        let label = self.names.label(&peer, self.lang);
        let text = match verified {
            true => format!("{label} verified"),
            false => format!("{label} no longer verified"),
        };
        self.toast(ToastKind::Info, text);
    }

    /// Mark `peer`'s conversation unread here: a badge of one when it had
    /// none. Nothing goes to the home node, as Signal keeps it local.
    pub(super) fn mark_unread(&mut self, peer: [u8; 16]) {
        let Some(conversation) = self.conversations.iter_mut().find(|c| c.peer == peer) else {
            return;
        };
        if conversation.unread > 0 {
            return;
        }
        conversation.unread = 1;
        self.changed_peers.insert(peer);
    }

    /// Mark `peer`'s conversation read without opening it: the same marks
    /// opening it sends.
    pub(super) fn mark_read(&mut self, peer: [u8; 16]) {
        if let Some(index) = self.conversations.iter().position(|c| c.peer == peer) {
            if let Some(conversation) = self.conversations.get_mut(index) {
                conversation.new_rule = None;
            }
            self.read_out(index);
        }
    }

    /// A message arrived in conversation `index`: an archived one comes back
    /// to the list unless it is muted. Whether to raise anything for it.
    pub(super) fn arrived(&mut self, index: usize) -> bool {
        let peer = self.conversations[index].peer;
        let muted = self.muted(&peer);
        if !muted && self.conversations[index].flags.archived {
            let state = PeerState {
                archived: false,
                ..self.peer_state(&peer)
            };
            self.set_peer_state(peer, state);
        }
        !muted
    }

    /// The peer states this device owes the home node, the newest of each
    /// peer, for the shell to hand it; it answers with
    /// [`super::UiEvent::PeerStatesHanded`]. Nothing while a hand is under
    /// way, so the home takes a peer's states in the order they were set
    /// (1640).
    pub fn take_peer_states(&mut self) -> Vec<([u8; 16], PeerState)> {
        self.owed_peer_states.take()
    }

    /// A hand ended (1640): what the home took is owed no more, unless the
    /// reader set another state for the peer meanwhile; what it did not
    /// take goes with the next hand.
    pub(super) fn peer_states_handed(&mut self, taken: Vec<([u8; 16], PeerState)>) {
        let now = self.now.map_or(0, |at| at.unix_secs);
        self.owed_peer_states.handed(taken, now);
    }

    /// The peer states the store kept as owed (1640), at the start: handed
    /// again with the first hand, and the reader's word over what the
    /// first pull brings.
    pub fn restore_owed_peer_states(
        &mut self,
        owed: alloc::collections::BTreeMap<[u8; 16], PeerState>,
    ) {
        self.owed_peer_states.restore(owed);
    }

    /// The palette's `archive`: the open conversation into the archive or
    /// back.
    pub(super) fn toggle_archive(&mut self) -> Vec<UiEffect> {
        let Some(peer) = self.conversations.get(self.selected).map(|c| c.peer) else {
            self.toast(ToastKind::Info, "no conversation here".to_string());
            return Vec::new();
        };
        let mut state = self.peer_state(&peer);
        state.archived = !state.archived;
        let label = self.names.label(&peer, self.lang);
        let text = match state.archived {
            true => format!("archived {label}: :archived lists it"),
            false => format!("{label} is back in the list"),
        };
        let effects = self.set_peer_state(peer, state);
        self.toast(ToastKind::Info, text);
        effects
    }

    /// The palette's `archived`: the archived conversations, or the others.
    pub(super) fn toggle_archived_view(&mut self) {
        self.show_archived = !self.show_archived;
        self.settle();
        let text = match (self.show_archived, self.order().len()) {
            (true, 0) => "nothing is archived".to_string(),
            (true, n) => format!("{n} archived, :archived returns"),
            (false, _) => "the conversations again".to_string(),
        };
        self.toast(ToastKind::Info, text);
    }
}

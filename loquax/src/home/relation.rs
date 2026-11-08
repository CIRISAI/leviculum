//! Which announced names the home node archives (1136): only those of a
//! peer the user has a relation to. The home node hangs on a transport that
//! hears the whole network's LXMF announces; archiving each of them grew
//! the archive by one entry an announce and handed every client names of
//! strangers. A relation is a conversation in the archive (a message from
//! the peer, or one sent to it), a `/send` an admitted device handed in for
//! it, or a petname in the home node's contacts file.
//!
//! The newest [`HEARD_CAP`] names heard are remembered, so a conversation
//! that starts with a peer heard lately archives its name at once. Pure:
//! the daemon (`super::daemon`) feeds it the announces and the relations
//! and appends what it names.

use alloc::collections::BTreeSet;
use alloc::string::String;
use leviculum_core::bounded_map::BoundedMap;

/// How many strangers' names are remembered, the newest heard (order
/// 1585): 4096.
///
/// Every name was remembered for the whole run, and a home on a transport
/// hears the whole network's LXMF announces: on miauhaus the node beside
/// it knew 80 000 destinations, and the map only grew. A peer whose name
/// fell out is named by its next announce, which every peer sends at least
/// every few hours; 4096 is more distinct announcers than a busy public
/// mesh shows in that time, at about half a megabyte.
pub const HEARD_CAP: usize = 4096;

/// The peers the user has a relation to, and the newest name heard from
/// each of the [`HEARD_CAP`] peers heard last, related or not.
pub struct Relations {
    related: BTreeSet<[u8; 16]>,
    heard: BoundedMap<[u8; 16], String>,
}

impl Default for Relations {
    fn default() -> Self {
        Self::new([])
    }
}

impl Relations {
    /// The relations the store and the contacts file name at start.
    pub fn new(related: impl IntoIterator<Item = [u8; 16]>) -> Self {
        Self {
            related: related.into_iter().collect(),
            heard: BoundedMap::new(HEARD_CAP),
        }
    }

    /// `peer` announced `name`: the name to archive when the user has a
    /// relation to the peer, `None` for a stranger, whose name is only
    /// remembered.
    pub fn announced(&mut self, peer: [u8; 16], name: &str) -> Option<String> {
        self.heard.insert(peer, String::from(name));
        self.related.contains(&peer).then(|| String::from(name))
    }

    /// Relations held and names remembered, with their estimated heap
    /// bytes, for the census (order 1585).
    pub fn census(&self) -> (usize, usize, usize) {
        use leviculum_core::heap_census::{btree_bytes_model, btree_set_bytes};
        let names: usize = self.heard.values().map(String::capacity).sum();
        // The map's entry and its order index's (sequence and key).
        let heard = btree_bytes_model(
            self.heard.len(),
            16 + core::mem::size_of::<(u64, String)>() + 8 + 16,
        ) + names;
        (
            self.related.len(),
            self.heard.len(),
            btree_set_bytes(&self.related) + heard,
        )
    }

    /// A conversation with `peer` began or went on: the newest name heard
    /// from it, to archive at once, when the relation is new.
    pub fn relate(&mut self, peer: [u8; 16]) -> Option<String> {
        if !self.related.insert(peer) {
            return None;
        }
        self.heard.get(&peer).cloned()
    }

    /// Whether the user has a relation to `peer`.
    pub fn is_related(&self, peer: &[u8; 16]) -> bool {
        self.related.contains(peer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRIEND: [u8; 16] = [1; 16];
    const STRANGER: [u8; 16] = [2; 16];

    #[test]
    fn a_stranger_s_name_is_remembered_and_archived_only_once_a_conversation_starts() {
        let mut relations = Relations::new([FRIEND]);
        assert_eq!(
            relations.announced(FRIEND, "Friend").as_deref(),
            Some("Friend")
        );
        assert_eq!(relations.announced(STRANGER, "Stranger"), None);
        assert_eq!(relations.announced(STRANGER, "Stranger 2"), None);
        assert!(!relations.is_related(&STRANGER));
        // The first message: the newest name heard, at once.
        assert_eq!(relations.relate(STRANGER).as_deref(), Some("Stranger 2"));
        assert_eq!(relations.relate(STRANGER), None, "only the first time");
        assert_eq!(
            relations.announced(STRANGER, "Known").as_deref(),
            Some("Known")
        );
    }

    /// Order 1585: the names of strangers are held to [`HEARD_CAP`], the
    /// newest heard; a relation's name heard last is still there.
    #[test]
    fn the_names_heard_are_held_to_the_cap_newest_first() {
        let mut relations = Relations::new([FRIEND]);
        let _ = relations.announced(STRANGER, "Old");
        for n in 0..HEARD_CAP as u32 + 10 {
            let mut peer = [0xA0; 16];
            peer[..4].copy_from_slice(&n.to_be_bytes());
            let _ = relations.announced(peer, "someone");
        }
        let (related, heard, bytes) = relations.census();
        assert_eq!((related, heard), (1, HEARD_CAP));
        assert!(bytes > 0);
        // The oldest name went: a conversation with it archives none yet.
        assert_eq!(relations.relate(STRANGER), None);
        let mut newest = [0xA0; 16];
        newest[..4].copy_from_slice(&(HEARD_CAP as u32 + 9).to_be_bytes());
        assert_eq!(relations.relate(newest).as_deref(), Some("someone"));
    }

    #[test]
    fn a_new_relation_to_a_peer_never_heard_archives_nothing_yet() {
        let mut relations = Relations::default();
        assert_eq!(relations.relate(FRIEND), None);
        assert!(relations.is_related(&FRIEND));
    }
}

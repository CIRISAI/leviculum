//! The model the board shows before it has a network (slice four, 1361):
//! a handful of conversations built in code, `no_std`, the same on the
//! board and on the host. The host's goldens of slices one and two draw
//! the ugly store (`loquax/tests/support/ugly_store.rs`), which is a store
//! on a disk and signed messages, neither of which the board has yet; so
//! the board draws this model, the host pins its frames as goldens of
//! their own (`tests/firmware.rs`), and the rig compares the board's dump
//! with those.
//!
//! Everything is a function of the constants here: the same frames on
//! every run, on every target. The clock stands still at [`NOW`], since
//! the board has no wall time without a host or a fix.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use leviculum_ui::delivery::DeliveryState;
use loquax::ui::history::{Draft, Flags, Restored};
use loquax::ui::{update, Message, Pane, Signature, UiEvent, UiModel};
use loquax::when::WallTime;

use crate::field::Field;
use crate::keys::{event, M9Key};

/// The fixture's clock: 2026-11-03 12:00 CET.
pub const NOW: WallTime = WallTime {
    unix_secs: 1_793_703_600,
    offset_secs: 3600,
};

/// The reader's own address.
pub const OWN: [u8; 16] = [0x30; 16];

/// One conversation: its peer's seed, the peer's name, pinned, unread,
/// and how many messages it holds.
struct Conversation {
    seed: u8,
    name: &'static str,
    pinned: bool,
    unread: usize,
    messages: usize,
}

/// The list as it reads top down once the model orders it: the pinned one
/// first, then the newest. More than the 11 rows one screen holds, so a
/// cursor scrolls the list.
const CONVERSATIONS: [Conversation; 13] = [
    Conversation {
        seed: 0x42,
        name: "Jürgen",
        pinned: true,
        unread: 0,
        messages: 16,
    },
    Conversation {
        seed: 0x43,
        name: "Zoë Ærø",
        pinned: false,
        unread: 2,
        messages: 6,
    },
    Conversation {
        seed: 0x44,
        name: "Mum",
        pinned: false,
        unread: 0,
        messages: 5,
    },
    Conversation {
        seed: 0x45,
        name: "Field team",
        pinned: false,
        unread: 1,
        messages: 4,
    },
    Conversation {
        seed: 0x46,
        name: "Ünal",
        pinned: false,
        unread: 0,
        messages: 3,
    },
    Conversation {
        seed: 0x47,
        name: "Peer 7",
        pinned: false,
        unread: 0,
        messages: 3,
    },
    Conversation {
        seed: 0x48,
        name: "Hütte",
        pinned: false,
        unread: 0,
        messages: 2,
    },
    Conversation {
        seed: 0x49,
        name: "Βασίλης",
        pinned: false,
        unread: 0,
        messages: 2,
    },
    Conversation {
        seed: 0x4a,
        name: "Rig lnsd",
        pinned: false,
        unread: 0,
        messages: 2,
    },
    Conversation {
        seed: 0x4b,
        name: "Ольга",
        pinned: false,
        unread: 0,
        messages: 2,
    },
    Conversation {
        seed: 0x4c,
        name: "Peer 12",
        pinned: false,
        unread: 0,
        messages: 1,
    },
    Conversation {
        seed: 0x4d,
        name: "Solar node",
        pinned: false,
        unread: 0,
        messages: 1,
    },
    Conversation {
        seed: 0x4e,
        name: "Ivan",
        pinned: false,
        unread: 0,
        messages: 1,
    },
];

/// What the messages say, in turn.
const BODIES: [&str; 8] = [
    "Are you at the hut tonight?",
    "Yes, from six. Bring the spare antenna, the SMA one.",
    "Grüße aus dem Tal! Der Weg über die Brücke ist frei.",
    "ok",
    "The repeater on the ridge answered at SF8, two hops.",
    "Can you read me on the M9 now? The keyboard works.",
    "Später mehr, der Akku ist bei 20 %.",
    "Bis gleich.",
];

/// A sent message's rung, by its place: every rung of the ladder shows in
/// the pinned chat.
const RUNGS: [DeliveryState; 5] = [
    DeliveryState::Delivered,
    DeliveryState::Read { at: 0 },
    DeliveryState::InMailbox,
    DeliveryState::Failed,
    DeliveryState::Sent,
];

/// The fixture's peer address of conversation seed `seed`.
pub fn peer(seed: u8) -> [u8; 16] {
    let mut peer = [seed; 16];
    peer[15] = !seed;
    peer
}

fn at(secs_before_now: i64) -> WallTime {
    WallTime {
        unix_secs: NOW.unix_secs - secs_before_now,
        offset_secs: NOW.offset_secs,
    }
}

/// Message `j` of conversation `i`: received on even `j`, sent on odd,
/// the newest last, the newest conversation first.
fn message(i: usize, j: usize, count: usize, activity: u64) -> Message {
    let outbound = j % 2 == 1;
    let body = String::from(BODIES[(i + j) % BODIES.len()]);
    // Conversation i's newest message is i hours old; its messages lie
    // 25 minutes apart, so the pinned chat crosses a day.
    let age = (i as i64) * 3600 + ((count - 1 - j) as i64) * 1500 + 300;
    let when = at(age);
    let mut id = [0u8; 32];
    id[0] = i as u8;
    id[1] = j as u8;
    id[31] = 0x61;
    Message {
        outbound,
        size: body.len(),
        body,
        id: Some(id),
        delivery: outbound.then(|| RUNGS[(j / 2) % RUNGS.len()]),
        signature: Signature::Verified,
        leg: None,
        unproven: false,
        at: Some(when),
        ledger: Vec::new(),
        last_leg: None,
        activity,
        fields: Vec::new(),
    }
}

/// The conversations as the store would hand them back at start.
pub fn restored() -> Vec<Restored> {
    let mut activity = 0;
    let mut out = Vec::with_capacity(CONVERSATIONS.len());
    // Oldest first, so activity grows with recency as it does in a store.
    for (i, c) in CONVERSATIONS.iter().enumerate().rev() {
        let messages = (0..c.messages)
            .map(|j| {
                activity += 1;
                message(i, j, c.messages, activity)
            })
            .collect();
        out.push(Restored {
            peer: peer(c.seed),
            pinned: c.pinned,
            flags: Flags::default(),
            unread: c.unread,
            messages,
            earlier: 0,
            draft: Draft::default(),
            group: None,
        });
    }
    out
}

/// The model the board starts with: the conversations restored, their
/// names known, the clock at [`NOW`], the keys on the list with the
/// pinned conversation under the cursor.
pub fn model() -> UiModel<Field> {
    let mut model = UiModel::<Field>::new(&[]);
    model.address = Some(OWN);
    for c in &CONVERSATIONS {
        model.names.remember(peer(c.seed), c.name);
    }
    update(&mut model, UiEvent::Clock(NOW));
    model.restore(restored());
    model.focus = Pane::Conversations;
    model
}

/// The screens a `FRAME_DUMP` may ask for by its payload byte, walked to
/// from a fresh [`model`] with the same keys a person would press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// The chats list, as the board starts: payload `00`.
    Chats,
    /// Enter on the pinned conversation: payload `01`.
    Chat,
}

impl Screen {
    /// The screen payload byte `byte` names.
    pub fn of(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Chats),
            1 => Some(Self::Chat),
            _ => None,
        }
    }

    /// Its payload byte.
    pub fn byte(self) -> u8 {
        match self {
            Self::Chats => 0,
            Self::Chat => 1,
        }
    }

    /// Its name in the log and in the goldens' file names.
    pub fn name(self) -> &'static str {
        match self {
            Self::Chats => "chats",
            Self::Chat => "chat",
        }
    }

    /// The keys from a fresh [`model`] to this screen.
    pub fn keys(self) -> Vec<M9Key> {
        match self {
            Self::Chats => vec![],
            Self::Chat => vec![M9Key::Enter],
        }
    }

    /// A fresh [`model`] walked to this screen with its [`Screen::keys`].
    pub fn model(self) -> UiModel<Field> {
        let mut model = model();
        for key in self.keys() {
            update(&mut model, UiEvent::Key(event(key)));
        }
        model
    }
}

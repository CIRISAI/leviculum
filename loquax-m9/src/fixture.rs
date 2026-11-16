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

use leviculum_lxmf::telemetry::Location;
use leviculum_ui::delivery::DeliveryState;
use loquax::telemetry::{Fix, Source};
use loquax::ui::groups::{GroupInfo, GroupMember, Standing};
use loquax::ui::history::{Draft, Flags, Restored};
use loquax::ui::{update, Message, Pane, Signature, UiEvent, UiModel};
use loquax::when::WallTime;

use crate::field::Field;
use crate::group;
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
            listing: None,
        });
    }
    out
}

/// The model the board starts with: the conversations restored, their
/// names known, the clock at [`NOW`], the keys on the list with the
/// pinned conversation under the cursor.
pub fn model() -> UiModel<Field> {
    let mut model = UiModel::<Field>::new(&[]);
    model.lang = crate::LANG;
    model.address = Some(OWN);
    for c in &CONVERSATIONS {
        model.names.remember(peer(c.seed), c.name);
    }
    update(&mut model, UiEvent::Clock(NOW));
    model.restore(restored());
    map(&mut model);
    model.focus = Pane::Conversations;
    // The board's screen, as a home's model has it (1532): no tab bar.
    model.viewing = crate::screen::viewing(crate::font::Size::DEFAULT);
    model
}

/// A place in the map fixture's Florence, degrees times 10^6.
struct Place {
    lat_e6: i32,
    lon_e6: i32,
}

/// Where the board is, by its GNSS: by the cathedral, 20 s ago.
const OWN_FIX: Place = Place {
    lat_e6: 43_773_200,
    lon_e6: 11_256_000,
};

/// The positions friends shared (seed, place, seconds before [`NOW`]):
/// Jürgen five minutes ago, Mum 25 minutes ago, Zoë three hours ago, which
/// is stale.
const SHARES: [(u8, Place, i64); 3] = [
    (
        0x42,
        Place {
            lat_e6: 43_776_500,
            lon_e6: 11_259_500,
        },
        300,
    ),
    (
        0x44,
        Place {
            lat_e6: 43_777_800,
            lon_e6: 11_252_500,
        },
        1_500,
    ),
    (
        0x43,
        Place {
            lat_e6: 43_774_500,
            lon_e6: 11_266_500,
        },
        3 * 3600,
    ),
];

/// A node nearby that announced its position (`lev.position`, 1302), no
/// contact: its identity, name, place and how long ago it was heard.
const NEARBY: ([u8; 16], &str, Place, u32) = (
    [0x77; 16],
    "Rifugio",
    Place {
        lat_e6: 43_775_000,
        lon_e6: 11_268_500,
    },
    600,
);

fn location(at: &Place, secs_before_now: i64) -> Location {
    Location::saturating(
        i64::from(at.lat_e6),
        i64::from(at.lon_e6),
        5_000,
        0,
        0,
        500,
        NOW.unix_secs - secs_before_now,
    )
}

/// The map the board shows (slice seven, 1375): the region of
/// [`crate::map::FIXTURE`], the picture under the bar, and the positions
/// a pin is made of on every target: the shares the store kept, a node's
/// `lev.position` announce, the board's own GNSS fix.
fn map(model: &mut UiModel<Field>) {
    crate::map::ready(model);
    let shares = SHARES
        .iter()
        .map(|(seed, at, ago)| Fix {
            peer: peer(*seed),
            via: peer(*seed),
            message: None,
            received_at: Some(NOW.unix_secs - ago),
            location: location(at, *ago),
            telemetry: Vec::new(),
            source: Source::Telemetry,
        })
        .collect();
    model.restore_positions(shares);
    let (identity, name, at, age) = NEARBY;
    update(
        model,
        UiEvent::MeshPosition {
            identity,
            fix: location(&at, i64::from(age)),
            name: Some(String::from(name)),
            age,
        },
    );
    update(model, UiEvent::OwnFix(Some(location(&OWN_FIX, 20))));
}

/// The fixture's groups (slice six, 1366): Jürgen's "Familie" the reader
/// is in, one member gone; the reader's own "Kletterhalle", one member
/// still invited; Jürgen's "Chor" the reader is invited to.
pub const FAMILIE: [u8; 16] = [0x61; 16];
pub const KLETTERHALLE: [u8; 16] = [0x62; 16];
pub const CHOR: [u8; 16] = [0x63; 16];

fn member(seed: u8, name: &str, admin: bool, joined: bool, left: bool) -> GroupMember {
    GroupMember {
        address: match seed {
            0 => OWN,
            seed => peer(seed),
        },
        name: String::from(name),
        admin,
        joined,
        left,
    }
}

/// The three groups as the store keeps them, with a line in each but the
/// invitation, newer than every other conversation's.
fn groups() -> Vec<Restored> {
    let jürgen = peer(0x42);
    let infos = [
        GroupInfo {
            address: FAMILIE,
            name: String::from("Familie"),
            admin_name: String::from("Jürgen"),
            inviter: Some(jürgen),
            ours: false,
            epoch: 2,
            members: vec![
                member(0x42, "Jürgen", true, true, false),
                member(0, "Me", false, true, false),
                member(0x44, "Mum", false, true, false),
                member(0x46, "Ünal", false, true, true),
            ],
            standing: Standing::Joined,
        },
        GroupInfo {
            address: KLETTERHALLE,
            name: String::from("Kletterhalle"),
            admin_name: String::new(),
            inviter: None,
            ours: true,
            epoch: 1,
            members: vec![
                member(0, "Me", true, true, false),
                member(0x43, "Zoë Ærø", false, true, false),
                member(0x49, "Βασίλης", false, true, false),
                member(0x4e, "Ivan", false, false, false),
            ],
            standing: Standing::Joined,
        },
        GroupInfo {
            address: CHOR,
            name: String::from("Chor"),
            admin_name: String::from("Jürgen"),
            inviter: Some(jürgen),
            ours: false,
            epoch: 1,
            members: vec![
                member(0x42, "Jürgen", true, true, false),
                member(0, "Me", false, false, false),
                member(0x4b, "Ольга", false, true, false),
            ],
            standing: Standing::Invited,
        },
    ];
    infos
        .into_iter()
        .enumerate()
        .map(|(i, info)| {
            let messages = match info.standing {
                Standing::Invited => Vec::new(),
                _ => {
                    // Newer than every conversation of the list: 1,000
                    // past any activity it counts, and written last, the
                    // later group the later (the list orders by the time
                    // written, 1445).
                    let mut line = message(0, i, i + 1, 1_000 + i as u64);
                    line.outbound = false;
                    line.delivery = None;
                    line.at = Some(at(60 * (2 - i as i64)));
                    line.body = String::from(GROUP_LINES[i]);
                    line.size = line.body.len();
                    vec![line]
                }
            };
            Restored {
                peer: info.address,
                pinned: false,
                flags: Flags::default(),
                unread: 0,
                messages,
                earlier: 0,
                draft: Draft::default(),
                group: Some(info),
                listing: None,
            }
        })
        .collect()
}

const GROUP_LINES: [&str; 2] = [
    "Jürgen: Sonntag Kaffee bei Oma, wer bringt Kuchen?",
    "Zoë Ærø: Donnerstag 18 Uhr an der Wand?",
];

/// The fixture [`model`] with its three groups.
pub fn group_model() -> UiModel<Field> {
    let mut model = model();
    model.restore(groups());
    model.focus = Pane::Conversations;
    // The list's cursor on its first row, as the board starts.
    if let Some(&first) = model.order().first() {
        model.selected = first;
    }
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
    /// The list with the three groups on top ([`group_model`]): payload
    /// `02`.
    Groups,
    /// The invitation to "Chor" with its Join: payload `03`.
    Invite,
    /// "Familie"'s member sheet, a member's: payload `04`.
    Members,
    /// "Kletterhalle"'s, the reader its admin, the cursor on Zoë: payload
    /// `05`.
    Admin,
    /// The admin's Delete asking yes or no: payload `06`.
    Ask,
    /// "New group", two friends ticked, the cursor on the third: payload
    /// `07`.
    Pick,
    /// "New group", its name typed: payload `08`.
    Name,
    /// The map of every pin, the first in `Tab` order chosen: payload `09`
    /// (slice seven, 1375).
    Map,
    /// The map zoomed in, the next pin chosen and the view panned east:
    /// payload `0a`.
    Pan,
}

/// Every screen, in payload order.
pub const SCREENS: [Screen; 11] = [
    Screen::Chats,
    Screen::Chat,
    Screen::Groups,
    Screen::Invite,
    Screen::Members,
    Screen::Admin,
    Screen::Ask,
    Screen::Pick,
    Screen::Name,
    Screen::Map,
    Screen::Pan,
];

impl Screen {
    /// The screen payload byte `byte` names.
    pub fn of(byte: u8) -> Option<Self> {
        SCREENS.get(usize::from(byte)).copied()
    }

    /// Its payload byte.
    pub fn byte(self) -> u8 {
        SCREENS.iter().position(|s| *s == self).unwrap_or(0) as u8
    }

    /// The budget its census is held to ([`crate::census`]).
    pub fn budget(self) -> crate::census::Budget {
        use crate::census::{CHAT, CHATS, MAP, SHEET};
        match self {
            Self::Map | Self::Pan => MAP,
            Self::Chats | Self::Groups => CHATS,
            Self::Chat | Self::Invite => CHAT,
            Self::Members | Self::Admin | Self::Ask | Self::Pick | Self::Name => SHEET,
        }
    }

    /// Its name in the log and in the goldens' file names.
    pub fn name(self) -> &'static str {
        match self {
            Self::Chats => "chats",
            Self::Chat => "chat",
            Self::Groups => "groups",
            Self::Invite => "invite",
            Self::Members => "members",
            Self::Admin => "admin",
            Self::Ask => "ask",
            Self::Pick => "pick",
            Self::Name => "name",
            Self::Map => "map",
            Self::Pan => "pan",
        }
    }

    /// The keys from a fresh model to this screen. The list reads the
    /// pinned Jürgen, Kletterhalle, Familie, the plain conversations, and
    /// last Chor.
    pub fn keys(self) -> Vec<M9Key> {
        use M9Key::{Char, Down, Enter, Up};
        match self {
            Self::Chats | Self::Groups => vec![],
            Self::Chat => vec![Enter],
            // `M` is the terminal's map of every pin; Space is the M9's
            // `Tab` on the map (`crate::map::key`).
            Self::Map => vec![Char('M')],
            Self::Pan => vec![Char('M'), Char('+'), Char(' '), M9Key::Right],
            // Chor is the list's last row: an invitation has no line yet.
            Self::Invite => {
                let mut keys = vec![Down; 15];
                keys.push(Enter);
                keys
            }
            Self::Members => vec![Down, Down, Enter, Char('m')],
            Self::Admin => vec![Down, Enter, Char('m'), Down],
            Self::Ask => vec![Down, Enter, Char('m'), Down, Char('d')],
            Self::Pick => vec![
                Char('n'),
                Char(' '),
                Down,
                Char(' '),
                Down,
                Down,
                Char(' '),
                Char(' '),
                Up,
            ],
            Self::Name => vec![
                Char('n'),
                Char(' '),
                Down,
                Char(' '),
                Enter,
                Char('H'),
                Char('ü'),
                Char('t'),
                Char('x'),
                M9Key::Backspace,
                Char('t'),
                Char('e'),
            ],
        }
    }

    /// The model this screen starts from: the board's first, or the one
    /// with groups.
    fn start(self) -> UiModel<Field> {
        match self {
            Self::Chats | Self::Chat | Self::Map | Self::Pan => model(),
            _ => group_model(),
        }
    }

    /// A fresh model walked to this screen with its [`Screen::keys`],
    /// pressed as the board presses them ([`group::press`]), and the M9's
    /// cursor over a group's rows where the walk left it.
    pub fn walk(self) -> (UiModel<Field>, usize) {
        let mut model = self.start();
        let mut row = 0;
        for key in self.keys() {
            group::press(&mut model, &mut row, &event(key));
        }
        (model, row)
    }

    /// [`Screen::walk`]'s model.
    pub fn model(self) -> UiModel<Field> {
        self.walk().0
    }
}

// 1642, the mailbox page: one fixture for the goldens of every target
// (the terminal, the window, the phone's facade). Lew's phone reads the
// mailbox on miauhaus, announced as "Lew Palm"; the Fairphone and an M9
// joined it. Reachable: connected for twelve minutes over
// leviculum.network's shielded port 4343, the last contact half a minute
// ago, the M9 last there two hours ago. Unreachable: down for two hours
// over the fallback port 4242, and the mailbox too old to answer `/about`.
//
// Included with `include!`, not as a module, so the crate's own tests can
// use it too: an includer inside loquax says `use crate as loquax;` first.
// No inner doc comments here for that reason.

use leviculum_ui::input::InputBuffer;
use loquax::home::about::{About, KnownDevice};
use loquax::ui::mailbox_page::{Reach, Told};
use loquax::ui::profile::{Device, Mailbox, ProfileFacts};
use loquax::ui::{update, UiEvent, UiModel};
use loquax::when::WallTime;

/// The fixture's clock: 2027-01-15 08:00 UTC.
pub const MAILBOX_NOW: i64 = 1_800_000_000;
/// The reader's address, the mailbox's.
pub const MAILBOX_ADDRESS: [u8; 16] = [
    0x5a, 0x3e, 0x91, 0x0c, 0x77, 0x42, 0xd8, 0x1b, 0x6f, 0x03, 0xa4, 0xe2, 0x58, 0xc9, 0x2d, 0x80,
];
/// The build both run, fixed so the goldens do not move with each commit.
pub const MAILBOX_VERSION: &str = "0.9.7 (5f0c2e1)";
/// This phone's device identity, as the mailbox lists it.
pub const MAILBOX_FAIRPHONE: [u8; 16] = [0xf1; 16];

/// What the mailbox on miauhaus says of itself.
pub fn mailbox_about() -> About {
    About {
        name: "Lew Palm".to_string(),
        host: "miauhaus".to_string(),
        version: MAILBOX_VERSION.to_string(),
        devices: vec![
            KnownDevice {
                device: MAILBOX_FAIRPHONE,
                name: "Fairphone".to_string(),
                admitted_at: MAILBOX_NOW - 20 * 86_400,
                last_seen: Some(MAILBOX_NOW - 30),
            },
            KnownDevice {
                device: [0x99; 16],
                name: "M9".to_string(),
                admitted_at: MAILBOX_NOW - 3 * 86_400,
                last_seen: Some(MAILBOX_NOW - 7_200),
            },
            // Admitted before the mailbox kept the time (schema 20, 1647):
            // named, no time.
            KnownDevice {
                device: [0x77; 16],
                name: "Laptop".to_string(),
                admitted_at: MAILBOX_NOW - 40 * 86_400,
                last_seen: None,
            },
        ],
        started: Some(MAILBOX_NOW - 5 * 86_400),
    }
}

/// The facts of the reachable mailbox, or the unreachable older one.
pub fn mailbox_facts(reachable: bool) -> ProfileFacts {
    let (told, reach) = match reachable {
        true => (
            Told::Told(mailbox_about()),
            Reach {
                up: Some(true),
                since: Some(MAILBOX_NOW - 720),
                last_contact: Some(MAILBOX_NOW - 30),
                entry: Some("leviculum.network:4343".to_string()),
                dials: true,
            },
        ),
        false => (
            Told::Older,
            Reach {
                up: Some(false),
                since: Some(MAILBOX_NOW - 7_200),
                last_contact: Some(MAILBOX_NOW - 7_300),
                entry: Some("leviculum.network:4242".to_string()),
                dials: true,
            },
        ),
    };
    ProfileFacts {
        name: String::new(),
        mailbox: Mailbox::Away {
            name: None,
            rung: None,
            word: None,
        },
        devices: vec![Device {
            name: "Fairphone".to_string(),
            this: true,
        }],
        told,
        reach,
        device: Some(MAILBOX_FAIRPHONE),
        version: MAILBOX_VERSION.to_string(),
    }
}

/// `model` with the fixture told: the address, the clock, the facts.
pub fn mailbox_told<B: InputBuffer + Default>(model: &mut UiModel<B>, reachable: bool) {
    model.address = Some(MAILBOX_ADDRESS);
    update(
        model,
        UiEvent::Clock(WallTime {
            unix_secs: MAILBOX_NOW,
            offset_secs: 3_600,
        }),
    );
    update(model, UiEvent::ProfileFacts(Box::new(mailbox_facts(reachable))));
}

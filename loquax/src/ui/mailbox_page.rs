//! The mailbox page (1642): when the mailbox is on another device, which
//! one, how this device reaches it, and which devices use it. One scene on
//! every target: the phone's settings page, the window's and the
//! terminal's section draw [`MailboxPage`] and no words of their own.
//!
//! Lew, 2026-11-10, on the Fairphone: Settings → Mailbox said only "Your
//! mailbox is on another device", and he could not tell which. Now the
//! page names it (its announced name, the address in full, the machine it
//! runs on, its version), says the connection (up or down since when, the
//! last contact, the server and port and whether that port is the shielded
//! one) and lists the devices the mailbox admitted with when each was last
//! there. The address is the one place the no-hash rule yields
//! (`feedback_no_hashes_on_screen`): Lew asked for it, and the page says
//! "Address" over it and offers to copy it.
//!
//! What the mailbox knows of itself comes from `/about`
//! ([`crate::home::about`]); a mailbox from before it is said as older, and
//! the page shows what this device knows on its own.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use serde_json::{json, Value};

use super::intent::Intent;
use super::profile::{Device, Mailbox};
use super::scene::{ago, Action};
use super::settings::PageRow;
use super::strings::{Arg, StrId, Text};
use super::UiModel;
use crate::address::to_hex;
use crate::home::about::About;

/// The rendezvous's shielded port, its access point
/// (`home::rendezvous::ACCESS_POINT_PORT`, which the core cannot name: it
/// is `std`), and its ordinary one, the fallback that hears the whole mesh.
const ACCESS_POINT_PORT: u16 = 4343;
const PEER_PORT: u16 = 4242;

/// What the mailbox said of itself, as far as the shell asked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Told {
    /// Not asked yet, or no answer yet.
    #[default]
    NotYet,
    /// Its answer to `/about`.
    Told(About),
    /// It did not know the question: a mailbox from before 1642.
    Older,
}

/// How this device reaches its mailbox now, as the shell knows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reach {
    /// Whether the connection is up; `None` before the first look.
    pub up: Option<bool>,
    /// When `up` last changed, unix seconds.
    pub since: Option<i64>,
    /// When this device last heard its mailbox, unix seconds.
    pub last_contact: Option<i64>,
    /// The server and port the connection runs over (or is dialled on
    /// now), `host:port`; `None` where it runs over this computer's own
    /// network connection (a shared instance).
    pub entry: Option<String>,
    /// Whether the device dials a server at all: a phone does, a
    /// computer's client reaches the mailbox over its own Reticulum.
    pub dials: bool,
}

/// One line of the page.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub label: Text,
    pub value: Option<Text>,
    /// The text the line's "Copy" puts on the clipboard: the address.
    pub copy: Option<String>,
    /// What a press on the line does.
    pub action: Option<Action>,
    /// A sentence that explains the line above, drawn smaller.
    pub note: bool,
}

/// A titled group of lines.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub title: Text,
    pub lines: Vec<Line>,
}

/// The page as every target draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct MailboxPage {
    pub title: Text,
    pub groups: Vec<Group>,
}

fn line(label: Text, value: Option<Text>) -> Line {
    Line {
        label,
        value,
        copy: None,
        action: None,
        note: false,
    }
}

fn note(id: StrId) -> Line {
    Line {
        note: true,
        ..line(Text::key(id), None)
    }
}

/// A span of time in the catalogue's words, "5 min", "3 h", "2 d", and
/// "less than a minute" below one.
pub fn span(secs: i64) -> Text {
    match secs {
        ..60 => Text::key(StrId::MailboxMoment),
        60..3600 => Text::Key(StrId::MinutesAgo, Vec::from([Arg::Int(secs / 60)])),
        3600..172_800 => Text::Key(StrId::HoursAgo, Vec::from([Arg::Int(secs / 3600)])),
        _ => Text::Key(StrId::DaysAgo, Vec::from([Arg::Int(secs / 86_400)])),
    }
}

/// A build's words and its commit: `0.1.8 (1aa683db…)` is `0.1.8` and the
/// commit; a version without the parenthesis has no commit.
fn split_version(version: &str) -> (&str, Option<&str>) {
    let version = version.trim();
    match version.split_once(" (") {
        Some((words, rest)) => (words.trim(), Some(rest.trim_end_matches(')').trim())),
        None => (version, None),
    }
}

/// The numbers of a version's words, `0.1.8-nightly.x` as `[0, 1, 8]`;
/// `None` when they do not read as numbers.
fn version_numbers(words: &str) -> Option<Vec<u64>> {
    let core = words.split(['-', '+']).next().unwrap_or(words);
    core.split('.').map(|n| n.parse().ok()).collect()
}

/// The mailbox's version as the page says it (1647, walk 6, finding 30):
/// the words only, never a commit (`feedback_no_hashes_on_screen`), and
/// how they stand to this device's: "0.1.9, newer", "0.1.7, older", and
/// "0.1.8, another build" where the words agree and the commits do not.
/// The commits are in the phone's log line (`HOME about`).
pub fn version_said(theirs: &str, ours: &str) -> Text {
    let (words, commit) = split_version(theirs);
    let (our_words, our_commit) = split_version(ours);
    let said = |id| Text::Key(id, Vec::from([Arg::Str(String::from(words))]));
    if ours.trim().is_empty() {
        return Text::Raw(String::from(words));
    }
    if words == our_words {
        return match (commit, our_commit) {
            (Some(a), Some(b)) if a != b => said(StrId::MailboxVersionBuild),
            _ => Text::Raw(String::from(words)),
        };
    }
    match (version_numbers(words), version_numbers(our_words)) {
        (Some(a), Some(b)) if a > b => said(StrId::MailboxVersionNewer),
        (Some(a), Some(b)) if a < b => said(StrId::MailboxVersionOlder),
        _ => said(StrId::MailboxVersionBuild),
    }
}

/// The server and port of `entry` (`host:port`, `[v6]:port`).
fn split_entry(entry: &str) -> Option<(&str, u16)> {
    let (host, port) = entry.rsplit_once(':')?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    Some((host, port.parse().ok()?))
}

/// The connection's lines: up or down since when, the last contact, the
/// way and what it means.
///
/// "Connected for" counts the link to the mailbox (1647, walk 6, finding
/// 37): the shell's `since` is the watching link's, and no earlier than
/// the mailbox's own start (`/about`'s `started`), so a mailbox restarted
/// under a transport that stayed up says "less than a minute". The way
/// names the mailbox's machine where it says one, else the server; a port
/// only where it is neither the shielded nor the ordinary one (finding 33).
fn reach_lines(reach: &Reach, now: Option<i64>, about: Option<&About>) -> Vec<Line> {
    let mut lines = Vec::new();
    let started = about.and_then(|a| a.started);
    let since = match (reach.up, reach.since, started) {
        (Some(true), Some(at), Some(started)) => Some(at.max(started)),
        (Some(true), None, Some(started)) => Some(started),
        (_, at, _) => at,
    };
    let since = since.zip(now).map(|(at, now)| now.saturating_sub(at));
    let state = match (reach.up, since) {
        (Some(true), Some(secs)) => Text::Key(
            StrId::MailboxUpFor,
            Vec::from([Arg::Text(Box::new(span(secs)))]),
        ),
        (Some(true), None) => Text::key(StrId::MailboxUp),
        (Some(false), Some(secs)) => Text::Key(
            StrId::MailboxDownFor,
            Vec::from([Arg::Text(Box::new(span(secs)))]),
        ),
        (Some(false) | None, _) => Text::key(StrId::MailboxDownHere),
    };
    lines.push(line(state, None));
    let contact = match (reach.last_contact, now) {
        (Some(at), Some(now)) => ago(now.saturating_sub(at)),
        _ => Text::key(StrId::MailboxContactNone),
    };
    lines.push(line(Text::key(StrId::MailboxContact), Some(contact)));
    match reach.entry.as_deref().and_then(split_entry) {
        Some((server, port)) => {
            let host = about
                .map(|a| a.host.trim())
                .filter(|h| !h.is_empty())
                .unwrap_or(server);
            let way = match port {
                ACCESS_POINT_PORT => StrId::MailboxWayShielded,
                PEER_PORT => StrId::MailboxWayOpen,
                _ => StrId::MailboxWayOther,
            };
            lines.push(line(
                Text::Key(
                    way,
                    Vec::from([Arg::Str(String::from(host)), Arg::Int(i64::from(port))]),
                ),
                None,
            ));
        }
        None => lines.push(line(
            Text::key(match reach.dials {
                true => StrId::MailboxWayNone,
                false => StrId::MailboxWayLocal,
            }),
            None,
        )),
    }
    lines
}

/// The line of this device: its name and "this device", or "This device"
/// alone where it has no name (1647, walk 6, finding 34: "Dieses Gerät:
/// dieses Gerät").
fn this_device(name: &str) -> Line {
    match name.trim() {
        "" => line(Text::key(StrId::ProfileThisDeviceUnnamed), None),
        name => line(
            Text::Raw(String::from(name)),
            Some(Text::key(StrId::MailboxThisDevice)),
        ),
    }
}

/// The devices' lines: each the mailbox admitted, by name, with when it was
/// last there, this one said as such; or why there is no list.
///
/// A device the mailbox has not heard since it began keeping the time
/// (`last_seen` NULL, schema 20, 1647) says "never connected yet": a row
/// with no second line read as broken (walk 11, finding 57, 1667). Its
/// first request gives it a time.
fn access_lines(
    told: &Told,
    this: Option<[u8; 16]>,
    now: Option<i64>,
    own: &[Device],
) -> Vec<Line> {
    let mut lines = Vec::new();
    match told {
        Told::Told(about) => {
            for device in &about.devices {
                if this == Some(device.device) {
                    lines.push(this_device(&device.name));
                    continue;
                }
                let name = match device.name.trim() {
                    "" => Text::key(StrId::ProfileDeviceUnnamed),
                    name => Text::Raw(String::from(name)),
                };
                let value = match (device.last_seen, now) {
                    (Some(at), Some(now)) => Text::Key(
                        StrId::MailboxSeen,
                        Vec::from([Arg::Text(Box::new(ago(now.saturating_sub(at))))]),
                    ),
                    // Walk 11, finding 57: a row without a second line
                    // read as broken. A device the mailbox never heard
                    // since it keeps the time says so (1667).
                    (None, _) => Text::key(StrId::MailboxSeenNever),
                    // No clock here: say nothing rather than guess.
                    (Some(_), None) => {
                        lines.push(line(name, None));
                        continue;
                    }
                };
                lines.push(line(name, Some(value)));
            }
        }
        Told::NotYet | Told::Older => {
            // What this device knows on its own: itself.
            for device in own.iter().filter(|d| d.this) {
                lines.push(this_device(&device.name));
            }
            lines.push(note(match told {
                Told::Older => StrId::MailboxDevicesOld,
                _ => StrId::MailboxDevicesAsking,
            }));
        }
    }
    lines.push(note(StrId::MailboxAccessThere));
    lines.push(Line {
        action: Some(Action::of(Intent::Profile)),
        ..line(Text::key(StrId::MailboxShowCode), None)
    });
    lines
}

/// The page of `model`: for a mailbox elsewhere the three groups; for one
/// here or none the line the profile says.
pub fn page<B>(model: &UiModel<B>) -> MailboxPage {
    let facts = &model.profile;
    let title = Text::key(StrId::Mailbox);
    let Mailbox::Away { name, .. } = &facts.mailbox else {
        let (said, state) = super::profile::mailbox_words(&facts.mailbox);
        let mut lines = Vec::from([line(said, None)]);
        lines.extend(state.map(|state| line(state, None)));
        return MailboxPage {
            title,
            groups: Vec::from([Group {
                title: Text::key(StrId::Mailbox),
                lines,
            }]),
        };
    };
    let now = model.now.map(|now| now.unix_secs);
    let own = model.address.unwrap_or([0; 16]);
    let about = match &facts.told {
        Told::Told(about) => Some(about),
        _ => None,
    };
    // The name the mailbox says, else the one its announce carries, else
    // the shell's (the same word the chat list shows for the reader).
    let heard = about
        .map(|a| a.name.trim())
        .filter(|n| !n.is_empty())
        .or_else(|| name.as_deref().map(str::trim).filter(|n| !n.is_empty()))
        .or_else(|| {
            model
                .names
                .announced(&own)
                .map(str::trim)
                .filter(|n| !n.is_empty())
        });
    let mut which = Vec::from([line(
        Text::key(StrId::MailboxName),
        Some(match heard {
            Some(name) => Text::Raw(String::from(name)),
            None => Text::key(StrId::MailboxNameUnheard),
        }),
    )]);
    let hex = to_hex(&own);
    which.push(Line {
        copy: Some(hex.clone()),
        ..line(Text::key(StrId::MailboxAddress), Some(Text::Raw(hex)))
    });
    // The mailbox's address is the reader's own (1647, walk 6, finding 32).
    which.push(note(StrId::MailboxAddressOwn));
    if let Some(host) = about.map(|a| a.host.trim()).filter(|h| !h.is_empty()) {
        which.push(line(
            Text::key(StrId::MailboxHost),
            Some(Text::Raw(String::from(host))),
        ));
    }
    match &facts.told {
        Told::Told(about) => {
            let value = version_said(&about.version, &facts.version);
            which.push(line(Text::key(StrId::MailboxVersion), Some(value)));
        }
        Told::Older => {
            which.push(line(
                Text::key(StrId::MailboxVersion),
                Some(Text::key(StrId::MailboxVersionOld)),
            ));
            which.push(note(StrId::MailboxOldText));
        }
        Told::NotYet => {}
    }
    MailboxPage {
        title,
        groups: Vec::from([
            Group {
                title: Text::key(StrId::MailboxWhich),
                lines: which,
            },
            Group {
                title: Text::key(StrId::MailboxReach),
                lines: reach_lines(&facts.reach, now, about),
            },
            Group {
                title: Text::key(StrId::MailboxAccess),
                lines: access_lines(&facts.told, facts.device, now, &facts.devices),
            },
        ]),
    }
}

impl MailboxPage {
    /// The page as the settings' rows (the terminal's, which draws them
    /// whole): each group's title, then its lines, a blank row between.
    pub fn rows(&self) -> Vec<PageRow> {
        let mut rows = Vec::new();
        for (i, group) in self.groups.iter().enumerate() {
            if i > 0 {
                rows.push(PageRow {
                    label: Text::Raw(String::new()),
                    value: None,
                    key: None,
                    action: None,
                });
            }
            if self.groups.len() > 1 {
                rows.push(PageRow {
                    label: group.title.clone(),
                    value: None,
                    key: None,
                    action: None,
                });
            }
            for line in &group.lines {
                rows.push(PageRow {
                    label: line.label.clone(),
                    value: line.value.clone(),
                    key: line.action.as_ref().map(|_| 'c'),
                    action: line.action.clone(),
                });
            }
        }
        rows
    }

    /// The node for a golden file and the facade's dump.
    pub fn to_json(&self) -> Value {
        json!({
            "title": self.title.to_json(),
            "groups": self.groups.iter().map(|g| json!({
                "title": g.title.to_json(),
                "lines": g.lines.iter().map(|l| json!({
                    "label": l.label.to_json(),
                    "value": l.value.as_ref().map(Text::to_json),
                    "copy": l.copy,
                    "action": l.action.as_ref().map(Action::to_json),
                    "note": l.note,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home::about::KnownDevice;
    use crate::ui::profile::ProfileFacts;
    use crate::ui::strings::Lang;
    use crate::ui::tests::Field;
    use crate::when::WallTime;

    const NOW: i64 = 1_800_000_000;
    const FAIRPHONE: [u8; 16] = [0xf1; 16];

    fn said(page: &MailboxPage, lang: Lang) -> Vec<String> {
        page.rows()
            .iter()
            .map(|row| {
                let label = row.label.resolve(lang);
                match &row.value {
                    Some(value) => alloc::format!("{label}: {}", value.resolve(lang)),
                    None => label,
                }
            })
            .collect()
    }

    fn model(told: Told, reach: Reach) -> UiModel<Field> {
        let mut model = UiModel::<Field>::new(&[]);
        model.address = Some([0x5a; 16]);
        model.now = Some(WallTime {
            unix_secs: NOW,
            offset_secs: 3600,
        });
        model.profile = ProfileFacts {
            name: String::new(),
            mailbox: Mailbox::Away {
                name: None,
                rung: None,
                word: None,
            },
            devices: Vec::from([Device {
                name: String::from("Fairphone"),
                this: true,
            }]),
            told,
            reach,
            device: Some(FAIRPHONE),
            version: String::from("0.9.7"),
        };
        model
    }

    fn about() -> About {
        About {
            name: String::from("Lew Palm"),
            host: String::from("miauhaus"),
            version: String::from("0.9.7"),
            devices: Vec::from([
                KnownDevice {
                    device: FAIRPHONE,
                    name: String::from("Fairphone"),
                    admitted_at: NOW - 86_400 * 20,
                    last_seen: Some(NOW),
                },
                KnownDevice {
                    device: [0x99; 16],
                    name: String::from("M9"),
                    admitted_at: NOW - 86_400 * 3,
                    last_seen: Some(NOW - 7200),
                },
            ]),
            started: Some(NOW - 86_400),
        }
    }

    /// Lew's case: a mailbox on miauhaus, reached over the shielded port,
    /// connected for twelve minutes: the page names it, its address, its
    /// machine, the way and what shielded means, and both devices.
    #[test]
    fn a_reachable_mailbox_is_named_and_its_devices_listed() {
        let reach = Reach {
            up: Some(true),
            since: Some(NOW - 720),
            last_contact: Some(NOW - 30),
            entry: Some(String::from("leviculum.network:4343")),
            dials: true,
        };
        let page = page(&model(Told::Told(about()), reach));
        let de = said(&page, Lang::De);
        let hex = to_hex(&[0x5a; 16]);
        for want in [
            "Welches Postfach",
            "Name: Lew Palm",
            &alloc::format!("Adresse: {hex}"),
            "Läuft auf: miauhaus",
            "Das ist auch deine Adresse: Postfach und du sind eine Identität.",
            "Version: 0.9.7",
            "Dieses Gerät verbunden seit 12 Min.",
            "Letzter Kontakt: gerade eben",
            "Verbindung zu miauhaus, geschirmt (das Handy hört nur sein Postfach)",
            "Fairphone: dieses Gerät",
            "M9: zuletzt da vor 2 Std.",
            "Meinen Code zeigen",
        ] {
            assert!(de.iter().any(|l| l == want), "{want:?} in {de:#?}");
        }
        let copy = page.groups[0].lines.iter().find_map(|l| l.copy.clone());
        assert_eq!(copy, Some(hex), "the address is the one to copy");
    }

    /// The mailbox down for two hours over the fallback port, and a
    /// mailbox from before 1642: said as such, the version as older, the
    /// devices as not listed, never a hash but the address.
    #[test]
    fn an_unreachable_older_mailbox_says_what_it_can() {
        let reach = Reach {
            up: Some(false),
            since: Some(NOW - 7200),
            last_contact: Some(NOW - 7300),
            entry: Some(String::from("leviculum.network:4242")),
            dials: true,
        };
        let en = said(&page(&model(Told::Older, reach)), Lang::En);
        for want in [
            "Name: not heard yet",
            "Version: older than this device",
            "This device not connected for 2 h",
            "Last contact: 2 h ago",
            "Connection to leviculum.network, not shielded (the phone hears the whole network, which costs battery and data)",
            "Fairphone: this device",
            "Your mailbox does not list its devices yet: that needs a newer version there.",
        ] {
            assert!(en.iter().any(|l| l == want), "{want:?} in {en:#?}");
        }
        assert!(!en.iter().any(|l| l.starts_with("Runs on")), "{en:#?}");
    }

    /// The version line says words, never a commit (1647, walk 6, finding
    /// 30, Lew's "0.1.8 (1aa683db…) (dieses Gerät: 0.1.8 (c274f885…))"):
    /// older, newer, or the same words on another build.
    #[test]
    fn the_version_says_words_and_how_they_stand_never_a_commit() {
        let a = "0.1.8 (1aa683db8a2c4f0e9d1b7c6a5f4e3d2c1b0a9f8e)";
        let b = "0.1.8 (c274f8853f6743cfe98237bd22203f93e38d69cf)";
        let cases = [
            (a, b, "0.1.8, anderer Stand"),
            (a, a, "0.1.8"),
            ("0.1.9 (1aa683db)", b, "0.1.9, neuer"),
            ("0.1.7 (1aa683db)", b, "0.1.7, älter"),
            ("0.1.10 (1aa683db)", "0.1.9 (c274f885)", "0.1.10, neuer"),
            (
                "0.1.8-nightly.1 (1aa683db)",
                b,
                "0.1.8-nightly.1, anderer Stand",
            ),
            (a, "", "0.1.8"),
            ("0.1.8", "0.1.8 (c274f885)", "0.1.8"),
        ];
        for (theirs, ours, want) in cases {
            assert_eq!(
                version_said(theirs, ours).resolve(Lang::De),
                want,
                "{theirs} vs {ours}"
            );
        }
        let mut other = about();
        other.version = String::from(a);
        let mut model = model(Told::Told(other), Reach::default());
        model.profile.version = String::from(b);
        let de = said(&page(&model), Lang::De);
        assert!(
            de.iter().any(|l| l == "Version: 0.1.8, anderer Stand"),
            "{de:#?}"
        );
        for line in &de {
            assert!(
                !line.contains("1aa683db") && !line.contains("c274f885"),
                "{line}"
            );
        }
    }

    /// Walk 11, finding 57: a device whose `last_seen` the mailbox does
    /// not know (never heard since schema 20) says "never connected yet"
    /// rather than a bare name that read as a broken row. Walk 6,
    /// finding 34: this device is said once.
    #[test]
    fn a_device_never_heard_says_never_connected() {
        let mut told = about();
        told.devices[1].last_seen = None;
        told.devices[0].name = String::new();
        let de = said(&page(&model(Told::Told(told), Reach::default())), Lang::De);
        assert!(de.iter().any(|l| l == "M9: noch nie verbunden"), "{de:#?}");
        assert!(!de.iter().any(|l| l == "M9"), "{de:#?}");
        assert!(de.iter().any(|l| l == "Dieses Gerät"), "{de:#?}");
        // In the devices' list; the connection's line names this device
        // too since walk 11 (1667).
        assert_eq!(
            de.iter()
                .filter(|l| *l == "Dieses Gerät" || l.ends_with(": dieses Gerät"))
                .count(),
            1,
            "{de:#?}"
        );
        let mut alone = model(Told::Older, Reach::default());
        alone.profile.devices[0].name = String::new();
        let de = said(&page(&alone), Lang::De);
        assert!(de.iter().any(|l| l == "Dieses Gerät"), "{de:#?}");
    }

    /// Walk 6, finding 37: "connected for" counts the link to the mailbox,
    /// and a mailbox restarted under a transport that stayed up says less
    /// than a minute.
    #[test]
    fn a_restarted_mailbox_is_connected_for_less_than_a_minute() {
        let reach = Reach {
            up: Some(true),
            since: Some(NOW - 3 * 3600),
            last_contact: Some(NOW),
            entry: Some(String::from("192.168.178.61:4343")),
            dials: true,
        };
        let mut restarted = about();
        restarted.started = Some(NOW - 20);
        let de = said(
            &page(&model(Told::Told(restarted), reach.clone())),
            Lang::De,
        );
        assert!(
            de.iter()
                .any(|l| l == "Dieses Gerät verbunden seit weniger als einer Minute"),
            "{de:#?}"
        );
        let de = said(&page(&model(Told::Told(about()), reach.clone())), Lang::De);
        assert!(
            de.iter().any(|l| l == "Dieses Gerät verbunden seit 3 Std."),
            "{de:#?}"
        );
        // Without a host word the way names the server; an unusual port
        // is said, the usual ones never.
        let mut nameless = about();
        nameless.host = String::new();
        let de = said(&page(&model(Told::Told(nameless.clone()), reach)), Lang::De);
        assert!(
            de.iter().any(|l| l
                == "Verbindung zu 192.168.178.61, geschirmt (das Handy hört nur sein Postfach)"),
            "{de:#?}"
        );
        let odd = Reach {
            entry: Some(String::from("192.168.178.61:5000")),
            ..Reach::default()
        };
        let de = said(&page(&model(Told::Told(nameless), odd)), Lang::De);
        assert!(
            de.iter()
                .any(|l| l == "Verbindung zu 192.168.178.61, Port 5000"),
            "{de:#?}"
        );
    }

    #[test]
    fn the_ports_are_the_rendezvous_ones() {
        use crate::home::rendezvous;
        assert_eq!(ACCESS_POINT_PORT, rendezvous::ACCESS_POINT_PORT);
        assert_eq!(PEER_PORT, rendezvous::PEER_PORT);
    }

    #[test]
    fn a_span_reads_in_minutes_hours_and_days() {
        let said = |secs| span(secs).resolve(Lang::De);
        assert_eq!(said(10), "weniger als einer Minute");
        assert_eq!(said(600), "10 Min.");
        assert_eq!(said(7200), "2 Std.");
        assert_eq!(said(86_400 * 3), "3 T.");
        assert_eq!(split_entry("[fd00::1]:4343"), Some(("fd00::1", 4343)));
    }
}

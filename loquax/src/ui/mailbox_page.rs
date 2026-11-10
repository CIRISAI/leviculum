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

/// The server and port of `entry` (`host:port`, `[v6]:port`).
fn split_entry(entry: &str) -> Option<(&str, u16)> {
    let (host, port) = entry.rsplit_once(':')?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    Some((host, port.parse().ok()?))
}

/// The connection's lines: up or down since when, the last contact, the
/// way and what it means.
fn reach_lines(reach: &Reach, now: Option<i64>) -> Vec<Line> {
    let mut lines = Vec::new();
    let since = reach.since.zip(now).map(|(at, now)| now.saturating_sub(at));
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
        (Some(false) | None, _) => Text::key(StrId::MailboxDown),
    };
    lines.push(line(state, None));
    let contact = match (reach.last_contact, now) {
        (Some(at), Some(now)) => ago(now.saturating_sub(at)),
        _ => Text::key(StrId::MailboxContactNone),
    };
    lines.push(line(Text::key(StrId::MailboxContact), Some(contact)));
    match reach.entry.as_deref().and_then(split_entry) {
        Some((host, port)) => {
            let (way, explained) = match port {
                ACCESS_POINT_PORT => (StrId::MailboxWayShielded, Some(StrId::MailboxShieldedText)),
                PEER_PORT => (StrId::MailboxWayOpen, Some(StrId::MailboxOpenText)),
                _ => (StrId::MailboxWayOther, None),
            };
            lines.push(line(
                Text::Key(
                    way,
                    Vec::from([Arg::Str(String::from(host)), Arg::Int(i64::from(port))]),
                ),
                None,
            ));
            lines.extend(explained.map(note));
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

/// The devices' lines: each the mailbox admitted, by name, with when it was
/// last there, this one said as such; or why there is no list.
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
                let name = match device.name.trim() {
                    "" => Text::key(StrId::ProfileDeviceUnnamed),
                    name => Text::Raw(String::from(name)),
                };
                let value = match (this == Some(device.device), device.last_seen, now) {
                    (true, _, _) => Text::key(StrId::MailboxThisDevice),
                    (false, Some(at), Some(now)) => Text::Key(
                        StrId::MailboxSeen,
                        Vec::from([Arg::Text(Box::new(ago(now.saturating_sub(at))))]),
                    ),
                    (false, _, _) => Text::key(StrId::MailboxSeenNone),
                };
                lines.push(line(name, Some(value)));
            }
        }
        Told::NotYet | Told::Older => {
            // What this device knows on its own: itself.
            for device in own.iter().filter(|d| d.this) {
                let name = match device.name.trim() {
                    "" => Text::key(StrId::ProfileThisDeviceUnnamed),
                    name => Text::Raw(String::from(name)),
                };
                lines.push(line(name, Some(Text::key(StrId::MailboxThisDevice))));
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
    if let Some(host) = about.map(|a| a.host.trim()).filter(|h| !h.is_empty()) {
        which.push(line(
            Text::key(StrId::MailboxHost),
            Some(Text::Raw(String::from(host))),
        ));
    }
    match &facts.told {
        Told::Told(about) => {
            let ours = facts.version.as_str();
            let value = match about.version.as_str() {
                theirs if ours.is_empty() || theirs == ours => Text::Raw(String::from(theirs)),
                theirs => Text::Key(
                    StrId::MailboxVersionOther,
                    Vec::from([Arg::Str(String::from(theirs)), Arg::Str(String::from(ours))]),
                ),
            };
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
                lines: reach_lines(&facts.reach, now),
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
            "Version: 0.9.7",
            "Verbunden seit 12 Min.",
            "Letzter Kontakt: gerade eben",
            "Verbindung über leviculum.network, Tor 4343, geschirmt",
            "Geschirmt: dieses Gerät hört nur, was für es bestimmt ist.",
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
            "Not connected for 2 h",
            "Last contact: 2 h ago",
            "Connection over leviculum.network, port 4242, not shielded",
            "Fairphone: this device",
            "Your mailbox does not list its devices yet: that needs a newer version there.",
        ] {
            assert!(en.iter().any(|l| l == want), "{want:?} in {en:#?}");
        }
        assert!(!en.iter().any(|l| l.starts_with("Runs on")), "{en:#?}");
    }

    /// A mailbox on another build says both: the stale mailbox is seen.
    #[test]
    fn a_mailbox_on_another_build_says_both_versions() {
        let mut older = about();
        older.version = String::from("0.9.2");
        let de = said(&page(&model(Told::Told(older), Reach::default())), Lang::De);
        assert!(
            de.iter()
                .any(|l| l == "Version: 0.9.2 (dieses Gerät: 0.9.7)"),
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

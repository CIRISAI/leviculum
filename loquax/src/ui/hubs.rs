//! The public rooms' hubs Loquax offers (1577, 1599, #232).
//!
//! Hubs are discovered from their announces (1599): every hub announces
//! its `rrc.hub` destination with its name in the app data, and every node
//! of the mesh hears it. [`Heard`] is the bounded list of what was heard,
//! on a device from its own node's announces and from its home, which
//! remembers them for every device of the home
//! ([`crate::home::KIND_HUB`]).
//!
//! The shipped list seeds a fresh install that has heard nothing yet: the
//! `[[hub]]` entries of `loquax/assets/hubs.toml`, compiled in. A hub is
//! there only because it answered when measured, published or heard on the
//! mesh (the file's head says how); the `[[unreached]]` entries are the
//! file's record, never offered. Nothing connects to a hub on its own: the
//! settings offer it, and the reader chooses ([`super::rooms`]).
//!
//! The file is read by the few lines this needs, not by a TOML crate: the
//! core is `no_std`, and the file's shape is held by the tests below.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// The file, as shipped.
pub const SHIPPED: &str = include_str!("../../assets/hubs.toml");

/// A hub the settings offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hub {
    /// What the settings show.
    pub name: String,
    /// Its `rrc.hub` destination.
    pub hash: [u8; 16],
    /// The rooms its own welcome named.
    pub rooms: Vec<String>,
    /// Its hops when it was first heard, or by its path when it never was.
    pub hops: Option<u8>,
}

/// A `[[hub]]` entry while its lines are read.
#[derive(Default)]
struct Partial {
    name: Option<String>,
    hash: Option<[u8; 16]>,
    rooms: Vec<String>,
    hops: Option<u8>,
}

/// The `[[hub]]` entries of `text`, in its order. An entry without a name
/// or a hash of 32 hex digits is left out (the tests hold the shipped
/// file to having none such).
pub fn parse(text: &str) -> Vec<Hub> {
    let mut hubs = Vec::new();
    let mut current: Option<Partial> = None;
    let mut finish = |entry: Option<Partial>| {
        if let Some(Partial {
            name: Some(name),
            hash: Some(hash),
            rooms,
            hops,
        }) = entry
        {
            hubs.push(Hub {
                name,
                hash,
                rooms,
                hops,
            });
        }
    };
    for raw in text.lines() {
        // Only whole-line comments: a room's name starts with `#`.
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with("[[") {
            finish(current.take());
            current = (line == "[[hub]]").then(Partial::default);
            continue;
        }
        let (Some(entry), Some((key, value))) = (current.as_mut(), line.split_once('=')) else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "name" => entry.name = Some(value.trim_matches('"').to_string()),
            "hash" => entry.hash = hash(value.trim_matches('"')),
            "hops" => entry.hops = value.parse().ok(),
            "rooms" => {
                entry.rooms = value
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .split(',')
                    .map(|room| room.trim().trim_matches('"'))
                    .filter(|room| !room.is_empty())
                    .map(ToString::to_string)
                    .collect();
            }
            _ => {}
        }
    }
    finish(current.take());
    hubs
}

fn hash(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (byte, pair) in out.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
        let digit = |c: u8| char::from(c).to_digit(16);
        *byte = u8::try_from(digit(pair[0])? * 16 + digit(pair[1])?).ok()?;
    }
    Some(out)
}

/// The hubs Loquax offers.
pub fn shipped() -> Vec<Hub> {
    parse(SHIPPED)
}

/// The shipped hub with this hash.
pub fn named(hash: &[u8; 16]) -> Option<Hub> {
    shipped().into_iter().find(|hub| &hub.hash == hash)
}

/// How many heard hubs a device or a home keeps; past it the one heard
/// longest ago goes. 79 hubs were heard on the mesh in half an hour on
/// 2026-11-09, so this holds that mesh about three times over.
pub const HEARD_BOUND: usize = 256;

/// A hub heard announcing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeardHub {
    /// Its `rrc.hub` destination.
    pub hash: [u8; 16],
    /// The name its announce gave, cleaned ([`shown_name`]); `None` for a
    /// nameless announce, which the settings call "Unnamed hub".
    pub name: Option<String>,
    /// Its hops when last heard.
    pub hops: Option<u8>,
    /// When it was first and last heard, unix seconds.
    pub first: i64,
    pub last: i64,
    /// Whether it answered when last opened: `Some(false)` after an open
    /// that got no welcome, which the settings say as "no answer". Nothing
    /// is removed for it.
    pub answered: Option<bool>,
}

/// The hubs heard, newest last seen first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Heard {
    hubs: Vec<HeardHub>,
    /// The hub "suggested" marks once it marked one (1629, walk 3 new 6:
    /// the mark wandered over three servers in ten minutes as closer ones
    /// were heard): it stays until it fails to answer or leaves the list
    /// ([`super::settings::settle_suggestion`]).
    held: Option<[u8; 16]>,
}

impl Heard {
    /// The hub "suggested" holds, if any.
    pub fn held(&self) -> Option<[u8; 16]> {
        self.held
    }

    /// Hold `hub` as the suggested one, or none.
    pub fn hold(&mut self, hub: Option<[u8; 16]>) {
        self.held = hub;
    }

    /// Take one hearing in: a new hub is added, a known one keeps its first
    /// hearing and takes the later last one, the newer name and hops. Over
    /// [`HEARD_BOUND`] the hub heard longest ago goes. Whether it was new.
    pub fn hear(&mut self, heard: HeardHub) -> bool {
        if let Some(known) = self.hubs.iter_mut().find(|h| h.hash == heard.hash) {
            known.first = known.first.min(heard.first);
            if heard.last >= known.last {
                known.last = heard.last;
                if heard.name.is_some() {
                    known.name = heard.name;
                }
                if heard.hops.is_some() {
                    known.hops = heard.hops;
                }
            }
            if heard.answered.is_some() {
                known.answered = heard.answered;
            }
            self.sort();
            return false;
        }
        self.hubs.push(heard);
        self.sort();
        self.hubs.truncate(HEARD_BOUND);
        true
    }

    /// Whether the hub answered when it was opened last.
    pub fn answered(&mut self, hash: &[u8; 16], answered: bool) {
        if let Some(known) = self.hubs.iter_mut().find(|h| &h.hash == hash) {
            known.answered = Some(answered);
        }
    }

    /// Last seen first; on a tie the closer first.
    fn sort(&mut self) {
        self.hubs.sort_by(|a, b| {
            b.last
                .cmp(&a.last)
                .then(a.hops.unwrap_or(u8::MAX).cmp(&b.hops.unwrap_or(u8::MAX)))
        });
    }

    /// The hubs, last seen first.
    pub fn hubs(&self) -> &[HeardHub] {
        &self.hubs
    }

    /// The hub with this hash, when it was heard.
    pub fn get(&self, hash: &[u8; 16]) -> Option<&HeardHub> {
        self.hubs.iter().find(|h| &h.hash == hash)
    }
}

/// The rooms a hub's notices name: the word after a `/join`, and every
/// `#word`, each once, in the order said. The probe that measured the
/// shipped list ([`rooms`](Hub::rooms), `examples/rrc_hubs_probe.rs`) and a
/// server's sheet (1621, [`super::rooms::RoomAsk::Peek`]) read them alike.
pub fn rooms_named(notices: &str) -> Vec<String> {
    let mut rooms: Vec<String> = Vec::new();
    let mut after_join = false;
    for word in notices.split_whitespace() {
        let clean =
            word.trim_matches(|c: char| !(c.is_alphanumeric() || c == '#' || c == '-' || c == '_'));
        let room = (after_join || (clean.len() > 1 && clean.starts_with('#'))).then_some(clean);
        after_join = word == "/join";
        if let Some(room) = room.filter(|r| !r.is_empty() && *r != "#") {
            let room = room.to_string();
            if !rooms.contains(&room) {
                rooms.push(room);
            }
        }
    }
    rooms
}

/// The command a client sends after the hub's WELCOME for its rooms (1652).
pub const LIST_COMMAND: &str = "/list";

/// The rooms a hub's answer to [`LIST_COMMAND`] names, or `None` when
/// `text` is no such answer (the greeting, another notice). rrcd answers
/// "Registered public rooms:" and a line `  name - topic` or `  name` per
/// room, or "No public rooms registered" (`commands.py`); a hub of
/// another implementation heads its list alike and puts a bare name on a
/// line, or several after the head's colon, comma-separated. A name is
/// kept as the hub spells it, spaces and slashes included: it is what a
/// join asks for. An empty list is an answer that names none.
pub fn list_answer(text: &str) -> Option<Vec<String>> {
    let lower = text.to_lowercase();
    if lower.contains("no public rooms") || lower.contains("no rooms") {
        return Some(Vec::new());
    }
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let head = lines.next()?;
    let (said, rest) = head.split_once(':')?;
    if !said.to_lowercase().contains("room") {
        return None;
    }
    let mut rooms: Vec<String> = Vec::new();
    let mut keep = |item: &str| {
        let item = item.trim_start_matches(['-', '*', '\u{2022}']).trim();
        let name = match item.split_once(" - ") {
            Some((name, _topic)) => name.trim(),
            None => item,
        };
        if !name.is_empty() && !rooms.iter().any(|r| r == name) {
            rooms.push(name.to_string());
        }
    };
    for item in rest.split(',') {
        keep(item);
    }
    for line in lines {
        keep(line);
    }
    Some(rooms)
}

/// The rooms a hub names (1652): its list first, as it spells them, then
/// what its greeting named ([`rooms_named`]) that the list has not, a
/// `#word` and the bare word counted as one.
pub fn rooms_merged(listed: &[String], greeting: &[String]) -> Vec<String> {
    let bare = |name: &str| name.trim_start_matches('#').to_lowercase();
    let mut out: Vec<String> = Vec::new();
    for room in listed.iter().chain(greeting) {
        if !out.iter().any(|r| bare(r) == bare(room)) {
            out.push(room.clone());
        }
    }
    out
}

/// Whether an announce's name hash is a hub's (`rrc.hub`).
pub fn is_hub(name_hash: &[u8]) -> bool {
    name_hash
        == leviculum_core::Destination::compute_name_hash(
            leviculum_rrc::HUB_APP,
            &[leviculum_rrc::HUB_ASPECT],
        )
}

/// The hub's name in its announce's app data: rrcd's CBOR map
/// `{"proto": "rrc", "v": 1, "hub": name}`, cleaned ([`shown_name`]).
pub fn announced_name(app_data: &[u8]) -> Option<String> {
    let Ok(leviculum_rrc::Value::Map(pairs)) = leviculum_rrc::Value::decode(app_data) else {
        return None;
    };
    pairs.iter().find_map(|(k, v)| match (k, v) {
        (leviculum_rrc::Value::Text(k), leviculum_rrc::Value::Text(v)) if k == "hub" => {
            shown_name(v)
        }
        _ => None,
    })
}

/// A hub's name as a reader sees it (1577's rule): without the protocol's
/// word, its underscores as spaces, at most 40 characters; `None` when
/// nothing is left or only "hub", which the settings call "Unnamed hub".
pub fn shown_name(name: &str) -> Option<String> {
    let spaced: String = name
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if c == '_' { ' ' } else { c })
        .collect();
    let mut words: Vec<String> = Vec::new();
    let mut said = spaced.split_whitespace().peekable();
    while let Some(word) = said.next() {
        let lower = word.to_lowercase();
        // "Relay Chat" as two words.
        if lower == "relay" && said.peek().is_some_and(|w| w.eq_ignore_ascii_case("chat")) {
            said.next();
            continue;
        }
        if lower == "rrc" || lower == "-" {
            continue;
        }
        // "CascadiaRRC", "Chatsurfer-RRC".
        let tail = word
            .len()
            .checked_sub(3)
            .filter(|at| word.is_char_boundary(*at));
        match tail {
            Some(at) if word[at..].eq_ignore_ascii_case("rrc") => {
                let rest = word[..at].trim_end_matches('-');
                if !rest.is_empty() {
                    words.push(rest.to_string());
                }
            }
            _ => words.push(word.to_string()),
        }
    }
    let out: String = words.join(" ").chars().take(40).collect();
    let lower = out.to_lowercase();
    if out.is_empty() || lower == "hub" || lower.contains("rrc") {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped file: five hubs that answered, each named, the two
    /// that did not left out, no hash twice.
    #[test]
    fn the_shipped_list_is_the_hubs_that_answered() {
        let hubs = shipped();
        let names: Vec<&str> = hubs.iter().map(|h| h.name.as_str()).collect();
        // 1599: 68 that answered on 2026-11-09, the five 1577 found
        // published among them, closest first.
        assert_eq!(hubs.len(), 68);
        for kept in [
            "RNS Community",
            "ShadowLink",
            "HADARC",
            "RNS Moscow",
            "Radio Oxford",
        ] {
            assert!(names.contains(&kept), "{kept}");
        }
        assert_eq!(names[0], "RNS Moscow", "three hops, first in the file");
        let hops: Vec<u8> = hubs.iter().map(|h| h.hops.unwrap_or(u8::MAX)).collect();
        assert!(
            hops.windows(2).all(|w| w[0] <= w[1]),
            "closest first: {hops:?}"
        );
        assert!(hops.iter().all(|h| *h != u8::MAX), "every hub has its hops");
        let mut hashes: Vec<[u8; 16]> = hubs.iter().map(|h| h.hash).collect();
        hashes.sort_unstable();
        hashes.dedup();
        assert_eq!(hashes.len(), hubs.len(), "no hash twice");
        let community = hubs.iter().find(|h| h.name == "RNS Community");
        assert_eq!(
            community.map(|h| h.rooms.clone()),
            Some(vec!["general".to_string()])
        );
        let heads = |head: &str| SHIPPED.lines().filter(|l| l.trim() == head).count();
        let unreached = heads("[[unreached]]");
        assert_eq!(unreached, 2, "MichMesh and Amber are recorded");
        assert_eq!(
            heads("[[hub]]"),
            hubs.len(),
            "every [[hub]] entry parses: a name and 32 hex digits"
        );
        let hadarc = [
            0xe7, 0xfd, 0x7f, 0x9e, 0xfd, 0x66, 0x08, 0xca, 0x4a, 0xde, 0x87, 0x9d, 0x18, 0x64,
            0x6e, 0xed,
        ];
        assert!(named(&hadarc).is_some_and(|h| h.name == "HADARC"));
        assert!(named(&[0; 16]).is_none());
    }

    /// The control: a broken hash and an unreached entry are no hubs.
    #[test]
    fn a_broken_entry_is_no_hub() {
        let text = "[[hub]]\nname = \"a\"\nhash = \"28c7\"\n\
                    [[unreached]]\nname = \"b\"\nhash = \"28c7c1a68c735693aa8e6b8193ed44b2\"\n\
                    [[hub]]\nname = \"c\"\nhash = \"28C7C1A68C735693AA8E6B8193ED44B2\"\nrooms = [\"#lab\"]\n";
        let hubs = parse(text);
        assert_eq!(hubs.len(), 1);
        assert_eq!(hubs[0].name, "c");
        assert_eq!(hubs[0].hash[0], 0x28);
        assert_eq!(hubs[0].rooms, ["#lab"], "a room's # is no comment");
    }

    /// A user never reads the protocol's name (1577): no hub the settings
    /// offer is called by it.
    #[test]
    fn no_shipped_name_says_the_protocol() {
        for hub in shipped() {
            assert!(
                !hub.name.to_lowercase().contains("rrc")
                    && !hub.name.to_lowercase().contains("relay chat"),
                "{}",
                hub.name
            );
        }
    }

    /// 1599: an announced name loses the protocol's word as the shipped
    /// ones did; "hub" alone and nothing are no name.
    #[test]
    fn an_announced_name_is_cleaned_as_the_shipped_ones() {
        for (said, shown) in [
            ("HADARC_rrc", Some("HADARC")),
            ("Minuteman RRC", Some("Minuteman")),
            ("CascadiaRRC", Some("Cascadia")),
            ("Chatsurfer-RRC", Some("Chatsurfer")),
            ("[SPB_RNS]_Relay_Chat_Service", Some("[SPB RNS] Service")),
            ("ValleyMesh RRC Hub", Some("ValleyMesh Hub")),
            ("Gothenburg SE (Swe) RRC", Some("Gothenburg SE (Swe)")),
            ("hub", None),
            ("rrc", None),
            ("", None),
            ("  ", None),
        ] {
            assert_eq!(shown_name(said).as_deref(), shown, "{said}");
        }
        // The announce's own shape (rrcd), and the control: no `hub` key.
        let data = leviculum_rrc::Value::Map(vec![
            (
                leviculum_rrc::Value::Text("proto".into()),
                leviculum_rrc::Value::Text("rrc".into()),
            ),
            (
                leviculum_rrc::Value::Text("hub".into()),
                leviculum_rrc::Value::Text("Le_Salon_FR".into()),
            ),
        ])
        .encode();
        assert_eq!(announced_name(&data).as_deref(), Some("Le Salon FR"));
        assert_eq!(announced_name(&[0xa0]), None, "an empty map names nothing");
        assert_eq!(announced_name(b"garbage"), None);
    }

    fn heard(hash: u8, name: Option<&str>, hops: u8, at: i64) -> HeardHub {
        HeardHub {
            hash: [hash; 16],
            name: name.map(Into::into),
            hops: Some(hops),
            first: at,
            last: at,
            answered: None,
        }
    }

    /// 1599: the heard list keeps a hub's first hearing, takes its newest
    /// name and hops, sorts last seen first and the closer first on a tie,
    /// and drops the one heard longest ago past its bound.
    #[test]
    fn the_heard_list_merges_sorts_and_bounds() {
        let mut list = Heard::default();
        assert!(list.hear(heard(1, Some("One"), 5, 100)));
        assert!(list.hear(heard(2, None, 3, 100)));
        assert!(list.hear(heard(3, Some("Three"), 7, 200)));
        let order: Vec<u8> = list.hubs().iter().map(|h| h.hash[0]).collect();
        assert_eq!(order, [3, 2, 1], "last seen first, closer first on a tie");
        assert!(!list.hear(heard(1, None, 4, 300)), "known");
        let one = list.get(&[1; 16]).cloned();
        assert_eq!(
            one.map(|h| (h.name, h.hops, h.first, h.last)),
            Some((Some("One".into()), Some(4), 100, 300)),
            "first kept, a nameless hearing keeps the name"
        );
        // An older hearing reported late moves nothing but the first.
        list.hear(heard(1, Some("Old"), 9, 50));
        let one = list.get(&[1; 16]).cloned();
        assert_eq!(
            one.map(|h| (h.name, h.hops, h.first, h.last)),
            Some((Some("One".into()), Some(4), 50, 300))
        );
        list.answered(&[2; 16], false);
        assert_eq!(list.get(&[2; 16]).and_then(|h| h.answered), Some(false));
        assert!(list.get(&[2; 16]).is_some(), "no answer removes nothing");
        // The bound: the oldest goes.
        let mut full = Heard::default();
        for i in 0..HEARD_BOUND {
            let mut hub = heard(0, None, 1, 1000 + i as i64);
            hub.hash[..8].copy_from_slice(&(i as u64).to_be_bytes());
            full.hear(hub);
        }
        let mut late = heard(0xff, None, 1, 5000);
        late.hash[0] = 0xff;
        full.hear(late);
        assert_eq!(full.hubs().len(), HEARD_BOUND);
        let mut oldest = [0u8; 16];
        oldest[..8].copy_from_slice(&0u64.to_be_bytes());
        assert!(
            full.get(&oldest).is_none(),
            "the one heard longest ago went"
        );
        assert!(full.get(&[0xff; 16]).is_some());
    }

    fn names(rooms: &[&str]) -> Vec<String> {
        rooms.iter().map(|r| r.to_string()).collect()
    }

    /// 1652: rrcd's answer with topics, as Colorado Mesh gave it.
    #[test]
    fn the_list_answer_with_topics_names_its_rooms() {
        let text = "Registered public rooms:\n  general - General chat\n  mesh-client - Clients: which one\n  ratspeak";
        assert_eq!(
            list_answer(text),
            Some(names(&["general", "mesh-client", "ratspeak"]))
        );
    }

    /// 1652: the other implementation's form, bare names with spaces and
    /// slashes, on lines or after the head's colon.
    #[test]
    fn the_list_answer_of_bare_names_keeps_them_as_spelled() {
        let lines = "Public rooms:\ngeneral chat\ngmrs/ham\nmeshcore\nreticulum\nwifihalow";
        let want = names(&[
            "general chat",
            "gmrs/ham",
            "meshcore",
            "reticulum",
            "wifihalow",
        ]);
        assert_eq!(list_answer(lines), Some(want.clone()));
        let one = "Rooms: general chat, gmrs/ham, meshcore, reticulum, wifihalow";
        assert_eq!(list_answer(one), Some(want));
        let bullets = "Rooms on this hub:\n - lobby\n * radio - ham radio";
        assert_eq!(list_answer(bullets), Some(names(&["lobby", "radio"])));
    }

    /// 1652: "No public rooms registered" is an answer naming none; a
    /// greeting is no answer at all.
    #[test]
    fn the_empty_answer_and_a_greeting_are_told_apart() {
        assert_eq!(list_answer("No public rooms registered"), Some(Vec::new()));
        assert_eq!(list_answer("Registered public rooms:\n"), Some(Vec::new()));
        assert_eq!(list_answer("Welcome to the Colorado Mesh RRC hub."), None);
        assert_eq!(
            list_answer("Welcome to Minuteman: try #lobby"),
            None,
            "a colon without rooms before it is no list"
        );
        assert_eq!(list_answer(""), None);
    }

    /// 1652: the list first as spelled, the greeting's hits after, `#lobby`
    /// and `lobby` one room.
    #[test]
    fn the_list_and_the_greeting_merge_without_doubles() {
        let listed = names(&["lobby", "radio"]);
        let greeting = rooms_named("Welcome! Join #lobby or #help.");
        assert_eq!(
            rooms_merged(&listed, &greeting),
            names(&["lobby", "radio", "#help"])
        );
        assert_eq!(rooms_merged(&[], &greeting), names(&["#lobby", "#help"]));
    }
}

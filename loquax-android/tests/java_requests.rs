//! Every request the app's Java builds (1207), in Java's bytes and with
//! exactly the fields Java puts in, decoded as the library decodes it:
//! one row per builder of `Core.java`, `Locator.share` and the two map
//! requests of `MapView`. 1199 and 1201 sent the settings pages' map
//! questions as a map frame without the frame's `w` and `h`; the facade
//! refused them with "missing field `w`" and no test noticed, because the
//! tests built those requests in Rust with every field present. A field
//! the Rust type needs and Java does not send fails here by name.

use loquax_android::schema::{self, MapRequest, Query, Request, Start};

mod support {
    pub mod java;
}
use support::java::{java_map, Java};

const PEER: &str = "0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c";
const OTHER: &str = "0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d";
const DIR: &str = "/data/user/0/network.leviculum.loquax/files";
const LXMA: &str = "lxma://0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c:00";

/// A key as `Core.num` reads one from a row: any long, here past i32.
const KEY: i64 = -5_000_000_000;

/// `Book.copy`'s map: contact_id, lookup, name, and photo when there is one.
fn copy<'a>(photo: Option<&'a [u8]>) -> Vec<(&'a str, Java<'a>)> {
    let mut copy = vec![
        ("contact_id", Java::Long(4711)),
        ("lookup", Java::Str("0r1-2C3E4A")),
        ("name", Java::Str("Anna Beispiel")),
    ];
    if let Some(photo) = photo {
        copy.push(("photo", Java::Bin(photo)));
    }
    copy
}

/// Each `Core` builder's bytes, named by its Java method, and what the
/// library must make of them.
fn commands() -> Vec<(&'static str, Vec<u8>, Request)> {
    let photo = [0x89u8, b'P', b'N', b'G'];
    let mut link = vec![("t", Java::Str("link")), ("peer", Java::Str(PEER))];
    link.extend(copy(Some(&photo)));
    let mut im_row = copy(None);
    im_row.push(("address", Java::Str(PEER)));
    let contact = schema::ContactCopy {
        lookup: "0r1-2C3E4A".to_string(),
        contact_id: 4711,
        name: "Anna Beispiel".to_string(),
        photo: None,
    };
    let s = |text: &str| text.to_string();
    vec![
        (
            "addContact",
            java_map(&[
                ("t", Java::Str("addcontact")),
                ("lxma", Java::Str(LXMA)),
                ("name", Java::Str("Bob")),
            ]),
            Request::AddContact {
                lxma: s(LXMA),
                name: Some(s("Bob")),
            },
        ),
        (
            "addContact (name null)",
            java_map(&[
                ("t", Java::Str("addcontact")),
                ("lxma", Java::Str(LXMA)),
                ("name", Java::Nil),
            ]),
            Request::AddContact {
                lxma: s(LXMA),
                name: None,
            },
        ),
        (
            "name",
            java_map(&[("t", Java::Str("name")), ("name", Java::Str("Pixel"))]),
            Request::Name { name: s("Pixel") },
        ),
        (
            "open",
            java_map(&[("t", Java::Str("open")), ("peer", Java::Str(PEER))]),
            Request::Open { peer: s(PEER) },
        ),
        (
            "close",
            java_map(&[("t", Java::Str("close"))]),
            Request::Close,
        ),
        (
            "send",
            java_map(&[
                ("t", Java::Str("send")),
                ("peer", Java::Str(PEER)),
                ("body", Java::Str("hallo")),
            ]),
            Request::Send {
                peer: s(PEER),
                body: s("hallo"),
            },
        ),
        (
            "react",
            java_map(&[
                ("t", Java::Str("react")),
                ("peer", Java::Str(PEER)),
                ("target", Java::Str("ab")),
                ("content", Java::Str("👍")),
            ]),
            Request::React {
                peer: s(PEER),
                target: s("ab"),
                content: s("👍"),
            },
        ),
        (
            "reply",
            java_map(&[
                ("t", Java::Str("reply")),
                ("peer", Java::Str(PEER)),
                ("target", Java::Str("ab")),
                ("body", Java::Str("ja")),
            ]),
            Request::Reply {
                peer: s(PEER),
                target: s("ab"),
                body: s("ja"),
            },
        ),
        (
            "noticeReply",
            java_map(&[
                ("t", Java::Str("noticereply")),
                ("peer", Java::Str(PEER)),
                ("key", Java::Long(KEY)),
                ("body", Java::Str("bin gleich da")),
            ]),
            Request::NoticeReply {
                peer: s(PEER),
                key: KEY,
                body: s("bin gleich da"),
            },
        ),
        (
            "forward",
            java_map(&[
                ("t", Java::Str("forward")),
                ("peer", Java::Str(PEER)),
                ("key", Java::Long(KEY)),
                ("to", Java::Str(OTHER)),
            ]),
            Request::Forward {
                peer: s(PEER),
                key: KEY,
                to: s(OTHER),
            },
        ),
        (
            "photo",
            java_map(&[
                ("t", Java::Str("photo")),
                ("peer", Java::Str(PEER)),
                ("data", Java::Bin(&[0xff, 0xd8, 0xff, 0xe0])),
            ]),
            Request::Photo {
                peer: s(PEER),
                data: schema::Blob(vec![0xff, 0xd8, 0xff, 0xe0]),
            },
        ),
        (
            "file",
            java_map(&[
                ("t", Java::Str("file")),
                ("peer", Java::Str(PEER)),
                ("name", Java::Str("Grüße.pdf")),
                ("data", Java::Bin(&[0x25, 0x50, 0x44, 0x46])),
                ("size", Java::Long(4_200_000)),
            ]),
            Request::File {
                peer: s(PEER),
                name: s("Grüße.pdf"),
                data: schema::Blob(vec![0x25, 0x50, 0x44, 0x46]),
                size: 4_200_000,
            },
        ),
        (
            "voice",
            java_map(&[
                ("t", Java::Str("voice")),
                ("peer", Java::Str(PEER)),
                ("pcm", Java::Bin(&[0x01, 0x00, 0xff, 0xff])),
                ("rate", Java::Long(16_000)),
            ]),
            Request::Voice {
                peer: s(PEER),
                pcm: schema::Blob(vec![0x01, 0x00, 0xff, 0xff]),
                rate: 16_000,
            },
        ),
        (
            "playVoice",
            java_map(&[
                ("t", Java::Str("playvoice")),
                ("peer", Java::Str(PEER)),
                ("key", Java::Long(KEY)),
                ("on", Java::Bool(true)),
            ]),
            Request::PlayVoice {
                peer: s(PEER),
                key: KEY,
                on: true,
            },
        ),
        (
            "sendAgain",
            java_map(&[
                ("t", Java::Str("sendagain")),
                ("peer", Java::Str(PEER)),
                ("key", Java::Long(KEY)),
            ]),
            Request::SendAgain {
                peer: s(PEER),
                key: KEY,
            },
        ),
        (
            "delete",
            java_map(&[
                ("t", Java::Str("delete")),
                ("peer", Java::Str(PEER)),
                ("keys", Java::Longs(&[KEY, 3])),
            ]),
            Request::Delete {
                peer: s(PEER),
                keys: vec![KEY, 3],
            },
        ),
        (
            "used",
            java_map(&[("t", Java::Str("used")), ("emoji", Java::Str("🙂"))]),
            Request::Used { emoji: s("🙂") },
        ),
        (
            "link",
            java_map(&link),
            Request::Link {
                peer: s(PEER),
                contact: schema::ContactCopy {
                    photo: Some(schema::Blob(photo.to_vec())),
                    ..contact.clone()
                },
            },
        ),
        (
            "unlink",
            java_map(&[("t", Java::Str("unlink")), ("peer", Java::Str(PEER))]),
            Request::Unlink { peer: s(PEER) },
        ),
        (
            "notNow",
            java_map(&[("t", Java::Str("notnow")), ("peer", Java::Str(PEER))]),
            Request::NotNow { peer: s(PEER) },
        ),
        (
            "rename",
            java_map(&[
                ("t", Java::Str("rename")),
                ("peer", Java::Str(PEER)),
                ("name", Java::Str("")),
            ]),
            Request::Rename {
                peer: s(PEER),
                name: String::new(),
            },
        ),
        (
            "relink",
            java_map(&[
                ("t", Java::Str("relink")),
                ("rows", Java::List(vec![Java::Map(im_row)])),
            ]),
            Request::Relink {
                rows: vec![schema::ImRow {
                    address: s(PEER),
                    contact,
                }],
            },
        ),
        (
            "pin",
            java_map(&[
                ("t", Java::Str("pin")),
                ("peer", Java::Str(PEER)),
                ("on", Java::Bool(true)),
            ]),
            Request::Pin {
                peer: s(PEER),
                on: true,
            },
        ),
        (
            "mute (always)",
            java_map(&[
                ("t", Java::Str("mute")),
                ("peer", Java::Str(PEER)),
                ("secs", Java::Long(-1)),
            ]),
            Request::Mute {
                peer: s(PEER),
                secs: -1,
            },
        ),
        (
            "mute (a week)",
            java_map(&[
                ("t", Java::Str("mute")),
                ("peer", Java::Str(PEER)),
                ("secs", Java::Long(604_800)),
            ]),
            Request::Mute {
                peer: s(PEER),
                secs: 604_800,
            },
        ),
        (
            "archive",
            java_map(&[
                ("t", Java::Str("archive")),
                ("peer", Java::Str(PEER)),
                ("on", Java::Bool(false)),
            ]),
            Request::Archive {
                peer: s(PEER),
                on: false,
            },
        ),
        (
            "block",
            java_map(&[
                ("t", Java::Str("block")),
                ("peer", Java::Str(PEER)),
                ("on", Java::Bool(true)),
            ]),
            Request::Block {
                peer: s(PEER),
                on: true,
            },
        ),
        (
            "verify",
            java_map(&[
                ("t", Java::Str("verify")),
                ("peer", Java::Str(PEER)),
                ("on", Java::Bool(true)),
            ]),
            Request::Verify {
                peer: s(PEER),
                on: true,
            },
        ),
        (
            "unread",
            java_map(&[
                ("t", Java::Str("unread")),
                ("peer", Java::Str(PEER)),
                ("on", Java::Bool(true)),
            ]),
            Request::Unread {
                peer: s(PEER),
                on: true,
            },
        ),
        (
            "forget",
            java_map(&[
                ("t", Java::Str("forget")),
                ("peer", Java::Str(PEER)),
                ("everywhere", Java::Bool(true)),
            ]),
            Request::Forget {
                peer: s(PEER),
                everywhere: true,
            },
        ),
        ("pull", java_map(&[("t", Java::Str("pull"))]), Request::Pull),
        (
            "profileShown",
            java_map(&[("t", Java::Str("profile")), ("open", Java::Bool(true))]),
            Request::Profile { open: true },
        ),
        (
            "front",
            java_map(&[("t", Java::Str("front")), ("front", Java::Bool(true))]),
            Request::Front { front: true },
        ),
        (
            "viewport",
            java_map(&[
                ("t", Java::Str("viewport")),
                ("peer", Java::Str(PEER)),
                ("keys", Java::Longs(&[KEY, 3])),
            ]),
            Request::Viewport {
                peer: s(PEER),
                keys: vec![KEY, 3],
            },
        ),
        (
            "network",
            java_map(&[
                ("t", Java::Str("network")),
                ("kind", Java::Str("mobile")),
                ("changed", Java::Bool(true)),
            ]),
            Request::Network {
                kind: "mobile".to_string(),
                changed: true,
            },
        ),
        (
            "Locator.share",
            java_map(&[
                ("t", Java::Str("share")),
                ("peer", Java::Str(PEER)),
                ("lat_e6", Java::Long(52_520_000)),
                ("lon_e6", Java::Long(-13_405_000)),
                ("alt_e2", Java::Long(0)),
                ("accuracy_e2", Java::Long(1_250)),
                ("at", Java::Long(1_793_475_600)),
            ]),
            Request::Share {
                peer: s(PEER),
                lat_e6: 52_520_000,
                lon_e6: -13_405_000,
                alt_e2: 0,
                accuracy_e2: 1_250,
                at: 1_793_475_600,
            },
        ),
        (
            "stage",
            java_map(&[
                ("t", Java::Str("stage")),
                ("peer", Java::Str(PEER)),
                ("data", Java::Bin(&[0xff, 0xd8, 0xff, 0xe1])),
            ]),
            Request::Stage {
                peer: s(PEER),
                data: schema::Blob(vec![0xff, 0xd8, 0xff, 0xe1]),
            },
        ),
        (
            "unstage",
            java_map(&[("t", Java::Str("unstage")), ("peer", Java::Str(PEER))]),
            Request::Unstage { peer: s(PEER) },
        ),
        (
            "sheet",
            java_map(&[
                ("t", Java::Str("sheet")),
                ("peer", Java::Str(PEER)),
                ("kind", Java::Str("position")),
            ]),
            Request::Sheet {
                peer: s(PEER),
                kind: s("position"),
            },
        ),
        (
            "sheet (closed)",
            java_map(&[
                ("t", Java::Str("sheet")),
                ("peer", Java::Str(PEER)),
                ("kind", Java::Str("")),
            ]),
            Request::Sheet {
                peer: s(PEER),
                kind: String::new(),
            },
        ),
        (
            "noFix",
            java_map(&[("t", Java::Str("nofix"))]),
            Request::NoFix,
        ),
        (
            "shareFix",
            java_map(&[("t", Java::Str("sharefix")), ("peer", Java::Str(PEER))]),
            Request::ShareFix { peer: s(PEER) },
        ),
        (
            "startShare",
            java_map(&[
                ("t", Java::Str("startshare")),
                ("peer", Java::Str(PEER)),
                ("span", Java::Str("15_min")),
            ]),
            Request::StartShare {
                peer: s(PEER),
                span: s("15_min"),
            },
        ),
        (
            "stopShare",
            java_map(&[("t", Java::Str("stopshare")), ("peer", Java::Str(PEER))]),
            Request::StopShare { peer: s(PEER) },
        ),
        // The microphone's gestures and the recorder's levels (1276):
        // `Core.record` writes every field, all of them longs.
        record("press", 5_000_123, 0, 0, 0),
        record("slide", 0, -120, -80, 0),
        record("release", 5_003_000, 0, 0, 0),
        record("cancel", 0, 0, 0, 0),
        record("level", 5_000_173, 0, 0, 32_767),
        record("tick", 5_000_400, 0, 0, 0),
        record("played", 0, 0, 0, 0),
        (
            "recordingVerb",
            java_map(&[
                ("t", Java::Str("recording")),
                ("peer", Java::Str(PEER)),
                ("verb", Java::Str("rec_lock")),
            ]),
            Request::Recording {
                peer: s(PEER),
                verb: s("rec_lock"),
            },
        ),
        (
            "deleteRegions",
            java_map(&[("t", Java::Str("deleteregions"))]),
            Request::DeleteRegions,
        ),
        (
            "Locator.tell",
            java_map(&[
                ("t", Java::Str("fix")),
                ("lat_e6", Java::Long(43_773_100)),
                ("lon_e6", Java::Long(11_256_000)),
                ("alt_e2", Java::Long(0)),
                ("accuracy_e2", Java::Long(500)),
                ("at", Java::Long(1_793_475_600)),
            ]),
            Request::Fix {
                lat_e6: 43_773_100,
                lon_e6: 11_256_000,
                alt_e2: 0,
                accuracy_e2: 500,
                at: 1_793_475_600,
            },
        ),
        // The Browse tab (1337): `Core.browse` and `Core.browseSize`.
        (
            "browse",
            java_map(&[
                ("t", Java::Str("browse")),
                ("verb", Java::Str("activate")),
                ("node", Java::Str("link_3")),
                ("text", Java::Str("")),
            ]),
            Request::Browse {
                verb: s("activate"),
                node: s("link_3"),
                text: String::new(),
                cols: 0,
                rows: 0,
            },
        ),
        (
            "browseSize",
            java_map(&[
                ("t", Java::Str("browse")),
                ("verb", Java::Str("size")),
                ("cols", Java::Long(53)),
                ("rows", Java::Long(22)),
            ]),
            Request::Browse {
                verb: s("size"),
                node: String::new(),
                text: String::new(),
                cols: 53,
                rows: 22,
            },
        ),
        // The Rooms tab (1384): `Core.rooms`.
        (
            "rooms",
            java_map(&[
                ("t", Java::Str("rooms")),
                ("verb", Java::Str("show")),
                ("text", Java::Str("")),
                ("index", Java::Long(-1)),
            ]),
            Request::Rooms {
                verb: s("show"),
                text: String::new(),
                index: -1,
            },
        ),
        // The Explore tab (1578): `Core.explore`.
        (
            "explore",
            java_map(&[
                ("t", Java::Str("explore")),
                ("verb", Java::Str("open")),
                ("text", Java::Str("")),
                ("dest", Java::Str(PEER)),
                ("index", Java::Long(0)),
            ]),
            Request::Explore {
                verb: s("open"),
                text: String::new(),
                dest: s(PEER),
                index: 0,
            },
        ),
    ]
}

/// `Core.record`'s bytes for `input`: every field, each a long.
fn record(
    input: &'static str,
    now: i64,
    dx: i64,
    dy: i64,
    rms: i64,
) -> (&'static str, Vec<u8>, Request) {
    (
        input,
        java_map(&[
            ("t", Java::Str("record")),
            ("peer", Java::Str(PEER)),
            ("input", Java::Str(input)),
            ("now", Java::Long(now)),
            ("dx", Java::Long(dx)),
            ("dy", Java::Long(dy)),
            ("rms", Java::Long(rms)),
        ]),
        Request::Record {
            peer: PEER.to_string(),
            input: input.to_string(),
            now,
            dx: dx as i32,
            dy: dy as i32,
            rms: rms as u32,
        },
    )
}

fn queries() -> Vec<(&'static str, Vec<u8>, Query)> {
    let s = |text: &str| text.to_string();
    let t = |tag: &str| java_map(&[("t", Java::Str(tag))]);
    let candidate = Java::Map(vec![
        ("key", Java::Str("0r1-2C3E4A")),
        ("display", Java::Str("Anna Beispiel")),
        ("nicknames", Java::Strs(&["Anni"])),
        ("given", Java::Str("Anna")),
        ("middle", Java::Str("")),
        ("family", Java::Str("Beispiel")),
    ]);
    vec![
        (
            "backup",
            java_map(&[
                ("t", Java::Str("backup")),
                ("passphrase", Java::Str("Kastanienallee 7")),
            ]),
            Query::Backup {
                passphrase: s("Kastanienallee 7"),
            },
        ),
        ("me", t("me"), Query::Me),
        ("profile", t("profile"), Query::Profile),
        ("mailboxPage", t("mailboxpage"), Query::MailboxPage),
        ("linkDevice", t("linkdevice"), Query::LinkDevice),
        ("linked", t("linked"), Query::Linked),
        ("movePhone", t("movephone"), Query::MovePhone),
        ("moving", t("moving"), Query::Moving),
        (
            "lxma",
            java_map(&[
                ("t", Java::Str("lxma")),
                ("text", Java::Str(LXMA)),
                ("lang", Java::Str("de-DE")),
            ]),
            Query::Lxma {
                text: s(LXMA),
                lang: Some(s("de-DE")),
            },
        ),
        (
            "code",
            java_map(&[
                ("t", Java::Str("code")),
                ("text", Java::Str(LXMA)),
                ("lang", Java::Str("de-DE")),
            ]),
            Query::Code {
                text: s(LXMA),
                lang: Some(s("de-DE")),
            },
        ),
        (
            "info",
            java_map(&[
                ("t", Java::Str("info")),
                ("peer", Java::Str(PEER)),
                ("key", Java::Long(KEY)),
            ]),
            Query::Info {
                peer: s(PEER),
                key: KEY,
            },
        ),
        ("recents", t("recents"), Query::Recents),
        (
            "suggest",
            java_map(&[
                ("t", Java::Str("suggest")),
                ("peer", Java::Str(PEER)),
                ("candidates", Java::List(vec![candidate])),
            ]),
            Query::Suggest {
                peer: s(PEER),
                candidates: vec![schema::CandidateRow {
                    key: s("0r1-2C3E4A"),
                    display: s("Anna Beispiel"),
                    given: s("Anna"),
                    middle: String::new(),
                    family: s("Beispiel"),
                    nicknames: vec![s("Anni")],
                    last_contacted: 0,
                }],
            },
        ),
        (
            "contact",
            java_map(&[
                ("t", Java::Str("contact")),
                ("lookup", Java::Str("0r1-2C3E4A")),
            ]),
            Query::Contact {
                lookup: s("0r1-2C3E4A"),
            },
        ),
        (
            "run",
            java_map(&[
                ("t", Java::Str("run")),
                ("peer", Java::Str(PEER)),
                ("key", Java::Long(KEY)),
            ]),
            Query::Run {
                peer: s(PEER),
                key: KEY,
            },
        ),
        (
            "friends",
            t("friends"),
            Query::Friends {
                lat_e6: None,
                lon_e6: None,
            },
        ),
        (
            "friends (own fix)",
            java_map(&[
                ("t", Java::Str("friends")),
                ("lat_e6", Java::Long(52_520_000)),
                ("lon_e6", Java::Long(13_405_000)),
            ]),
            Query::Friends {
                lat_e6: Some(52_520_000),
                lon_e6: Some(13_405_000),
            },
        ),
        ("shared", t("shared"), Query::Shared),
        (
            "mapRegions",
            t("mapregions"),
            Query::MapRegions {
                delete: None,
                set_autoload: None,
            },
        ),
        (
            "mapRegions (delete)",
            java_map(&[
                ("t", Java::Str("mapregions")),
                ("delete", Java::Str("berlin.lmr")),
            ]),
            Query::MapRegions {
                delete: Some(s("berlin.lmr")),
                set_autoload: None,
            },
        ),
        (
            "mapRegions (switch)",
            java_map(&[
                ("t", Java::Str("mapregions")),
                ("set_autoload", Java::Bool(false)),
            ]),
            Query::MapRegions {
                delete: None,
                set_autoload: Some(false),
            },
        ),
        ("mapTheme", t("maptheme"), Query::MapTheme { set: None }),
        (
            "mapTheme (set)",
            java_map(&[("t", Java::Str("maptheme")), ("set", Java::Str("dark"))]),
            Query::MapTheme {
                set: Some(s("dark")),
            },
        ),
        ("photoSize", t("photosize"), Query::PhotoSize { set: None }),
        (
            "photoSize (set)",
            java_map(&[("t", Java::Str("photosize")), ("set", Java::Str("small"))]),
            Query::PhotoSize {
                set: Some(s("small")),
            },
        ),
        (
            "photoBitmap",
            java_map(&[
                ("t", Java::Str("photo")),
                ("peer", Java::Str(PEER)),
                ("key", Java::Long(KEY)),
                ("edge", Java::Long(720)),
            ]),
            Query::Photo {
                peer: s(PEER),
                key: KEY,
                edge: 720,
            },
        ),
        (
            "saveFile",
            java_map(&[
                ("t", Java::Str("savefile")),
                ("peer", Java::Str(PEER)),
                ("key", Java::Long(KEY)),
                ("index", Java::Long(0)),
            ]),
            Query::SaveFile {
                peer: s(PEER),
                key: KEY,
                index: 0,
            },
        ),
        (
            "voicePcm",
            java_map(&[
                ("t", Java::Str("voice")),
                ("peer", Java::Str(PEER)),
                ("key", Java::Long(KEY)),
            ]),
            Query::Voice {
                peer: s(PEER),
                key: KEY,
            },
        ),
        (
            "bar",
            java_map(&[("t", Java::Str("bar")), ("peer", Java::Str(PEER))]),
            Query::Bar { peer: s(PEER) },
        ),
        (
            "recording",
            java_map(&[("t", Java::Str("recording")), ("peer", Java::Str(PEER))]),
            Query::Recording { peer: s(PEER) },
        ),
        (
            "shares",
            java_map(&[("t", Java::Str("shares"))]),
            Query::Shares,
        ),
        (
            "stagedBitmap",
            java_map(&[
                ("t", Java::Str("staged")),
                ("peer", Java::Str(PEER)),
                ("edge", Java::Long(256)),
            ]),
            Query::Staged {
                peer: s(PEER),
                edge: 256,
            },
        ),
        (
            "zoom",
            java_map(&[
                ("t", Java::Str("zoom")),
                ("view_w", Java::Real(1080.0)),
                ("view_h", Java::Real(2340.0)),
                ("picture_w", Java::Real(1600.0)),
                ("picture_h", Java::Real(1200.0)),
                ("scale", Java::Real(1.0)),
                ("pan_x", Java::Real(0.0)),
                ("pan_y", Java::Real(0.0)),
                ("gesture", Java::Str("pinch")),
                ("factor", Java::Real(1.25)),
                ("x", Java::Real(540.5)),
                ("y", Java::Real(1170.0)),
            ]),
            Query::Zoom(schema::ZoomAsk {
                view_w: 1080.0,
                view_h: 2340.0,
                picture_w: 1600.0,
                picture_h: 1200.0,
                scale: 1.0,
                pan_x: 0.0,
                pan_y: 0.0,
                gesture: s("pinch"),
                factor: 1.25,
                x: 540.5,
                y: 1170.0,
                dx: 0.0,
                dy: 0.0,
            }),
        ),
        ("mapFling", t("mapfling"), Query::MapFling { set: None }),
        (
            "mapFling (set)",
            java_map(&[("t", Java::Str("mapfling")), ("set", Java::Bool(false))]),
            Query::MapFling { set: Some(false) },
        ),
        ("links", t("links"), Query::Links),
        (
            "detail",
            java_map(&[("t", Java::Str("detail")), ("peer", Java::Str(PEER))]),
            Query::Detail { peer: s(PEER) },
        ),
        ("archived", t("archived"), Query::Archived { search: None }),
        ("blocked", t("blocked"), Query::Blocked),
        (
            "conversations",
            t("conversations"),
            Query::Conversations { search: None },
        ),
        (
            "conversations (search)",
            java_map(&[
                ("t", Java::Str("conversations")),
                ("search", Java::Str("anna rome")),
            ]),
            Query::Conversations {
                search: Some(s("anna rome")),
            },
        ),
        (
            "archived (search)",
            java_map(&[("t", Java::Str("archived")), ("search", Java::Str("anna"))]),
            Query::Archived {
                search: Some(s("anna")),
            },
        ),
        (
            "messages",
            java_map(&[("t", Java::Str("messages")), ("peer", Java::Str(PEER))]),
            Query::Messages { peer: s(PEER) },
        ),
        (
            "browseScreen",
            java_map(&[("t", Java::Str("browse"))]),
            Query::Browse,
        ),
        (
            "roomsScreen",
            java_map(&[("t", Java::Str("rooms"))]),
            Query::Rooms,
        ),
        (
            "exploreScreen",
            java_map(&[("t", Java::Str("explore"))]),
            Query::Explore,
        ),
        (
            "settingsList",
            java_map(&[("t", Java::Str("settings"))]),
            Query::Settings,
        ),
    ]
}

/// Every command and query builder of `Core.java` and `Locator.share`,
/// in Java's bytes, decodes to what it means.
#[test]
fn every_request_java_builds_decodes_as_java_means_it() {
    let mut wrong = Vec::new();
    for (name, bytes, want) in commands() {
        match schema::decode::<Request>(&bytes) {
            Ok(got) if got == want => {}
            got => wrong.push(format!("{name}: {got:?}")),
        }
    }
    for (name, bytes, want) in queries() {
        match schema::decode::<Query>(&bytes) {
            Ok(got) if got == want => {}
            got => wrong.push(format!("{name}: {got:?}")),
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Every request Java builds decodes into an intent at the boundary
/// (1217), its addresses and ids parsed, but the address book's four,
/// which are the phone's own glue; the intent carries what Java said.
#[test]
fn every_request_java_builds_is_an_intent_or_the_phones_glue() {
    use loquax::ui::intent::{Intent, Switch, Target};
    use loquax_android::session::{decode, Decoded};
    // The microphone's gestures and levels (1276) are the platform's
    // input, not intents: `Core.record`'s rows, named by their input.
    let glue = [
        "link",
        "unlink",
        "notNow",
        "relink",
        "press",
        "slide",
        "release",
        "cancel",
        "level",
        "tick",
        "played",
        "browse",
        "browseSize",
        "rooms",
        "explore",
    ];
    let mut wrong = Vec::new();
    for (name, bytes, _) in commands() {
        let request = schema::decode::<Request>(&bytes).expect("decodes");
        match (decode(request), glue.contains(&name)) {
            (Ok(Decoded::Intent(_)), false) | (Ok(Decoded::Platform(_)), true) => {}
            (Ok(Decoded::Intent(intent)), true) => wrong.push(format!("{name}: {intent:?}")),
            (Ok(Decoded::Platform(_)), false) => wrong.push(format!("{name}: platform")),
            // The table's react and reply name the message `ab`, which no id
            // is: refused at the boundary, as the session refused it before.
            (Err(error), false)
                if ["react", "reply"].contains(&name) && error.contains("is not a message id") => {}
            (Err(error), _) => wrong.push(format!("{name}: {error}")),
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    let peer = [0x3f; 16];
    let pin = Request::Pin {
        peer: loquax::address::to_hex(&peer),
        on: false,
    };
    match decode(pin) {
        Ok(Decoded::Intent(intent)) => assert_eq!(
            intent,
            Intent::Pin {
                peer: Target::Peer(peer),
                on: Switch::Off
            }
        ),
        _ => panic!("pin is an intent"),
    }
    let bad = Request::Open {
        peer: "not hex".to_string(),
    };
    assert!(decode(bad).is_err());
}

/// `Core.start` (later start, and the first with server, code and name),
/// `Core.startHere` (with and without a restore, and a restore anyway),
/// `Core.moveIn` and `Core.takeBack` (1281), as Java sends them.
#[test]
fn every_start_java_builds_decodes() {
    let s = |text: &str| Some(text.to_string());
    let cases = [
        (
            java_map(&[
                ("dir", Java::Str(DIR)),
                ("offset", Java::Long(3_600)),
                ("lang", Java::Str("de-DE")),
                ("camera", Java::Bool(true)),
            ]),
            Start {
                dir: DIR.to_string(),
                offset: 3_600,
                lang: s("de-DE"),
                camera: true,
                ..Start::default()
            },
        ),
        (
            java_map(&[
                ("dir", Java::Str(DIR)),
                ("offset", Java::Long(-18_000)),
                ("server", Java::Str("leviculum.network:4242")),
                ("code", Java::Str("lqxh1abc")),
                ("name", Java::Str("Pixel")),
            ]),
            Start {
                dir: DIR.to_string(),
                offset: -18_000,
                server: s("leviculum.network:4242"),
                code: s("lqxh1abc"),
                name: s("Pixel"),
                ..Start::default()
            },
        ),
        (
            java_map(&[
                ("dir", Java::Str(DIR)),
                ("offset", Java::Long(7_200)),
                ("lang", Java::Str("en-US")),
                ("here", Java::Bool(true)),
                ("camera", Java::Bool(false)),
            ]),
            Start {
                dir: DIR.to_string(),
                offset: 7_200,
                lang: s("en-US"),
                here: true,
                ..Start::default()
            },
        ),
        (
            java_map(&[
                ("dir", Java::Str(DIR)),
                ("offset", Java::Long(0)),
                ("here", Java::Bool(true)),
                ("restore", Java::Str("lqxb1line")),
                ("passphrase", Java::Str("Kastanienallee 7")),
            ]),
            Start {
                dir: DIR.to_string(),
                here: true,
                restore: s("lqxb1line"),
                passphrase: s("Kastanienallee 7"),
                ..Start::default()
            },
        ),
        (
            java_map(&[
                ("dir", Java::Str(DIR)),
                ("offset", Java::Long(0)),
                ("here", Java::Bool(true)),
                ("restore", Java::Str("lqxb1line")),
                ("passphrase", Java::Str("Kastanienallee 7")),
                ("anyway", Java::Bool(true)),
            ]),
            Start {
                dir: DIR.to_string(),
                here: true,
                restore: s("lqxb1line"),
                passphrase: s("Kastanienallee 7"),
                anyway: true,
                ..Start::default()
            },
        ),
        (
            java_map(&[
                ("dir", Java::Str(DIR)),
                ("offset", Java::Long(3_600)),
                ("lang", Java::Str("de-DE")),
                ("here", Java::Bool(true)),
                ("camera", Java::Bool(true)),
                ("moving", Java::Str("lqxh1abc@leviculum.network:4343")),
                ("name", Java::Str("Pixel 9")),
            ]),
            Start {
                dir: DIR.to_string(),
                offset: 3_600,
                lang: s("de-DE"),
                here: true,
                camera: true,
                moving: s("lqxh1abc@leviculum.network:4343"),
                name: s("Pixel 9"),
                ..Start::default()
            },
        ),
        (
            java_map(&[
                ("dir", Java::Str(DIR)),
                ("offset", Java::Long(0)),
                ("lang", Java::Str("en-US")),
                ("camera", Java::Bool(false)),
                ("take_back", Java::Bool(true)),
            ]),
            Start {
                dir: DIR.to_string(),
                lang: s("en-US"),
                take_back: true,
                ..Start::default()
            },
        ),
    ];
    for (bytes, want) in cases {
        assert_eq!(schema::decode::<Start>(&bytes), Ok(want));
    }
}

/// `MapView`'s frame request and its prefetch, with the fields Java
/// sends: the first frame (no centre yet), a later one with centre,
/// selection, a place to open at its zoom and the phone's fix.
#[test]
fn the_map_views_requests_decode() {
    let base = || {
        vec![
            ("dir", Java::Str(DIR)),
            ("w", Java::Long(1080)),
            ("h", Java::Long(2_100)),
            ("margin_x", Java::Long(270)),
            ("margin_y", Java::Long(525)),
            ("moving", Java::Bool(false)),
            ("density_milli", Java::Long(2_750)),
            ("dark", Java::Bool(true)),
            ("pan_x", Java::Long(-12)),
            ("pan_y", Java::Long(300)),
            ("zoom_by_milli", Java::Long(-415)),
            ("about_x", Java::Long(540)),
            ("about_y", Java::Long(1_050)),
            ("avatars", Java::Bool(true)),
        ]
    };
    let first: MapRequest = schema::decode(&java_map(&base())).expect("the first frame");
    assert_eq!(
        (first.w, first.h, first.margin_x, first.density_milli),
        (1080, 2_100, 270, 2_750)
    );
    assert!(first.dark && first.avatars && first.x.is_none());
    assert_eq!(
        (first.pan_x, first.pan_y, first.zoom_by_milli),
        (-12, 300, -415)
    );

    let mut later = base();
    later.extend([
        ("x", Java::Long(2_253_000_000)),
        ("y", Java::Long(1_376_000_000)),
        ("zoom_milli", Java::Long(16_250)),
        ("selected", Java::Str(PEER)),
        ("at_lat_e6", Java::Long(-33_868_000)),
        ("at_lon_e6", Java::Long(151_209_000)),
        ("at_zoom_milli", Java::Long(16_000)),
        ("own_lat_e6", Java::Long(52_520_000)),
        ("own_lon_e6", Java::Long(13_405_000)),
    ]);
    let later: MapRequest = schema::decode(&java_map(&later)).expect("a later frame");
    assert_eq!(
        (later.x, later.y, later.zoom_milli),
        (Some(2_253_000_000), Some(1_376_000_000), Some(16_250))
    );
    assert_eq!(later.selected.as_deref(), Some(PEER));
    assert_eq!(
        (later.at_lat_e6, later.at_lon_e6, later.at_zoom_milli),
        (Some(-33_868_000), Some(151_209_000), Some(16_000))
    );
    assert_eq!(
        (later.own_lat_e6, later.own_lon_e6),
        (Some(52_520_000), Some(13_405_000))
    );

    let prefetch: MapRequest = schema::decode(&java_map(&[
        ("dir", Java::Str(DIR)),
        ("prefetch", Java::Bool(true)),
        ("w", Java::Long(1080)),
        ("h", Java::Long(2_100)),
        ("density_milli", Java::Long(2_750)),
    ]))
    .expect("the prefetch");
    assert!(prefetch.prefetch && prefetch.w == 1080);
}

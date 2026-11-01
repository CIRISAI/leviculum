//! What crosses JNI, as msgpack maps with string keys
//! (`rmp_serde::to_vec_named`). Java reads and writes them with its own
//! small codec (`MsgPack.java`), so the shapes stay flat: strings, integers,
//! booleans, arrays of maps. Addresses and message ids travel as lowercase
//! hex, the form the screen shows and hands back.
//!
//! # Version 1
//!
//! Java to Rust:
//!
//! | call | payload |
//! |---|---|
//! | `start` | [`Start`] |
//! | `command` | [`Request`], tagged by `t`: `open`, `close`, `send`, `react`, `reply`, `forward`, `delete`, `used`, `pull`, `front`, `link`, `unlink`, `notnow`, `rename`, `relink`, `share`, `addcontact`, `draft` |
//! | `query` | [`Query`], tagged by `t`: `conversations`, `messages`, `info`, `recents`, `suggest`, `contact`, `links`, `run`, `friends`, `shared`, `profile`, `lxma`, `mapregions`, `maptheme`, `mapfling` |
//! | `renderMapInto` | [`MapRequest`], and the Bitmap to draw into (null for a prefetch) |
//!
//! Rust to Java:
//!
//! | call | answer |
//! |---|---|
//! | `start` | [`Started`] |
//! | `command` | [`Done`] |
//! | `query` | [`Conversations`], [`Messages`], [`Info`], [`Recents`], [`Suggestions`], [`Addresses`], [`Links`], [`Run`], [`Friends`], [`Shared`], [`Profile`], [`Lxma`], [`MapRegions`], [`MapThemeChoice`], [`MapFlingChoice`] or [`ReceiptsChoice`], or [`Done`] with the error |
//! | `nextEvent` | [`Event`], tagged by `t`: `changed`, `notice`, `sync`, `link`, `stopped`; none when the wait ran out |
//! | `renderMapInto` | [`MapShown`]; its `w` times `h` RGBA pixels are in the Bitmap |
//!
//! The map's pixels do not cross as bytes (1155): Rust composes them
//! straight into the Bitmap Java hands over, locked through the NDK's
//! jnigraphics, so twenty megabytes of a phone's frame are neither copied
//! into a Java `byte[]` nor from there into the Bitmap. The host's tests
//! read the frame as [`frame`] lays it out: four bytes big endian, the
//! length of a [`MapShown`], the [`MapShown`], and its pixels. The map
//! call needs no session: it draws the regions in the
//! app's maps directory and the pins the store knows, with or without one.
//! A change that Java cannot read is a new [`VERSION`]: every answer
//! carries it, and Java refuses one it does not know. A field or an event
//! added is no such change: Java reads keys by name and skips an event it
//! does not know, so `link` (event and field) came within version 1, and a
//! code with the address behind it in [`Start::code`] is still a string,
//! and the avatar's `initials`, `icon_fg` and `icon_bg` (1129) are fields,
//! as are the list row's `kind`, `reaction` and `state` and the list's
//! own `address` and `initials` (1130).
//! A request added is none either: an older app never sends `front`, and
//! the session then counts itself in the back, pulling on wakes and its
//! schedule as before. The map's margins and `moving` (1131) are such
//! fields too: an app that sends none gets the frame of the screen. The
//! long press's requests and queries (1137) and a row's `key` and quote
//! came the same way, and so did the address book's (1139): the link
//! requests and queries, a row's `photo` and `linked`, and the header's
//! link fields; and the map's `prefetch` with its answer's `prefetching`
//! and `prefetched` (1135).
//! A message key crossing as a signed integer (1166) is no new version
//! either: Java always held it in a `long`, read a uint64 into one bit for
//! bit and wrote it back as an int64, so the values Java sees and keeps
//! (a pinned message's key) stay what they were; only Rust's side now
//! reads what Java writes. See [`wire_key`].
//! A run of a peer's position reports folding into one row (1184) is no
//! new version either: [`Query::Messages`] answers the run's newest report
//! with [`MessageRow::updates`] and [`MessageRow::since`] and leaves the
//! others out, [`ConversationRow::updates`] counts it in the list, and
//! [`Query::Run`] expands it. An app without them shows the newest report
//! alone, which is what it draws as it is; nothing is merged on disk.
//! The list's `home_link` (1199) and the frame's `theme` came within
//! version 1 too: an older app reads neither.
//! The map's people (1200) came the same way: [`Query::Friends`] and its
//! [`Friends`], the map's `avatars` with its answer's `marks`, and the
//! answer's `offline` and `no_map`. An app that sends no `avatars` gets the
//! pins drawn into the frame as before.
//! Location (1201) came the same way: [`Request::Share`], the map's
//! `at_zoom_milli`, `own_lat_e6` and `own_lon_e6` with the answer's
//! `own_x` and `own_y`, the frame's `autoload`, and [`Query::Shared`]
//! with its [`Shared`].
//! The profile (1187) came the same way: [`Query::Profile`] with its
//! [`Profile`], [`Query::Lxma`] with its [`Lxma`], and
//! [`Request::AddContact`].
//! A position report asking for no attention (1209) is no new version
//! either: [`ConversationRow::unread`] no longer counts reports, so the
//! app's notification, which grows with it, stays quiet for them; and the
//! liveness of a peer's position came as fields, `liveness`, `cadence_s`
//! and `last_fix_at` on [`ConversationRow`], [`FriendRow`] and
//! [`Messages`], `liveness` and `cadence_s` on the newest report's
//! [`MessageRow`]. An older app reads none of them and draws no ring.
//! The settings pages' map questions (1207) are queries of their own:
//! [`Query::MapRegions`] with its [`MapRegions`] and [`Query::MapTheme`]
//! with its [`MapThemeChoice`]. 1199 and 1201 had sent them as a
//! [`MapRequest`] (`ask_theme`, `set_theme`, `ask_regions`,
//! `delete_region`, `set_autoload`) without the frame's `w`, `h` and
//! `density_milli`, which that struct needs, so the request never decoded
//! and both pages showed their empty defaults; those fields are gone. No
//! new version: an app that sent them got nothing but the error, and the
//! frame's own fields are unchanged. The Advanced page's switch of the
//! map's fling (1188) is [`Query::MapFling`] with its [`MapFlingChoice`];
//! the frame says the choice as `fling`, and how far its view may still
//! move north and south before the world's edge as `room_north` and
//! `room_south`, which a coast stops at. An older app reads none of them.
//! The scene's words (1217) came within version 1 too: [`Start::lang`],
//! the list's `search`, a row's `live` and `ring`, and a bubble's
//! `joins_before`, `joins_after`, `day`, `meta`, `newest` and `live`. The
//! app draws them and computes none of them again; an app that sends no
//! `lang` gets English, one that sends no `search` the whole list.
//! The list's map pin (1224) came within version 1 too:
//! [`ConversationRow::pin`] and [`ConversationRow::pin_label`], and
//! [`ConversationRow::ring`] empty in the list from then on. An older app
//! draws no pin and, without the ring, no liveness in the list; the
//! newest report's row still words it.
//! The draft (1219) came within version 1 too: [`Request::Draft`], the
//! list row's [`ConversationRow::draft`] and the chat's [`Messages::draft`]
//! and [`Messages::draft_reply`]. An older app sends no draft, and keeps
//! none.
//! The viewport (1231) came within version 1 too: [`Request::Viewport`].
//! An app that sends none sends no read receipts.
//! Read receipts (1230) came within version 1 too: the delivery word
//! `read` on [`MessageRow::state`] and [`ConversationRow::state`], with
//! [`MessageRow::read_at`], and the peer's flag [`Messages::receipts`]. An
//! app that does not know `read` draws its default mark; the app of this
//! tree draws it as `delivered` until the glyph ladder of
//! `docs/src/concepts/loquax-read-receipts.md` lands.
//! The switch (1233) came within version 1 too: [`Query::Receipts`] with
//! its [`ReceiptsChoice`]. An older app shows no switch, and the phone
//! sends receipts unless its config says `read_receipts = off`.

use serde::{Deserialize, Serialize};

/// The schema's version, in every answer as `v`.
pub const VERSION: u32 = 1;

/// Start the session: on the first run with the code and the address, after
/// that with neither (both are kept in `dir`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Start {
    /// The app's files directory: the loquax home on the phone.
    pub dir: String,
    /// The home node's TCP server, `host:port`; the saved one when absent.
    #[serde(default)]
    pub server: Option<String>,
    /// The admission code from `loquax home admit` (`lqxh1...`, or
    /// `lqxh1...@host:port` with the server behind it, which stands in for
    /// [`Start::server`] when that is absent); needed only while the phone
    /// is no client yet.
    #[serde(default)]
    pub code: Option<String>,
    /// The name the home node lists this phone under.
    #[serde(default)]
    pub name: Option<String>,
    /// The phone's offset from UTC in seconds, for the times the model
    /// writes into a message's history.
    #[serde(default)]
    pub offset: i32,
    /// The phone's language, a locale tag (`de-DE`): the words of the
    /// scene (liveness, the day line) are said in it (1217). English when
    /// absent or not one the catalogue speaks.
    #[serde(default)]
    pub lang: Option<String>,
    /// Make this phone the mailbox (the first start's choice, 1152): the
    /// session starts the home node in the app and joins it itself, with no
    /// code and no server. A phone that chose it once keeps it without
    /// saying it again.
    #[serde(default)]
    pub here: bool,
    /// A backup line (`lqxb1...`) to restore the address from before the
    /// mailbox on this phone first starts ("I already have a Loquax
    /// address"), with its passphrase; only with [`Start::here`].
    #[serde(default)]
    pub restore: Option<String>,
    #[serde(default)]
    pub passphrase: Option<String>,
}

/// What became of [`Start`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Started {
    pub v: u32,
    pub ok: bool,
    /// Why not, in words for the screen.
    pub error: Option<String>,
    /// What the join screen has to ask for when `ok` is false: `code`,
    /// `server`, or nothing when the error is of another kind.
    pub need: Option<String>,
    /// What went wrong in the mailbox's own words, behind the screen's
    /// Details (1263); `error` is the one plain sentence.
    #[serde(default)]
    pub details: Option<String>,
    /// The home node's LXMF address, which is this phone's own.
    pub address: Option<String>,
    /// The server the session talks to.
    pub server: Option<String>,
    /// Whether its TCP interface to the home node is up right now; the
    /// `link` event tells each change after.
    #[serde(default)]
    pub link: bool,
    /// Whether the mailbox is on this phone.
    #[serde(default)]
    pub here: bool,
}

/// What the user did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Request {
    /// Opened a conversation, a new one by typing the address: its unread
    /// are read and marked so, and a pull starts.
    Open { peer: String },
    /// Went back to the list: no conversation is open, so what arrives
    /// stays unread.
    Close,
    /// Sent text in a conversation, through the home node's `/send`.
    Send { peer: String, body: String },
    /// Reacted to a message.
    React {
        peer: String,
        target: String,
        content: String,
    },
    /// Replied to the message `target` with `body`: sent with the target's
    /// id and a quote of it (`loquax::ui::replies`).
    Reply {
        peer: String,
        target: String,
        body: String,
    },
    /// Forwarded the message `key` ([`MessageRow::key`]) of `peer`'s
    /// conversation to `to`: its text and content fields again.
    Forward { peer: String, key: i64, to: String },
    /// Tapped the red mark of the message `key` of `peer`'s conversation,
    /// one of ours not sent: it goes again (1232), the failed one leaves.
    SendAgain { peer: String, key: i64 },
    /// Picked a picture with the paperclip's "Photo" (1220): the system
    /// picker's bytes as they came. The session scales and strips them
    /// by the reader's photo size (`loquax::photo::prepare`) and sends
    /// them to `peer` as the image field; a picture it cannot send
    /// answers the request with the reason.
    Photo { peer: String, data: Blob },
    /// Picked a file with the paperclip's "File" (1221): its display name
    /// and bytes as the picker gave them. The session sends them to `peer`
    /// as the file attachments field; one past the 990 KB a receiver
    /// takes answers the request with both sizes. Java stops reading a
    /// file past 1 MB and only counts the rest: `size` is the whole
    /// file's bytes it counted, 0 for `data`'s length.
    File {
        peer: String,
        name: String,
        data: Blob,
        #[serde(default)]
        size: u64,
    },
    /// Held the microphone and let go (1222): the recording as
    /// `AudioRecord` made it, mono 16-bit PCM, little endian, at `rate`.
    /// The session codes it as Opus in Ogg off the lock
    /// (`loquax::voice::encode`) and sends it to `peer` as the audio
    /// field; one shorter than 0.6 s or longer than 5 min answers the
    /// request with the reason.
    Voice { peer: String, pcm: Blob, rate: u32 },
    /// Tapped a voice message's play button (`on`) or its pause button,
    /// or the player came to its end (1222): the bubble's button follows.
    /// [`Query::Voice`] has the sound.
    #[serde(rename = "playvoice")]
    PlayVoice { peer: String, key: i64, on: bool },
    /// Deleted the messages `keys` of `peer`'s conversation, the user asked
    /// already; on a client of a home node from its every device.
    Delete { peer: String, keys: Vec<i64> },
    /// Picked `emoji` into a message: it joins the recents, as a reaction
    /// sent does.
    Used { emoji: String },
    /// Asked for a pull now: the refresh button.
    Pull,
    /// A screen of the app came to the front (`true`) or left it: while in
    /// front and with the watching link down, the session pulls every
    /// minute.
    Front { front: bool },
    /// These bubbles of `peer`'s chat are on screen, at least half of each,
    /// by their [`MessageRow::key`], on every change (1231): the core
    /// decides from it, and from the front, what was read.
    Viewport { peer: String, keys: Vec<i64> },
    /// The name recipients see, for the mailbox on this phone (the first
    /// start's last screen, 1152).
    Name { name: String },
    /// Linked `peer` to a contact of the address book the user picked or
    /// confirmed (1139), or the link's copy refreshed: what the contact
    /// said right then, its thumbnail's bytes among it.
    Link {
        peer: String,
        #[serde(flatten)]
        contact: ContactCopy,
    },
    /// Took `peer`'s link away; its copied name and photo go with it.
    Unlink { peer: String },
    /// "Not now" on `peer`'s banner: it does not ask again.
    #[serde(rename = "notnow")]
    NotNow { peer: String },
    /// The user's own name for `peer`, the empty one clearing it: kept
    /// here and handed to the home node for the home's other devices.
    Rename { peer: String, name: String },
    /// The address book's IM rows with protocol LXMF (with
    /// `READ_CONTACTS`): each address loquax knows and has not linked
    /// becomes a link, unless two contacts carry it.
    Relink { rows: Vec<ImRow> },
    /// Pinned the conversation to the top of the list, or loosened it
    /// (1146). Pin, mute, archive and block reach the home's other devices.
    Pin { peer: String, on: bool },
    /// Muted the conversation for `secs` from now, for always when
    /// negative, or unmuted it with 0.
    Mute { peer: String, secs: i64 },
    /// Moved the conversation into the archive, or back.
    Archive { peer: String, on: bool },
    /// Blocked the peer, or lifted the block.
    Block { peer: String, on: bool },
    /// Marked the peer's identity verified after comparing it in person,
    /// or took it back; on this device only.
    Verify { peer: String, on: bool },
    /// Marked the conversation unread on this device, or read as opening
    /// it would.
    Unread { peer: String, on: bool },
    /// Deleted the conversation from this device, or from every device of
    /// the home.
    Forget { peer: String, everywhere: bool },
    /// Shared the phone's own position with `peer`, the user asked for it
    /// (1201): from the empty map, a friend's card or the chat. The fix in
    /// the wire's integers; `at` is when the phone took it, unix seconds.
    Share {
        peer: String,
        lat_e6: i32,
        lon_e6: i32,
        #[serde(default)]
        alt_e2: i32,
        /// Metres times a hundred; 0 for unknown.
        #[serde(default)]
        accuracy_e2: u32,
        at: i64,
    },
    /// A contact from its contact code, `lxma://<address>:<key>` (1187,
    /// `loquax::lxma`), or from its address alone (1263, an old code):
    /// scanned or pasted. `name`, when given, becomes the
    /// reader's name for it; the home node is handed the key, so a message
    /// can go out before any announce was heard; the conversation opens.
    #[serde(rename = "addcontact")]
    AddContact {
        lxma: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// What the compose field of `peer`'s chat holds now (1219), on every
    /// change: `body`, and the id of the message a reply being written
    /// answers in `reply`, empty for none. Kept as the conversation's
    /// draft; the empty one clears it.
    Draft {
        peer: String,
        body: String,
        #[serde(default)]
        reply: String,
    },
}

/// What a contact said when loquax read it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactCopy {
    /// Its `LOOKUP_KEY` and `_ID`.
    pub lookup: String,
    #[serde(default)]
    pub contact_id: i64,
    /// Its display name.
    pub name: String,
    /// Its thumbnail's bytes, as the provider gave them; none for none.
    #[serde(default)]
    pub photo: Option<Blob>,
}

/// One IM row of protocol LXMF, and the contact it belongs to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImRow {
    /// The row's data: the address, as the user or loquax wrote it.
    pub address: String,
    #[serde(flatten)]
    pub contact: ContactCopy,
}

/// Bytes that cross as msgpack `bin`, as Java's codec writes a `byte[]`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Blob(pub Vec<u8>);

impl std::fmt::Debug for Blob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Blob({} bytes)", self.0.len())
    }
}

impl Serialize for Blob {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for Blob {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Blob;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("bytes")
            }
            fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<Blob, E> {
                Ok(Blob(bytes.to_vec()))
            }
            fn visit_byte_buf<E: serde::de::Error>(self, bytes: Vec<u8>) -> Result<Blob, E> {
                Ok(Blob(bytes))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Blob, A::Error> {
                let mut out = Vec::new();
                while let Some(byte) = seq.next_element::<u8>()? {
                    out.push(byte);
                }
                Ok(Blob(out))
            }
        }
        deserializer.deserialize_bytes(Visitor)
    }
}

/// What the screen wants to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Query {
    /// The conversation list, in the list's order; with `search` only the
    /// rows it finds, every word of it in the name or the last line
    /// (`loquax::ui::chat::row_matches`, 1217).
    Conversations {
        #[serde(default)]
        search: Option<String>,
    },
    /// One conversation's messages, oldest first.
    Messages { peer: String },
    /// The detail of the message `key` of `peer`'s conversation.
    Info { peer: String, key: i64 },
    /// The emoji the user used last, newest first.
    Recents,
    /// Which of `candidates`, contacts of the address book, `peer`'s
    /// announced name may be, best first (`loquax::name_match`).
    Suggest {
        peer: String,
        candidates: Vec<CandidateRow>,
    },
    /// Every address linked to the contact `lookup`: its detail screen.
    Contact { lookup: String },
    /// Every link, for the refresh that reads the contacts again.
    Links,
    /// What the contact screen shows of `peer` (1146).
    Detail { peer: String },
    /// The archived conversations, as the list's rows; `search` as in
    /// [`Query::Conversations`].
    Archived {
        #[serde(default)]
        search: Option<String>,
    },
    /// The blocked peers, as the list's rows, conversation or not.
    Blocked,
    /// This phone's own address, for the first start's last screen (1152).
    Me,
    /// The address's backup, sealed with `passphrase` (1152).
    Backup { passphrase: String },
    /// Every report the row `key` ([`MessageRow::key`]) of `peer`'s
    /// conversation folds (1184): the long press's "Show all positions".
    Run { peer: String, key: i64 },
    /// Everyone with a position, for the map's Friends sheet and its pins
    /// (1200), with distances from the phone's own fix when given.
    Friends {
        #[serde(default)]
        lat_e6: Option<i32>,
        #[serde(default)]
        lon_e6: Option<i32>,
    },
    /// The positions we shared, newest first (1201): the privacy page's
    /// list of who got one and when.
    Shared,
    /// The profile screen behind our avatar (1187): name, address, the
    /// contact code and its QR, where the mailbox is, and the advanced
    /// facts.
    Profile,
    /// Whether `text` is a code, which kind and whose (1187, 1263,
    /// `loquax::code::read`): what the scanner and the new conversation
    /// screen ask before the name. A text that is none is refused in one
    /// sentence of `lang` (a BCP 47 tag; the session's language when
    /// none). Answered without a session too, `known` then empty: the
    /// first start's scanner asks before anything runs.
    Lxma {
        text: String,
        #[serde(default)]
        lang: Option<String>,
    },
    /// The Maps page (1207): with `delete` that region file deleted
    /// ([`RegionRow::name`]), with `set_autoload` the automatic download
    /// stored, then the regions of the app's maps directory. A page's
    /// question of its own: it carries none of a frame's fields.
    MapRegions {
        #[serde(default)]
        delete: Option<String>,
        #[serde(default)]
        set_autoload: Option<bool>,
    },
    /// The Appearance page's map theme (1207): `set` (`auto`, `light`,
    /// `dark`) stored when given, the choice in force answered.
    MapTheme {
        #[serde(default)]
        set: Option<String>,
    },
    /// The Advanced page's map fling (1188): `set` stored when given, the
    /// choice in force answered.
    MapFling {
        #[serde(default)]
        set: Option<bool>,
    },
    /// The Privacy page's "Send read receipts" (1233): `set` stored when
    /// given, the switch in force answered as [`ReceiptsChoice`].
    Receipts {
        #[serde(default)]
        set: Option<bool>,
    },
    /// The Chats page's photo size (1220): `set` (`small`, `phone`,
    /// `original`) stored when given, the size in force answered as
    /// [`PhotoSizeChoice`].
    PhotoSize {
        #[serde(default)]
        set: Option<String>,
    },
    /// The photo of the message `key` of `peer`'s conversation, decoded
    /// upright and scaled into an `edge` by `edge` box, never up (1220):
    /// a bubble's thumbnail, the full view. Answered as [`PhotoPixels`].
    Photo { peer: String, key: i64, edge: u32 },
    /// The `index`th file of the message `key` of `peer`'s conversation,
    /// saved under the app's files (`files/<peer>-<key>-<index>/<name>`,
    /// the name made safe) for the open or share intent (1221). Answered
    /// as [`SavedFile`].
    SaveFile { peer: String, key: i64, index: u32 },
    /// The voice message of the message `key` of `peer`'s conversation,
    /// decoded for `AudioTrack` (1222): Opus at 24 kHz, Codec2 at 8 kHz.
    /// Answered as [`VoicePcm`].
    Voice { peer: String, key: i64 },
}

/// What [`Query::Voice`] answers: mono 16-bit PCM, little endian, the
/// layout `AudioTrack` takes with `ENCODING_PCM_16BIT`, and its rate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoicePcm {
    pub v: u32,
    pub ok: bool,
    pub rate: u32,
    pub pcm: Blob,
}

/// What [`Query::SaveFile`] answers: where the file is, under the app's
/// files, and the name it was saved under.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedFile {
    pub v: u32,
    pub ok: bool,
    pub path: String,
    pub name: String,
}

/// One file a bubble carries (1221).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRow {
    /// The name the sender wrote, made safe to show and save
    /// (`loquax::ui::files::safe_name`).
    pub name: String,
    pub bytes: u32,
    /// `312 KB`.
    pub size: String,
    /// What a screen reader says: `File report.pdf, 312 KB`.
    pub words: String,
}

/// What [`Query::PhotoSize`] answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhotoSizeChoice {
    pub v: u32,
    pub ok: bool,
    /// `small`, `phone` or `original`.
    pub size: String,
}

/// What [`Query::Photo`] answers: RGBA rows without padding, the layout
/// of `Bitmap.Config.ARGB_8888`'s buffer, which
/// `Bitmap.copyPixelsFromBuffer` takes as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhotoPixels {
    pub v: u32,
    pub ok: bool,
    pub width: u32,
    pub height: u32,
    pub rgba: Blob,
}

/// What [`Query::MapRegions`] answers: the regions, newest first, their
/// bytes together, and whether the automatic download is on (on unless
/// the config's `map_autoload` says off, 1089).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapRegions {
    pub v: u32,
    pub ok: bool,
    pub regions: Vec<RegionRow>,
    pub region_bytes: u64,
    pub autoload: bool,
}

/// What [`Query::MapTheme`] answers: the config's `map_theme` as the
/// page shows it (`auto` when unset), and whether it overrules the phone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapThemeChoice {
    pub v: u32,
    pub ok: bool,
    pub theme: String,
    pub theme_chosen: bool,
}

/// What [`Query::MapFling`] answers: whether the map coasts on after a
/// pan (on unless the config's `map_fling` says off, 1188).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapFlingChoice {
    pub v: u32,
    pub ok: bool,
    pub fling: bool,
}

/// What [`Query::Receipts`] answers: whether this phone sends read
/// receipts and shows them (on unless the config's `read_receipts` says
/// off, 1233).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptsChoice {
    pub v: u32,
    pub ok: bool,
    pub on: bool,
}

/// What [`Query::Shared`] answers: the positions we sent, newest first, at
/// most [`crate::session::SHARED_SHOWN`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shared {
    pub v: u32,
    pub shared: Vec<SharedRow>,
}

/// One position we sent: to whom, as the list draws them, and when.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedRow {
    pub peer: String,
    pub name: String,
    pub initials: String,
    #[serde(default)]
    pub icon_fg: Option<String>,
    #[serde(default)]
    pub icon_bg: Option<String>,
    #[serde(default)]
    pub photo: Option<String>,
    /// When it was sent, unix seconds; the fix time where the store kept
    /// no send time.
    pub at: i64,
    pub lat_e6: i32,
    pub lon_e6: i32,
}

/// What [`Query::Friends`] answers (`crate::friends`): every peer with a
/// position but ourselves and the blocked, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Friends {
    pub v: u32,
    pub friends: Vec<FriendRow>,
    /// Our own position, when the store has one: the map's pin of ours.
    #[serde(default)]
    pub me: Option<FriendRow>,
}

/// One person on the map: the avatar as the list draws it, the fix, how
/// far, and where Navigate goes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FriendRow {
    pub peer: String,
    /// As the list names the peer, with its avatar's letters and colours
    /// ([`ConversationRow`]'s fields of the same names).
    pub name: String,
    pub initials: String,
    #[serde(default)]
    pub icon_fg: Option<String>,
    #[serde(default)]
    pub icon_bg: Option<String>,
    #[serde(default)]
    pub photo: Option<String>,
    /// When the fix was taken, unix seconds: its claim, or when it came
    /// where the claim is ahead of our clock.
    pub at: i64,
    pub lat_e6: i32,
    pub lon_e6: i32,
    /// Metres from the phone's own fix; absent without one.
    #[serde(default)]
    pub distance_m: Option<u32>,
    /// Within the last day: the sheet's upper part; the older ones are
    /// greyed under a divider.
    #[serde(default)]
    pub recent: bool,
    /// Old enough that the map draws it faded.
    #[serde(default)]
    pub stale: bool,
    /// The `geo:` URI Navigate hands to the phone.
    #[serde(default)]
    pub geo: String,
    /// Whether the peer's position is alive (1209): `live`, `recent` or
    /// `stale` (`loquax::ui::liveness`), the avatar's ring; the learned
    /// report cadence in seconds, 0 before three reports taught one; when
    /// the newest report's fix was taken, unix seconds, 0 for none.
    #[serde(default)]
    pub liveness: String,
    #[serde(default)]
    pub cadence_s: u32,
    #[serde(default)]
    pub last_fix_at: i64,
    /// The card's words for a peer that reports, in the phone's language
    /// (1217, `scene::row_live`), empty for one known from a text alone;
    /// the ring's colour as [`ConversationRow::ring`].
    #[serde(default)]
    pub live: String,
    #[serde(default)]
    pub ring: String,
}

/// What [`Query::Run`] answers: the run's reports, newest first; one for a
/// row that folds none, none for a key the conversation lacks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub v: u32,
    pub positions: Vec<RunRow>,
}

/// One report of a run, as its row would have shown it alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRow {
    /// The report's key, as [`MessageRow::key`].
    pub key: i64,
    /// When it came, unix seconds.
    pub at: i64,
    /// Its fix, as [`MessageRow`] has it; absent when it carried none
    /// readable.
    #[serde(default)]
    pub lat_e6: Option<i32>,
    #[serde(default)]
    pub lon_e6: Option<i32>,
    #[serde(default)]
    pub place: Option<String>,
    #[serde(default)]
    pub accuracy: Option<String>,
    #[serde(default)]
    pub fix_at: Option<i64>,
}

/// What [`Query::Backup`] answers: the line the file holds, and the same
/// line as the paper code's modules.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backup {
    pub v: u32,
    pub line: String,
    pub qr_size: u32,
    pub qr: Blob,
}

/// What [`Query::Me`] answers: the address to give people, as text and as
/// a QR code, and the mailbox's facts the screen shows beside it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    pub v: u32,
    pub address: String,
    /// The address in groups of four, as the contact screen shows one.
    pub fingerprint: String,
    /// The contact code, the same as [`Profile::lxma`]: what the first
    /// start's QR holds and its Share sends (1263), one code everywhere.
    #[serde(default)]
    pub lxma: String,
    pub qr_size: u32,
    /// The QR code of [`Me::lxma`] as [`Profile::qr`] has it: `qr_size`
    /// squared modules, one byte each, row by row.
    pub qr: Blob,
    /// The name recipients see; empty when none was given.
    pub name: String,
    /// Whether the mailbox is on this phone.
    pub here: bool,
}

/// What [`Query::Profile`] answers, top to bottom as the screen shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub v: u32,
    /// The name our announce carries; empty when none was given.
    pub name: String,
    /// Whether the name can be changed here: only with the mailbox on this
    /// phone, which announces it.
    pub name_editable: bool,
    /// Our LXMF address, the one to give people, as lowercase hex.
    pub address: String,
    /// The avatar's letters, as the list's own avatar has them.
    pub initials: String,
    /// The contact code Columba and Sideband read,
    /// `lxma://<address>:<public key>` (`loquax::lxma`).
    pub lxma: String,
    pub qr_size: u32,
    /// The QR code of [`Profile::lxma`], byte mode, verbatim:
    /// `qr_size` squared modules, one byte each, row by row.
    pub qr: Blob,
    /// Whether the mailbox is on this phone.
    pub here: bool,
    /// Where it is otherwise: the host of its server.
    pub mailbox: String,
    /// Since when this phone reads it, unix seconds; 0 when unknown.
    pub since: i64,
    /// Advanced: the identity hash behind the address.
    pub identity: String,
    /// Advanced: the home node's own destination, which the phone talks to.
    pub home_node: String,
    /// Advanced: the servers the phone dials one at a time to reach it
    /// (1196), one a line, in order, the one whose connection is up marked
    /// `●` and the others `○`.
    #[serde(default)]
    pub rendezvous: String,
    /// Advanced: the propagation node the mailbox is told to use; empty
    /// when it chooses one itself.
    pub propagation_node: String,
    /// Advanced: the app's version and commit.
    pub version: String,
}

/// What [`Query::Lxma`] answers for a code (`loquax::code::read`, 1263):
/// what it is, whose, and the code as it parsed, for
/// [`Request::AddContact`] or the join.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lxma {
    pub v: u32,
    /// `contact` for a contact code, `address` for a friend's address
    /// alone, `mailbox` for a mailbox's join code.
    #[serde(default)]
    pub kind: String,
    /// The friend's address as lowercase hex; empty for a mailbox's code.
    pub address: String,
    /// What [`Request::AddContact`] or the join takes: the contact code
    /// as Columba writes it, the address, or the mailbox's code as read.
    pub lxma: String,
    /// The name every screen gives that address today: a petname or an
    /// announced name when one is known, else empty.
    pub known: String,
}

/// One contact's name fields, for [`Query::Suggest`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateRow {
    /// The app's handle on it, handed back: the lookup key.
    pub key: String,
    #[serde(default)]
    pub display: String,
    #[serde(default)]
    pub given: String,
    #[serde(default)]
    pub middle: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub nicknames: Vec<String>,
    #[serde(default)]
    pub last_contacted: i64,
}

/// The suggestions, best first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestions {
    pub v: u32,
    pub suggestions: Vec<SuggestionRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestionRow {
    pub key: String,
    pub score: u32,
    /// What matched: `display`, `nickname`, `name`, `given`, `words` or
    /// `prefix`; and the nickname when it was one.
    pub field: String,
    #[serde(default)]
    pub nickname: Option<String>,
}

/// The addresses of one contact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Addresses {
    pub v: u32,
    pub addresses: Vec<AddressRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressRow {
    pub peer: String,
    /// As every screen names it.
    pub name: String,
    /// The announced name, when one was heard.
    #[serde(default)]
    pub announced: Option<String>,
}

/// Every link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Links {
    pub v: u32,
    pub links: Vec<LinkRow>,
}

/// The contact screen (1146), after Signal's: who the peer is, the
/// reader's word on it, its identity to compare, what this device knows of
/// reaching it, and its positions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detail {
    pub v: u32,
    pub peer: String,
    pub name: String,
    pub initials: String,
    #[serde(default)]
    pub icon_fg: Option<String>,
    #[serde(default)]
    pub icon_bg: Option<String>,
    #[serde(default)]
    pub photo: Option<String>,
    pub linked: bool,
    #[serde(default)]
    pub lookup: Option<String>,
    #[serde(default)]
    pub contact_id: Option<i64>,
    #[serde(default)]
    pub petname: Option<String>,
    #[serde(default)]
    pub announced: Option<String>,
    /// The address in groups of four hex digits, to read out and compare.
    pub fingerprint: String,
    /// The address as a QR code: the side in modules and one byte a
    /// module, row by row, 1 for dark, without the quiet zone.
    pub qr_size: u32,
    pub qr: Blob,
    pub verified: bool,
    pub blocked: bool,
    pub pinned: bool,
    pub archived: bool,
    /// Whether it is muted now, and until when: unix seconds, -1 for
    /// always, 0 for not.
    pub muted: bool,
    pub muted_until: i64,
    /// When the newest message from the peer came, unix seconds, 0 for
    /// none; when the newest to it was written, and how it went.
    pub last_received: i64,
    pub last_sent: i64,
    #[serde(default)]
    pub last_sent_how: String,
    /// Whether this device is a client of a home node, which routes for
    /// it: then no path of its own is known here.
    pub home_client: bool,
    /// The peer's newest positions, newest first.
    pub positions: Vec<PositionRow>,
}

/// One position of a peer, for the contact screen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionRow {
    /// The fix time the sender claims, unix seconds.
    pub at: i64,
    pub lat_e6: i32,
    pub lon_e6: i32,
    /// Its day in the phone's language, as the chat's day line says it
    /// (1217, `scene::day_words`): `Today`, `Saturday, 24 October`.
    #[serde(default)]
    pub day: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRow {
    pub peer: String,
    pub lookup: String,
    pub contact_id: i64,
    pub name: String,
}

/// A message's detail, what the terminal's `v` shows: who, the peer, the
/// id, the size, the delivery state or the signature, the fields read and
/// what this device saw happen to it, one line each, an empty one between
/// the parts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    pub v: u32,
    pub rows: Vec<String>,
}

/// The recent emoji, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recents {
    pub v: u32,
    pub emoji: Vec<String>,
}

/// The answer to a call that has nothing else to say.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Done {
    pub v: u32,
    pub ok: bool,
    pub error: Option<String>,
}

impl Done {
    pub fn ok() -> Self {
        Self {
            v: VERSION,
            ok: true,
            error: None,
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            v: VERSION,
            ok: false,
            error: Some(error.into()),
        }
    }
}

/// One row of the conversation list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationRow {
    pub peer: String,
    /// The peer as every loquax screen names it: petname or announced name
    /// with the short hash, or the address.
    pub name: String,
    /// The last message's first line, a reaction as `reacted 👍`.
    pub last: String,
    /// Whether that message is ours.
    pub ours: bool,
    /// When it was written or came, seconds since 1970 UTC; 0 for none.
    pub at: i64,
    /// Unread rows: a position report counts nothing (1209), so it raises
    /// no count and no notification; the raw count the read marks name is
    /// not here.
    pub unread: u32,
    /// The avatar's letters (1129): from the name, or the address's first
    /// two hex digits for a peer without one.
    #[serde(default)]
    pub initials: String,
    /// The peer's own avatar colours, `rrggbb`, from the icon appearance of
    /// its latest message that carried one; absent, the app picks a colour
    /// from the address.
    #[serde(default)]
    pub icon_fg: Option<String>,
    #[serde(default)]
    pub icon_bg: Option<String>,
    /// What the last message is (1130): `text`, `position` for a report
    /// without text, `reaction`; the app words the last two in the phone's
    /// language, `last` stays the English line for an app that does not.
    #[serde(default)]
    pub kind: String,
    /// The reaction's content when `kind` is `reaction`.
    #[serde(default)]
    pub reaction: Option<String>,
    /// Ours only: the last message's delivery, as [`MessageRow::state`];
    /// the list draws its mark where an unread count would be.
    #[serde(default)]
    pub state: String,
    /// When `state` is `read`: the reader's time, unix seconds, else 0
    /// (1232), as [`MessageRow::read_at`].
    #[serde(default)]
    pub read_at: i64,
    /// The linked contact's thumbnail as loquax copied it, a file the app
    /// decodes (1139); absent, the avatar is the initials.
    #[serde(default)]
    pub photo: Option<String>,
    /// Whether the peer is linked to a contact of the address book.
    #[serde(default)]
    pub linked: bool,
    /// Pinned to the top, and muted now (1146): the list's marks, and no
    /// notification for a muted one.
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub muted: bool,
    /// How many reports the last row folds when it is a run of them, else
    /// 0 (1184). The list says whether the position is alive instead
    /// (1209), `📍 live` or `📍 3 h ago`, from the fields below.
    #[serde(default)]
    pub updates: u32,
    /// Whether the peer's position is alive (1209): `live`, `recent` or
    /// `stale` (`loquax::ui::liveness`), the avatar's ring; the learned
    /// report cadence in seconds, 0 before three reports taught one; when
    /// the newest report's fix was taken, unix seconds, 0 for none.
    #[serde(default)]
    pub liveness: String,
    #[serde(default)]
    pub cadence_s: u32,
    #[serde(default)]
    pub last_fix_at: i64,
    /// The last line in the phone's language when the last row is a
    /// report (1217, `scene::list_live`): `live`, `3 h ago`; empty for
    /// any other row.
    #[serde(default)]
    pub live: String,
    /// The avatar's ring (1217, `scene::Ring`): `accent`, `secondary`, or
    /// empty for none. Always empty in the list since 1224: the row's map
    /// pin says the position there.
    #[serde(default)]
    pub ring: String,
    /// The row's map pin (1224, `scene::PositionBadge`): its tint,
    /// `accent` for a live position, `secondary` for a recent one, empty
    /// for none (no position younger than a day); a tap on it opens the
    /// map on the peer. Its content description in the phone's language,
    /// `Position of Anna, 2 min ago`.
    #[serde(default)]
    pub pin: String,
    #[serde(default)]
    pub pin_label: String,
    /// What was typed in the conversation and not sent (1219), in the
    /// phone's language, `Draft: …`: shown in italics instead of
    /// [`ConversationRow::last`]; empty for none, and always for the open
    /// conversation.
    #[serde(default)]
    pub draft: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversations {
    pub v: u32,
    pub conversations: Vec<ConversationRow>,
    /// Our own address, the home node's, and its avatar's letters, for
    /// the list's bar (1130).
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub initials: String,
    /// How many conversations are archived: the list's last row (1146).
    #[serde(default)]
    pub archived: u32,
    /// Whether a delete may reach every device of the home.
    #[serde(default)]
    pub home_client: bool,
    /// Whether the watching link to the mailbox is up (1123): the status
    /// dot on the bar's avatar, green or grey (1199).
    #[serde(default)]
    pub home_link: bool,
}

/// A reaction under a message: its content, how many sent it, and whether
/// one of them is ours.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chip {
    pub content: String,
    pub count: u32,
    pub ours: bool,
}

/// One bubble.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageRow {
    /// The LXMF id; empty for a send the home node has not taken yet.
    pub id: String,
    pub ours: bool,
    pub body: String,
    /// Ours only: `queued`, `sending`, `sent`, `delivered`, `read` or
    /// `failed`.
    pub state: String,
    /// Ours only, when `state` is `read` (1230): when the peer read it, by
    /// the reader's clock kept within when it was sent and when the receipt
    /// came, unix seconds; 0 otherwise.
    #[serde(default)]
    pub read_at: i64,
    pub at: i64,
    pub chips: Vec<Chip>,
    /// The fix the message carries, a report's or a text's that came with
    /// telemetry, degrees times a million, which a tap opens the map on
    /// with the sender's pin selected; absent for any other message.
    #[serde(default)]
    pub lat_e6: Option<i32>,
    #[serde(default)]
    pub lon_e6: Option<i32>,
    /// The bubble's position line beside the fix: the coordinates short,
    /// `52.51627 N 13.37770 E`, and the accuracy, `±5 m`, when the sender
    /// gave one.
    #[serde(default)]
    pub place: Option<String>,
    #[serde(default)]
    pub accuracy: Option<String>,
    /// When the fix was taken, unix seconds (the receive time where the
    /// sender's clock claims the future), for the line's age, and from
    /// when the line is drawn dimmed as stale.
    #[serde(default)]
    pub fix_at: Option<i64>,
    #[serde(default)]
    pub stale_at: Option<i64>,
    /// The message's key on this device, for the long press's forward,
    /// delete and info; a send not yet queued has one too. As
    /// [`wire_key`] lays it out.
    #[serde(default)]
    pub key: i64,
    /// A reply's quote: the target's text, or what the sender quoted when
    /// this device lacks it; whether the target is ours, absent when
    /// unknown; and the target's id.
    #[serde(default)]
    pub quote: Option<String>,
    #[serde(default)]
    pub quote_ours: Option<bool>,
    #[serde(default)]
    pub quote_to: Option<String>,
    /// A run of the peer's position reports with nothing between them is
    /// one row, the newest (1184): how many reports it stands for, 0 for a
    /// row that folds none, and when the run's first came, unix seconds.
    /// [`Query::Run`] lists them. A new report of the run replaces this row
    /// under its own key.
    #[serde(default)]
    pub updates: u32,
    #[serde(default)]
    pub since: i64,
    /// The newest report's row only (1209): the peer's liveness and the
    /// learned cadence, as in [`ConversationRow`], which the row says
    /// instead of a count, `live · every 10 min · 2 min ago` or `last
    /// position 3 h ago` with [`MessageRow::fix_at`]'s age; empty and 0 on
    /// every other row.
    #[serde(default)]
    pub liveness: String,
    #[serde(default)]
    pub cadence_s: u32,
    /// Where the bubble sits (1217, `chat::Placement`): it continues the
    /// group above, the next continues its group, it shows its time and
    /// mark; `day` is the day line above it in the phone's language, empty
    /// when no day begins with it; `newest` whether it is in the newest
    /// group, which says its time as an age.
    #[serde(default)]
    pub joins_before: bool,
    #[serde(default)]
    pub joins_after: bool,
    #[serde(default)]
    pub meta: bool,
    #[serde(default)]
    pub newest: bool,
    #[serde(default)]
    pub day: String,
    /// The newest report's words in the phone's language (1217,
    /// `scene::row_live`): `live · every 10 min · 2 min ago`, `last
    /// position 3 h ago`; empty on every other row.
    #[serde(default)]
    pub live: String,
    /// The photo it carries (1220), else empty and 0: the format word
    /// the sender wrote, its bytes, the size it shows at upright (0 by 0
    /// for one that does not decode), and what a screen reader says,
    /// `Photo, 279 KB`. [`Query::Photo`] has its pixels.
    #[serde(default)]
    pub photo: String,
    #[serde(default)]
    pub photo_bytes: u32,
    #[serde(default)]
    pub photo_w: u32,
    #[serde(default)]
    pub photo_h: u32,
    #[serde(default)]
    pub photo_words: String,
    /// The files it carries (1221), in the sender's order; a tap on one
    /// asks [`Query::SaveFile`] with its place.
    #[serde(default)]
    pub files: Vec<FileRow>,
    /// The voice message it carries (1222), else 0 and empty: the
    /// sender's audio mode, its length in ms and as `0:07`, the level bar
    /// (32 heights 0 to 100, empty for an even bar), whether this build
    /// plays it and whether it plays now, and what a screen reader says,
    /// `Voice message, 0:07`. [`Query::Voice`] has the sound.
    #[serde(default)]
    pub voice_mode: u32,
    #[serde(default)]
    pub voice_ms: u32,
    #[serde(default)]
    pub voice_duration: String,
    #[serde(default)]
    pub voice_levels: Blob,
    #[serde(default)]
    pub voice_playable: bool,
    #[serde(default)]
    pub voice_playing: bool,
    #[serde(default)]
    pub voice_words: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Messages {
    pub v: u32,
    pub peer: String,
    pub name: String,
    pub messages: Vec<MessageRow>,
    /// The header's avatar, as in [`ConversationRow`].
    #[serde(default)]
    pub initials: String,
    #[serde(default)]
    pub icon_fg: Option<String>,
    #[serde(default)]
    pub icon_bg: Option<String>,
    /// Whether a delete reaches the home's other devices (1137): the
    /// question says where it deletes.
    #[serde(default)]
    pub home_client: bool,
    /// The address book (1139): the photo and whether it is linked, as in
    /// [`ConversationRow`]; the contact's lookup key and `_ID` for "Open
    /// contact"; whether the banner asks to add the peer (unlinked, not
    /// answered "Not now", a message came from it); the announced name for
    /// a new contact; the user's own name for the rename dialog; the
    /// contact's other addresses as every screen names them.
    #[serde(default)]
    pub photo: Option<String>,
    #[serde(default)]
    pub linked: bool,
    #[serde(default)]
    pub lookup: Option<String>,
    #[serde(default)]
    pub contact_id: Option<i64>,
    #[serde(default)]
    pub ask_link: bool,
    #[serde(default)]
    pub announced: Option<String>,
    #[serde(default)]
    pub petname: Option<String>,
    #[serde(default)]
    pub others: Vec<String>,
    /// The reader verified the peer's identity in person (1146): the
    /// header's mark.
    #[serde(default)]
    pub verified: bool,
    /// The peer's latest message carried the read-receipt mark (1230): it
    /// sends read receipts and is sent them. Without it, ours stay
    /// `delivered`, and the detail view says the peer sends none.
    #[serde(default)]
    pub receipts: bool,
    /// The bar avatar's ring (1209), as in [`ConversationRow`].
    #[serde(default)]
    pub liveness: String,
    #[serde(default)]
    pub cadence_s: u32,
    #[serde(default)]
    pub last_fix_at: i64,
    /// The ring's colour, as [`ConversationRow::ring`].
    #[serde(default)]
    pub ring: String,
    /// What the compose field holds when the chat opens (1219, the scene's
    /// compose node): the conversation's draft, which the field reports
    /// back with [`Request::Draft`] on every change.
    #[serde(default)]
    pub draft: String,
    /// The message the draft's reply answers, its id in hex; empty for
    /// none.
    #[serde(default)]
    pub draft_reply: String,
}

/// What happened, for the screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Event {
    /// Something the screen shows changed: ask again.
    Changed { v: u32 },
    /// A line for the user: what the terminal shows as a toast.
    Notice { v: u32, text: String },
    /// A pull began (`running`), ended (`done`, with how many new
    /// messages came), or failed (`failed`, with why). A pull the user did
    /// not ask for says it runs only once it is slow, and ends only when
    /// it said so or brought a message (`crate::pull_line`).
    Sync {
        v: u32,
        state: String,
        detail: String,
    },
    /// The TCP interface to the home node came up or went down. Once
    /// soon after the start, then on each change.
    Link { v: u32, up: bool },
    /// The session ended; `detail` says why when it was not asked to. One
    /// that ended by itself is gone from the library then: the next
    /// `start` makes a fresh one.
    Stopped { v: u32, detail: String },
}

/// One frame of the map screen, and what the user did to the last one
/// (1128). Java keeps only the view it got back ([`MapShown`]'s centre,
/// zoom and selection) and hands it in again with the gestures since; the
/// rules of the view are loquax's (`loquax::ui::map`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapRequest {
    /// The app's files directory: the maps under `maps/`, the store and
    /// the config beside them.
    pub dir: String,
    /// The view's size in the screen's pixels.
    pub w: u32,
    pub h: u32,
    /// The screen's density times a thousand: 2750 on a phone of 2.75.
    pub density_milli: u32,
    /// Whether the phone is in its dark setting.
    #[serde(default)]
    pub dark: bool,
    /// The last frame's centre in world units and its zoom; absent on the
    /// first frame, which fits the pins.
    #[serde(default)]
    pub x: Option<u32>,
    #[serde(default)]
    pub y: Option<u32>,
    #[serde(default)]
    pub zoom: Option<u8>,
    /// The last frame's exact zoom in thousandths of a step (1134): where
    /// a pinch left it. Wins over `zoom` when given.
    #[serde(default)]
    pub zoom_milli: Option<u32>,
    /// The selected pin's peer, hex.
    #[serde(default)]
    pub selected: Option<String>,
    /// Moves the view by this many pixels, after the centre above: what a
    /// finger dragged the content by is the negative of it.
    #[serde(default)]
    pub pan_x: i32,
    #[serde(default)]
    pub pan_y: i32,
    /// Zooms by this many steps about the pixel `(about_x, about_y)` of the
    /// view, after the pan.
    #[serde(default)]
    pub zoom_by: i32,
    /// Zooms by this many thousandths of a step about the same pixel,
    /// after `zoom_by`: a pinch's live scale, `1000 * log2(scale)`, which
    /// the view keeps exactly (1134).
    #[serde(default)]
    pub zoom_by_milli: i32,
    #[serde(default)]
    pub about_x: i32,
    #[serde(default)]
    pub about_y: i32,
    /// A tap at this pixel of the last frame: the pin there is selected
    /// and named.
    #[serde(default)]
    pub tap_x: Option<i32>,
    #[serde(default)]
    pub tap_y: Option<i32>,
    /// Fit every pin, as `0` does in the terminal.
    #[serde(default)]
    pub fit: bool,
    /// Centre on this place, degrees times a million, as a report's fix
    /// opens the map in the terminal (1076); its region is loaded when no
    /// region holds it.
    #[serde(default)]
    pub at_lat_e6: Option<i32>,
    #[serde(default)]
    pub at_lon_e6: Option<i32>,
    /// Load the region of the view: "load this area", `d` in the terminal.
    #[serde(default)]
    pub load: bool,
    /// Pixels drawn past the view on each side, left and right, top and
    /// bottom (1131): a finger that pans first reveals drawn map, and a
    /// pinch that zooms out shrinks a bitmap that still fills the screen.
    /// The view's rules (fit, the one-world floor, a tap) are the `w` x
    /// `h` screen's; the frame is that much larger about the same centre.
    #[serde(default)]
    pub margin_x: u32,
    #[serde(default)]
    pub margin_y: u32,
    /// A finger is on the map: the frame shows the names its last frame at
    /// rest placed, where the view moved them, and places none anew; it
    /// keeps the zoom step it was composed from within a step of the
    /// pinch, scaled, and draws only a few missing tiles (1134).
    #[serde(default)]
    pub moving: bool,
    /// Switch the map, and only the map, to the other table than the one
    /// it shows (1133): the choice is stored as the config's `map_theme`
    /// and wins over the phone's setting from then on.
    #[serde(default)]
    pub toggle_theme: bool,
    /// Drop that choice: the map follows the phone again, `map_theme =
    /// auto`.
    #[serde(default)]
    pub follow_phone: bool,
    /// Draw a batch of the tiles around the last frame at rest instead of
    /// a frame (1135): the answer has no pixels. Java asks while the view
    /// is idle, the app in front and the battery saver off.
    #[serde(default)]
    pub prefetch: bool,
    /// The app draws the pins itself, as avatar circles (1200): the frame
    /// draws none and says in its `marks` where they and their clusters
    /// lie.
    #[serde(default)]
    pub avatars: bool,
    /// With `at_lat_e6` and `at_lon_e6`: centre there at this zoom, in
    /// thousandths of a step, held to the regions' zooms, instead of the
    /// zoom a fit picks (1201): the crosshair's z16, and follow mode
    /// keeping the zoom the user chose.
    #[serde(default)]
    pub at_zoom_milli: Option<u32>,
    /// The phone's own fix, degrees times a million (1201): the answer
    /// says where it lies in the view, `own_x` and `own_y`, for the app's
    /// dot. Reading it sends nothing.
    #[serde(default)]
    pub own_lat_e6: Option<i32>,
    #[serde(default)]
    pub own_lon_e6: Option<i32>,
}

/// One region of the maps directory, for the Maps page (1201, 1207).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionRow {
    /// The file's name, which [`Query::MapRegions`]' `delete` hands back.
    pub name: String,
    /// Its bytes on the phone.
    pub bytes: u64,
    /// When it was written or last viewed, unix seconds; 0 for unknown.
    pub modified: i64,
    /// The region's middle, degrees times a million: what the page names
    /// it by, as the files carry no place name.
    pub lat_e6: i32,
    pub lon_e6: i32,
    pub min_zoom: u8,
    pub max_zoom: u8,
    /// The autoload wrote it, so it may evict it under the cap; one the
    /// user put there stays.
    #[serde(default)]
    pub auto: bool,
}

/// One avatar pin or cluster of them in a frame asked with `avatars`
/// (1200): where it sits in the view's pixels (the frame's less its
/// margins, so a margin's pin is negative or past the size), who it is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapMark {
    pub x: i32,
    pub y: i32,
    /// The pins' peers, hex: one for a pin, the first the one the cluster
    /// sits on.
    pub peers: Vec<String>,
    /// The cluster's tap: the zoom in thousandths at which its pins lie a
    /// circle apart; absent for a cluster no zoom of the regions parts,
    /// whose tap opens the Friends sheet filtered to it. A pin's own zoom
    /// for a pin.
    #[serde(default)]
    pub split_milli: Option<u32>,
}

/// The map frame's description; its pixels follow it (see the table
/// above).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapShown {
    pub v: u32,
    pub ok: bool,
    pub error: Option<String>,
    /// The frame's size: the view's and the margins on both sides.
    pub w: u32,
    pub h: u32,
    /// The margins drawn past the view on each side, the request's: the
    /// view's top left corner is this pixel of the frame.
    #[serde(default)]
    pub margin_x: u32,
    #[serde(default)]
    pub margin_y: u32,
    /// The view drawn, for the next request.
    pub x: u32,
    pub y: u32,
    /// The zoom step nearest the view's exact zoom.
    pub zoom: u8,
    /// The view's exact zoom in thousandths of a step (1134). While a
    /// pinch moves, the frame is drawn at its tile step one to one and
    /// this is that step's: Java scales it to the fingers' zoom (1155).
    #[serde(default)]
    pub zoom_milli: u32,
    /// The zoom step whose tiles the frame is composed from.
    #[serde(default)]
    pub tile_zoom: u8,
    /// The frame still owes tiles it showed another step's tiles for, or
    /// a name in it fades in or out (1205): ask again.
    #[serde(default)]
    pub pending: bool,
    /// The shallowest and the deepest zoom the regions allow.
    pub min_zoom: u8,
    pub max_zoom: u8,
    pub selected: Option<String>,
    /// The credit the frame carries.
    pub attribution: String,
    /// The pins drawn, and the one a tap found, with its name.
    pub pins: u32,
    pub tapped: Option<String>,
    pub tapped_name: Option<String>,
    /// A region is loading: ask for the frame again in a while.
    pub loading: bool,
    /// A line for the user: a load that failed, a view loaded already.
    pub notice: Option<String>,
    /// How long the frame took to draw, milliseconds.
    pub ms: u32,
    /// The frame is in the map's dark table, and whether a stored choice
    /// put it there rather than the phone's setting.
    #[serde(default)]
    pub dark: bool,
    #[serde(default)]
    pub theme_chosen: bool,
    /// The config's `map_theme` as the settings page shows it: `auto`,
    /// `light` or `dark`; `auto` when unset.
    #[serde(default)]
    pub theme: String,
    /// Tiles around the view are still to prefetch (1135): a frame at
    /// rest that owes none says so, a prefetch answer until its plan is
    /// done.
    #[serde(default)]
    pub prefetching: bool,
    /// Tiles a prefetch answer drew.
    #[serde(default)]
    pub prefetched: u32,
    /// The pins and clusters to draw, with `avatars` (1200), in the order
    /// to draw them.
    #[serde(default)]
    pub marks: Vec<MapMark>,
    /// The last load found no internet, and none landed since: the map's
    /// "Offline" chip.
    #[serde(default)]
    pub offline: bool,
    /// No region holds the view at its zoom and none loads for it: the
    /// map's "No map here yet" chip.
    #[serde(default)]
    pub no_map: bool,
    /// Where the request's own fix lies, in view pixels (1201); absent
    /// without one.
    #[serde(default)]
    pub own_x: Option<i32>,
    #[serde(default)]
    pub own_y: Option<i32>,
    /// Whether the automatic download is on.
    #[serde(default)]
    pub autoload: bool,
    /// Whether a pan coasts on after the finger lifts (1188): the config's
    /// `map_fling`, on when unset.
    #[serde(default)]
    pub fling: bool,
    /// How many view pixels the view's centre may still move north, and
    /// south, before the view is held at the world's edge (1131, 1188):
    /// what a coast may travel. x has no edge, the world goes round.
    #[serde(default)]
    pub room_north: u32,
    #[serde(default)]
    pub room_south: u32,
}

/// A map frame as the host's tests and tools read it: the description's
/// length, the description, the pixels. The app gets its pixels in its
/// Bitmap instead (1155).
pub fn frame(shown: &MapShown, rgba: &[u8]) -> Vec<u8> {
    let head = encode(shown);
    let mut out = Vec::with_capacity(4 + head.len() + rgba.len());
    out.extend_from_slice(&u32::try_from(head.len()).unwrap_or(0).to_be_bytes());
    out.extend_from_slice(&head);
    out.extend_from_slice(rgba);
    out
}

/// Split a frame into its description and its pixels.
pub fn unframe(frame: &[u8]) -> Result<(MapShown, &[u8]), String> {
    let (len, rest) = frame
        .split_first_chunk::<4>()
        .ok_or_else(|| "a frame without its length".to_string())?;
    let len = u32::from_be_bytes(*len) as usize;
    if rest.len() < len {
        return Err("a frame cut short".to_string());
    }
    let (head, pixels) = rest.split_at(len);
    Ok((decode(head)?, pixels))
}

/// A message's key as it crosses: the model's unsigned key
/// ([`loquax::store::key_of`]) with its bits read as Java's `long`. Since
/// the import (1159) a key is a store row's seq with its sign bit flipped,
/// so every row at seq 0 and above, an imported block's newest and all of
/// this device's own, has a key above `i64::MAX`. Java's codec reads such
/// a uint64 into a negative `long` and writes it back as an int64, which an
/// unsigned field refused: "invalid value: integer
/// `-9223372036854775803`, expected u64" (1166).
pub fn wire_key(key: u64) -> i64 {
    key as i64
}

/// The model's key for a key Java hands back; [`wire_key`] undone.
pub fn model_key(wire: i64) -> u64 {
    wire as u64
}

/// Encode an answer.
pub fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    // A map of strings, numbers and vectors always encodes; an empty
    // answer is what Java takes for a broken library.
    rmp_serde::to_vec_named(value).unwrap_or_default()
}

/// Decode a request, the error in words.
pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, String> {
    rmp_serde::from_slice(bytes).map_err(|error| format!("the request did not decode: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_a_map_tagged_by_t() {
        let request = Request::Send {
            peer: "ab".repeat(16),
            body: "hallo".to_string(),
        };
        let bytes = encode(&request);
        // fixmap of three, then "t" => "send".
        assert_eq!(bytes[0], 0x83);
        assert_eq!(&bytes[1..3], b"\xa1t");
        assert_eq!(&bytes[3..8], b"\xa4send");
        assert_eq!(decode::<Request>(&bytes), Ok(request));
        assert_eq!(
            decode::<Request>(&encode(&Request::Pull)),
            Ok(Request::Pull)
        );
        assert_eq!(
            decode::<Request>(&encode(&Request::Front { front: true })),
            Ok(Request::Front { front: true })
        );
    }

    /// A link carries the thumbnail as `bin`, Java's `byte[]`, and decodes
    /// from the flat map Java sends.
    #[test]
    fn a_link_carries_its_photo_as_bin() {
        let link = Request::Link {
            peer: "ab".repeat(16),
            contact: ContactCopy {
                lookup: "0r1-2C".to_string(),
                contact_id: 7,
                name: "Alice".to_string(),
                photo: Some(Blob(vec![0x89, b'P', b'N', b'G'])),
            },
        };
        let bytes = encode(&link);
        assert!(
            bytes
                .windows(6)
                .any(|w| w == [0xc4, 4, 0x89, b'P', b'N', b'G']),
            "{bytes:02x?}"
        );
        assert_eq!(decode::<Request>(&bytes), Ok(link));
        let bare = encode(&Request::NotNow {
            peer: "ab".repeat(16),
        });
        assert_eq!(&bare[3..10], b"\xa6notnow");
    }

    #[test]
    fn a_start_without_code_or_server_decodes() {
        #[derive(Serialize)]
        struct Bare {
            dir: String,
        }
        let bytes = encode(&Bare {
            dir: "/data".to_string(),
        });
        let start: Start = decode(&bytes).expect("a bare start");
        assert_eq!(start.dir, "/data");
        assert_eq!((start.server, start.code, start.offset), (None, None, 0));
    }

    #[test]
    fn a_map_frame_is_its_length_its_description_and_its_pixels() {
        let shown = MapShown {
            v: VERSION,
            ok: true,
            w: 2,
            h: 1,
            attribution: "© OpenStreetMap contributors".to_string(),
            ..MapShown::default()
        };
        let pixels = [1u8, 2, 3, 255, 4, 5, 6, 255];
        let bytes = frame(&shown, &pixels);
        let head = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        assert_eq!(bytes.len(), 4 + head + pixels.len());
        assert_eq!(&bytes[4 + head..], &pixels);
        assert_eq!(unframe(&bytes), Ok((shown, &pixels[..])));
        assert!(unframe(&bytes[..3]).is_err());
        assert!(unframe(&bytes[..4 + head - 1]).is_err());
    }

    /// 1166: a key of either side of seq 0 crosses both ways, written as
    /// Java writes a `long` (`d3`, eight bytes) and as rmp writes it.
    #[test]
    fn a_message_key_round_trips_whatever_its_sign() {
        for seq in [i64::MIN + 1, -1, 0, 1, 5, i64::MAX] {
            let key = loquax::store::key_of(seq);
            let wire = wire_key(key);
            assert_eq!(model_key(wire), key);
            let info = Query::Info {
                peer: "ab".repeat(16),
                key: wire,
            };
            assert_eq!(decode::<Query>(&encode(&info)), Ok(info));
            let mut java = b"\x83\xa1t\xa4info\xa4peer\xd9\x20".to_vec();
            java.extend_from_slice("ab".repeat(16).as_bytes());
            java.extend_from_slice(b"\xa3key\xd3");
            java.extend_from_slice(&wire.to_be_bytes());
            assert_eq!(
                decode::<Query>(&java),
                Ok(Query::Info {
                    peer: "ab".repeat(16),
                    key: wire,
                })
            );
            let delete = Request::Delete {
                peer: "ab".repeat(16),
                keys: vec![wire, wire_key(loquax::store::key_of(seq.wrapping_add(1)))],
            };
            assert_eq!(decode::<Request>(&encode(&delete)), Ok(delete));
        }
        assert_eq!(wire_key(loquax::store::key_of(i64::MIN + 1)), 1);
        assert_eq!(wire_key(loquax::store::key_of(5)), i64::MIN + 5);
    }

    #[test]
    fn garbage_is_an_error_in_words() {
        let error = decode::<Request>(b"\xc1").expect_err("not msgpack");
        assert!(error.starts_with("the request did not decode"), "{error}");
        let error = decode::<Request>(&encode(&Query::Conversations { search: None }))
            .expect_err("a query");
        assert!(error.contains("conversations"), "{error}");
    }
}

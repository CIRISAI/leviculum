//! A client's side of one hub link, without I/O: what to send for a
//! HELLO, a join, a part and a line, and what the hub's envelopes mean.
//! The caller owns the link, the clock and the randomness; every envelope
//! made here takes its id and time from the call, so a test replays a
//! session exactly.
//!
//! The order on a link, from the specification and the vectors: the
//! client identifies the link, sends HELLO, the hub answers WELCOME with
//! its limits; JOIN is answered by JOINED without a nick to the joiner and
//! with the joiner's nick to the others, PART by PARTED alike; a MSG,
//! NOTICE or ACTION goes to every member of the room, the sender too, as
//! the very envelope the sender made (`msg_up` and `msg_down` are the same
//! bytes); a PING is answered by a PONG carrying its body unchanged.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::cbor::Value;
use crate::envelope::{hello_body, welcome_hub, Envelope, EnvelopeError, Kind, Limits};

/// How many of our own envelope ids are remembered to know the hub's echo.
const SENT_KEPT: usize = 64;

/// The three kinds of line a room carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Msg,
    Notice,
    Action,
}

impl LineKind {
    fn kind(self) -> Kind {
        match self {
            LineKind::Msg => Kind::Msg,
            LineKind::Notice => Kind::Notice,
            LineKind::Action => Kind::Action,
        }
    }
}

/// Why something was not sent. Each says in a plain sentence what to do
/// (`Display`); a frontend may put its own words on the variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The hub has not welcomed us yet.
    NotReady,
    /// A line for a room we are not in.
    NotInRoom(String),
    /// An empty line.
    Empty,
    /// The line's body is longer than the hub takes.
    TooLong { bytes: usize, limit: usize },
    /// The envelope does not fit one packet on this link.
    TooBig { bytes: usize, mdu: usize },
    /// A room name the hub would not take.
    BadRoom,
    /// The session is in as many rooms as the hub allows.
    TooManyRooms(usize),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::NotReady => {
                f.write_str("The hub has not answered yet. Wait a moment and try again.")
            }
            Refusal::NotInRoom(room) => write!(f, "You are not in {room}. Join it first."),
            Refusal::Empty => f.write_str("There is nothing to send."),
            Refusal::TooLong { bytes, limit } => write!(
                f,
                "This line is {bytes} bytes long and a room takes at most {limit}. Shorten it."
            ),
            Refusal::TooBig { bytes, mdu } => write!(
                f,
                "This line needs {bytes} bytes and the link carries {mdu}. Shorten it."
            ),
            Refusal::BadRoom => f.write_str("A room name needs a few letters on one line."),
            Refusal::TooManyRooms(n) => {
                write!(
                    f,
                    "You are in {n} rooms, as many as this hub allows. Leave one first."
                )
            }
        }
    }
}

/// What one envelope from the hub means.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The hub welcomed us; its name, version and limits.
    Welcome {
        hub: Option<String>,
        version: Option<String>,
        limits: Limits,
    },
    /// Someone joined a room; `nick` is `None` when it was us.
    Joined { room: String, nick: Option<String> },
    /// Someone left a room; `nick` is `None` when it was us.
    Parted { room: String, nick: Option<String> },
    /// A line in a room. `ours` when it is the hub's echo of one we sent.
    Line {
        room: String,
        kind: LineKind,
        text: String,
        nick: Option<String>,
        src: Option<Vec<u8>>,
        id: Vec<u8>,
        ts: u64,
        ours: bool,
    },
    /// A notice from the hub itself (no nick), to a room or to us.
    HubNotice { room: Option<String>, text: String },
    /// The hub refused something.
    Error { room: Option<String>, text: String },
    /// The hub answered a PING of ours.
    Pong,
    /// The hub pinged us; the PONG is in [`Received::reply`].
    Pinged,
    /// An envelope of a type a client has no use for, or one it does not
    /// know; nothing to show.
    Ignored(Kind),
}

/// What [`Session::receive`] made of one envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct Received {
    pub event: Event,
    /// An envelope to send back at once (a PONG).
    pub reply: Option<Vec<u8>>,
}

/// One client's state on one hub link.
#[derive(Debug, Clone)]
pub struct Session {
    me: [u8; 16],
    nick: Option<String>,
    name: String,
    version: String,
    welcomed: bool,
    limits: Limits,
    rooms: Vec<String>,
    sent: Vec<Vec<u8>>,
    mdu: Option<usize>,
}

impl Session {
    /// A session for the identity hash `me` (the one the link is
    /// identified with), calling itself `name` `version`, showing `nick`.
    pub fn new(me: [u8; 16], nick: Option<String>, name: &str, version: &str) -> Session {
        Session {
            me,
            nick,
            name: String::from(name),
            version: String::from(version),
            welcomed: false,
            limits: Limits::default(),
            rooms: Vec::new(),
            sent: Vec::new(),
            mdu: None,
        }
    }

    /// The link's packet payload limit, checked before every send.
    pub fn set_mdu(&mut self, mdu: usize) {
        self.mdu = Some(mdu);
    }

    pub fn set_nick(&mut self, nick: Option<String>) {
        self.nick = nick;
    }

    pub fn is_welcomed(&self) -> bool {
        self.welcomed
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// The rooms the hub says we are in.
    pub fn rooms(&self) -> &[String] {
        &self.rooms
    }

    fn envelope(&self, kind: Kind, id: [u8; 8], now_ms: u64) -> Envelope {
        let mut envelope = Envelope::new(kind, id, now_ms);
        envelope.src = Some(self.me.to_vec());
        envelope
    }

    fn fits(&self, bytes: Vec<u8>) -> Result<Vec<u8>, Refusal> {
        match self.mdu {
            Some(mdu) if bytes.len() > mdu => Err(Refusal::TooBig {
                bytes: bytes.len(),
                mdu,
            }),
            _ => Ok(bytes),
        }
    }

    /// The HELLO that opens the session; sent again until WELCOME comes.
    pub fn hello(&self, id: [u8; 8], now_ms: u64) -> Vec<u8> {
        let mut envelope = self.envelope(Kind::Hello, id, now_ms);
        envelope.body = Some(hello_body(&self.name, &self.version));
        envelope.nick.clone_from(&self.nick);
        envelope.encode()
    }

    /// The JOIN for `room`, with a `+k` key when the room asks for one.
    pub fn join(
        &self,
        room: &str,
        key: Option<&str>,
        id: [u8; 8],
        now_ms: u64,
    ) -> Result<Vec<u8>, Refusal> {
        if !self.welcomed {
            return Err(Refusal::NotReady);
        }
        let room = room_name(room, self.limits.room)?;
        if !self.rooms.contains(&room) && self.rooms.len() >= self.limits.rooms {
            return Err(Refusal::TooManyRooms(self.rooms.len()));
        }
        let mut envelope = self.envelope(Kind::Join, id, now_ms);
        envelope.room = Some(room);
        envelope.body = key.map(|k| Value::Text(String::from(k)));
        self.fits(envelope.encode())
    }

    /// The PART for `room`.
    pub fn part(&self, room: &str, id: [u8; 8], now_ms: u64) -> Result<Vec<u8>, Refusal> {
        if !self.welcomed {
            return Err(Refusal::NotReady);
        }
        let room = room_name(room, self.limits.room)?;
        let mut envelope = self.envelope(Kind::Part, id, now_ms);
        envelope.room = Some(room);
        self.fits(envelope.encode())
    }

    /// A line for `room`, refused before anything is sent when the hub
    /// would not take it or the link could not carry it.
    pub fn say(
        &mut self,
        room: &str,
        kind: LineKind,
        text: &str,
        id: [u8; 8],
        now_ms: u64,
    ) -> Result<Vec<u8>, Refusal> {
        if !self.welcomed {
            return Err(Refusal::NotReady);
        }
        let room = room_name(room, self.limits.room)?;
        if !self.rooms.contains(&room) {
            return Err(Refusal::NotInRoom(room));
        }
        if text.trim().is_empty() {
            return Err(Refusal::Empty);
        }
        if text.len() > self.limits.body {
            return Err(Refusal::TooLong {
                bytes: text.len(),
                limit: self.limits.body,
            });
        }
        let mut envelope = self.envelope(kind.kind(), id, now_ms);
        envelope.room = Some(room);
        envelope.body = Some(Value::Text(String::from(text)));
        envelope.nick.clone_from(&self.nick);
        let bytes = self.fits(envelope.encode())?;
        if self.sent.len() >= SENT_KEPT {
            self.sent.remove(0);
        }
        self.sent.push(id.to_vec());
        Ok(bytes)
    }

    /// A command to the hub (1652): a MSG without a room whose body starts
    /// with "/", which rrcd reads as a command before it asks for a room
    /// (`router.py` `_handle_message`). `/list` is the one a client sends
    /// on its own; the hub answers with a NOTICE.
    pub fn command(&self, text: &str, id: [u8; 8], now_ms: u64) -> Result<Vec<u8>, Refusal> {
        if !self.welcomed {
            return Err(Refusal::NotReady);
        }
        if !text.starts_with('/') || text.trim().len() < 2 {
            return Err(Refusal::Empty);
        }
        let mut envelope = self.envelope(Kind::Msg, id, now_ms);
        envelope.body = Some(Value::Text(String::from(text)));
        self.fits(envelope.encode())
    }

    /// A PING of ours, `body` whatever we choose (the reference client
    /// sends its clock in milliseconds).
    pub fn ping(&self, body: u64, id: [u8; 8], now_ms: u64) -> Vec<u8> {
        let mut envelope = self.envelope(Kind::Ping, id, now_ms);
        envelope.body = Some(Value::Uint(body));
        envelope.encode()
    }

    /// What one envelope from the hub means; `id` and `now_ms` go into a
    /// reply when one is owed.
    pub fn receive(
        &mut self,
        bytes: &[u8],
        id: [u8; 8],
        now_ms: u64,
    ) -> Result<Received, EnvelopeError> {
        let envelope = Envelope::decode(bytes)?;
        let mut reply = None;
        let event = match envelope.kind {
            Kind::Welcome => {
                self.welcomed = true;
                self.limits = Limits::of_welcome(envelope.body.as_ref());
                let (hub, version) = welcome_hub(envelope.body.as_ref());
                Event::Welcome {
                    hub: hub.map(String::from),
                    version: version.map(String::from),
                    limits: self.limits,
                }
            }
            Kind::Joined => match envelope.room {
                Some(room) => {
                    if envelope.nick.is_none() && !self.rooms.contains(&room) {
                        self.rooms.push(room.clone());
                    }
                    Event::Joined {
                        room,
                        nick: envelope.nick,
                    }
                }
                None => Event::Ignored(Kind::Joined),
            },
            Kind::Parted => match envelope.room {
                Some(room) => {
                    if envelope.nick.is_none() {
                        self.rooms.retain(|r| *r != room);
                    }
                    Event::Parted {
                        room,
                        nick: envelope.nick,
                    }
                }
                None => Event::Ignored(Kind::Parted),
            },
            Kind::Msg | Kind::Notice | Kind::Action => {
                let kind = match envelope.kind {
                    Kind::Msg => LineKind::Msg,
                    Kind::Notice => LineKind::Notice,
                    _ => LineKind::Action,
                };
                let text = envelope.text().map(String::from).unwrap_or_default();
                match (
                    envelope.room,
                    envelope.nick.is_some() || kind != LineKind::Notice,
                ) {
                    // A NOTICE without a nick is the hub speaking.
                    (room, false) => Event::HubNotice { room, text },
                    (Some(room), true) => {
                        let ours = envelope.src.as_deref() == Some(&self.me[..])
                            && self.sent.contains(&envelope.id);
                        Event::Line {
                            room,
                            kind,
                            text,
                            nick: envelope.nick,
                            src: envelope.src,
                            id: envelope.id,
                            ts: envelope.ts,
                            ours,
                        }
                    }
                    (None, true) => Event::HubNotice { room: None, text },
                }
            }
            Kind::Error => {
                let text = envelope.text().map(String::from).unwrap_or_default();
                Event::Error {
                    room: envelope.room,
                    text,
                }
            }
            Kind::Ping => {
                let mut pong = self.envelope(Kind::Pong, id, now_ms);
                pong.body = envelope.body;
                reply = Some(pong.encode());
                Event::Pinged
            }
            Kind::Pong => Event::Pong,
            other => Event::Ignored(other),
        };
        Ok(Received { event, reply })
    }
}

/// A room name as the hub keeps it: lower case, trimmed, at most `limit`
/// bytes. A space inside is the hub's to allow (1652): rrcd's `_norm_room`
/// only trims and lowers, and a hub of another implementation lists rooms
/// like "general chat", which are joined as the hub spells them. A line
/// break or another control character is no name.
pub fn room_name(room: &str, limit: usize) -> Result<String, Refusal> {
    let room = room.trim().to_lowercase();
    if room.is_empty() || room.len() > limit || room.chars().any(char::is_control) {
        return Err(Refusal::BadRoom);
    }
    Ok(room)
}

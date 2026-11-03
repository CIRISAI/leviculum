//! The RRC envelope: one CBOR map with integer keys, the plaintext of one
//! link packet. Keys and types as the RRC specification 0.1.3 names them
//! and the reference hub (rrcd 0.3.2) sends them, recorded in the vectors
//! (`tests/vectors/envelopes.json`).

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::cbor::{DecodeError, Value};

/// Envelope keys.
pub mod key {
    pub const VERSION: u64 = 0;
    pub const TYPE: u64 = 1;
    pub const ID: u64 = 2;
    pub const TS: u64 = 3;
    pub const SRC: u64 = 4;
    pub const ROOM: u64 = 5;
    pub const BODY: u64 = 6;
    pub const NICK: u64 = 7;
    pub const DST: u64 = 8;
}

/// The protocol version an envelope carries.
pub const VERSION: u64 = 1;

/// The destination name of a hub: application `rrc`, aspect `hub`.
pub const HUB_APP: &str = "rrc";
pub const HUB_ASPECT: &str = "hub";

/// An envelope's type (key 1): the twelve core types and the resource
/// envelope, or a code this codec does not know, kept so it re-encodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Hello,
    Welcome,
    Join,
    Joined,
    Part,
    Parted,
    Msg,
    Notice,
    Action,
    Ping,
    Pong,
    Error,
    ResourceEnvelope,
    Other(u64),
}

impl Kind {
    pub fn code(self) -> u64 {
        match self {
            Kind::Hello => 1,
            Kind::Welcome => 2,
            Kind::Join => 10,
            Kind::Joined => 11,
            Kind::Part => 12,
            Kind::Parted => 13,
            Kind::Msg => 20,
            Kind::Notice => 21,
            Kind::Action => 22,
            Kind::Ping => 30,
            Kind::Pong => 31,
            Kind::Error => 40,
            Kind::ResourceEnvelope => 50,
            Kind::Other(code) => code,
        }
    }

    pub fn of_code(code: u64) -> Kind {
        match code {
            1 => Kind::Hello,
            2 => Kind::Welcome,
            10 => Kind::Join,
            11 => Kind::Joined,
            12 => Kind::Part,
            13 => Kind::Parted,
            20 => Kind::Msg,
            21 => Kind::Notice,
            22 => Kind::Action,
            30 => Kind::Ping,
            31 => Kind::Pong,
            40 => Kind::Error,
            50 => Kind::ResourceEnvelope,
            other => Kind::Other(other),
        }
    }
}

/// One envelope. The optional keys are absent, not empty, when an
/// envelope does not carry them; the vectors show which type carries
/// which. Keys past 8 are kept in [`extra`](Envelope::extra), in order.
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub version: u64,
    pub kind: Kind,
    /// Key 2: eight random bytes from whoever made the envelope.
    pub id: Vec<u8>,
    /// Key 3: milliseconds since the Unix epoch.
    pub ts: u64,
    /// Key 4: the sender's identity hash; the hub sets it to the link's.
    pub src: Option<Vec<u8>>,
    pub room: Option<String>,
    pub body: Option<Value>,
    pub nick: Option<String>,
    /// Key 8 (rrcd 0.3.1): a recipient's identity hash.
    pub dst: Option<Vec<u8>>,
    pub extra: Vec<(Value, Value)>,
}

/// Why bytes are not an envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeError {
    Cbor(DecodeError),
    /// The item is not a map.
    NotAMap,
    /// A key every envelope carries (0 to 3) is missing.
    Missing(u64),
    /// A key holds the wrong CBOR type, or appears twice.
    BadKey(u64),
}

impl From<DecodeError> for EnvelopeError {
    fn from(e: DecodeError) -> Self {
        EnvelopeError::Cbor(e)
    }
}

impl fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnvelopeError::Cbor(e) => write!(f, "not CBOR: {e}"),
            EnvelopeError::NotAMap => f.write_str("the envelope is not a map"),
            EnvelopeError::Missing(k) => write!(f, "the envelope lacks key {k}"),
            EnvelopeError::BadKey(k) => write!(f, "key {k} has the wrong type or comes twice"),
        }
    }
}

impl Envelope {
    /// An envelope of `kind` with the four keys every one carries.
    pub fn new(kind: Kind, id: [u8; 8], ts: u64) -> Envelope {
        Envelope {
            version: VERSION,
            kind,
            id: id.to_vec(),
            ts,
            src: None,
            room: None,
            body: None,
            nick: None,
            dst: None,
            extra: Vec::new(),
        }
    }

    /// Decode one envelope.
    pub fn decode(bytes: &[u8]) -> Result<Envelope, EnvelopeError> {
        let Value::Map(pairs) = Value::decode(bytes)? else {
            return Err(EnvelopeError::NotAMap);
        };
        let mut seen = [false; 9];
        let mut version = None;
        let mut kind = None;
        let mut id = None;
        let mut ts = None;
        let mut envelope = Envelope::new(Kind::Other(0), [0; 8], 0);
        for (k, v) in pairs {
            let Some(k) = k.as_uint().filter(|k| *k <= key::DST) else {
                envelope.extra.push((k, v));
                continue;
            };
            let slot = &mut seen[k as usize];
            if *slot {
                return Err(EnvelopeError::BadKey(k));
            }
            *slot = true;
            let bad = EnvelopeError::BadKey(k);
            match k {
                key::VERSION => version = Some(v.as_uint().ok_or(bad)?),
                key::TYPE => kind = Some(Kind::of_code(v.as_uint().ok_or(bad)?)),
                key::ID => id = Some(bytes_of(v).ok_or(bad)?),
                key::TS => ts = Some(v.as_uint().ok_or(bad)?),
                key::SRC => envelope.src = Some(bytes_of(v).ok_or(bad)?),
                key::ROOM => envelope.room = Some(text_of(v).ok_or(bad)?),
                key::BODY => envelope.body = Some(v),
                key::NICK => envelope.nick = Some(text_of(v).ok_or(bad)?),
                _ => envelope.dst = Some(bytes_of(v).ok_or(bad)?),
            }
        }
        envelope.version = version.ok_or(EnvelopeError::Missing(key::VERSION))?;
        envelope.kind = kind.ok_or(EnvelopeError::Missing(key::TYPE))?;
        envelope.id = id.ok_or(EnvelopeError::Missing(key::ID))?;
        envelope.ts = ts.ok_or(EnvelopeError::Missing(key::TS))?;
        Ok(envelope)
    }

    /// The envelope as a map, keys ascending as the reference writes
    /// them, the extra keys after.
    pub fn to_value(&self) -> Value {
        let mut pairs = Vec::with_capacity(9 + self.extra.len());
        let mut put = |k: u64, v: Value| pairs.push((Value::Uint(k), v));
        put(key::VERSION, Value::Uint(self.version));
        put(key::TYPE, Value::Uint(self.kind.code()));
        put(key::ID, Value::Bytes(self.id.clone()));
        put(key::TS, Value::Uint(self.ts));
        if let Some(src) = &self.src {
            put(key::SRC, Value::Bytes(src.clone()));
        }
        if let Some(room) = &self.room {
            put(key::ROOM, Value::Text(room.clone()));
        }
        if let Some(body) = &self.body {
            put(key::BODY, body.clone());
        }
        if let Some(nick) = &self.nick {
            put(key::NICK, Value::Text(nick.clone()));
        }
        if let Some(dst) = &self.dst {
            put(key::DST, Value::Bytes(dst.clone()));
        }
        pairs.extend(self.extra.iter().cloned());
        Value::Map(pairs)
    }

    pub fn encode(&self) -> Vec<u8> {
        self.to_value().encode()
    }

    /// The body as text, for the types that carry a line.
    pub fn text(&self) -> Option<&str> {
        self.body.as_ref().and_then(Value::as_text)
    }
}

fn bytes_of(v: Value) -> Option<Vec<u8>> {
    match v {
        Value::Bytes(b) => Some(b),
        _ => None,
    }
}

fn text_of(v: Value) -> Option<String> {
    match v {
        Value::Text(s) => Some(s),
        _ => None,
    }
}

/// What a hub allows, from WELCOME's body key 3, with the values the
/// reference hub sends for any key it leaves out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes in a nick.
    pub nick: usize,
    /// Bytes in a room name.
    pub room: usize,
    /// Bytes in a line's body.
    pub body: usize,
    /// Rooms one session may be in.
    pub rooms: usize,
    /// Envelopes per minute.
    pub per_minute: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            nick: 32,
            room: 64,
            body: 350,
            rooms: 32,
            per_minute: 240,
        }
    }
}

impl Limits {
    /// The limits in a WELCOME body, the defaults where a key is absent.
    pub fn of_welcome(body: Option<&Value>) -> Limits {
        let mut limits = Limits::default();
        let Some(map) = body.and_then(|b| b.get(3)) else {
            return limits;
        };
        let read = |k: u64, slot: &mut usize| {
            if let Some(n) = map.get(k).and_then(Value::as_uint) {
                *slot = usize::try_from(n).unwrap_or(usize::MAX);
            }
        };
        read(0, &mut limits.nick);
        read(1, &mut limits.room);
        read(2, &mut limits.body);
        read(3, &mut limits.rooms);
        read(4, &mut limits.per_minute);
        limits
    }
}

/// The body of a HELLO: the client's name and version, and its
/// capabilities (an empty map: this client claims none).
pub fn hello_body(name: &str, version: &str) -> Value {
    Value::Map(alloc::vec![
        (Value::Uint(0), Value::Text(String::from(name))),
        (Value::Uint(1), Value::Text(String::from(version))),
        (Value::Uint(2), Value::Map(Vec::new())),
    ])
}

/// A hub's name and version from a WELCOME body.
pub fn welcome_hub(body: Option<&Value>) -> (Option<&str>, Option<&str>) {
    let get = |k| body.and_then(|b| b.get(k)).and_then(Value::as_text);
    (get(0), get(1))
}

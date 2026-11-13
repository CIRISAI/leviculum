//! What a mailbox says about itself (1642): its name, the machine it runs
//! on, its version and the devices it admitted, for the mailbox page of
//! every device that reads it. `no_std`, as [`super::wire`].
//!
//! Lew, 2026-11-10, on the Fairphone: the page said only "your mailbox is
//! on another device", and nothing on the phone told which one. The phone
//! knows the address it reads and how it reaches the mailbox; the rest
//! only the mailbox knows, so the device asks.
//!
//! ```text
//! /about []  -> [status, name: str, host: str, version: str,
//!                [[device: bin16, name: str, admitted_at: int, last_seen: int], ...],
//!                started: int]
//! ```
//!
//! `host` is the machine's name as the mailbox reads it (`miauhaus`), empty
//! where it knows none; `version` the build's (`loquax::VERSION`).
//! `last_seen` is 0 for a device the mailbox has not heard since it
//! started keeping it. `started` (1647) is when the mailbox's daemon came
//! up, unix seconds, 0 for unknown: a device's link to the mailbox counts
//! from no earlier than that, so a mailbox restart shows on the page even
//! where the device's transport stayed up through it. A reader from 1642
//! reads five fields and leaves the sixth. Only an admitted device is answered. A mailbox from
//! before 1642 has no handler for the path and stays silent: the device
//! shows what it knows itself and says the mailbox is older.

use alloc::string::String;
use alloc::vec::Vec;

use leviculum_lxmf::msgpack;

use super::wire::{self, Malformed, Status};

/// What the mailbox says of itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct About {
    /// The name it announces the identity under; empty for none.
    pub name: String,
    /// The machine it runs on; empty when it knows none.
    pub host: String,
    /// Its build's version.
    pub version: String,
    /// The devices it admitted, the earliest first.
    pub devices: Vec<KnownDevice>,
    /// When the mailbox came up, unix seconds; `None` from a mailbox that
    /// does not say it (1642).
    pub started: Option<i64>,
}

/// A device the mailbox admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownDevice {
    /// Its RNS identity hash: how a device finds itself in the list. Never
    /// shown.
    pub device: [u8; 16],
    /// The name it joined under.
    pub name: String,
    /// When it joined, unix seconds.
    pub admitted_at: i64,
    /// When the mailbox last heard from it, unix seconds.
    pub last_seen: Option<i64>,
}

/// The request: nothing to say.
pub fn about_request() -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 0);
    out
}

/// Whether `data` is an `/about` request.
pub fn read_about_request(data: &[u8]) -> Result<(), Malformed> {
    let mut p = 0;
    msgpack::array_len(data, &mut p)?;
    Ok(())
}

/// The answer to an admitted device.
pub fn about_answer(about: &About) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 6);
    msgpack::uint(&mut out, 0);
    msgpack::string(&mut out, &about.name);
    msgpack::string(&mut out, &about.host);
    msgpack::string(&mut out, &about.version);
    msgpack::array(&mut out, about.devices.len());
    for device in &about.devices {
        msgpack::array(&mut out, 4);
        msgpack::bin(&mut out, &device.device);
        msgpack::string(&mut out, &device.name);
        msgpack::int(&mut out, device.admitted_at);
        msgpack::int(&mut out, device.last_seen.unwrap_or(0));
    }
    msgpack::int(&mut out, about.started.unwrap_or(0));
    out
}

/// An `/about` answer: what the mailbox said, or the status that refused.
pub fn read_about_answer(data: &[u8]) -> Result<Result<About, Status>, Malformed> {
    let status = wire::read_status(data)?;
    if status != Status::Ok {
        return Ok(Err(status));
    }
    let mut p = 0;
    let fields = msgpack::array_len(data, &mut p)?;
    if fields < 5 {
        return Err(Malformed);
    }
    msgpack::read_uint(data, &mut p)?;
    let name = msgpack::read_str(data, &mut p)?.into();
    let host = msgpack::read_str(data, &mut p)?.into();
    let version = msgpack::read_str(data, &mut p)?.into();
    let count = msgpack::array_len(data, &mut p)?;
    let mut devices = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        let fields = msgpack::array_len(data, &mut p)?;
        if fields < 4 {
            return Err(Malformed);
        }
        let device =
            <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?).map_err(|_| Malformed)?;
        let name = msgpack::read_str(data, &mut p)?.into();
        let admitted_at = msgpack::read_int(data, &mut p)?;
        let last_seen = Some(msgpack::read_int(data, &mut p)?).filter(|at| *at > 0);
        for _ in 4..fields {
            msgpack::skip(data, &mut p)?;
        }
        devices.push(KnownDevice {
            device,
            name,
            admitted_at,
            last_seen,
        });
    }
    let started = match fields {
        5 => None,
        _ => Some(msgpack::read_int(data, &mut p)?).filter(|at| *at > 0),
    };
    Ok(Ok(About {
        name,
        host,
        version,
        devices,
        started,
    }))
}

/// A machine's name as a mailbox says it: trimmed, and empty for the
/// names that tell nothing (`localhost`, what Android answers).
pub fn host_word(raw: &str) -> String {
    match raw.trim() {
        "" | "localhost" | "localhost.localdomain" => String::new(),
        name => String::from(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_about_answer_round_trips_and_a_refusal_is_its_status() {
        let about = About {
            name: String::from("Lew Palm"),
            host: String::from("miauhaus"),
            version: String::from("0.9.7"),
            devices: Vec::from([
                KnownDevice {
                    device: [1; 16],
                    name: String::from("Fairphone"),
                    admitted_at: 1_800_000_000,
                    last_seen: Some(1_800_000_600),
                },
                KnownDevice {
                    device: [2; 16],
                    name: String::from("M9"),
                    admitted_at: 1_800_000_100,
                    last_seen: None,
                },
            ]),
            started: Some(1_800_000_050),
        };
        assert_eq!(
            read_about_answer(&about_answer(&about)),
            Ok(Ok(about.clone()))
        );
        // A mailbox from 1642 says five fields: no start, the rest read.
        let mut five = Vec::new();
        msgpack::array(&mut five, 5);
        msgpack::uint(&mut five, 0);
        msgpack::string(&mut five, "Lew Palm");
        msgpack::string(&mut five, "miauhaus");
        msgpack::string(&mut five, "0.9.7");
        msgpack::array(&mut five, 0);
        let old = read_about_answer(&five).expect("decodes").expect("ok");
        assert_eq!(old.started, None);
        assert_eq!(old.host, "miauhaus");
        assert_eq!(
            read_about_answer(&wire::status_answer(Status::NotAdmitted)),
            Ok(Err(Status::NotAdmitted))
        );
        assert_eq!(read_about_request(&about_request()), Ok(()));
    }

    #[test]
    fn a_host_that_tells_nothing_is_empty() {
        assert_eq!(host_word(" miauhaus\n"), "miauhaus");
        assert_eq!(host_word("localhost"), "");
        assert_eq!(host_word(""), "");
    }
}

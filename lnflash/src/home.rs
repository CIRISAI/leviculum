//! `lnflash --home <code> --port <tty>` (order 1415): make a ThinkNode M9
//! a device of a Loquax home without typing the 140 characters of its
//! admission code on the board's keys.
//!
//! The M9 has one UART, behind a CH340K, and no transport port of its own,
//! so this session does not look for LNodes on the bus: it opens the port
//! it is given. Opening the CH340 resets the ESP32 through DTR and RTS,
//! and the board answers control frames only once its boot (the screens'
//! census, the node) is through, so the session asks for the capability
//! report until one comes or [`BOOT_WITHIN`] passes, then sends the code
//! as one `TYPE_HOME_CODE` frame. The board writes the pending record
//! before it acks; it then joins the home over its node, which the
//! `[HOME]` lines on the same port say.
//!
//! The port must be free: on the bench lnsd holds it for the node's
//! packets, so lnsd stops for the few seconds this takes. The frames the
//! session writes carry none of the serial mux's flow bytes (a code is
//! base32 and ASCII punctuation), so they need no escaping on the M9's
//! line; the board's text lines between frames are read past.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use leviculum_core::envelope::TYPE_HOME_CODE;

use crate::envelope::{probe_capabilities, send_home_code, ControlOutcome, SessionReply};
use crate::sys::Fd;

/// How long the board may take from the port's open, which resets it, to
/// answering the capability probe: the boot and its census with margin.
pub const BOOT_WITHIN: Duration = Duration::from_secs(60);

/// Give `code` to the board on `port`, waiting up to `boot_within` for it
/// to answer at all.
pub fn give(port: &Path, code: &str, boot_within: Duration) -> io::Result<SessionReply> {
    let fd = Fd::open_serial(port)?;
    fd.set_transport_port()?;
    give_on(&fd, code, boot_within)
}

/// [`give`] on a port already open.
pub fn give_on(fd: &Fd, code: &str, boot_within: Duration) -> io::Result<SessionReply> {
    let deadline = Instant::now() + boot_within;
    let caps = loop {
        if let Some(caps) = probe_capabilities(fd)? {
            break caps;
        }
        if Instant::now() >= deadline {
            return Ok(SessionReply::ProbeSilent);
        }
    };
    if !caps.accepts(TYPE_HOME_CODE) {
        return Ok(SessionReply::NotAccepted);
    }
    Ok(match send_home_code(fd, code)? {
        ControlOutcome::Acked => SessionReply::Acked,
        ControlOutcome::Refused { reason } => SessionReply::Refused(reason),
        ControlOutcome::NoAnswer => SessionReply::NoAnswer,
    })
}

/// What the session says of a reply, in the words the other sessions use.
pub fn said(port: &Path, reply: SessionReply) -> String {
    let port = port.display();
    match reply {
        SessionReply::Acked => format!(
            "{port}: the board took the code and joins its home now. The [HOME] lines on this port say how it goes."
        ),
        SessionReply::Refused(leviculum_core::envelope::REFUSE_VALUE) => format!(
            "{port}: the board did not read that as a home code. Copy it again from `loquax mailbox admit` or the phone's Link a device screen."
        ),
        SessionReply::Refused(reason) => format!(
            "{port}: the board refused the code: {}.",
            crate::envelope::reason_str(reason)
        ),
        SessionReply::NoAnswer => format!(
            "{port}: the board did not answer the code, so assume it was not written."
        ),
        SessionReply::ProbeSilent => format!(
            "{port}: the board did not answer within {} s of the port opening. Is lnsd or a terminal holding the port? {}",
            BOOT_WITHIN.as_secs(),
            crate::envelope::PROBE_SILENCE_HINT
        ),
        SessionReply::NotAccepted => format!(
            "{port}: this firmware runs no Loquax client. Flash the M9 image with `just flash-m9` first."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::testpty::{spawn_stub, Pty};
    use leviculum_core::envelope::{
        decode_frame, decode_home_code_payload, encode_ack, encode_capability_report_with_facts,
        encode_refusal, FACT_SERIAL_MUX, REFUSE_VALUE, TYPE_CAPABILITIES, TYPE_FRAME_DUMP,
    };
    use std::sync::{Arc, Mutex};

    const CODE: &str = "lqxh1aaaaabbbbbcccccdddddeeeee";

    /// An M9 as the 1415 image is: capabilities with the serial mux's
    /// fact, the code acked when it starts with the prefix, refused as a
    /// value otherwise; every code it took kept for the test to read.
    fn m9(pty: &Pty, accepts: bool, took: Arc<Mutex<Vec<String>>>) {
        spawn_stub(pty, move |data| {
            let frame = decode_frame(data).ok()?;
            match frame.frame_type {
                TYPE_CAPABILITIES => {
                    let types: &[u8] = match accepts {
                        true => &[TYPE_CAPABILITIES, TYPE_FRAME_DUMP, TYPE_HOME_CODE],
                        false => &[TYPE_CAPABILITIES, TYPE_FRAME_DUMP],
                    };
                    Some(encode_capability_report_with_facts(
                        types,
                        &[FACT_SERIAL_MUX],
                    ))
                }
                TYPE_HOME_CODE => {
                    let code = decode_home_code_payload(frame.payload)?.to_string();
                    let good = code.starts_with("lqxh1");
                    took.lock().expect("lock").push(code);
                    Some(match good {
                        true => encode_ack(TYPE_HOME_CODE),
                        false => encode_refusal(TYPE_HOME_CODE, REFUSE_VALUE),
                    })
                }
                _ => None,
            }
        });
    }

    fn open(pty: &Pty) -> Fd {
        let fd = Fd::open_serial(&pty.slave_path).expect("open");
        fd.set_transport_port().expect("line");
        fd
    }

    /// The code reaches the board through its debug text: the board's
    /// lines stand between the frames on its one UART, and the session
    /// reads past them to the ack.
    #[test]
    fn the_code_reaches_an_m9_through_its_text_and_is_acked() {
        let pty = Pty::open();
        let took = Arc::new(Mutex::new(Vec::new()));
        m9(&pty, true, took.clone());
        pty.write_raw(b"[HOME] client=none page=412 t=9000\r\nKBRAW 9010 0000\r\n");
        let fd = open(&pty);
        let reply = give_on(&fd, CODE, Duration::from_secs(5)).expect("session");
        assert_eq!(reply, SessionReply::Acked);
        assert_eq!(*took.lock().expect("lock"), [CODE]);
    }

    /// Unhappy: a text that is no code is refused by the board as a
    /// value, and the session says so in words that send the user back to
    /// the code; a board without the client says it is the wrong image; a
    /// board that never answers is silence after the boot window, not a
    /// hang.
    #[test]
    fn a_bad_code_the_wrong_image_and_a_silent_port_are_named() {
        let pty = Pty::open();
        let took = Arc::new(Mutex::new(Vec::new()));
        m9(&pty, true, took.clone());
        let fd = open(&pty);
        let reply = give_on(&fd, "hello", Duration::from_secs(5)).expect("session");
        assert_eq!(reply, SessionReply::Refused(REFUSE_VALUE));
        assert!(said(Path::new("/dev/m9"), reply).contains("did not read that as a home code"));

        let pty = Pty::open();
        m9(&pty, false, Arc::new(Mutex::new(Vec::new())));
        let fd = open(&pty);
        let reply = give_on(&fd, CODE, Duration::from_secs(5)).expect("session");
        assert_eq!(reply, SessionReply::NotAccepted);
        assert!(said(Path::new("/dev/m9"), reply).contains("flash-m9"));

        let pty = Pty::open();
        let fd = open(&pty);
        let start = Instant::now();
        let reply = give_on(&fd, CODE, Duration::from_millis(500)).expect("session");
        assert_eq!(reply, SessionReply::ProbeSilent);
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    /// The bench's case: the port opens on a board still booting, which
    /// answers nothing for a while; the session keeps asking and sends the
    /// code once the board answers.
    #[test]
    fn a_board_still_booting_is_waited_for() {
        let pty = Pty::open();
        let took = Arc::new(Mutex::new(Vec::new()));
        let booted = Instant::now() + Duration::from_millis(1500);
        let kept = took.clone();
        spawn_stub(&pty, move |data| {
            if Instant::now() < booted {
                return None;
            }
            let frame = decode_frame(data).ok()?;
            match frame.frame_type {
                TYPE_CAPABILITIES => Some(encode_capability_report_with_facts(
                    &[TYPE_CAPABILITIES, TYPE_HOME_CODE],
                    &[FACT_SERIAL_MUX],
                )),
                TYPE_HOME_CODE => {
                    let code = decode_home_code_payload(frame.payload)?.to_string();
                    kept.lock().expect("lock").push(code);
                    Some(encode_ack(TYPE_HOME_CODE))
                }
                _ => None,
            }
        });
        let fd = open(&pty);
        let reply = give_on(&fd, CODE, Duration::from_secs(10)).expect("session");
        assert_eq!(reply, SessionReply::Acked);
        assert_eq!(*took.lock().expect("lock"), [CODE]);
    }
}

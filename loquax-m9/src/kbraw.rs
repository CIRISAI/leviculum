//! The keyboard controller's answer (slice four, 1361; measured in 1427):
//! what the board reads over I2C, logged raw, and what one answer says.
//!
//! The firmware writes [`Protocol::register`], reads [`Protocol::len`]
//! bytes, and prints every answer as a `KBRAW <ms> <hex>` line before it
//! reads anything into it, so a capture replays on the host through
//! [`parse_raw`]. The capture of 2026-11-04 (`thinknode-m9.md`, "The
//! keyboard controller, measured"; `tests/keyboard/capture-20261104.txt`)
//! read the protocol: an idle answer is all zeros, a press is one answer
//! with one key code in every byte ([`Protocol::read`]), and the code is
//! the driver's ([`crate::keyboard::Key::of`]).

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

/// The longest answer the firmware reads in one poll.
pub const MAX_ANSWER: usize = 16;

/// What the firmware asks the controller each poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Protocol {
    /// The controller's address: 0x6D on V1.1, 0x6C on V1.0.
    pub address: u8,
    /// The register written before the read; none reads without a write.
    pub register: Option<u8>,
    /// How many bytes it reads, at most [`MAX_ANSWER`].
    pub len: usize,
}

/// The protocol the capture was taken with and read: register 0x01 of
/// 0x6D, eight bytes. Every byte of an answer carried the same code, so
/// one byte would do; whether a shorter read or register 0x00 or 0x02
/// answers the same is a bench question, and until it is asked the board
/// keeps the read the capture proved, and [`Protocol::read`] holds the
/// eight bytes to agreeing.
pub const MEASURED: Protocol = Protocol {
    address: 0x6D,
    register: Some(0x01),
    len: 8,
};

/// The V1.0 controller's address: the same protocol at 0x6C
/// (`thinknode-m9.md`, "The keyboard controller, measured": the probe
/// asks both; 1493 made the probe's answer the address used — before,
/// [`MEASURED`]'s 0x6D was polled even when only 0x6C answered, and a
/// V1.0 keyboard was dead).
pub const ADDRESS_V10: u8 = 0x6C;

/// The protocol at the address the probe found: [`MEASURED`] where its
/// own address answered (or nothing answered: a controller can sleep
/// through one probe, and polling the measured address stays the bench's
/// behaviour), and the same reads at [`ADDRESS_V10`] where only the V1.0
/// address did.
pub fn probed(v11: bool, v10: bool) -> Protocol {
    match (v11, v10) {
        (false, true) => Protocol {
            address: ADDRESS_V10,
            ..MEASURED
        },
        _ => MEASURED,
    }
}

/// What one answer says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    /// All zeros: no key since the last read.
    Idle,
    /// One press: the key code every byte carried.
    Code(u8),
    /// The bytes disagree, which the capture never saw: no key is taken
    /// from it, the firmware says so, and the `KBRAW` line keeps it.
    Mixed,
}

impl Protocol {
    /// What `answer` says: idle, one code, or bytes that disagree.
    pub fn read(&self, answer: &[u8]) -> Read {
        match answer.split_first() {
            None | Some((0, [])) => Read::Idle,
            Some((code, rest)) if rest.iter().all(|b| b == code) => match code {
                0 => Read::Idle,
                code => Read::Code(*code),
            },
            Some(_) => Read::Mixed,
        }
    }
}

fn hex(out: &mut String, bytes: &[u8]) {
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
}

/// The raw line: `KBRAW <ms> <hex>`, the answer as it came.
pub fn raw_line(ms: u32, answer: &[u8]) -> String {
    let mut out = String::with_capacity(8 + 11 + 2 * answer.len());
    let _ = write!(out, "KBRAW {ms} ");
    hex(&mut out, answer);
    out
}

/// The answers the queue holds before it drops the newest: a key every
/// 10 ms poll for a third of a second of a loop that does not turn.
pub const QUEUE: usize = 32;

/// One answer a poll took that was no idle one, and when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Press {
    /// The board's uptime at the poll.
    pub ms: u32,
    answer: [u8; MAX_ANSWER],
    len: u8,
}

impl Press {
    const EMPTY: Self = Self {
        ms: 0,
        answer: [0; MAX_ANSWER],
        len: 0,
    };

    /// The bytes the controller answered.
    pub fn answer(&self) -> &[u8] {
        &self.answer[..usize::from(self.len)]
    }
}

/// The keyboard read apart from the drawing (1463): a timer interrupt
/// polls the controller every [`crate::keyboard::POLL_MS`] and keeps every
/// answer that is no idle one here, and the loop takes them when it turns.
/// Before, the loop read the controller once per turn, and the controller
/// has no FIFO: a key pressed and let go inside a three-second turn was
/// never read (Lew's hand test, 2026-11-07 12:52). A queue that is full
/// drops the newest and counts it.
#[derive(Debug)]
pub struct Presses {
    ring: [Press; QUEUE],
    head: usize,
    len: usize,
    lost: u32,
    polls: u32,
}

impl Default for Presses {
    fn default() -> Self {
        Self::new()
    }
}

impl Presses {
    pub const fn new() -> Self {
        Self {
            ring: [Press::EMPTY; QUEUE],
            head: 0,
            len: 0,
            lost: 0,
            polls: 0,
        }
    }

    /// One poll's `answer` at `ms`, read by `protocol`: kept unless idle.
    pub fn poll(&mut self, protocol: &Protocol, ms: u32, answer: &[u8]) {
        self.polls = self.polls.wrapping_add(1);
        if protocol.read(answer) == Read::Idle {
            return;
        }
        if self.len == QUEUE {
            self.lost = self.lost.saturating_add(1);
            return;
        }
        let mut press = Press::EMPTY;
        let len = answer.len().min(MAX_ANSWER);
        press.answer[..len].copy_from_slice(&answer[..len]);
        press.len = len as u8;
        press.ms = ms;
        self.ring[(self.head + self.len) % QUEUE] = press;
        self.len += 1;
    }

    /// The oldest answer kept.
    pub fn pop(&mut self) -> Option<Press> {
        if self.len == 0 {
            return None;
        }
        let press = self.ring[self.head];
        self.head = (self.head + 1) % QUEUE;
        self.len -= 1;
        Some(press)
    }

    /// A press waits to be taken (1575): what the UI core's fold asks
    /// after each entry, to yield to the keys at once.
    pub fn waiting(&self) -> bool {
        self.len > 0
    }

    /// Answers dropped because the queue was full.
    pub fn lost(&self) -> u32 {
        self.lost
    }

    /// Polls taken.
    pub fn polls(&self) -> u32 {
        self.polls
    }
}

/// How often a key the bench holds answers again (1533): the cadence of
/// the quickest same-key answers in the bench logs of 2026-11-07 (180 to
/// 240 ms, `rig-run/m9/*/board.log`). Injection only: the controller does
/// not repeat a held key (1561, measured by Lew at the 840cdaae bench on
/// 2026-11-08 13:0x: the house key held 1.2 s did what a tap does,
/// `thinknode-m9.md`, "The keyboard controller, measured"). A hold here
/// is presses at this interval, which the hardware never sends; nothing
/// on the board may rely on a held key.
pub const HOLD_REPEAT_MS: u32 = 200;

/// The answer the controller gives for `code` under `protocol`: the code
/// in every byte it reads, as the capture saw every press (1533). Its
/// [`Protocol::read`] is [`Read::Code`] for any code but 0.
pub fn answer_of(protocol: &Protocol, code: u8) -> ([u8; MAX_ANSWER], usize) {
    let len = protocol.len.clamp(1, MAX_ANSWER);
    let mut answer = [0u8; MAX_ANSWER];
    answer[..len].fill(code);
    (answer, len)
}

/// The bench's keys (1533, a test hook): a press is one answer now, a
/// hold is that answer again every [`HOLD_REPEAT_MS`] until it ends. The
/// caller puts each answer where the keyboard interrupt puts a polled one
/// ([`Presses::poll`]), so the rest of the path is a finger's. One key is
/// held at a time; a new press or hold ends the one before.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Inject {
    held: Option<Hold>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hold {
    code: u8,
    next: u32,
    until: u32,
}

impl Inject {
    pub const fn new() -> Self {
        Self { held: None }
    }

    /// `code` pressed at `now`, held `hold_ms` when some: the code of the
    /// answer to queue now.
    pub fn press(&mut self, code: u8, now: u32, hold_ms: Option<u16>) -> u8 {
        self.held = hold_ms.map(|ms| Hold {
            code,
            next: now.wrapping_add(HOLD_REPEAT_MS),
            until: now.wrapping_add(u32::from(ms)),
        });
        code
    }

    /// The held key's code when its next answer is due at `now`, and the
    /// hold's end once it is past.
    pub fn due(&mut self, now: u32) -> Option<u8> {
        let hold = self.held.as_mut()?;
        if now.wrapping_sub(hold.next) as i32 >= 0 {
            if hold.until.wrapping_sub(hold.next) as i32 >= 0 {
                let code = hold.code;
                hold.next = hold.next.wrapping_add(HOLD_REPEAT_MS);
                return Some(code);
            }
            self.held = None;
        }
        None
    }

    /// Whether a key is held.
    pub fn holding(&self) -> bool {
        self.held.is_some()
    }
}

/// The answers of a capture, `(ms, bytes)` per `KBRAW` line; every other
/// line is skipped and a `KBRAW` line that does not read is too, by number.
pub fn parse_raw(text: &str) -> impl Iterator<Item = Result<(u32, Vec<u8>), usize>> + '_ {
    text.lines().enumerate().filter_map(|(number, line)| {
        let rest = line.trim().strip_prefix("KBRAW ")?;
        let mut words = rest.split_whitespace();
        let mut read = || -> Option<(u32, Vec<u8>)> {
            let ms = words.next()?.parse().ok()?;
            let hex = words.next()?;
            if words.next().is_some() || hex.len() % 2 != 0 {
                return None;
            }
            let bytes = (0..hex.len() / 2)
                .map(|i| u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok())
                .collect::<Option<Vec<u8>>>()?;
            Some((ms, bytes))
        };
        Some(read().ok_or(number + 1))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::Key;

    const CAPTURE: &str = include_str!("../tests/keyboard/capture-20261104.txt");
    const DISTINCT: &str = include_str!("../tests/keyboard/distinct-20261104.txt");

    /// The key names of `capture`'s answers, in order, idle reads skipped.
    fn names(capture: &str) -> Vec<String> {
        parse_raw(capture)
            .map(|answer| answer.expect("every captured line reads"))
            .filter_map(|(_, bytes)| match MEASURED.read(&bytes) {
                Read::Idle => None,
                Read::Code(code) => Some(Key::of(code).expect("a non-zero code").name()),
                Read::Mixed => panic!("the capture has no mixed answer"),
            })
            .collect()
    }

    /// The first 60 presses of the bench capture, through the measured
    /// protocol and the driver, name the keys Lew pressed: the top letter
    /// row, the home row backwards, the bottom row, then the keys that
    /// are no character, by the roles the walk of 2026-11-06 measured
    /// (1459, [`crate::keyboard::SPECIALS`]); `0x90` is no key it pressed.
    #[test]
    fn the_bench_capture_replays_to_its_key_names() {
        let names = names(CAPTURE);
        let want = "q w e new_message r t y ring_down u enter i o p pin \
                    i u y t g f d s z x c v b n m m j h g t \
                    chats new_message home \
                    back back back back \
                    ring_right ring_right ring_down ring_left ring_up \
                    back map pin back e d \
                    new_message home new_message space special_0x90 \
                    new_message back back";
        assert_eq!(names.join(" "), want);
        assert_eq!(names.len(), 60);
    }

    /// Every distinct answer of the capture is one code in all eight
    /// bytes, and the "state byte" the matrix decoder's `KB` line showed
    /// last was no state: it is that code's two low bits, the two matrix
    /// positions (48, 49) the bitmap table read from byte 6.
    #[test]
    fn every_captured_answer_is_one_code_and_the_kb_state_byte_was_its_low_bits() {
        let mut seen = 0;
        for line in DISTINCT.lines().filter(|l| !l.starts_with('#')) {
            let mut words = line.split_whitespace();
            let (Some(raw), Some(_count), Some(kb)) = (words.next(), words.next(), words.next())
            else {
                panic!("a distinct line has three words: {line}");
            };
            let bytes: Vec<u8> = (0..raw.len() / 2)
                .map(|i| u8::from_str_radix(&raw[2 * i..2 * i + 2], 16).expect("hex"))
                .collect();
            let Read::Code(code) = MEASURED.read(&bytes) else {
                panic!("{raw} is one code");
            };
            let last = u8::from_str_radix(&kb[12..14], 16).expect("hex");
            assert_eq!(last, code & 0x03, "{raw} -> {kb}");
            seen += 1;
        }
        assert_eq!(seen, 42, "the capture's 42 distinct codes");
    }

    #[test]
    fn an_answer_reads_idle_one_code_or_mixed() {
        assert_eq!(MEASURED.read(&[0; 8]), Read::Idle);
        assert_eq!(MEASURED.read(&[]), Read::Idle);
        assert_eq!(MEASURED.read(&[0x6b; 8]), Read::Code(0x6b));
        assert_eq!(MEASURED.read(&[0x6b]), Read::Code(0x6b));
        // The positive control: one byte unlike the others is no key.
        let mut answer = [0x6b; 8];
        answer[7] = 0x6c;
        assert_eq!(MEASURED.read(&answer), Read::Mixed);
        answer = [0; 8];
        answer[3] = 0x20;
        assert_eq!(MEASURED.read(&answer), Read::Mixed);
    }

    #[test]
    fn a_capture_line_that_does_not_read_is_named_by_number() {
        let capture = "[FW_BUILD] git_sha=x t=1\r\n\
                       KBRAW 10 0000000000000000\r\n\
                       KEY 20 code=0x6b name=k\r\n\
                       KBRAW 20 6b6b6b6b6b6b6b6b\r\n\
                       KBRAW 30 zz\r\n";
        let answers: Vec<_> = parse_raw(capture).collect();
        assert_eq!(answers.len(), 3);
        assert_eq!(answers[2], Err(5));
        assert_eq!(answers[1], Ok((20, alloc::vec![0x6b; 8])));
    }

    /// 1463, Lew's hand test: a ring key pressed for 10 ms in the middle
    /// of a 3 s turn of the loop. The timer polls every 10 ms into the
    /// queue and the loop drains it at the turn's end: the press is there,
    /// once, at its time. The positive control is the loop before 1463,
    /// which read the controller once per turn, at the turn's start and
    /// its end, and saw nothing.
    #[test]
    fn a_press_inside_a_three_second_stall_is_kept() {
        let ring_up = [0x5a; 8];
        let answer_at = |ms: u32| match ms {
            1500 => ring_up,
            _ => [0; 8],
        };
        let mut queue = Presses::new();
        for ms in (0..=3000).step_by(crate::keyboard::POLL_MS as usize) {
            queue.poll(&MEASURED, ms, &answer_at(ms));
        }
        let press = queue.pop().expect("the press was kept");
        assert_eq!((press.ms, press.answer()), (1500, &ring_up[..]));
        assert_eq!(queue.pop(), None, "one press, one answer");
        assert_eq!((queue.polls(), queue.lost()), (301, 0));
        // The control: a read per turn of a loop that turns every 3 s.
        let per_turn: Vec<Read> = [0, 3000]
            .iter()
            .map(|&ms| MEASURED.read(&answer_at(ms)))
            .collect();
        assert_eq!(per_turn, [Read::Idle, Read::Idle]);
    }

    /// A queue that is full keeps what it has, drops the newest and says
    /// how many; a mixed answer is kept for its `KBRAW` line.
    #[test]
    fn a_full_queue_counts_what_it_drops() {
        let mut queue = Presses::new();
        for i in 0..QUEUE as u32 + 3 {
            queue.poll(&MEASURED, i, &[0x61; 8]);
        }
        assert_eq!(queue.lost(), 3);
        let mut mixed = [0x61; 8];
        mixed[0] = 0x62;
        assert_eq!(queue.pop().map(|p| p.ms), Some(0));
        queue.poll(&MEASURED, 99, &mixed);
        let kept: Vec<u32> = core::iter::from_fn(|| queue.pop()).map(|p| p.ms).collect();
        assert_eq!(kept.len(), QUEUE);
        assert_eq!(kept.last(), Some(&99));
    }

    /// 1533: an injected press is an answer the measured protocol reads as
    /// its code, queued as a poll's; a 500 ms hold answers at 0, 200 and
    /// 400 ms and then ends; a new press ends a hold.
    #[test]
    fn an_injected_key_is_a_polled_answer_and_a_hold_repeats() {
        let (answer, len) = answer_of(&MEASURED, 0xb7);
        assert_eq!(len, MEASURED.len);
        assert_eq!(MEASURED.read(&answer[..len]), Read::Code(0xb7));
        let mut queue = Presses::new();
        queue.poll(&MEASURED, 5, &answer[..len]);
        let press = queue.pop().expect("kept as a poll's answer");
        assert_eq!((press.ms, press.answer()), (5, &[0xb7; 8][..]));
        let short = Protocol { len: 1, ..MEASURED };
        assert_eq!(answer_of(&short, 0x61).1, 1);

        let mut inject = Inject::new();
        let mut at = alloc::vec![(1000, inject.press(0xb5, 1000, Some(500)))];
        for now in (1000..=2000).step_by(crate::keyboard::POLL_MS as usize) {
            if let Some(code) = inject.due(now) {
                at.push((now, code));
            }
        }
        assert_eq!(at, [(1000, 0xb5), (1200, 0xb5), (1400, 0xb5)]);
        assert!(!inject.holding(), "the hold ended");

        // A press without a hold is one answer; a press ends a hold.
        inject.press(0xb5, 0, Some(1000));
        inject.press(0x0d, 50, None);
        assert_eq!((0..2000).find_map(|now| inject.due(now)), None);
        // The positive control: the hold alone would have answered.
        inject.press(0xb5, 0, Some(1000));
        assert_eq!((0..2000).find_map(|now| inject.due(now)), Some(0xb5));
    }

    #[test]
    fn a_raw_line_is_the_answer_in_hex() {
        assert_eq!(raw_line(7, &[0xab, 0x01]), "KBRAW 7 ab01");
    }

    /// 1493: the probe's answer is the address polled. A V1.0 board
    /// (only 0x6C answers) gets its keyboard; the measured 0x6D stays
    /// the default everywhere else, the rest of the protocol unchanged.
    #[test]
    fn the_probe_picks_the_keyboard_address() {
        assert_eq!(probed(true, false), MEASURED);
        assert_eq!(
            probed(true, true),
            MEASURED,
            "both answer: the measured one"
        );
        assert_eq!(
            probed(false, false),
            MEASURED,
            "a sleeping controller is polled as before"
        );
        let v10 = probed(false, true);
        assert_eq!(v10.address, ADDRESS_V10);
        assert_eq!((v10.register, v10.len), (MEASURED.register, MEASURED.len));
    }
}

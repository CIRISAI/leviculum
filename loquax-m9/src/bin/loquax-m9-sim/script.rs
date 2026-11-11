//! The presses a headless run makes (1530): `--keys "down down enter"` or
//! a file of timed steps. A key is named as the board's `KEY` line names
//! it (`loquax_m9::keyboard::Key::name`: `ring_down`, `enter`, `back`,
//! `map`, `k`, `space`, ...), or by the host's word for it (`down`, `esc`,
//! `f5`); each becomes the controller's key code, which goes the board's
//! way from there: the answer the controller gives for it
//! (`kbraw::answer_of`), the poll queue, `Protocol::read`, `Key::of`.
//!
//! The file, one step a line, `#` to the line's end a comment:
//!
//! ```text
//! key <name>          press it (`<name>` alone says the same)
//! hold <name> <ms>    hold it: a press now and every 200 ms until <ms>
//! wait <ms>           let the clock run, the board's turns with it
//! text <words>        type the words, a press per character
//! reboot              power-cycle the board on its flash
//! dump <file.png>     the panel as it is now, as a PNG
//! bar                 the top row's text as a `[SIM] bar="..."` line
//!                     (1564): what the top bar says, for a walk to check
//! screen <name> <file.png>
//!                     the board's FRAME_DUMP of a fixture screen
//!                     (`chats`, `chat`, `groups`, ..., `pan`, or
//!                     `pairing`): walked and drawn in place of the
//!                     model, written as a PNG, as the goldens are
//! bench arrive        TYPE_BENCH_ARRIVE (1562): a message into "Bench",
//!                     the bench's own conversation, never a real chat;
//!                     needs `--bench` and a home (`--store real`)
//! bench forget        TYPE_BENCH_FORGET (1562): every arrival out of the
//!                     store, the store opened again
//! pull <pages> [<n>]  the next <pages> pages of <n> entries (72) of the
//!                     archive a `--store pulling` board pulls (1575),
//!                     handed over at once, folded by the turns after
//! ```
//!
//! Every press advances the clock [`STEP_MS`] unless the file waits.

use loquax_m9::kbraw::HOLD_REPEAT_MS;
use loquax_m9::keyboard::{Key, SPECIALS};

/// The time between two presses of a script that says none: a finger's
/// quick press, slower than the 10 ms poll, under the 100 ms turn bar.
pub const STEP_MS: u64 = 150;

/// One thing a script does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// A press of the key with this controller code.
    Key(u8),
    /// The clock runs this long.
    Wait(u64),
    /// A power cycle.
    Reboot,
    /// The panel as a PNG at this path.
    Dump(String),
    /// The top row's text in a `[SIM] bar=` line (1564).
    Bar,
    /// A `FRAME_DUMP` of the fixture's screen with this byte, as a PNG at
    /// this path.
    Screen(u8, String),
    /// `bench arrive` (1562).
    BenchArrive,
    /// `bench forget` (1562).
    BenchForget,
    /// `pull <pages> <entries>` (1575).
    Pull(usize, usize),
}

/// The `FRAME_DUMP` byte of the screen named `name`.
pub fn screen_byte(name: &str) -> Option<u8> {
    if name == loquax_m9::pair::DUMP_NAME {
        return Some(loquax_m9::pair::DUMP_BYTE);
    }
    loquax_m9::fixture::SCREENS
        .iter()
        .find(|screen| screen.name() == name)
        .map(|screen| screen.byte())
}

/// The controller's code for a key named `name`.
pub fn code(name: &str) -> Option<u8> {
    let lower = name.to_ascii_lowercase();
    let alias = match lower.as_str() {
        "up" => "ring_up",
        "down" => "ring_down",
        "left" => "ring_left",
        "right" => "ring_right",
        "esc" | "escape" => "back",
        "centre" | "center" | "ok" | "return" => "enter",
        "f1" | "bubbles" => "chats",
        "f2" | "house" => "home",
        "f3" | "compose" => "new_message",
        "f4" => "pin",
        "f5" => "map",
        "bs" => "backspace",
        other => other,
    };
    match alias {
        "enter" => return Some(0x0d),
        "backspace" => return Some(0x08),
        "space" => return Some(0x20),
        _ => {}
    }
    if let Some((code, _)) = SPECIALS
        .iter()
        .find(|(code, _)| Key::of(*code).is_some_and(|key| key.name() == alias))
    {
        return Some(*code);
    }
    // One printable character is its own code; the case as given, so
    // `A` is the shifted key.
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_graphic() => Some(c as u8),
        _ => None,
    }
}

/// `--keys`: names apart by spaces or commas.
pub fn keys(text: &str) -> Result<Vec<Step>, String> {
    text.split(|c: char| c.is_whitespace() || c == ',')
        .filter(|word| !word.is_empty())
        .map(|word| {
            code(word)
                .map(Step::Key)
                .ok_or_else(|| format!("--keys: no M9 key is named {word:?}"))
        })
        .collect()
}

/// A script file's steps.
pub fn file(text: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let at = |what: String| format!("line {}: {what}", n + 1);
        let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        let ms = |word: &str| {
            word.parse::<u64>()
                .map_err(|_| at(format!("{word:?} is no time in ms")))
        };
        let key = |word: &str| code(word).ok_or_else(|| at(format!("no M9 key is named {word:?}")));
        match verb {
            "key" => steps.push(Step::Key(key(rest)?)),
            "bench" => match rest {
                "arrive" => steps.push(Step::BenchArrive),
                "forget" => steps.push(Step::BenchForget),
                other => return Err(at(format!("bench arrive|forget, not {other:?}"))),
            },
            "hold" => {
                let (name, held) = rest
                    .split_once(char::is_whitespace)
                    .ok_or_else(|| at(String::from("hold <key> <ms>")))?;
                let (code, held) = (key(name)?, ms(held.trim())?);
                // A press every HOLD_REPEAT_MS, as the bench injects: the
                // press's own step is part of the interval (1532; before,
                // the presses stood STEP_MS further apart than a hold's).
                let repeat = u64::from(HOLD_REPEAT_MS);
                let mut t = 0;
                while t < held.max(1) {
                    steps.push(Step::Key(code));
                    let rest = repeat.min(held.saturating_sub(t)).saturating_sub(STEP_MS);
                    if rest > 0 {
                        steps.push(Step::Wait(rest));
                    }
                    t += repeat;
                }
            }
            "wait" | "sleep" | "idle" => steps.push(Step::Wait(ms(rest)?)),
            "pull" => {
                let mut words = rest.split_whitespace();
                let count = |word: Option<&str>, default: Option<usize>| match word {
                    Some(word) => word
                        .parse::<usize>()
                        .ok()
                        .filter(|n| *n > 0)
                        .ok_or_else(|| at(format!("{word:?} is no count"))),
                    None => default.ok_or_else(|| at(String::from("pull <pages> [<entries>]"))),
                };
                let pages = count(words.next(), None)?;
                let entries = count(words.next(), Some(72))?;
                steps.push(Step::Pull(pages, entries));
            }
            "text" => {
                for c in rest.chars() {
                    match c {
                        ' ' => steps.push(Step::Key(0x20)),
                        c if c.is_ascii_graphic() => steps.push(Step::Key(c as u8)),
                        c => return Err(at(format!("{c:?} has no key on the M9"))),
                    }
                }
            }
            "reboot" => steps.push(Step::Reboot),
            "bar" => steps.push(Step::Bar),
            "screen" => {
                let (name, path) = rest
                    .split_once(char::is_whitespace)
                    .ok_or_else(|| at(String::from("screen <name> <file.png>")))?;
                let byte = screen_byte(name)
                    .ok_or_else(|| at(format!("no fixture screen is named {name:?}")))?;
                steps.push(Step::Screen(byte, path.trim().to_string()));
            }
            "dump" if !rest.is_empty() => steps.push(Step::Dump(rest.to_string())),
            other => match code(other) {
                Some(code) if rest.is_empty() => steps.push(Step::Key(code)),
                _ => return Err(at(format!("{line:?} is no step"))),
            },
        }
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_names_are_the_key_lines_and_the_hosts() {
        assert_eq!(code("ring_down"), Some(0xb6));
        assert_eq!(code("down"), Some(0xb6));
        assert_eq!(code("enter"), Some(0x0d));
        assert_eq!(code("esc"), Some(0x86));
        assert_eq!(code("back"), Some(0x86));
        assert_eq!(code("map"), Some(0x85));
        assert_eq!(code("f1"), Some(0x81));
        assert_eq!(code("t"), Some(b't'));
        assert_eq!(code("T"), Some(b'T'));
        assert_eq!(code("space"), Some(0x20));
        assert_eq!(code("nope"), None);
    }

    #[test]
    fn a_file_says_its_steps() {
        let steps = file(
            "# a walk\ndown\nkey enter\nwait 500\nhold up 400\nreboot\ndump a.png\nscreen pan p.png\npull 2 234\npull 1\n",
        )
            .expect("a script");
        assert_eq!(
            steps,
            [
                Step::Key(0xb6),
                Step::Key(0x0d),
                Step::Wait(500),
                // A press every 200 ms: its own 150 ms step, then 50.
                Step::Key(0xb5),
                Step::Wait(50),
                Step::Key(0xb5),
                Step::Wait(50),
                Step::Reboot,
                Step::Dump(String::from("a.png")),
                Step::Screen(10, String::from("p.png")),
                Step::Pull(2, 234),
                Step::Pull(1, 72),
            ]
        );
        assert!(file("frobnicate\n").is_err(), "the negative: no such step");
        assert!(file("screen nowhere x.png\n").is_err(), "no such screen");
        assert!(file("pull\n").is_err(), "a pull says its pages");
        assert!(file("pull 0\n").is_err(), "a pull of no pages");
        assert_eq!(screen_byte("pairing"), Some(11));
    }
}

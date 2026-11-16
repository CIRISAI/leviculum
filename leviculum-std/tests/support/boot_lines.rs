//! The boot-line extraction the board pins share (#469 step 7): a board's
//! boot read as TEXT, in the order the board runs it. The binary's `main`,
//! with every `node_loop::<step>(` call replaced by the lines of that step's
//! body, then the skeleton's tasks and helpers. From that text it takes
//! every line a board writes (`log_critical!`, `info!`, `warn!`,
//! `log_fmt`/`log_fmt_critical` with their prefix, every call of a `log_*`
//! helper, every `boot_trace` `Phase::`) in order.
//!
//! Written for the T114's move (order 1085, `t114_boot_lines`) and shared
//! since the RAK4631's (order 1572, `rak4631_boot_lines`), so the two pins
//! read a board the same way.

#![allow(dead_code)]

use regex::Regex;
use std::path::{Path, PathBuf};

pub fn tree(rel: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// `src` without its `//` comments (doc comments included); string and
/// char literals are kept whole, so a `//` or a brace inside one survives.
pub fn strip_comments(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            let start = i;
            i += 1;
            while b[i] != b'"' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
            out.push_str(&src[start..i]);
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i] == b'\'' && b.get(i + 2) == Some(&b'\'') {
            out.push_str(&src[i..i + 3]);
            i += 3;
        } else {
            let ch = src[i..].chars().next().expect("in bounds");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// The byte span of `fn <name>(`'s body, braces included, in stripped text.
/// Braces inside string literals do not count.
pub fn fn_body_span(src: &str, name: &str) -> (usize, usize) {
    let sig = Regex::new(&format!(r"\bfn {name}\b")).expect("regex");
    let at = sig
        .find(src)
        .unwrap_or_else(|| panic!("no `fn {name}` in the source"))
        .start();
    let b = src.as_bytes();
    let open = at + src[at..].find('{').expect("fn has a body");
    let mut depth = 0usize;
    let mut i = open;
    loop {
        match b[i] {
            b'"' => {
                i += 1;
                while b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'\'' if b.get(i + 2) == Some(&b'\'') => i += 2,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return (open, i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
}

/// Every line `src` writes, with its byte offset, in source order.
pub fn lines_written(src: &str) -> Vec<(usize, String)> {
    let lit = r#""((?:[^"\\]|\\.)*)""#;
    let mac = Regex::new(&format!(r"\b(log_critical|info|warn)!\(\s*{lit}")).expect("regex");
    let fmt = Regex::new(&format!(
        r"\b(log_fmt_critical|log_fmt)\(\s*{lit},\s*format_args!\(\s*{lit}"
    ))
    .expect("regex");
    // `thread_watch::Phase` (order 1641) is the main loop's work phase, set
    // on every arm, not a boot stamp: only `boot_trace`'s phases are lines.
    let phase = Regex::new(r"(thread_watch::)?Phase::(\w+)").expect("regex");
    let call = Regex::new(r"(?:(\w+)(::|\.))?\b(log_\w+)\(").expect("regex");
    let mut out = Vec::new();
    for c in mac.captures_iter(src) {
        let at = c.get(0).expect("match").start();
        out.push((at, format!("{} {}", &c[1], &c[2])));
    }
    for c in fmt.captures_iter(src) {
        let at = c.get(0).expect("match").start();
        out.push((at, format!("{} {}{}", &c[1], &c[2], &c[3])));
    }
    for c in phase.captures_iter(src) {
        if c.get(1).is_some() {
            continue;
        }
        let at = c.get(0).expect("match").start();
        out.push((at, format!("phase {}", &c[2])));
    }
    for c in call.captures_iter(src) {
        let name = &c[3];
        let at = c.get(0).expect("match").start();
        // A definition is not a line, `log_fmt` is the shape above, and a
        // `node_loop::` step is spliced by `boot_lines`.
        if name == "log_fmt"
            || name == "log_fmt_critical"
            || src[..at].ends_with("fn ")
            || c.get(1).is_some_and(|m| m.as_str() == "node_loop")
        {
            continue;
        }
        let prefix = match c.get(1).map(|m| m.as_str()) {
            None | Some("crate") | Some("leviculum_nrf") => String::new(),
            Some(module) => format!("{module}{}", &c[2]),
        };
        out.push((at, format!("call {prefix}{name}")));
    }
    out.sort_by_key(|(at, _)| *at);
    out
}

/// A board's boot in the order it runs, from the binary, the skeleton and
/// the board file (all three raw source text). `board_mod` is the board
/// module's name as the binary spells it (`t114`, `rak4631`), the one a
/// `log_task_spawned` call names its module constant through.
pub fn boot_lines(bin: &str, skeleton: &str, board: &str, board_mod: &str) -> Vec<String> {
    let bin = strip_comments(bin);
    let skeleton = strip_comments(skeleton);
    let step = Regex::new(r"node_loop::(\w+)\(").expect("regex");
    let spawned = Regex::new(&format!(
        r#"^node_loop::log_task_spawned\(\s*"(\w+)",\s*(?:Some\({board_mod}::(\w+)\)|None)\s*\)"#
    ))
    .expect("regex");

    let (main_start, main_end) = fn_body_span(&bin, "main");
    let main = &bin[main_start..main_end];
    let mut events: Vec<(usize, Vec<String>)> = lines_written(main)
        .into_iter()
        .map(|(at, line)| (at, vec![line]))
        .collect();
    let mut spliced: Vec<(usize, usize)> = Vec::new();
    for c in step.captures_iter(main) {
        let at = c.get(0).expect("match").start();
        let name = &c[1];
        if name == "log_task_spawned" {
            // The one step whose line depends on its arguments: render it
            // the way the function does, with the module from the board file.
            spliced.push(fn_body_span(&skeleton, name));
            let a = spawned.captures(&main[at..]).unwrap_or_else(|| {
                panic!("unreadable log_task_spawned call in bin/{board_mod}.rs")
            });
            let line = match a.get(2) {
                Some(module) => {
                    let konst = Regex::new(&format!(
                        r#"pub const {}: &str = "([^"]*)";"#,
                        module.as_str()
                    ))
                    .expect("regex");
                    let value = konst.captures(board).unwrap_or_else(|| {
                        panic!("boards/{board_mod}.rs has no {}", module.as_str())
                    });
                    format!("info {} task spawned ({})", &a[1], &value[1])
                }
                None => format!("info {} task spawned", &a[1]),
            };
            events.push((at, vec![line]));
        } else {
            let span = fn_body_span(&skeleton, name);
            spliced.push(span);
            let body = lines_written(&skeleton[span.0..span.1])
                .into_iter()
                .map(|(_, l)| l)
                .collect();
            events.push((at, body));
        }
    }
    events.sort_by_key(|(at, _)| *at);

    let mut lines: Vec<String> = lines_written(&bin[..main_start])
        .into_iter()
        .map(|(_, l)| l)
        .collect();
    lines.extend(events.into_iter().flat_map(|(_, l)| l));
    lines.extend(lines_written(&bin[main_end..]).into_iter().map(|(_, l)| l));
    // The skeleton's tasks and helpers no binary calls by name, after the
    // binary, where the old file had them after `main`.
    spliced.sort();
    spliced.dedup();
    let mut rest = String::new();
    let mut from = 0;
    for (s, e) in spliced {
        if s >= from {
            rest.push_str(&skeleton[from..s]);
            from = e;
        }
    }
    rest.push_str(&skeleton[from..]);
    lines.extend(lines_written(&rest).into_iter().map(|(_, l)| l));
    lines
}

pub fn mismatch(got: &[String], want: &[String]) -> Option<String> {
    if got == want {
        return None;
    }
    let first = got
        .iter()
        .zip(want)
        .position(|(g, w)| g != w)
        .unwrap_or(got.len().min(want.len()));
    Some(format!(
        "first difference at line {first}: got {:?}, want {:?} ({} lines got, {} want)",
        got.get(first),
        want.get(first),
        got.len(),
        want.len()
    ))
}

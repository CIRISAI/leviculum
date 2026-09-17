//! The end-to-end path: the built binary, real stdin, real exit codes.
//!
//! The unit tests cover the decoder; this file covers the thing an operator
//! actually types. A green decoder behind a binary that writes nothing to
//! stdout is not a working tool, and only running the binary shows that.

use std::io::Write;
use std::process::{Command, Stdio};

/// Run the built binary with `input` on stdin.
fn run(args: &[&str], input: &str) -> (String, String, i32) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lndecode"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lndecode");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// A header-type-1 announce frame, hand-built, 148-byte payload.
fn announce_hex() -> String {
    let mut raw = vec![0x01u8, 0x00];
    raw.extend_from_slice(&[0xAB; 16]);
    raw.push(0x00);
    let mut payload = vec![0u8; 148];
    payload[79..84].copy_from_slice(&[0x00, 0x6B, 0x49, 0xD2, 0x00]); // 1800000000
    raw.extend_from_slice(&payload);
    raw.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn one_json_object_per_input_line_and_comments_are_skipped() {
    let input = format!(
        "# a capture may carry comments\n{}\n\n{}\n",
        announce_hex(),
        announce_hex()
    );
    let (stdout, stderr, code) = run(&["--now", "1800000000"], &input);
    assert_eq!(code, 0, "stderr: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "expected one object per frame: {stdout}");
    for line in lines {
        let v: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
        assert_eq!(v["tool"], "lndecode", "the tool names itself in its output");
        assert_eq!(v["flags"]["packet_type"], "announce");
        assert_eq!(
            v["announce"]["random_hash"]["emission_secs"],
            1_800_000_000u64
        );
    }
}

#[test]
fn a_wrapped_hex_dump_decodes_as_one_frame_under_single() {
    let hex = announce_hex();
    let wrapped: String = hex
        .as_bytes()
        .chunks(32)
        .map(|c| format!("{}\n", std::str::from_utf8(c).unwrap()))
        .collect();
    let (stdout, stderr, code) = run(&["--single", "--now", "1800000000"], &wrapped);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout.lines().count(), 1);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["bytes"], 167);
}

#[test]
fn an_undecodable_line_fails_loudly_rather_than_silently() {
    let (stdout, stderr, code) = run(&[], "not-bytes-at-all!!\n");
    assert_eq!(code, 1);
    assert!(stdout.is_empty(), "nothing may reach stdout: {stdout}");
    assert!(
        stderr.contains("lndecode:"),
        "the error names the tool: {stderr}"
    );
}

#[test]
fn an_empty_input_is_not_reported_as_a_successful_decode() {
    let (_stdout, stderr, code) = run(&[], "\n# only a comment\n");
    assert_eq!(code, 1);
    assert!(stderr.contains("no frames"), "stderr: {stderr}");
}

#[test]
fn pretty_output_is_indented_json_of_the_same_document() {
    let (compact, _, _) = run(&["--now", "1800000000"], &format!("{}\n", announce_hex()));
    let (pretty, _, code) = run(
        &["--pretty", "--now", "1800000000"],
        &format!("{}\n", announce_hex()),
    );
    assert_eq!(code, 0);
    assert!(pretty.contains("\n  \""), "expected indentation: {pretty}");
    let a: serde_json::Value = serde_json::from_str(compact.trim()).unwrap();
    let b: serde_json::Value = serde_json::from_str(pretty.trim()).unwrap();
    assert_eq!(a, b);
}

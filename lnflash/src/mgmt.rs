//! `--management-identity` / `--clear-management` — who may read a board's
//! status remotely (Codeberg #235).
//!
//! The board-facing half is two control-envelope frames
//! (`TYPE_MGMT_ALLOW` to set or clear, `TYPE_MGMT_ALLOW_QUERY` to read);
//! everything in this module is the human-facing half: validate what a
//! person typed before a byte reaches a board, and say what the board
//! answered. Nothing here opens a port — the open lives with
//! [`crate::flow`], which proves it landed on the intended board — so the
//! parsing and the wire bytes are testable without one.
//!
//! # Why the flags exist
//!
//! A daemon with remote management enabled answers `rnstatus -R` and
//! `lnstatus -R`: interface counters, uptime, link-table size, over
//! whichever carrier reaches it. A board could not, so the only way to
//! read a mast node's counters was a serial cable and a debug-port capture
//! — which is to say, a ladder. #235 gives the board the same
//! `rnstransport.remote.management` destination and the same `/status`
//! handler, gated by an identity allow-list.
//!
//! # Why the list is written here and nowhere else
//!
//! The list rides the USB control envelope, and that is the whole of the
//! access control: the envelope is parsed on the board's transport CDC and
//! nowhere else, so nothing on the air can add itself. An operator wanting
//! to change who may read a field node has to be holding it. That is a
//! deliberate cost, and it is the reason a board may carry a management
//! destination at all.
//!
//! # A board with no list serves nobody
//!
//! Not "serves everybody", and not "serves the daemon's default". A
//! factory-fresh board has no record, registers no management destination,
//! and answers no `rnstatus -R` — the rule is
//! [`leviculum_core::mgmt_allow_store::remote_mgmt_decision`] and the
//! reasoning is there. `--clear-management` puts a board back in that
//! state on purpose.
//!
//! # Two moments, and the board says which
//!
//! The management destination is created while the node is built, from the
//! record read at boot, so a list set over USB is live at the **next
//! reset**. The report says so through its `running` flag, and
//! [`reboot_note`] turns that into a sentence — the same honesty
//! [`crate::name`] publishes for the BLE name and [`crate::media`] for a
//! carrier switched back on. An operator who has just *revoked* an
//! identity needs that sentence most: the old list is still being served
//! until the board is reset.

use std::io;

use leviculum_core::envelope::{MgmtAllowState, TYPE_MGMT_ALLOW, TYPE_MGMT_ALLOW_QUERY};
use leviculum_core::mgmt_allow_store::MGMT_ALLOW_MAX_IDENTITIES;

use crate::envelope::{self, SessionReply};
use crate::sys::Fd;
use crate::ui::Ui;

/// The one question the flash flow asks about remote management. `[y/N]`
/// rather than `[Y/n]` for the reason the telemetry prompt gives: Enter
/// leaves the board exactly as it was, and here that default is also the
/// safe one — a board nobody may read remotely.
pub const ASK_MANAGEMENT: &str = "\nAllow remote status queries (rnstatus -R)? [y/N]";

/// And the one input a "yes" needs, asked until an answer parses or the
/// operator presses Enter.
pub const ASK_IDENTITY: &str = "  management identity hash (32 hex characters)";

/// What the `--management-*` flags said, before any board is touched.
///
/// `Ask` is the default, and its default answer is no — so a flash run
/// that nobody is watching (`--yes`, a closed stdin) leaves the board with
/// no management destination, which is the state #235 requires of an
/// unattended board.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MgmtPlan {
    /// Ask.
    #[default]
    Ask,
    /// Decided on the command line; do not ask. An empty list is
    /// `--clear-management`.
    Fixed(Vec<IdentityHash>),
    /// Do not ask and send nothing: the board keeps whatever list it had.
    /// What `--no-management` means, and what answering "no" at the prompt
    /// does — a board with a list somebody set keeps it unless somebody
    /// says to clear it.
    Skip,
}

/// Turn the plan into the list to send. `None` is "send nothing".
///
/// **Non-interactive runs never block here**, and not by detecting a
/// terminal: [`Ui::ask`] answers `None` both for `--yes`
/// ([`crate::ui::Assumed`]) and for a closed or piped stdin. Every loop
/// below treats `None` as the default, which is what keeps the re-prompt
/// finite. The same mechanism the radio and telemetry prompts use; there
/// is no second rule here.
///
/// The prompt collects identities one at a time until an empty answer,
/// because an operator typing 32 hex digits wants to check each one before
/// the next, and a comma-separated line of four hashes is 131 characters
/// nobody can proofread.
pub fn resolve(ui: &mut dyn Ui, plan: &MgmtPlan) -> io::Result<Option<Vec<IdentityHash>>> {
    match plan {
        MgmtPlan::Skip => return Ok(None),
        MgmtPlan::Fixed(list) => return Ok(Some(list.clone())),
        MgmtPlan::Ask => {}
    }

    let answer = ui.ask(ASK_MANAGEMENT)?.unwrap_or_default();
    if !matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes" | "j" | "ja"
    ) {
        return Ok(None);
    }

    let mut allowed: Vec<IdentityHash> = Vec::new();
    while allowed.len() < MGMT_ALLOW_MAX_IDENTITIES {
        let Some(typed) = ui.ask(ASK_IDENTITY)? else {
            break;
        };
        if typed.trim().is_empty() {
            break;
        }
        match parse_identity(&typed) {
            Ok(hash) => allowed.push(hash),
            // Said and re-asked rather than fatal: the firmware is already
            // on the board by the time this runs, and one mistyped hash
            // must not cost the operator the whole step.
            Err(err) => ui.say(&format!("  {err}")),
        }
    }
    if allowed.is_empty() {
        // "Yes" and then no identity is not a clear: a clear is a command
        // an operator gives on purpose, and this is a half-answered prompt.
        // Nothing goes out, and the line says why rather than leaving it to
        // be inferred from a transcript that just stops.
        ui.say(
            "  no identity given, so nothing is sent: the board keeps whatever allow-list it \
             already had, and `--clear-management` is what empties one",
        );
        return Ok(None);
    }
    Ok(Some(allowed))
}

/// One identity hash, as `rnid`/`rnstatus` print it and as `lnsd`'s
/// `identity_data` file holds it: 32 lowercase or uppercase hex digits.
pub type IdentityHash = [u8; 16];

/// Validate one `--management-identity` value.
///
/// Refuses anything that is not exactly 32 hex digits. Deliberately strict
/// about the length: a 16-digit value is a *destination* hash prefix or a
/// typo, and silently zero-padding it would produce a permission for an
/// identity that does not exist, which reads as "I set it" and behaves as
/// "nobody may read this board". The error names the flag and what was
/// typed, so an operator reads what to type instead rather than "invalid".
pub fn parse_identity(text: &str) -> Result<IdentityHash, String> {
    let trimmed = text.trim();
    // A leading `<` / trailing `>` is how RNS *prints* a hash
    // (`prettyhexrep`), so an operator pasting one back is doing the
    // obvious thing and must not be punished for it.
    let hex = trimmed
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .unwrap_or(trimmed);
    if hex.len() != 32 {
        return Err(format!(
            "--management-identity {text:?}: an identity hash is 32 hex digits ({} given)",
            hex.len()
        ));
    }
    let mut hash = [0u8; 16];
    for (i, byte) in hash.iter_mut().enumerate() {
        let pair = &hex[i * 2..i * 2 + 2];
        *byte = u8::from_str_radix(pair, 16).map_err(|_| {
            format!("--management-identity {text:?}: {pair:?} is not two hex digits")
        })?;
    }
    Ok(hash)
}

/// Validate the whole `--management-identity` set, refusing more than the
/// board can hold before any board is touched.
///
/// Duplicates are NOT dropped here: the board drops them and reports what
/// it stored, and a host that pre-filtered would hide from the operator
/// that they listed the same station twice. The bound is checked against
/// the raw count for the same reason the board checks the distinct one —
/// here it is a chance to say "you listed nine"; there it is the record's
/// capacity.
pub fn parse_identities(values: &[String]) -> Result<Vec<IdentityHash>, String> {
    if values.len() > MGMT_ALLOW_MAX_IDENTITIES {
        return Err(format!(
            "--management-identity given {} times: a board holds at most {} identities",
            values.len(),
            MGMT_ALLOW_MAX_IDENTITIES
        ));
    }
    values.iter().map(|text| parse_identity(text)).collect()
}

/// Lowercase hex, the form `rnid -i` prints and `lnsd`'s config file takes.
fn hex(hash: &IdentityHash) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

/// One line describing who may read a board, for the transcript and for
/// grep.
///
/// Deliberately the same `key=value` shape the board's own `[MGMT]` banner
/// uses, so comparing the two is comparing identical text. The identities
/// are spelled out rather than counted: the count alone cannot tell an
/// operator whether the identity they just revoked is gone.
pub fn describe(state: &MgmtAllowState) -> String {
    let who = if state.allowed.is_empty() {
        "none".to_string()
    } else {
        state.allowed.iter().map(hex).collect::<Vec<_>>().join(",")
    };
    format!(
        "allowed={} serving={} src={} identities={}",
        state.allowed.len(),
        if state.running { "yes" } else { "no" },
        if state.stored { "flash" } else { "unset" },
        who
    )
}

/// Read the board's allow-list on an already-opened transport port.
///
/// Behind the capability probe like every other command, so firmware
/// without the frame is reported as such rather than read blind.
pub fn query(fd: &Fd) -> io::Result<Result<MgmtAllowState, SessionReply>> {
    let Some(caps) = envelope::probe_capabilities(fd)? else {
        return Ok(Err(SessionReply::ProbeSilent));
    };
    if !caps.accepts(TYPE_MGMT_ALLOW_QUERY) {
        return Ok(Err(SessionReply::NotAccepted));
    }
    Ok(envelope::query_mgmt_allow(fd)?.map_err(SessionReply::from))
}

/// Set the board's allow-list, or clear it (`allowed` empty).
///
/// Returns the board's report — the list it actually stored and whether it
/// is serving one yet — because that is the only place those facts exist:
/// the board drops duplicates, and only the board knows what its boot
/// registered.
pub fn send(fd: &Fd, allowed: &[IdentityHash]) -> io::Result<Result<MgmtAllowState, SessionReply>> {
    let Some(caps) = envelope::probe_capabilities(fd)? else {
        return Ok(Err(SessionReply::ProbeSilent));
    };
    if !caps.accepts(TYPE_MGMT_ALLOW) {
        return Ok(Err(SessionReply::NotAccepted));
    }
    Ok(envelope::send_mgmt_allow(fd, allowed)?.map_err(SessionReply::from))
}

/// What to tell an operator beyond the list itself: that the board is not
/// serving it yet, or is still serving the previous one.
///
/// Empty when neither applies, so a caller can append it unconditionally.
pub fn reboot_note(state: &MgmtAllowState) -> String {
    if state.running {
        if state.allowed.is_empty() {
            // Cleared on a board that booted with a list: the door is
            // still open until the reset, which is the one case an
            // operator must not misread as "revoked".
            return " The board is still serving its PREVIOUS allow-list: the management \
                     destination is created at boot and cannot be withdrawn while the node runs. \
                     Reset the board to make this clear take effect."
                .to_string();
        }
        // Running with a list, and the list was just rewritten: the
        // identities being served are the boot's, not these.
        return " The identities above are on the page; the board is serving the list it read at \
                boot until it is reset."
            .to_string();
    }
    if state.allowed.is_empty() {
        // Nothing stored, nothing running: the state a board ships in,
        // and the state a clear leaves a board that was never serving.
        return String::new();
    }
    " The board is not serving remote management yet: the management destination is created at \
      boot. Reset the board and `rnstatus -R` / `lnstatus -R` will answer for the identities \
      above."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::testing::{
        envelope_firmware_stub, envelope_firmware_stub_with_mgmt, mgmt_allow_frame,
        mgmt_state_serving, mgmtless_firmware_stub, old_firmware_stub, pre_236_firmware_stub, seen,
        unpersisting_mgmt_firmware_stub,
    };
    use crate::sys::testpty::Pty;
    use crate::ui::testing::Fake;

    fn hashes(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| format!("{:02x}", i + 1).repeat(16))
            .collect()
    }

    // -----------------------------------------------------------------
    // What a human may type
    // -----------------------------------------------------------------

    #[test]
    fn a_plain_hash_is_accepted_in_either_case() {
        let lower = "0123456789abcdef0123456789abcdef";
        let upper = "0123456789ABCDEF0123456789ABCDEF";
        assert_eq!(
            parse_identity(lower).unwrap(),
            parse_identity(upper).unwrap()
        );
        assert_eq!(parse_identity(lower).unwrap()[0], 0x01);
        assert_eq!(parse_identity(lower).unwrap()[15], 0xef);
    }

    #[test]
    fn the_form_rns_prints_is_accepted_back() {
        // `RNS.prettyhexrep` wraps a hash in angle brackets, and an
        // operator pasting one back is doing the obvious thing.
        let plain = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            parse_identity(&format!("<{plain}>")).unwrap(),
            parse_identity(plain).unwrap()
        );
    }

    #[test]
    fn a_refused_hash_is_named_with_the_flag_and_the_rule() {
        for typed in [
            "0123456789abcdef",                   // a 16-digit prefix
            "0123456789abcdef0123456789abcdefff", // one byte too long
            "0123456789abcdef0123456789abcdeg",   // not hex
            "",
        ] {
            let err = parse_identity(typed).unwrap_err();
            assert!(err.contains("--management-identity"), "{err}");
        }
        // The length refusal says both numbers, so the operator can see
        // what they pasted.
        let err = parse_identity("0123456789abcdef").unwrap_err();
        assert!(err.contains("32"), "{err}");
        assert!(err.contains("16"), "{err}");
    }

    #[test]
    fn nothing_here_pads_or_truncates_what_was_typed() {
        // A permission that arrives different from the one that was typed
        // is worse than an error message: it reads as "set" and behaves as
        // "nobody".
        assert!(parse_identity("abcd").is_err());
        assert!(parse_identity("0123456789abcdef0123456789abcdef00").is_err());
    }

    #[test]
    fn the_bound_is_refused_at_the_command_line() {
        assert_eq!(
            parse_identities(&hashes(MGMT_ALLOW_MAX_IDENTITIES))
                .unwrap()
                .len(),
            MGMT_ALLOW_MAX_IDENTITIES
        );
        let err = parse_identities(&hashes(MGMT_ALLOW_MAX_IDENTITIES + 1)).unwrap_err();
        assert!(err.contains("--management-identity"), "{err}");
        assert!(
            err.contains(&MGMT_ALLOW_MAX_IDENTITIES.to_string()),
            "{err}"
        );
    }

    #[test]
    fn a_duplicate_is_passed_on_rather_than_hidden() {
        // The board drops it and reports what it stored; filtering here
        // would hide from the operator that they listed one station twice.
        let one = "0123456789abcdef0123456789abcdef".to_string();
        let parsed = parse_identities(&[one.clone(), one]).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], parsed[1]);
    }

    // -----------------------------------------------------------------
    // What the transcript says
    // -----------------------------------------------------------------

    fn state(allowed: Vec<IdentityHash>, stored: bool, running: bool) -> MgmtAllowState {
        MgmtAllowState {
            stored,
            running,
            allowed,
        }
    }

    #[test]
    fn the_description_spells_the_identities_out() {
        let hash = parse_identity("0123456789abcdef0123456789abcdef").unwrap();
        let line = describe(&state(vec![hash], true, true));
        assert!(line.contains("allowed=1"), "{line}");
        assert!(line.contains("serving=yes"), "{line}");
        assert!(line.contains("src=flash"), "{line}");
        // The identity itself, because a count cannot tell an operator
        // whether the one they revoked is gone.
        assert!(line.contains("0123456789abcdef0123456789abcdef"), "{line}");
    }

    #[test]
    fn an_empty_list_reads_as_nobody_not_as_everybody() {
        let line = describe(&state(Vec::new(), true, false));
        assert!(line.contains("allowed=0"), "{line}");
        assert!(line.contains("identities=none"), "{line}");
        assert!(line.contains("serving=no"), "{line}");
    }

    #[test]
    fn a_fresh_set_says_the_board_is_not_serving_it_yet() {
        let hash = parse_identity("0123456789abcdef0123456789abcdef").unwrap();
        let note = reboot_note(&state(vec![hash], true, false));
        assert!(note.contains("Reset the board"), "{note}");
        assert!(note.contains("rnstatus -R"), "{note}");
    }

    #[test]
    fn a_clear_on_a_serving_board_says_the_door_is_still_open() {
        // The one case an operator must not misread as "revoked".
        let note = reboot_note(&state(Vec::new(), true, true));
        assert!(note.contains("PREVIOUS allow-list"), "{note}");
        assert!(note.contains("Reset the board"), "{note}");
    }

    #[test]
    fn a_board_that_was_never_serving_needs_no_note_after_a_clear() {
        assert_eq!(reboot_note(&state(Vec::new(), true, false)), "");
    }

    #[test]
    fn a_rewrite_on_a_serving_board_says_which_list_is_live() {
        let hash = parse_identity("0123456789abcdef0123456789abcdef").unwrap();
        let note = reboot_note(&state(vec![hash], true, true));
        assert!(note.contains("read at boot"), "{note}");
    }

    // -----------------------------------------------------------------
    // The flash-time question
    // -----------------------------------------------------------------

    #[test]
    fn the_default_answer_is_no_and_nothing_is_sent() {
        // What an unwatched flash run does. #235's requirement is that an
        // unattended board comes up serving nobody, and the way that is
        // guaranteed is that Enter — and `--yes`, and a closed stdin —
        // leaves the board alone.
        for typed in ["", "n", "no", "nein", "later", "Y E S"] {
            let mut ui = Fake::typing(&[typed]);
            assert_eq!(
                resolve(&mut ui, &MgmtPlan::default()).unwrap(),
                None,
                "{typed:?} was taken as a yes"
            );
            assert!(
                !ui.transcript().contains("management identity"),
                "{typed:?} reached the identity prompt"
            );
        }
    }

    #[test]
    fn yes_collects_identities_until_an_empty_answer() {
        let a = "0123456789abcdef0123456789abcdef";
        let b = "fedcba9876543210fedcba9876543210";
        let mut ui = Fake::typing(&["y", a, b]);
        assert_eq!(
            resolve(&mut ui, &MgmtPlan::default()).unwrap(),
            Some(vec![parse_identity(a).unwrap(), parse_identity(b).unwrap()])
        );
        // The question, then one prompt per identity, then the one that
        // ended the list: three asks for two identities.
        let said = ui.transcript();
        assert!(said.contains(ASK_MANAGEMENT), "{said}");
        assert_eq!(said.matches(ASK_IDENTITY).count(), 3, "{said}");
    }

    #[test]
    fn a_mistyped_hash_is_asked_again_rather_than_sent() {
        let a = "0123456789abcdef0123456789abcdef";
        let mut ui = Fake::typing(&["y", "abcd", "zzzz", a]);
        assert_eq!(
            resolve(&mut ui, &MgmtPlan::default()).unwrap(),
            Some(vec![parse_identity(a).unwrap()])
        );
        let said = ui.transcript();
        assert!(said.contains("32 hex digits"), "{said}");
        // The firmware is already on the board by then, so one typo must
        // not cost the whole step.
        assert_eq!(said.matches(ASK_IDENTITY).count(), 4, "{said}");
    }

    #[test]
    fn yes_with_no_identity_is_not_a_clear() {
        // A half-answered prompt must not empty a list somebody set: a
        // clear is a command an operator gives on purpose.
        let mut ui = Fake::typing(&["y"]);
        assert_eq!(resolve(&mut ui, &MgmtPlan::default()).unwrap(), None);
        let said = ui.transcript();
        assert!(said.contains("--clear-management"), "{said}");
        assert!(said.contains("keeps whatever allow-list"), "{said}");
    }

    #[test]
    fn the_prompt_stops_at_the_boards_bound() {
        // One more hash typed than the board can hold: the prompt stops
        // asking at the bound rather than collecting a list the board would
        // refuse whole.
        let mut typed = vec!["y".to_string()];
        for i in 0..=MGMT_ALLOW_MAX_IDENTITIES {
            typed.push(format!("{:02x}", i + 1).repeat(16));
        }
        let refs: Vec<&str> = typed.iter().map(String::as_str).collect();
        let mut ui = Fake::typing(&refs);
        let allowed = resolve(&mut ui, &MgmtPlan::default()).unwrap().unwrap();
        assert_eq!(allowed.len(), MGMT_ALLOW_MAX_IDENTITIES);
        assert_eq!(
            ui.transcript().matches(ASK_IDENTITY).count(),
            MGMT_ALLOW_MAX_IDENTITIES,
            "the prompt asked past the bound"
        );
    }

    #[test]
    fn a_plan_the_flags_decided_is_not_asked_about() {
        let a = "0123456789abcdef0123456789abcdef";
        let fixed = vec![parse_identity(a).unwrap()];
        let mut ui = Fake::typing(&[]);
        assert_eq!(
            resolve(&mut ui, &MgmtPlan::Fixed(fixed.clone())).unwrap(),
            Some(fixed)
        );
        assert!(ui.said.is_empty(), "{:?}", ui.said);
        // And Skip sends nothing without asking either.
        let mut ui = Fake::typing(&[]);
        assert_eq!(resolve(&mut ui, &MgmtPlan::Skip).unwrap(), None);
        assert!(ui.said.is_empty(), "{:?}", ui.said);
    }

    // -----------------------------------------------------------------
    // The bytes that reach the board
    // -----------------------------------------------------------------

    fn hash_a() -> IdentityHash {
        parse_identity("0123456789abcdef0123456789abcdef").unwrap()
    }

    fn hash_b() -> IdentityHash {
        parse_identity("fedcba9876543210fedcba9876543210").unwrap()
    }

    #[test]
    fn a_list_reaches_the_board_and_comes_back_as_a_report() {
        let pty = Pty::open();
        let seen = seen();
        envelope_firmware_stub(&pty, seen.clone());
        let fd = Fd::open_serial(&pty.slave_path).unwrap();

        let state = send(&fd, &[hash_a(), hash_b()]).unwrap().unwrap();
        assert!(state.stored);
        assert_eq!(state.allowed, vec![hash_a(), hash_b()]);
        // And the bytes the board actually saw, so a host that reported a
        // list it never sent shows up here.
        assert_eq!(mgmt_allow_frame(&seen), Some(vec![hash_a(), hash_b()]));
    }

    #[test]
    fn a_fresh_board_reports_no_list_rather_than_an_empty_one() {
        let pty = Pty::open();
        envelope_firmware_stub(&pty, seen());
        let fd = Fd::open_serial(&pty.slave_path).unwrap();

        let state = query(&fd).unwrap().unwrap();
        assert!(!state.stored, "a fresh board has no record");
        assert!(!state.running, "and serves nobody");
        assert!(state.allowed.is_empty());
        // The transcript must not read as "everybody".
        assert!(describe(&state).contains("identities=none"));
    }

    #[test]
    fn a_set_on_a_serving_board_is_reported_as_one_reset_behind() {
        // The honesty this report exists for. The page carries the new list
        // the moment the frame is answered; the management destination was
        // created at boot from the old one and cannot be rebuilt.
        let pty = Pty::open();
        envelope_firmware_stub_with_mgmt(&pty, seen(), mgmt_state_serving(&[hash_a()]));
        let fd = Fd::open_serial(&pty.slave_path).unwrap();

        let state = send(&fd, &[hash_b()]).unwrap().unwrap();
        assert_eq!(state.allowed, vec![hash_b()], "the page took the new list");
        assert!(state.running, "and the old one is still being served");
        let note = reboot_note(&state);
        assert!(note.contains("read at boot"), "{note}");
    }

    #[test]
    fn a_clear_on_a_serving_board_reports_the_door_as_still_open() {
        // The revocation case: an operator who reads "cleared" and walks
        // away has been told the wrong thing until the board is reset.
        let pty = Pty::open();
        envelope_firmware_stub_with_mgmt(&pty, seen(), mgmt_state_serving(&[hash_a()]));
        let fd = Fd::open_serial(&pty.slave_path).unwrap();

        let state = send(&fd, &[]).unwrap().unwrap();
        assert!(state.allowed.is_empty());
        assert!(state.running);
        assert!(reboot_note(&state).contains("PREVIOUS allow-list"));
    }

    #[test]
    fn a_duplicate_comes_back_collapsed_because_the_board_collapsed_it() {
        // The report is the board's, not an echo: the same identity twice
        // is one permission, and the host prints what was stored.
        let pty = Pty::open();
        envelope_firmware_stub(&pty, seen());
        let fd = Fd::open_serial(&pty.slave_path).unwrap();

        let state = send(&fd, &[hash_a(), hash_a()]).unwrap().unwrap();
        assert_eq!(state.allowed, vec![hash_a()]);
    }

    #[test]
    fn firmware_without_the_allow_list_is_named_rather_than_timed_out() {
        // A binary that never read the record: the type is advertised, the
        // capability is not there, and the refusal says which.
        let pty = Pty::open();
        mgmtless_firmware_stub(&pty, seen());
        let fd = Fd::open_serial(&pty.slave_path).unwrap();

        assert_eq!(
            send(&fd, &[hash_a()]).unwrap(),
            Err(SessionReply::Refused(
                leviculum_core::envelope::REFUSE_UNSUPPORTED
            ))
        );
        assert_eq!(
            query(&fd).unwrap(),
            Err(SessionReply::Refused(
                leviculum_core::envelope::REFUSE_UNSUPPORTED
            ))
        );
    }

    #[test]
    fn firmware_from_before_the_frame_refuses_it_by_name() {
        let pty = Pty::open();
        pre_236_firmware_stub(&pty, seen());
        let fd = Fd::open_serial(&pty.slave_path).unwrap();
        assert_eq!(
            send(&fd, &[hash_a()]).unwrap(),
            Err(SessionReply::NotAccepted)
        );
        assert_eq!(query(&fd).unwrap(), Err(SessionReply::NotAccepted));
    }

    #[test]
    fn pre_envelope_firmware_is_the_silent_probe_and_nothing_is_sent() {
        let pty = Pty::open();
        let seen = seen();
        old_firmware_stub(&pty, seen.clone());
        let fd = Fd::open_serial(&pty.slave_path).unwrap();
        assert_eq!(
            send(&fd, &[hash_a()]).unwrap(),
            Err(SessionReply::ProbeSilent)
        );
        // And no allow-list frame went out on a guess: the frame is 134
        // bytes, well over the 19-byte Reticulum minimum, so a board that
        // never answered the probe must never receive it.
        assert_eq!(mgmt_allow_frame(&seen), None);
    }

    #[test]
    fn a_list_that_did_not_reach_flash_is_a_refusal_and_not_a_report() {
        // #358: the only thing a set is for is the next boot, so a board
        // that could not write the page must not be read as having stored
        // the list.
        let pty = Pty::open();
        unpersisting_mgmt_firmware_stub(&pty, seen());
        let fd = Fd::open_serial(&pty.slave_path).unwrap();
        assert_eq!(
            send(&fd, &[hash_a()]).unwrap(),
            Err(SessionReply::Refused(
                leviculum_core::envelope::REFUSE_PERSIST
            ))
        );
    }
}

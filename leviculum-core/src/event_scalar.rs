//! Scalar rendering for values that go into a structured event field.
//!
//! The event-log contract (`docs/src/structured-event-logs.md`) is one event
//! per line, tokenised on whitespace into `key=value` pairs. A value that
//! carries a space therefore splits into bare tokens and the line no longer
//! parses back to the key set it was emitted with; an embedded `=` collides
//! with the next key.
//!
//! Interface names are the case that matters here: they are free text from
//! the config file (`[[TCP Uplink]]`) or from discovery
//! (`autoconnect/Dark Doodad 23`), so a space is legitimate INPUT, not a
//! source bug — the emission site is what has to render it as a scalar.
//!
//! The substitution (`_` for whitespace, `=` and anything non-graphic) is the
//! same one `leviculum_std::event_log` applies as its last-resort rescue in
//! the sink, so a name reads identically whether it was rendered here or
//! caught there. Substituting rather than dropping keeps the field present
//! and keeps the value recognisable: a reader maps it back to the configured
//! name by reading `_` as "any single separator".
//!
//! Quoting was the alternative and is rejected: every consumer of the format
//! (`jl`, `jldiff`, the field-violation detector, and the `awk`/`grep`
//! one-liners the format exists for) splits on whitespace, so a quoted value
//! with a space is still two tokens to all of them.

/// Display wrapper that renders any string as a single event-log token:
/// every character that would break the whitespace `key=value` tokenisation
/// becomes `_`.
pub(crate) struct Scalar<'a>(pub &'a str);

impl core::fmt::Display for Scalar<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        use core::fmt::Write as _;
        for c in self.0.chars() {
            f.write_char(if c.is_ascii_graphic() && c != '=' {
                c
            } else {
                '_'
            })?;
        }
        Ok(())
    }
}

/// Display wrapper for an event field that is an age in milliseconds but may
/// not exist: `Some(ms)` renders the number, `None` renders `none` — the
/// `<n>|none` shape other fields already use (`iface_out=`,
/// `old_data_silence_ms=`).
///
/// A missing age has to be SAID, not computed. Subtracting an unset timestamp
/// from the clock yields the process uptime, which reads as a perfectly
/// plausible age and is wrong by hours: the field log of 2026-09-27 printed
/// `elapsed_since_activity_ms=9581430` against `threshold_ms=85824` for culls
/// that were 72-90 s old, because a handshake that never completes has no last
/// inbound packet to measure from (Codeberg #354).
pub(crate) struct MsOrNone(pub Option<u64>);

impl core::fmt::Display for MsOrNone {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(ms) => write!(f, "{ms}"),
            None => f.write_str("none"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MsOrNone, Scalar};
    use alloc::format;

    #[test]
    fn a_name_with_spaces_becomes_one_token() {
        assert_eq!(
            format!("{}", Scalar("autoconnect/Dark Doodad 23")),
            "autoconnect/Dark_Doodad_23"
        );
    }

    #[test]
    fn an_embedded_equals_cannot_fake_a_second_key() {
        assert_eq!(format!("{}", Scalar("a=b")), "a_b");
    }

    #[test]
    fn a_tab_or_newline_cannot_split_the_line() {
        assert_eq!(format!("{}", Scalar("a\tb\nc")), "a_b_c");
    }

    #[test]
    fn non_ascii_is_replaced_rather_than_passed_through() {
        // Matches the sink's rescue exactly, so a value renders the same
        // whichever of the two produced it.
        assert_eq!(format!("{}", Scalar("Café")), "Caf_");
    }

    #[test]
    fn a_plain_name_is_untouched() {
        assert_eq!(format!("{}", Scalar("lora0")), "lora0");
    }

    #[test]
    fn an_age_that_exists_renders_as_digits() {
        assert_eq!(format!("{}", MsOrNone(Some(69_961))), "69961");
    }

    #[test]
    fn an_age_that_does_not_exist_is_said_not_computed() {
        assert_eq!(format!("{}", MsOrNone(None)), "none");
    }
}

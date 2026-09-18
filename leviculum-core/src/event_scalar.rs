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

#[cfg(test)]
mod tests {
    use super::Scalar;
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
}

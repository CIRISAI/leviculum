//! Command-line argument resolution for the `lnomad` binary.
//!
//! The positional argument means different things in the two modes: in page
//! mode it is the URL to fetch, in `--discover` mode it is an optional listen
//! duration in seconds. [`resolve_args`] is the pure decision function that maps
//! the raw positional plus the explicit `--duration` flag onto a [`Mode`], with
//! no argv or terminal access, so the whole rule set is unit-testable.

/// The resolved intent of an `lnomad` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Discover NomadNet nodes, listening for the given number of seconds.
    Discover { duration: u64 },
    /// Fetch and render the page at the given URL string (not yet parsed).
    Page { url: String },
}

/// Resolve the positional argument and the explicit `--duration` flag onto a
/// [`Mode`].
///
/// In `--discover` mode the positional, if present, is the listen duration in
/// seconds:
/// - A bare integer is used as the duration.
/// - A non-numeric positional is an error (it is not a page URL here).
/// - If both the positional and an explicit `--duration` are given and they
///   disagree, that is an error; if they agree, or only one is given, it is
///   accepted.
/// - If neither is given, `default_duration` is used.
///
/// In page mode the positional is the required URL string and `--duration` is
/// irrelevant.
pub fn resolve_args(
    discover: bool,
    positional: Option<&str>,
    explicit_duration: Option<u64>,
    default_duration: u64,
) -> Result<Mode, String> {
    if discover {
        let positional_duration = match positional {
            Some(arg) => Some(arg.parse::<u64>().map_err(|_| {
                format!(
                    "--discover takes an optional listen duration in seconds, \
                     not a page URL (\"{arg}\")"
                )
            })?),
            None => None,
        };

        let duration = match (positional_duration, explicit_duration) {
            (Some(from_positional), Some(from_flag)) if from_positional != from_flag => {
                return Err(
                    "specify the discover duration once, via --duration or the positional"
                        .to_string(),
                );
            }
            (Some(value), _) => value,
            (None, Some(value)) => value,
            (None, None) => default_duration,
        };

        Ok(Mode::Discover { duration })
    } else {
        match positional {
            Some(url) => Ok(Mode::Page {
                url: url.to_string(),
            }),
            None => Err("a page URL is required (or pass --discover)".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: u64 = 30;

    #[test]
    fn discover_bare_integer_is_the_duration() {
        assert_eq!(
            resolve_args(true, Some("5"), None, DEFAULT),
            Ok(Mode::Discover { duration: 5 })
        );
    }

    #[test]
    fn discover_non_numeric_positional_is_an_error() {
        let err = resolve_args(true, Some("abcd:/page/x.mu"), None, DEFAULT).unwrap_err();
        assert!(err.contains("listen duration in seconds"), "{err}");
        assert!(err.contains("abcd:/page/x.mu"), "{err}");
    }

    #[test]
    fn discover_explicit_duration_only() {
        assert_eq!(
            resolve_args(true, None, Some(8), DEFAULT),
            Ok(Mode::Discover { duration: 8 })
        );
    }

    #[test]
    fn discover_positional_and_matching_duration_is_ok() {
        assert_eq!(
            resolve_args(true, Some("8"), Some(8), DEFAULT),
            Ok(Mode::Discover { duration: 8 })
        );
    }

    #[test]
    fn discover_conflicting_positional_and_duration_is_an_error() {
        let err = resolve_args(true, Some("3"), Some(8), DEFAULT).unwrap_err();
        assert!(err.contains("once"), "{err}");
    }

    #[test]
    fn discover_no_positional_no_flag_uses_default() {
        assert_eq!(
            resolve_args(true, None, None, DEFAULT),
            Ok(Mode::Discover { duration: DEFAULT })
        );
    }

    #[test]
    fn page_mode_positional_stays_the_url() {
        assert_eq!(
            resolve_args(false, Some("abcd:/page/index.mu"), None, DEFAULT),
            Ok(Mode::Page {
                url: "abcd:/page/index.mu".to_string()
            })
        );
    }

    #[test]
    fn page_mode_without_positional_is_an_error() {
        let err = resolve_args(false, None, None, DEFAULT).unwrap_err();
        assert!(err.contains("page URL is required"), "{err}");
    }
}

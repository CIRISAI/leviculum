//! Resource receive-window policy selection via environment variable
//! (Codeberg #85).
//!
//! The policy is an experiment knob for benchmarking window adaptation
//! algorithms, so it is deliberately an environment variable and NOT a
//! config-file key: the config format is shared with Python rnsd and must
//! not grow leviculum-only keys.

pub use leviculum_core::resource::WindowPolicy;

/// Environment variable selecting the resource receive-window policy.
pub const RESOURCE_WINDOW_POLICY_ENV: &str = "LEVICULUM_RESOURCE_WINDOW_POLICY";

/// Read the resource receive-window policy from
/// `LEVICULUM_RESOURCE_WINDOW_POLICY`. Unset means the default
/// [`WindowPolicy::Adaptive`]; an unknown value falls back to the default
/// with a warning. Adaptive is the default since the #85 rig A/B: it matches
/// PythonLike on a clean link, cannot be worse under loss (it keeps
/// window_max where PythonLike lowers it), and beats the legacy Current
/// window-4 cap by ~15% throughput with far lower variance, all at 100%
/// delivery. The policy stays receiver-local and never rides the wire, so
/// Python-RNS interop is unaffected.
pub fn resource_window_policy_from_env() -> WindowPolicy {
    match std::env::var(RESOURCE_WINDOW_POLICY_ENV) {
        Ok(value) => parse_resource_window_policy(&value),
        Err(_) => WindowPolicy::Adaptive,
    }
}

/// Parse a `LEVICULUM_RESOURCE_WINDOW_POLICY` value. Unknown values fall
/// back to the default [`WindowPolicy::Adaptive`] with a warning.
pub fn parse_resource_window_policy(value: &str) -> WindowPolicy {
    match WindowPolicy::parse(value) {
        Some(policy) => policy,
        None => {
            tracing::warn!(
                "unknown {RESOURCE_WINDOW_POLICY_ENV} value {value:?}, using \"adaptive\""
            );
            WindowPolicy::Adaptive
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_current() {
        assert_eq!(
            parse_resource_window_policy("current"),
            WindowPolicy::Current
        );
        assert_eq!(
            parse_resource_window_policy("CURRENT"),
            WindowPolicy::Current
        );
    }

    #[test]
    fn test_parse_unknown_falls_back_to_default_adaptive() {
        assert_eq!(
            parse_resource_window_policy("bogus"),
            WindowPolicy::Adaptive
        );
        assert_eq!(parse_resource_window_policy(""), WindowPolicy::Adaptive);
    }

    #[test]
    fn test_explicit_policies_still_selectable() {
        assert_eq!(
            parse_resource_window_policy("current"),
            WindowPolicy::Current
        );
        assert_eq!(
            parse_resource_window_policy("adaptive"),
            WindowPolicy::Adaptive
        );
        assert_eq!(
            parse_resource_window_policy("pythonlike"),
            WindowPolicy::PythonLike
        );
    }
}

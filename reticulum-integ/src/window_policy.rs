//! Pass-through of the resource receive-window policy selector (Codeberg
//! #85) from the integ executor to spawned tools.
//!
//! The policy is receiver-local, so A/B runs on hardware need the SAME
//! test executed with different receiver policies. The executor forwards
//! the variable verbatim to every process it spawns (lnsd via the compose
//! environment, lncp listener and sender via docker exec); when the
//! variable is unset in the executor's own environment, nothing changes.

/// Environment variable selecting the resource receive-window policy,
/// read by lnsd and lncp. The executor does not interpret the value.
pub const RESOURCE_WINDOW_POLICY_ENV: &str = "LEVICULUM_RESOURCE_WINDOW_POLICY";

/// The policy value from the executor's own environment, if set.
pub fn from_env() -> Option<String> {
    std::env::var(RESOURCE_WINDOW_POLICY_ENV).ok()
}

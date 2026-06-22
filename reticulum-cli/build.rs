// Compose a full version string for the three CLI binaries (lnsd, lns,
// lncp). Local builds get the plain crate version ("0.7.0"). CI nightly
// builds get a suffix via the LEVICULUM_BUILD_ID env var, producing
// something like "0.7.0-nightly.20260419-5a5df20". The result lands in
// env var LEVICULUM_VERSION, which the binaries pick up with env!() and
// hand to clap's #[command(version = …)] attribute.
fn main() {
    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let build_id = std::env::var("LEVICULUM_BUILD_ID").unwrap_or_default();
    let full = if build_id.is_empty() {
        pkg_version
    } else {
        format!("{pkg_version}-{build_id}")
    };
    println!("cargo:rustc-env=LEVICULUM_VERSION={full}");
    println!("cargo:rerun-if-env-changed=LEVICULUM_BUILD_ID");
}

// Detect the one case where `[build] target` in `.cargo/config.toml` is
// the wrong answer: a host whose architecture is not the one the pin
// names. Cargo has no per-host conditional in `[build]`, so the pin is
// unconditional and an arm64 machine builds x86_64 binaries from a fresh
// clone — successfully, and then they do not run (Codeberg #291).
//
// `include!`d by `leviculum-cli/build.rs`, which turns a hit into a cargo
// warning, and by `leviculum-std/tests/build_target_host_arch.rs`, which
// is where it is actually tested: a build script is not compiled as a
// test target, so logic living only there is logic nobody checks.

/// The architecture component of a target triple: `aarch64` out of
/// `aarch64-unknown-linux-gnu`.
fn triple_arch(triple: &str) -> &str {
    triple.split('-').next().unwrap_or(triple)
}

/// The musl-static sibling of a host triple, which is what this workspace
/// wants to build even when it is not the pinned one: `aarch64-unknown-
/// linux-gnu` -> `aarch64-unknown-linux-musl`. Anything that is not a
/// `-gnu` triple is returned unchanged, because guessing further would
/// print a triple that does not exist.
fn musl_variant(host: &str) -> String {
    match host.strip_suffix("-gnu") {
        Some(stem) => format!("{stem}-musl"),
        None => host.to_string(),
    }
}

/// `[build] target` out of `.cargo/config.toml` text, or `None` if it is
/// not set. Parsed by hand: a build script that pulls in a TOML crate
/// pays for it on every clean build of the workspace.
fn configured_build_target(config: &str) -> Option<String> {
    let mut in_build = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_build = line == "[build]";
            continue;
        }
        if !in_build {
            continue;
        }
        let value = line.strip_prefix("target")?.trim_start().strip_prefix('=')?;
        return Some(value.trim().trim_matches('"').to_string());
    }
    None
}

/// `Some(message)` when this build is producing binaries for a foreign
/// architecture *only because the workspace default named one*.
///
/// The `target == pinned` test is what keeps deliberate cross-compiles
/// quiet: `scripts/build-deb.sh arm64` passes `--target aarch64-unknown-
/// linux-musl`, `cargo build-ffi-arm64` names a gnu triple, the firmware
/// gates name thumbv7em, and a host that sets `CARGO_BUILD_TARGET` has
/// said what it wants. None of those equal the pin, so none of them warn;
/// only the silent default does.
fn foreign_default_target(host: &str, target: &str, pinned: Option<&str>) -> Option<String> {
    if pinned? != target || triple_arch(host) == triple_arch(target) {
        return None;
    }
    Some(format!(
        "this build produces {target} binaries and this host is {}, so they link and then \
         fail to run with \"cannot execute binary file\". `.cargo/config.toml` pins that \
         target for every host because cargo has no per-host conditional in [build]. On this \
         machine build with `rustup target add {musl}` once, then \
         `CARGO_BUILD_TARGET={musl} cargo build ...`.",
        triple_arch(host),
        musl = musl_variant(host),
    ))
}

//! What the `loquax` package asks of the system, read off the manifest
//! (1265, walk finding 5 of the ninety percent walk).
//!
//! The package is what a person installs who has never heard of Reticulum,
//! and the messenger alone has nothing to attach to, so it depends on the
//! `leviculum` package, whose systemd-units enable and start lnsd. A
//! Recommends would leave a `--no-install-recommends` install, and every
//! front end configured that way, with a messenger that cannot start.
//! `scripts/verify-deb-packaging.sh` checks the same field on the built
//! `.deb`; this catches the manifest without a package build.

use std::path::Path;

fn manifest(crate_dir: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(crate_dir)
            .join("Cargo.toml"),
    )
    .expect("the manifest is readable")
}

/// The keys of one table, up to the next table header, comments dropped.
fn section<'a>(manifest: &'a str, header: &str) -> Vec<&'a str> {
    let body = manifest
        .split(&format!("\n{header}\n"))
        .nth(1)
        .unwrap_or_else(|| panic!("no {header}"));
    body.split("\n[")
        .next()
        .expect("a section body")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

#[test]
fn the_package_depends_on_the_daemon_package() {
    let own = manifest(".");
    let deb = section(&own, "[package.metadata.deb]");
    let depends = deb
        .iter()
        .find_map(|line| line.strip_prefix("depends = "))
        .expect("loquax declares depends");
    let names: Vec<&str> = depends
        .trim_matches('"')
        .split(',')
        .map(|dep| dep.split_whitespace().next().unwrap_or(""))
        .collect();
    assert!(
        names.contains(&"leviculum"),
        "the messenger needs lnsd; depends was {depends}"
    );
    assert!(
        !deb.iter()
            .any(|line| line.starts_with("recommends = \"leviculum")),
        "leviculum is a Depends, not also a Recommends: {deb:?}"
    );
}

/// The dependency brings lnsd up only because the leviculum package
/// enables and starts its unit itself; loquax's postinst does not.
#[test]
fn the_daemon_package_enables_and_starts_lnsd() {
    let cli = manifest("../leviculum-cli");
    let deb = section(&cli, "[package.metadata.deb]");
    assert!(deb.contains(&"name = \"leviculum\""), "{deb:?}");
    let units = section(&cli, "[package.metadata.deb.systemd-units]");
    for key in ["unit-name = \"lnsd\"", "enable = true", "start = true"] {
        assert!(units.contains(&key), "{key} missing: {units:?}");
    }
}

/// The asset lines of one table: the `[source, dest, mode]` rows.
fn assets<'a>(section: &[&'a str]) -> Vec<&'a str> {
    section
        .iter()
        .copied()
        .filter(|line| line.starts_with("[\""))
        .collect()
}

/// 1469, #474: the amd64 package is the base's files and the v3 build
/// beside them, which the starter (loquax/src/cpu.rs) finds at
/// `/usr/lib/loquax/loquax-v3`; arm64 (the base) and armhf (terminal)
/// carry no second binary.
#[test]
fn only_the_amd64_package_carries_the_v3_build() {
    let own = manifest(".");
    let base = assets(&section(&own, "[package.metadata.deb]"));
    let amd64 = assets(&section(&own, "[package.metadata.deb.variants.amd64]"));
    let terminal = assets(&section(&own, "[package.metadata.deb.variants.terminal]"));
    let v3 = "[\"../target/x86-64-v3/x86_64-unknown-linux-gnu/release/loquax\", \"usr/lib/loquax/loquax-v3\", \"755\"],";
    assert!(amd64.contains(&v3), "{amd64:?}");
    let mut rest: Vec<&str> = amd64.iter().copied().filter(|a| *a != v3).collect();
    let mut want = base.clone();
    rest.sort_unstable();
    want.sort_unstable();
    assert_eq!(rest, want, "amd64 is the base's files and the v3 build");
    for other in [&base, &terminal] {
        assert!(!other.iter().any(|a| a.contains("loquax-v3")), "{other:?}");
    }
}

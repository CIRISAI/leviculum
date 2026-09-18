//! Copying `lxmd`'s identity file must give `lnpnd` the same node address.
//!
//! A propagation node's destination hash is what every client and every peer
//! has configured. If the swap from `lxmd` to `lnpnd` changes it, the node
//! comes back as a stranger: clients keep uploading to a hash nobody
//! answers, and every peer has to rediscover it. So the identity has to
//! carry over, and "it should, both use RNS identities" is a reading of the
//! code, not a measurement.
//!
//! Both daemons keep the file at `<configdir>/identity` in RNS's own
//! `Identity.to_file` form (`lxmd`: `program_setup`,
//! `reference/LXMF/LXMF/Utilities/lxmd.py:337/389`; `lnpnd`:
//! `lnpnd::identity::load_or_create`). Python mints one and computes the
//! hashes; this test loads the same file and has to arrive at the same
//! bytes.

use std::path::PathBuf;
use std::process::Command;

use leviculum_core::{Destination, DestinationType, Direction};
use leviculum_lxmf::node::APP_NAME;
use leviculum_lxmf::propagation_client::PROPAGATION_ASPECT;

/// `<identity path>`, `<lxmf.propagation hash>`, `<lxmf.delivery hash>` —
/// or `None` when `reference/Reticulum` is not checked out.
fn python_identity(dir: &std::path::Path) -> Option<(PathBuf, String, String)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    if !root.join("reference/Reticulum/RNS").is_dir() {
        eprintln!("reference/Reticulum not checked out, skipping");
        return None;
    }
    let output = Command::new("python3")
        .arg(root.join("scripts/make-lxmd-identity.py"))
        .arg(dir)
        .output()
        .expect("python3 runs");
    assert!(
        output.status.success(),
        "the reference could not mint an identity:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut lines = stdout.lines();
    Some((
        PathBuf::from(lines.next().expect("identity path")),
        lines.next().expect("propagation hash").to_string(),
        lines.next().expect("delivery hash").to_string(),
    ))
}

fn hash_of(identity: &leviculum_std::Identity, aspect: &str) -> String {
    let destination = Destination::new(
        Some(identity.clone()),
        Direction::In,
        DestinationType::Single,
        APP_NAME,
        &[aspect],
    )
    .expect("destination builds");
    destination
        .hash()
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn an_lxmd_identity_keeps_the_node_address() {
    let dir = tempfile::tempdir().expect("temp config dir");
    let Some((identity_path, propagation_hex, delivery_hex)) = python_identity(dir.path()) else {
        return;
    };

    // The path lnpnd looks at is the path lxmd wrote: no conversion step,
    // no import command. Copying the config directory is the migration.
    assert_eq!(
        identity_path,
        dir.path().join("identity"),
        "both daemons keep the identity at <configdir>/identity"
    );

    let (identity, provenance) = lnpnd::identity::load_or_create(&identity_path)
        .expect("lnpnd loads an identity Python wrote");
    assert_eq!(
        provenance,
        lnpnd::identity::Provenance::Loaded,
        "a file that is already there is loaded, never re-minted"
    );

    assert_eq!(
        hash_of(&identity, PROPAGATION_ASPECT),
        propagation_hex,
        "the propagation node address must survive the swap"
    );
    assert_eq!(
        hash_of(&identity, "delivery"),
        delivery_hex,
        "and so must the daemon's own mailbox address"
    );

    // load_or_create must not have rewritten the file it found: a re-mint
    // here would strand every client, and it is the one failure mode this
    // whole test exists to rule out.
    let (reloaded, provenance) =
        lnpnd::identity::load_or_create(&identity_path).expect("a second load is a load");
    assert_eq!(provenance, lnpnd::identity::Provenance::Loaded);
    assert_eq!(
        hash_of(&reloaded, PROPAGATION_ASPECT),
        propagation_hex,
        "loading an existing identity must never replace it"
    );
}

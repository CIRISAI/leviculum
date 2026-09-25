//! `-v` must work in client mode (Codeberg #427).
//!
//! The field shape this pins: `lnpnd --status` timed out 7 of 8 times on a
//! loaded node, and `-vvv` with `RUST_LOG=trace` produced not one client
//! side line, because only the daemon path installed a subscriber. The
//! operator therefore could not answer the first question of the
//! investigation — did the client dial the same instance the daemon uses —
//! without strace, which the host did not have.
//!
//! The contract: the shipped binary, run with `-vv` against an instance
//! that does not exist, says on stderr which instance it is dialling and
//! which socket that resolves to. Both halves matter — the attempt line
//! proves the subscriber is installed before the dial, the socket string
//! is what an operator compares against `ss -xlp` on the daemon side.

use std::process::Command;

#[test]
fn vv_against_a_missing_instance_puts_the_dial_on_stderr() {
    let config = tempfile::tempdir().expect("tempdir config");
    // Client mode refuses to mint an identity (querying with an address the
    // remote never allowed fails without a hint why), so give it one.
    lnpnd::identity::load_or_create(&config.path().join("identity")).expect("mint an identity");

    let instance = format!("lnpnd_vv_missing_{}", std::process::id());
    let output = Command::new(env!("CARGO_BIN_EXE_lnpnd"))
        .args([
            "--status",
            "-vv",
            "--timeout",
            "1",
            "--config",
            config.path().to_str().expect("utf-8 tempdir"),
            "--instance",
            &instance,
        ])
        // The assertion is about the level -v computes, not an ambient one.
        .env_remove("RUST_LOG")
        .env_remove("LEVICULUM_EVENT_LOG")
        .output()
        .expect("run the client binary");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "a missing instance must fail the client; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!("connecting to shared instance '{instance}'")),
        "-vv must put the connect attempt on stderr; got:\n{stderr}"
    );
    let socket = leviculum_std::interfaces::shared_instance_socket_display(&instance);
    assert!(
        stderr.contains(&socket),
        "-vv must name the resolved socket {socket} on stderr; got:\n{stderr}"
    );
}

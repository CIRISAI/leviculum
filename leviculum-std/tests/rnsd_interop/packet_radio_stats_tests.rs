//! D6 of `docs/src/audits/2026-10-11-shared-instance-rpc.md`: `rnprobe`
//! against lnsd prints the RSSI, SNR and link quality of the proof that came
//! back over a radio, as it does against rnsd.
//!
//! Python stamps every packet from an interface with radio readings and keeps
//! them by packet hash for local clients
//! (`reference/Reticulum/RNS/Transport.py:1458-1479`); `rnprobe` asks for its
//! proof packet's readings over the shared-instance RPC
//! (`reference/Reticulum/RNS/Utilities/rnprobe.py:166-178`). lnsd answered
//! None to all three verbs, so the decoration never appeared.
//!
//! ## Topology
//!
//! ```text
//!   rnprobe ── shared instance ── daemon A ── RNode channel ──┐
//!                                                             fake air
//!                    node B (respond_to_probes) ── RNode channel ──┘
//! ```
//!
//! The fake air is two scripted modems that answer the detect probe and the
//! radio configuration and carry each `CMD_DATA` frame across, preceded by the
//! `CMD_STAT_RSSI` / `CMD_STAT_SNR` pair a real RNode sends ahead of every
//! received frame. The readings are fixed, so the decoration is exact.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use leviculum_core::framing::kiss::{self, KissDeframeResult, KissDeframer};
use leviculum_core::rnode;
use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::config::Config;
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::interfaces::{RNodeChannelFactory, RNodeChannelHalves, RNodeChannelOpenFuture};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::common::{cleanup_config_dir, init_tracing};

static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

const RNPROBE_PY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../reference/Reticulum/RNS/Utilities/rnprobe.py"
);
const VENDOR_RNS_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../reference/Reticulum");

const FREQ_HZ: u64 = 869_463_000;
const BW_HZ: u32 = 125_000;
const SF: u8 = 7;
const CR: u8 = 5;
const TX_POWER_DBM: i8 = 2;

/// `CMD_STAT_RSSI` carries dBm + 157 (`rnode::decode_rssi`): -61 dBm.
const RAW_RSSI: u8 = 96;
/// `CMD_STAT_SNR` carries a signed quarter-dB count: 5.5 dB.
const RAW_SNR: u8 = 22;

/// Hands an RNode channel interface its end of an in-memory duplex.
struct DuplexFactory(Mutex<Option<RNodeChannelHalves>>);

impl RNodeChannelFactory for DuplexFactory {
    fn open(&self) -> RNodeChannelOpenFuture {
        let taken = self.0.lock().expect("factory lock").take();
        Box::pin(async move { taken.ok_or_else(|| "channel already opened".into()) })
    }
}

fn channel() -> (Arc<DuplexFactory>, tokio::io::DuplexStream) {
    let (port, peer) = tokio::io::duplex(64 * 1024);
    let (read_half, write_half) = tokio::io::split(port);
    let factory = DuplexFactory(Mutex::new(Some((
        Box::new(read_half) as Box<dyn AsyncRead + Send + Unpin>,
        Box::new(write_half) as Box<dyn AsyncWrite + Send + Unpin>,
    ))));
    (Arc::new(factory), peer)
}

fn push(reply: &mut Vec<u8>, cmd: u8, payload: &[u8]) {
    let mut one = Vec::new();
    kiss::frame(cmd, payload, &mut one);
    reply.extend_from_slice(&one);
}

/// One modem of the fake air: answers its host's detect and configuration,
/// sends every data frame its host hands it to the other modem, and delivers
/// every frame from the other modem behind an RSSI/SNR report.
async fn air_modem(
    mut peer: tokio::io::DuplexStream,
    to_far: mpsc::UnboundedSender<Vec<u8>>,
    mut from_far: mpsc::UnboundedReceiver<Vec<u8>>,
) {
    let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
    let mut buf = [0u8; 1024];
    loop {
        let mut reply: Vec<u8> = Vec::new();
        tokio::select! {
            read = peer.read(&mut buf) => {
                let n = match read {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                for f in deframer.process(&buf[..n]) {
                    let KissDeframeResult::Frame { command, payload } = f else {
                        continue;
                    };
                    match command {
                        rnode::CMD_DETECT => {
                            push(&mut reply, rnode::CMD_DETECT, &[rnode::DETECT_RESP]);
                            push(
                                &mut reply,
                                rnode::CMD_FW_VERSION,
                                &[rnode::REQUIRED_FW_MAJ, rnode::REQUIRED_FW_MIN],
                            );
                            push(&mut reply, rnode::CMD_PLATFORM, &[rnode::PLATFORM_ESP32]);
                            push(&mut reply, rnode::CMD_MCU, &[0x00]);
                        }
                        rnode::CMD_RADIO_STATE
                        | rnode::CMD_FREQUENCY
                        | rnode::CMD_BANDWIDTH
                        | rnode::CMD_TXPOWER
                        | rnode::CMD_SF
                        | rnode::CMD_CR => push(&mut reply, command, &payload),
                        rnode::CMD_DATA => {
                            let _ = to_far.send(payload.to_vec());
                        }
                        _ => {}
                    }
                }
            }
            frame = from_far.recv() => {
                let Some(frame) = frame else { return };
                push(&mut reply, rnode::CMD_STAT_RSSI, &[RAW_RSSI]);
                push(&mut reply, rnode::CMD_STAT_SNR, &[RAW_SNR]);
                push(&mut reply, rnode::CMD_DATA, &frame);
            }
        }
        if !reply.is_empty() && peer.write_all(&reply).await.is_err() {
            return;
        }
    }
}

/// The Python client's config: the daemon's instance and its transport
/// identity, so the client derives the daemon's RPC key.
fn python_config_dir(instance_name: &str, identity_bytes: &[u8; 64]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("radio_stats_{instance_name}"));
    let _ = std::fs::remove_dir_all(&dir);
    let storage = dir.join("storage");
    std::fs::create_dir_all(&storage).expect("create storage dir");
    std::fs::write(storage.join("transport_identity"), identity_bytes)
        .expect("write transport_identity");
    std::fs::write(
        dir.join("config"),
        format!(
            "[reticulum]\n  enable_transport = no\n  share_instance = Yes\n  \
             instance_name = {instance_name}\n\n[logging]\n  loglevel = 4\n\n[interfaces]\n"
        ),
    )
    .expect("write config");
    dir
}

#[tokio::test]
async fn test_rnprobe_prints_the_radio_readings_of_its_proof() {
    init_tracing();

    let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let instance_name = format!("radiostats_{}_{}", std::process::id(), test_id);

    let (a_to_b_tx, a_to_b_rx) = mpsc::unbounded_channel();
    let (b_to_a_tx, b_to_a_rx) = mpsc::unbounded_channel();
    let (factory_a, peer_a) = channel();
    let (factory_b, peer_b) = channel();
    let modem_a = tokio::spawn(air_modem(peer_a, a_to_b_tx, b_to_a_rx));
    let modem_b = tokio::spawn(air_modem(peer_b, b_to_a_tx, a_to_b_rx));

    let identity_a = Identity::generate(&mut rand_core::OsRng);
    let identity_a_bytes = identity_a
        .private_key_bytes()
        .expect("generated identity has private keys");
    let storage_a = crate::common::temp_storage("radio_stats", "daemon");
    let mut daemon = ReticulumNodeBuilder::new()
        .identity(identity_a)
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance_name.clone())
        .add_rnode_channel_interface(factory_a, FREQ_HZ, BW_HZ, SF, CR, TX_POWER_DBM)
        .storage_path(storage_a.path().to_path_buf())
        .build()
        .await
        .expect("build daemon");
    daemon.start().await.expect("start daemon");

    // Node B answers probes on the destination rnsd would: its transport
    // identity's `rnstransport.probe`.
    let identity_b = Identity::generate(&mut rand_core::OsRng);
    let probe_hash = *Destination::new(
        Some(identity_b.clone()),
        Direction::In,
        DestinationType::Single,
        "rnstransport",
        &["probe"],
    )
    .expect("probe destination")
    .hash();
    let mut config_b = Config::default();
    config_b.reticulum.respond_to_probes = true;
    let storage_b = crate::common::temp_storage("radio_stats", "responder");
    let mut responder = ReticulumNodeBuilder::new()
        .identity(identity_b)
        .config(config_b)
        .enable_transport(false)
        .add_rnode_channel_interface(factory_b, FREQ_HZ, BW_HZ, SF, CR, TX_POWER_DBM)
        .storage_path(storage_b.path().to_path_buf())
        .build()
        .await
        .expect("build responder");
    responder.start().await.expect("start responder");

    let config_dir = python_config_dir(&instance_name, &identity_a_bytes);
    let probe_hex = hex::encode(probe_hash.as_bytes());

    // The daemon's RPC and both radios come up asynchronously; rnprobe's own
    // path request covers the path, so only a daemon that is not answering
    // yet is retried.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let (stdout, stderr) = loop {
        let output = tokio::process::Command::new("python3")
            .arg(RNPROBE_PY)
            .arg("--config")
            .arg(&config_dir)
            .args(["-n", "1", "-t", "15"])
            .args(["rnstransport.probe", &probe_hex])
            .env("PYTHONPATH", VENDOR_RNS_ROOT)
            .output()
            .await
            .expect("run rnprobe");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if stdout.contains("Valid reply") || tokio::time::Instant::now() >= deadline {
            break (stdout, stderr);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    eprintln!("=== rnprobe STDOUT ===\n{stdout}\n=== rnprobe STDERR ===\n{stderr}");

    assert!(
        stdout.contains("Valid reply"),
        "rnprobe got no reply over the fake air:\n{stdout}\n{stderr}"
    );
    // -61 dBm, 5.5 dB, and the reference's quality at SF7:
    // (5.5 - -9) / (6 - -9) * 100 = 96.7 % (RNodeInterface.py:881-890).
    for want in ["[RSSI -61 dBm]", "[SNR 5.5 dB]", "[Link Quality 96.7%]"] {
        assert!(
            stdout.contains(want),
            "rnprobe printed no {want} for a proof that came over the radio:\n{stdout}"
        );
    }

    cleanup_config_dir(&config_dir);
    modem_a.abort();
    modem_b.abort();
}

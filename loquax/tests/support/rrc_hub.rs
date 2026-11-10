//! A fake Reticulum Relay Chat hub (1379, #232), written from the RRC
//! specification, with the envelopes periculum 1378 recorded from the
//! reference hub (rrcd 0.3.2) as its answers: WELCOME and its PING are
//! the recorded bytes, JOINED, PARTED and the echo of a line are what the
//! spec and the recording say rrcd sends. The pinned rrcd itself lives in
//! periculum's netns cells and needs Python, which these harnesses do not
//! start. Shared by `#[path]`: `loquax/tests/rooms_hub.rs` and
//! `loquax-android/tests/rooms_hub.rs` (1384).

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use leviculum_rrc::{Envelope, Kind, Value};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::{
    Destination, DestinationType, Direction, NodeEvent, ProofStrategy, ReticulumNode,
};

/// A recorded envelope by its name in the vectors.
pub fn vector(name: &str) -> Vec<u8> {
    let file: serde_json::Value = serde_json::from_str(include_str!(
        "../../../leviculum-rrc/tests/vectors/envelopes.json"
    ))
    .expect("the vectors");
    let v = file["vectors"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|v| v["name"] == name)
        .unwrap_or_else(|| panic!("no vector {name}"));
    hex::decode(v["cbor"].as_str().expect("cbor")).expect("hex")
}

/// The fake hub's answer to `/list` (1652), in rrcd's words: a room with
/// a topic and one whose name has a space, as another implementation's.
pub const LIST_ANSWER: &str = "Registered public rooms:\n  general chat\n  lab - the lab";

/// What the fake hub saw.
#[derive(Default)]
pub struct Seen {
    /// The identity each link identified with.
    pub identified: Vec<[u8; 16]>,
    /// Every envelope a client sent, decoded.
    pub envelopes: Vec<Envelope>,
}

/// The fake hub: one destination, the spec's answers.
pub struct Hub {
    pub dest: [u8; 16],
    pub seen: Arc<Mutex<Seen>>,
    pub task: tokio::task::JoinHandle<()>,
    _storage: tempfile::TempDir,
}

impl Hub {
    /// A hub node dialling the TCP server at `daemon_tcp`, its
    /// destination announced.
    pub async fn start(daemon_tcp: SocketAddr) -> Hub {
        let storage = tempfile::tempdir().expect("hub storage");
        let mut node = ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_tcp_client(daemon_tcp)
            .storage_path(storage.path().to_path_buf())
            .build_sync()
            .expect("build the hub");
        let events = node.take_event_receiver().expect("the hub's events");
        node.start().await.expect("start the hub");
        let mut destination = Destination::new(
            Some(leviculum_std::generate_identity()),
            Direction::In,
            DestinationType::Single,
            leviculum_rrc::HUB_APP,
            &[leviculum_rrc::HUB_ASPECT],
        )
        .expect("the hub destination");
        destination.set_accepts_links(true);
        // rrcd sets no proof strategy: PROVE_NONE (the study, 2026-11-03).
        destination.set_proof_strategy(ProofStrategy::None);
        let dest = *destination.hash();
        node.register_destination(destination);
        // The announce's app data as rrcd's: {"proto": "rrc", "v": 1, "hub": name}.
        let app_data = Value::Map(vec![
            (Value::Text("proto".into()), Value::Text("rrc".into())),
            (Value::Text("v".into()), Value::Uint(1)),
            (Value::Text("hub".into()), Value::Text("fake".into())),
        ])
        .encode();
        node.announce_destination(&dest, Some(&app_data))
            .await
            .expect("announce the hub");
        let seen = Arc::new(Mutex::new(Seen::default()));
        let task = tokio::spawn(serve(node, events, Arc::clone(&seen)));
        Hub {
            dest: *dest.as_bytes(),
            seen,
            task,
            _storage: storage,
        }
    }
}

/// The hub's side, as the spec and the recording say: WELCOME to a
/// HELLO, then a PING; JOINED to a JOIN (without a nick to the joiner);
/// a line back to its sender unchanged but for key 4, the link's
/// identity; PARTED to a PART.
async fn serve(
    node: ReticulumNode,
    mut events: leviculum_std::EventReceiver,
    seen: Arc<Mutex<Seen>>,
) {
    let mut who = std::collections::HashMap::new();
    while let Some(event) = events.recv().await {
        match event {
            NodeEvent::LinkIdentified {
                link_id,
                identity_hash,
            } => {
                who.insert(link_id, identity_hash);
                seen.lock().expect("seen").identified.push(identity_hash);
            }
            NodeEvent::LinkDataReceived { link_id, data } => {
                let Ok(mut envelope) = Envelope::decode(&data) else {
                    continue;
                };
                seen.lock().expect("seen").envelopes.push(envelope.clone());
                let mut answers = Vec::new();
                match envelope.kind {
                    Kind::Hello => {
                        answers.push(vector("welcome_down"));
                        answers.push(vector("ping_down"));
                    }
                    Kind::Join | Kind::Part => {
                        let name = if envelope.kind == Kind::Join {
                            "joined_down"
                        } else {
                            "parted_down"
                        };
                        let mut answer = Envelope::decode(&vector(name)).expect("a vector");
                        answer.room.clone_from(&envelope.room);
                        answers.push(answer.encode());
                    }
                    // A room-less command (1652): `/list` answered as
                    // rrcd's `commands.py` does, one NOTICE from the hub.
                    Kind::Msg if envelope.room.is_none() && envelope.text() == Some("/list") => {
                        let mut answer = Envelope::new(Kind::Notice, [9; 8], envelope.ts);
                        answer.body = Some(Value::Text(LIST_ANSWER.into()));
                        answers.push(answer.encode());
                    }
                    Kind::Msg | Kind::Notice | Kind::Action => {
                        envelope.src = who.get(&link_id).map(|hash: &[u8; 16]| hash.to_vec());
                        answers.push(envelope.encode());
                    }
                    _ => {}
                }
                for answer in answers {
                    let _ = node.send_link_packet(&link_id, &answer).await;
                }
            }
            _ => {}
        }
    }
}

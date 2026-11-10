//! The `/dfu` sender against a node that serves the verb (OTA stage 1,
//! Codeberg #235).
//!
//! `lnmgmt dfu` runs [`leviculum_std::remote_dfu::send_dfu`]. Its counterpart
//! on a board is the core's `/dfu` decision and typed reply
//! (`leviculum_core::node::remote_dfu`), which the firmware runs in its engine
//! pass. Here a host node stands in for the board: built with `remote_dfu`,
//! remote management on and an allow-list, and a core processor that runs the
//! same two core functions the firmware does, `dfu_command` and
//! `send_dfu_reply`. What it does not do is reset, which is the bench's.
//!
//! Topology: two in-process nodes on 127.0.0.1 over TCP loopback, the board
//! the server, the sender a client, as in
//! `rnsd_interop/remote_management_server_tests.rs` for `/status`.
//!
//! ```sh
//! cargo test -p leviculum-std --test remote_dfu_sender
//! ```

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand_core::OsRng;

use leviculum_core::node::remote_dfu::{dfu_command, send_dfu_reply, DfuCommand};
use leviculum_core::node::NodeEvent;
use leviculum_core::transport::TickOutput;
use leviculum_core::Identity;
use leviculum_std::config::Config;
use leviculum_std::driver::{CoreProcessor, ReticulumNode, ReticulumNodeBuilder, StdNodeCore};
use leviculum_std::remote_dfu::{send_dfu, DfuOutcome, DfuVerdict};
use leviculum_std::remote_status::ManagementError;

/// The board's engine pass for `/dfu`, minus the exit: decide, record, reply.
struct BoardDfu {
    decided: Arc<Mutex<Vec<DfuCommand>>>,
}

impl CoreProcessor for BoardDfu {
    fn on_event(&mut self, core: &mut StdNodeCore, event: &NodeEvent) -> TickOutput {
        let Some(command) = dfu_command(core, event) else {
            return TickOutput::empty();
        };
        if let Ok(mut decided) = self.decided.lock() {
            decided.push(command);
        }
        match send_dfu_reply(core, &command) {
            Ok(output) => output,
            Err(e) => {
                eprintln!("board stand-in: send_dfu_reply failed: {e:?}");
                TickOutput::empty()
            }
        }
    }
}

struct Rig {
    board: ReticulumNode,
    client: ReticulumNode,
    events: leviculum_std::driver::EventReceiver,
    board_identity_hash: [u8; 16],
    decided: Arc<Mutex<Vec<DfuCommand>>>,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

/// Start a board stand-in with `allowed` on its list (serving `/dfu` iff
/// `dfu`), and a client connected to it.
async fn rig(allowed: &[[u8; 16]], dfu: bool) -> Rig {
    let mut config = Config::default();
    config.reticulum.enable_transport = true;
    config.reticulum.remote_management_enabled = true;
    config.reticulum.remote_management_allowed =
        allowed.iter().map(|hash| hex_lower(hash)).collect();

    let decided = Arc::new(Mutex::new(Vec::new()));
    let board_dir = tempfile::tempdir().expect("board storage");
    let mut board = ReticulumNodeBuilder::new()
        .config(config)
        .remote_dfu(dfu)
        .core_processor(BoardDfu {
            decided: Arc::clone(&decided),
        })
        .without_events()
        .add_tcp_server("127.0.0.1:0".parse().expect("loopback addr"))
        .storage_path(board_dir.path().to_path_buf())
        .build()
        .await
        .expect("build board stand-in");
    board.start().await.expect("start board stand-in");
    let addr = *board
        .tcp_listen_addrs()
        .first()
        .expect("board reports its bound TCP listener");
    let board_identity_hash = board.identity_hash();

    let client_dir = tempfile::tempdir().expect("client storage");
    let mut client = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(addr)
        .storage_path(client_dir.path().to_path_buf())
        .build()
        .await
        .expect("build sender node");
    let events = client.take_event_receiver().expect("event receiver");
    client.start().await.expect("start sender node");

    Rig {
        board,
        client,
        events,
        board_identity_hash,
        decided,
        _dirs: (board_dir, client_dir),
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Rig {
    async fn send(&mut self, identity: &Identity, timeout: Duration) -> DfuOutcome {
        send_dfu(
            &self.client,
            &mut self.events,
            &self.board_identity_hash,
            identity,
            timeout,
            true,
        )
        .await
    }

    fn decided(&self) -> Vec<DfuCommand> {
        self.decided.lock().map(|d| d.clone()).unwrap_or_default()
    }

    async fn stop(mut self) {
        self.client.stop().await.ok();
        self.board.stop().await.ok();
    }
}

/// A listed identity is accepted, and the sender reads the accepted reply.
#[tokio::test]
async fn a_listed_identity_reads_accepted() {
    let operator = Identity::generate(&mut OsRng);
    let mut rig = rig(&[*operator.hash()], true).await;

    let outcome = rig.send(&operator, Duration::from_secs(10)).await;
    assert_eq!(outcome, DfuOutcome::Verdict(DfuVerdict::Accepted));

    let decided = rig.decided();
    assert_eq!(decided.len(), 1, "one request, one decision: {decided:?}");
    assert_eq!(decided[0].verdict, DfuVerdict::Accepted);
    assert_eq!(decided[0].from, Some(*operator.hash()));
    rig.stop().await;
}

/// An identity the list does not hold is refused, and the sender reads the
/// typed refusal rather than a timeout.
#[tokio::test]
async fn an_unlisted_identity_reads_not_listed() {
    let operator = Identity::generate(&mut OsRng);
    let stranger = Identity::generate(&mut OsRng);
    let mut rig = rig(&[*operator.hash()], true).await;

    let outcome = rig.send(&stranger, Duration::from_secs(10)).await;
    assert_eq!(outcome, DfuOutcome::Verdict(DfuVerdict::NotListed));

    let decided = rig.decided();
    assert_eq!(decided.len(), 1, "one request, one decision: {decided:?}");
    assert_eq!(decided[0].verdict, DfuVerdict::NotListed);
    assert_eq!(decided[0].from, Some(*stranger.hash()));
    rig.stop().await;
}

/// Negative control on the same scenario: a node that does not serve `/dfu`
/// (as `lnsd` does not) drops the request, and the sender reports a timeout,
/// not a verdict. Without it the two tests above would not tell the board's
/// reply from something answering on its own.
#[tokio::test]
async fn a_node_without_the_verb_times_out() {
    let operator = Identity::generate(&mut OsRng);
    let mut rig = rig(&[*operator.hash()], false).await;

    let outcome = rig.send(&operator, Duration::from_secs(2)).await;
    assert!(
        matches!(
            outcome,
            DfuOutcome::TimedOut(
                ManagementError::RequestTimedOut | ManagementError::DeadlineExpired
            )
        ),
        "expected a timeout, got {outcome:?}"
    );
    assert!(rig.decided().is_empty(), "nothing was decided");
    rig.stop().await;
}

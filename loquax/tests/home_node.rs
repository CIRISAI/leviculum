//! The home node against the real Python LXMF stack (#232, slices 1, 2 and
//! 4 of `docs/src/concepts/loquax-home-node.md`): `loquax --daemon`,
//! `loquax home admit|join|revoke|devices`, a client's `loquax fetch` and
//! `loquax send` and two scripted `loquax tui` clients, each as the shipped
//! binary, around a stock Python sender, recipient and propagation node.
//! Needs the `tui` feature, which carries the home node's store.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

#[path = "support/python.rs"]
mod python;

use python::{Mesh, PythonPeer, Sent, LNMSG};

/// One `loquax` run with no daemon flag appended: `home admit`, `home
/// revoke`, `address`, the commands that only read and write the state
/// directory.
fn local(home: &std::path::Path, args: &[&str]) -> Sent {
    let output = Command::new(LNMSG)
        .args(args)
        .env("LNMSG_HOME", home)
        .output()
        .expect("run loquax");
    Sent {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        log: String::new(),
    }
}

/// `loquax mailbox run` on `home`, killed with its handle. The test of a
/// second registrar below spawns it as `loquax --daemon`, the old spelling.
struct HomeNode {
    process: std::process::Child,
    stderr: PathBuf,
}

impl HomeNode {
    /// Wait for `needle` in the daemon's log, for at most `secs`.
    async fn wait_for(&self, needle: &str, secs: u64) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
        while !self.log().contains(needle) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "no {needle:?} in the home node's log within {secs} s:\n{}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    async fn start(home: &std::path::Path, instance: &str) -> Self {
        Self::start_with(home, instance, &[]).await
    }

    /// [`HomeNode::start`] with more arguments, `--pn` for one.
    async fn start_with(home: &std::path::Path, instance: &str, more: &[&str]) -> Self {
        let stderr = home.join("daemon.stderr");
        let file = std::fs::File::create(&stderr).expect("the daemon's stderr file");
        let mut command = Command::new(LNMSG);
        command
            .args([
                "mailbox",
                "run",
                "--instance",
                instance,
                "--sync-interval",
                "off",
            ])
            .args(more)
            .env("LNMSG_HOME", home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(file));
        let process = leviculum_std::process::spawn_supervised(command).expect("spawn the daemon");
        let node = Self { process, stderr };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !node.log().contains("HOME up") {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the home node never came up:\n{}",
                node.log()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        node
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }
}

impl Drop for HomeNode {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// Slice 5's pairing on the home node, without a network: `home admit
/// --server` writes the address into the code and prints the QR code
/// beside it; a bad address is refused before a token is issued;
/// `home revoke-token` withdraws an open code by the start of its hash.
#[test]
fn admit_carries_the_address_and_an_open_code_can_be_withdrawn() {
    let home = tempfile::tempdir().expect("home");
    let home = home.path();
    let admitted = local(
        home,
        &[
            "mailbox",
            "admit",
            "--name",
            "phone",
            "--server",
            "homenode.example:4242",
        ],
    );
    assert_eq!(admitted.code, Some(0), "{admitted}");
    let text = admitted
        .stdout
        .split_whitespace()
        .find(|word| word.starts_with("lqxh1"))
        .unwrap_or_else(|| panic!("no code in {admitted}"));
    let (code, server) =
        loquax::home::code::HomeCode::parse_with_server(text).expect("the code parses");
    assert_eq!(server.as_deref(), Some("homenode.example:4242"));
    // The QR code: a block of equal lines of half blocks.
    let drawn: Vec<&str> = admitted
        .stdout
        .lines()
        .filter(|line| !line.is_empty() && line.chars().all(|c| " ▀▄█".contains(c)))
        .collect();
    assert!(drawn.len() > 20, "{admitted}");
    let width = drawn[0].chars().count();
    assert!(drawn.iter().all(|line| line.chars().count() == width));
    assert_eq!(drawn.len(), width.div_ceil(2));
    // The desktop's line carries the code alone.
    assert!(
        admitted
            .stdout
            .contains(&format!("loquax mailbox join {}\n", code.encode())),
        "{admitted}"
    );

    // Negative: an address without a port, refused, and no token issued.
    let refused = local(home, &["mailbox", "admit", "--server", "homenode.example"]);
    assert_eq!(refused.code, Some(2), "{refused}");
    assert!(refused.stderr.contains("has no port"), "{refused}");

    let tokens = local(home, &["home", "tokens"]);
    assert_eq!(tokens.stdout.lines().count(), 1, "{tokens}");
    let prefix = hex::encode(&code.token[..2]);
    assert!(tokens.stdout.starts_with(&prefix), "{tokens}");

    // Negative: too short, and a prefix nothing starts with.
    let short = local(home, &["home", "revoke-token", "ab"]);
    assert_eq!(short.code, Some(2), "{short}");
    let other = if prefix == "0000" { "1111" } else { "0000" };
    let none = local(home, &["home", "revoke-token", other]);
    assert_eq!(none.code, Some(1), "{none}");

    let withdrawn = local(home, &["home", "revoke-token", &prefix]);
    assert_eq!(withdrawn.code, Some(0), "{withdrawn}");
    assert!(withdrawn.stdout.contains("withdrew") && withdrawn.stdout.contains("phone"));
    let tokens = local(home, &["home", "tokens"]);
    assert_eq!(tokens.stdout, "", "{tokens}");
}

/// 1153: `loquax mailbox` is the command group, `loquax home` the old
/// spelling of the same one, and `loquax --daemon` the old spelling of
/// `loquax mailbox run`; the help says "Loquax Mailbox".
#[test]
fn mailbox_and_home_are_one_command_group() {
    let home = tempfile::tempdir().expect("home");
    let home = home.path();
    let admitted = local(home, &["mailbox", "admit", "--name", "phone"]);
    assert_eq!(admitted.code, Some(0), "{admitted}");
    let new = local(home, &["mailbox", "tokens"]);
    let old = local(home, &["home", "tokens"]);
    assert_eq!(new.code, Some(0), "{new}");
    assert_eq!(new.stdout.lines().count(), 1, "{new}");
    assert!(new.stdout.contains("phone  expires"), "{new}");
    assert_eq!((old.code, &old.stdout), (new.code, &new.stdout), "{old}");

    let help = local(home, &["--help"]);
    assert_eq!(help.code, Some(0), "{help}");
    assert!(help.stdout.contains("Loquax Mailbox"), "{help}");
    let group = local(home, &["mailbox", "--help"]);
    assert_eq!(group.code, Some(0), "{group}");
    for command in [
        "run", "enable", "disable", "admit", "join", "devices", "revoke", "tokens",
    ] {
        assert!(
            group
                .stdout
                .lines()
                .any(|line| line.trim_start().starts_with(command)),
            "no {command} in {group}"
        );
    }
    let run = local(home, &["home", "run", "--help"]);
    assert_eq!(run.code, Some(0), "{run}");
    assert!(run.stdout.contains("--sync-interval"), "{run}");

    // Negative: the messenger's own options are not `run`'s, and an
    // unknown verb under either spelling is an argument error.
    let stray = local(home, &["mailbox", "run", "--ascii"]);
    assert_eq!(stray.code, Some(2), "{stray}");
    let unknown = local(home, &["home", "nonsense"]);
    assert_eq!(unknown.code, Some(2), "{unknown}");
}

/// The home node's slice 1 end to end (#232): a stock Python LXMF sender
/// delivers DIRECT to `loquax --daemon` and gets DELIVERED; two admitted
/// clients both pull the message, and pulling deletes nothing; a reused
/// admission code is refused; a revoked client gets nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_home_node_takes_direct_mail_and_its_admitted_clients_pull_it() {
    let peer = PythonPeer::start();
    peer.lxmf_init();
    let mut mesh = Mesh::start(peer.rns_port).await;
    let home = mesh.home.path().to_path_buf();

    // Three admissions on the home node, before it runs: the store is a
    // file, and admitting needs no daemon.
    let mut codes = Vec::new();
    for name in ["laptop", "desk", "stolen"] {
        let admitted = local(&home, &["home", "admit", "--name", name]);
        assert_eq!(admitted.code, Some(0), "{admitted}");
        let code = admitted
            .stdout
            .split_whitespace()
            .find(|word| word.starts_with("lqxh1"))
            .unwrap_or_else(|| panic!("no code in {admitted}"))
            .to_string();
        codes.push(code);
    }
    let address = local(&home, &["address"]);
    assert_eq!(address.code, Some(0), "{address}");
    let address = address.stdout.trim().to_string();

    let node = HomeNode::start(&home, &mesh.instance).await;

    let clients: Vec<tempfile::TempDir> = (0..3)
        .map(|_| tempfile::tempdir().expect("client home"))
        .collect();
    for (client, code) in clients.iter().zip(&codes) {
        let joined = mesh
            .run(
                "home",
                client.path().to_path_buf(),
                vec![
                    "join".to_string(),
                    code.clone(),
                    "--timeout".to_string(),
                    "40".to_string(),
                ],
                Vec::new(),
            )
            .await;
        assert_eq!(joined.code, Some(0), "{joined}\nhome node:\n{}", node.log());
        assert_first_hop_answered(&joined);
        assert!(joined.stdout.contains(&address), "{joined}");
        // A client's address is its home node's.
        let shown = local(client.path(), &["address"]);
        assert_eq!(shown.stdout.trim(), address, "{shown}");
    }

    // Negative: a used code admits nobody.
    let late = tempfile::tempdir().expect("late client");
    let reused = mesh
        .run(
            "home",
            late.path().to_path_buf(),
            vec![
                "join".to_string(),
                codes[0].clone(),
                "--timeout".to_string(),
                "40".to_string(),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(reused.code, Some(1), "{reused}");
    assert!(reused.stderr.contains("was used"), "{reused}");

    let revoked = local(&home, &["home", "revoke", "stolen"]);
    assert_eq!(revoked.code, Some(0), "{revoked}");
    let devices = local(&home, &["home", "devices"]);
    assert!(
        devices.stdout.contains("laptop")
            && devices.stdout.contains("desk")
            && !devices.stdout.contains("stolen"),
        "{devices}"
    );

    // A stock Python sender delivers DIRECT. It learns the home node's key
    // from the announce the daemon sent at start; until that has crossed,
    // its router says it knows no identity, and the send is asked again.
    let body = "vom Sender, direkt nach Hause";
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let message_hash = loop {
        match peer.try_rpc(
            "lxmf_send",
            serde_json::json!({
                "dest_hash": address,
                "method": "direct",
                "content": hex::encode(body),
            }),
        ) {
            Ok(result) => {
                break result
                    .get("message_hash")
                    .and_then(|v| v.as_str())
                    .expect("a message hash")
                    .to_string()
            }
            Err(error) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "Python never learned the home node: {error}\n{}",
                    node.log()
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    loop {
        let state = peer.rpc(
            "lxmf_get_outbound_status",
            serde_json::json!({ "message_hash": message_hash }),
        );
        match state.get("state").and_then(|v| v.as_str()) {
            Some("DELIVERED") => break,
            Some("FAILED") | Some("REJECTED") => {
                panic!("the sender gave up: {state}\n{}", node.log())
            }
            _ => {}
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the sender never got DELIVERED: {state}\n{}",
            node.log()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        node.log().contains("HOME archived message="),
        "{}",
        node.log()
    );

    // Both admitted clients pull it, the second after the first: fetching
    // deletes nothing on the home node.
    for client in &clients[..2] {
        let fetched = mesh
            .run(
                "fetch",
                client.path().to_path_buf(),
                vec!["--timeout".to_string(), "40".to_string()],
                Vec::new(),
            )
            .await;
        assert_eq!(
            fetched.code,
            Some(0),
            "{fetched}\nhome node:\n{}",
            node.log()
        );
        assert!(
            fetched
                .stdout
                .contains(&format!("message {message_hash} from")),
            "{fetched}"
        );
        assert!(fetched.stdout.contains(body), "{fetched}");
    }
    // The same client again: its cursor is past the message, so nothing new.
    let again = mesh
        .run(
            "fetch",
            clients[0].path().to_path_buf(),
            vec!["--timeout".to_string(), "40".to_string()],
            Vec::new(),
        )
        .await;
    assert_eq!(again.code, Some(0), "{again}");
    assert_eq!(again.stdout, "", "{again}");

    // The revoked client gets nothing, and is told why.
    let refused = mesh
        .run(
            "fetch",
            clients[2].path().to_path_buf(),
            vec!["--timeout".to_string(), "40".to_string()],
            Vec::new(),
        )
        .await;
    assert_eq!(refused.code, Some(1), "{refused}");
    assert_eq!(refused.stdout, "", "{refused}");
    assert!(refused.stderr.contains("not admitted"), "{refused}");

    drop(node);
    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// The home node's slice 2 end to end (#232): a client sends through its
/// home node to a stock Python LXMF recipient that is offline. The client is
/// done once the home node answered and is not running afterwards. The
/// home node's direct leg fails (the router's stock attempts), it has no
/// propagation node to hand a copy to, and it keeps the message. The
/// recipient comes up later and announces; the home node delivers DIRECT
/// and gets the proof; the client's next fetch shows DELIVERED from the
/// archive. A second daemon on the same home is refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_sends_through_its_home_node_to_a_recipient_that_comes_up_later() {
    let peer = PythonPeer::start();
    let mut mesh = Mesh::start(peer.rns_port).await;
    let home = mesh.home.path().to_path_buf();
    // Time compression: the slow cadence of direct retries down from an hour
    // to five seconds, so the announce is not the only way back.
    std::fs::write(home.join("config"), "outbox_retry_secs = 5\n").expect("config");
    let admitted = local(&home, &["home", "admit", "--name", "laptop"]);
    assert_eq!(admitted.code, Some(0), "{admitted}");
    let code = admitted
        .stdout
        .split_whitespace()
        .find(|word| word.starts_with("lqxh1"))
        .unwrap_or_else(|| panic!("no code in {admitted}"))
        .to_string();
    let tokens = local(&home, &["home", "tokens"]);
    assert_eq!(tokens.code, Some(0), "{tokens}");
    assert!(tokens.stdout.contains("laptop  expires"), "{tokens}");

    let node = HomeNode::start(&home, &mesh.instance).await;

    // Negative: one registrar of the delivery destination per home.
    let second = Command::new(LNMSG)
        .args([
            "--daemon",
            "--instance",
            &mesh.instance,
            "--sync-interval",
            "off",
        ])
        .env("LNMSG_HOME", &home)
        .output()
        .expect("run a second daemon");
    assert_eq!(second.status.code(), Some(1));
    let refusal = String::from_utf8_lossy(&second.stderr);
    assert!(
        refusal.contains("Loquax Mailbox (pid") && refusal.contains("already runs"),
        "{refusal}"
    );
    // Negative: nor do a local send and fetch, which would register the
    // same destination beside it.
    let pid = format!("Loquax Mailbox (pid {})", node.process.id());
    let nobody = "00112233445566778899aabbccddeeff".to_string();
    for (command, args, stdin) in [
        ("send", vec![nobody], b"x".to_vec()),
        ("fetch", Vec::new(), Vec::new()),
    ] {
        let refused = mesh.run(command, home.clone(), args, stdin).await;
        assert_eq!(refused.code, Some(1), "{command}: {refused}");
        assert!(refused.stderr.contains(&pid), "{command}: {refused}");
    }

    let client = tempfile::tempdir().expect("client home");
    let joined = mesh
        .run(
            "home",
            client.path().to_path_buf(),
            vec![
                "join".to_string(),
                code,
                "--timeout".to_string(),
                "40".to_string(),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(joined.code, Some(0), "{joined}\nhome node:\n{}", node.log());
    let tokens = local(&home, &["home", "tokens"]);
    assert_eq!(tokens.stdout, "", "a used code is no longer open: {tokens}");

    // The recipient's identity exists before its router does, so the client
    // can write to an address nobody has announced yet.
    let recipient = leviculum_std::generate_identity();
    let recipient_address =
        hex::encode(loquax::engine::delivery_address(&recipient).expect("address"));
    let body = "durch das Zuhause, spaeter zugestellt";
    let sent = mesh
        .run(
            "send",
            client.path().to_path_buf(),
            vec![
                recipient_address.clone(),
                "--timeout".to_string(),
                "40".to_string(),
            ],
            body.as_bytes().to_vec(),
        )
        .await;
    assert_eq!(sent.code, Some(0), "{sent}\nhome node:\n{}", node.log());
    let message_id = sent.enqueued_id();
    node.wait_for("HOME send accepted", 5).await;
    // The send pulled right after: its own entry is there already.
    assert!(
        sent.stdout
            .contains(&format!("message {message_id} accepted")),
        "{sent}"
    );

    // The client is gone. The home node's direct leg runs out of the
    // router's attempts against a recipient that is not there, and with no
    // propagation node configured the message stays in its outbox.
    node.wait_for(
        &format!("HOME outbox direct failed message={message_id}"),
        120,
    )
    .await;
    node.wait_for(
        &format!("HOME outbox no propagation node message={message_id}"),
        5,
    )
    .await;
    assert!(peer.received().is_empty(), "delivered to nobody yet");

    // The recipient comes up and announces.
    let started = peer.rpc(
        "lxmf_init",
        serde_json::json!({
            "identity_prv": hex::encode(recipient.private_key_bytes().expect("private key")),
        }),
    );
    assert_eq!(
        started.get("delivery_hash").and_then(|v| v.as_str()),
        Some(recipient_address.as_str()),
        "Python derives the same address from the same identity"
    );
    peer.lxmf_announce();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let arrived =
        loop {
            if let Some(message) = peer.received().into_iter().find(|m| {
                m.get("content").and_then(|v| v.as_str()) == Some(hex::encode(body).as_str())
            }) {
                break message;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the recipient never got it:\n{}",
                node.log()
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
    assert_eq!(arrived["method"], "direct", "{arrived}");
    assert_eq!(arrived["message_hash"], message_id.as_str(), "{arrived}");
    node.wait_for(&format!("HOME outbox delivered message={message_id}"), 30)
        .await;
    assert_eq!(
        peer.received().len(),
        1,
        "one message, however often it was tried"
    );

    // The client, started again, sees what happened from the archive.
    let fetched = mesh
        .run(
            "fetch",
            client.path().to_path_buf(),
            vec!["--timeout".to_string(), "40".to_string()],
            Vec::new(),
        )
        .await;
    assert_eq!(
        fetched.code,
        Some(0),
        "{fetched}\nhome node:\n{}",
        node.log()
    );
    assert!(
        !fetched.stdout.contains("accepted"),
        "the send's own pull took that entry: {fetched}"
    );
    assert!(
        fetched
            .stdout
            .contains(&format!("message {message_id} delivered")),
        "{fetched}"
    );
    // Its cursor moved past it: nothing again.
    let again = mesh
        .run(
            "fetch",
            client.path().to_path_buf(),
            vec!["--timeout".to_string(), "40".to_string()],
            Vec::new(),
        )
        .await;
    assert_eq!(again.code, Some(0), "{again}");
    assert_eq!(again.stdout, "", "{again}");

    drop(node);
    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// `loquax home admit --name <name>` on the home node: the code it prints.
fn admit(home: &std::path::Path, name: &str) -> String {
    let admitted = local(home, &["home", "admit", "--name", name]);
    assert_eq!(admitted.code, Some(0), "{admitted}");
    admitted
        .stdout
        .split_whitespace()
        .find(|word| word.starts_with("lqxh1"))
        .unwrap_or_else(|| panic!("no code in {admitted}"))
        .to_string()
}

/// A fresh client home, joined to the home node with `code`.
async fn joined(mesh: &Mesh, node: &HomeNode, code: &str) -> tempfile::TempDir {
    let client = tempfile::tempdir().expect("client home");
    let joined = mesh
        .run(
            "home",
            client.path().to_path_buf(),
            vec![
                "join".to_string(),
                code.to_string(),
                "--timeout".to_string(),
                "40".to_string(),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(joined.code, Some(0), "{joined}\nhome node:\n{}", node.log());
    assert_first_hop_answered(&joined);
    client
}

/// 1119: a client asks the daemon's RPC with the daemon's key, so the
/// first hop's timeout is the daemon's answer, not the local fallback that
/// followed "authentication failed" on every join and fetch.
fn assert_first_hop_answered(run: &Sent) {
    assert!(
        !run.stderr.contains("authentication failed") && !run.log.contains("authentication failed"),
        "{run}"
    );
}

/// A client's `loquax fetch`, which must succeed.
async fn fetch(mesh: &Mesh, node: &HomeNode, client: &std::path::Path) -> Sent {
    let fetched = mesh
        .run(
            "fetch",
            client.to_path_buf(),
            vec!["--timeout".to_string(), "40".to_string()],
            Vec::new(),
        )
        .await;
    assert_eq!(
        fetched.code,
        Some(0),
        "{fetched}\nhome node:\n{}",
        node.log()
    );
    assert_first_hop_answered(&fetched);
    fetched
}

/// Wait until `peer` received a message with `body`: every copy it holds.
async fn received_by(peer: &PythonPeer, body: &str, secs: u64, node: &HomeNode) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    let content = hex::encode(body);
    while !peer
        .received()
        .iter()
        .any(|m| m.get("content").and_then(|v| v.as_str()) == Some(content.as_str()))
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the recipient never got {body:?}:\n{}",
            node.log()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The phone's way to its home node (1113): a leaf with its own node, one
/// TCP client interface to the home node's daemon and no shared instance
/// (`loquax::engine::attach_leaf`), joins with an admission code, pulls a
/// message a loquax sender delivered to the home node, and marks it read.
/// No Python: the sender is the `loquax` CLI on the daemon's instance.
/// Negative: an address without a port is refused before any node starts,
/// and a leaf whose server is not there finds no path.
#[tokio::test(flavor = "multi_thread")]
async fn a_leaf_with_its_own_tcp_interface_joins_pulls_and_marks() {
    use loquax::engine::{attach_leaf, LeafConfig, Role};
    use loquax::outbox::OutboxEvent;

    let mut mesh = Mesh::start_serving(&[]).await;
    let port = mesh.tcp_port.expect("the daemon's TCP server");
    let home = mesh.home.path().to_path_buf();
    let code = admit(&home, "phone");
    let code = loquax::home::code::HomeCode::parse(&code).expect("the code parses");
    let address = loquax::home::lxmf_address(&code.public_key).expect("the home address");
    let node = HomeNode::start(&home, &mesh.instance).await;

    // A loquax messenger on the daemon's instance sends to the home node.
    let sender = tempfile::tempdir().expect("sender home");
    let body = "an das Telefon";
    let sent = mesh
        .run(
            "send",
            sender.path().to_path_buf(),
            vec![
                loquax::address::to_hex(&address),
                "--timeout".to_string(),
                "40".to_string(),
            ],
            body.as_bytes().to_vec(),
        )
        .await;
    assert_eq!(sent.code, Some(0), "{sent}\nhome node:\n{}", node.log());
    node.wait_for("HOME archived message=", 10).await;

    let leaf = |server: String, storage: std::path::PathBuf| LeafConfig {
        server,
        storage_dir: storage,
        identity: leviculum_core::Identity::generate(&mut rand_core::OsRng),
        display_name: Vec::new(),
        notices: loquax::effects::Terminal::new(std::env::temp_dir()),
        tcp: Default::default(),
    };
    let phone = tempfile::tempdir().expect("phone home");
    let storage = phone.path().join("storage");
    std::fs::create_dir_all(&storage).expect("storage dir");

    // Negative: no port.
    let refused = attach_leaf(
        leaf("127.0.0.1".to_string(), storage.clone()),
        Role::Client { address },
    )
    .await
    .err()
    .expect("an address without a port is refused");
    assert!(refused.to_string().contains("has no port"), "{refused}");

    let config = leaf(format!("127.0.0.1:{port}"), storage);
    let device = config.identity.clone();
    let mut attached = attach_leaf(config, Role::Client { address })
        .await
        .expect("the leaf's node comes up");
    let link = attached.node();
    loquax::home::client::join(
        &link,
        &code.public_key,
        &device,
        &code.token,
        "phone",
        Duration::from_secs(40),
    )
    .await
    .unwrap_or_else(|error| panic!("join: {error}\nhome node:\n{}", node.log()));
    loquax::home::client::save_client(phone.path(), &code.public_key).expect("client file");

    let pull = loquax::home::client::pull_into(
        &mut attached.outbox,
        &link,
        phone.path(),
        &code.public_key,
        &device,
        Duration::from_secs(40),
    )
    .await
    .unwrap_or_else(|error| panic!("pull: {error}\nhome node:\n{}", node.log()));
    let ids: Vec<[u8; 32]> = pull
        .received
        .iter()
        .filter_map(|event| match event {
            OutboxEvent::Received {
                message_id,
                body: b,
                ..
            } if b == body.as_bytes() => Some(*message_id),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 1, "{pull:?}");
    assert!(
        pull.cursor.is_some(),
        "the engine took the entries: {pull:?}"
    );

    loquax::home::client::mark(
        &link,
        &code.public_key,
        &device,
        &loquax::home::wire::Mark {
            flag: loquax::home::wire::MarkFlag::Read,
            ids,
        },
        Duration::from_secs(40),
    )
    .await
    .unwrap_or_else(|error| panic!("mark: {error}\nhome node:\n{}", node.log()));
    drop(link);
    attached.stop().await;

    // Negative: a leaf pointed at a port nobody serves finds no path.
    let lonely = tempfile::tempdir().expect("lonely home");
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free port")
        .port();
    let config = leaf(format!("127.0.0.1:{closed}"), lonely.path().to_path_buf());
    let device = config.identity.clone();
    let attached = attach_leaf(config, Role::Client { address })
        .await
        .expect("a leaf starts without its server");
    let link = attached.node();
    let joined = loquax::home::client::join(
        &link,
        &code.public_key,
        &device,
        &[0; 16],
        "lonely",
        Duration::from_secs(3),
    )
    .await;
    assert_eq!(joined, Err(loquax::home::client::HomeError::NoPath));
    drop(link);
    attached.stop().await;

    drop(node);
    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// A leaf client of the home node, joined and keeping its watching link,
/// as the phone is.
struct Leaf {
    phone: tempfile::TempDir,
    device: leviculum_core::Identity,
    attached: loquax::engine::Attached,
    link: std::sync::Arc<leviculum_std::ReticulumNode>,
    events: tokio::sync::mpsc::UnboundedReceiver<loquax::home::watch::WatchEvent>,
    watching: tokio::task::JoinHandle<()>,
}

/// A leaf dialling the home node's TCP server on `port`, joined with
/// `code`, its watching link up.
async fn watching_leaf(
    port: u16,
    code: &loquax::home::code::HomeCode,
    address: [u8; 16],
    node: &HomeNode,
) -> Leaf {
    use loquax::engine::{attach_leaf, LeafConfig, Role};
    use loquax::home::watch::{self, WatchEvent};

    let phone = tempfile::tempdir().expect("phone home");
    let storage = phone.path().join("storage");
    std::fs::create_dir_all(&storage).expect("storage dir");
    let config = LeafConfig {
        server: format!("127.0.0.1:{port}"),
        storage_dir: storage,
        identity: leviculum_core::Identity::generate(&mut rand_core::OsRng),
        display_name: Vec::new(),
        notices: loquax::effects::Terminal::new(std::env::temp_dir()),
        tcp: Default::default(),
    };
    let device = config.identity.clone();
    let attached = attach_leaf(config, Role::Client { address })
        .await
        .expect("the leaf's node comes up");
    let link = attached.node();
    loquax::home::client::join(
        &link,
        &code.public_key,
        &device,
        &code.token,
        "phone",
        Duration::from_secs(40),
    )
    .await
    .unwrap_or_else(|error| panic!("join: {error}\nhome node:\n{}", node.log()));
    loquax::home::client::save_client(phone.path(), &code.public_key).expect("client file");

    let (events_tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let watching = tokio::spawn(watch::run(
        watch::Watch {
            node: link.clone(),
            public_key: code.public_key,
            device: device.clone(),
            home: phone.path().to_path_buf(),
        },
        events_tx,
    ));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(WatchEvent::Watching(true))) => break,
            Ok(Some(_)) => {}
            _ => panic!("the link never watched:\n{}", node.log()),
        }
    }
    node.wait_for("HOME watch device=", 5).await;
    Leaf {
        phone,
        device,
        attached,
        link,
        events,
        watching,
    }
}

/// What one run of [`wake_run`] saw: how long after the home node archived
/// the message its wake reached the watching client, `None` when none came
/// within the wait.
struct Woken {
    latency: Option<Duration>,
}

/// The wake end to end (1123): a leaf joins, keeps its watching link
/// (`loquax::home::watch`), and a loquax sender on the daemon's instance
/// delivers to the home node. With wakes on, the leaf hears one past its
/// cursor within 2 s of the home node's archive line, without any fetch,
/// and the pull it then makes brings the message. A client without the
/// link, as every client was before, still gets it with `loquax fetch`.
/// With `home_wake = off` in the home node's config, the positive control,
/// the same wait runs out.
async fn wake_run(wake: bool) -> Woken {
    use loquax::home::watch::{self, WatchEvent};
    use loquax::outbox::OutboxEvent;

    let mut mesh = Mesh::start_serving(&[]).await;
    let port = mesh.tcp_port.expect("the daemon's TCP server");
    let home = mesh.home.path().to_path_buf();
    if !wake {
        std::fs::write(home.join(loquax::config::CONFIG_FILE), "home_wake = off\n")
            .expect("the home node's config");
    }
    let code = admit(&home, "phone");
    let old = admit(&home, "old");
    let code = loquax::home::code::HomeCode::parse(&code).expect("the code parses");
    let address = loquax::home::lxmf_address(&code.public_key).expect("the home address");
    let node = HomeNode::start(&home, &mesh.instance).await;

    let Leaf {
        phone,
        device,
        mut attached,
        link,
        mut events,
        watching,
    } = watching_leaf(port, &code, address, &node).await;

    // A loquax messenger on the daemon's instance sends to the home node;
    // meanwhile the archive line and the wake are timed as they come.
    let sender = tempfile::tempdir().expect("sender home");
    let body = "geweckt";
    let send = mesh.run(
        "send",
        sender.path().to_path_buf(),
        vec![
            loquax::address::to_hex(&address),
            "--timeout".to_string(),
            "40".to_string(),
        ],
        body.as_bytes().to_vec(),
    );
    let watch_wake = async {
        let mut archived = None;
        let mut woken = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        while tokio::time::Instant::now() < deadline {
            if archived.is_none() && node.log().contains("HOME archived message=") {
                archived = Some(tokio::time::Instant::now());
            }
            while let Ok(event) = events.try_recv() {
                if let WatchEvent::Wake(newest) = event {
                    let cursor = loquax::home::client::load_cursor(phone.path());
                    if watch::wants_pull(newest, cursor) && woken.is_none() {
                        woken = Some(tokio::time::Instant::now());
                    }
                }
            }
            match (archived, woken) {
                (Some(_), Some(_)) => break,
                // The wait the client grants a wake: 2 s past the archive.
                (Some(at), None) if at.elapsed() > Duration::from_secs(2) => break,
                _ => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
        (archived, woken)
    };
    let (sent, (archived, woken)) = tokio::join!(send, watch_wake);
    assert_eq!(sent.code, Some(0), "{sent}\nhome node:\n{}", node.log());
    let archived = archived.unwrap_or_else(|| panic!("never archived:\n{}", node.log()));
    let latency = woken.map(|woken| woken.saturating_duration_since(archived));

    if latency.is_some() {
        // The pull the wake calls for brings the message.
        let pull = loquax::home::client::pull_into(
            &mut attached.outbox,
            &link,
            phone.path(),
            &code.public_key,
            &device,
            Duration::from_secs(40),
        )
        .await
        .unwrap_or_else(|error| panic!("pull: {error}\nhome node:\n{}", node.log()));
        assert!(
            pull.received.iter().any(|event| matches!(
                event,
                OutboxEvent::Received { body: b, .. } if b == body.as_bytes()
            )),
            "{pull:?}"
        );
    }

    // The old way: no link kept, the message on fetch.
    let client = joined(&mesh, &node, &old).await;
    let fetched = fetch(&mesh, &node, client.path()).await;
    assert!(fetched.stdout.contains(body), "{fetched}");

    watching.abort();
    drop(link);
    attached.stop().await;
    drop(node);
    mesh.daemon.stop().await.expect("stop the Rust daemon");
    Woken { latency }
}

/// How often `needle` is in the home node's log.
fn count(node: &HomeNode, needle: &str) -> usize {
    node.log().matches(needle).count()
}

/// Announce `identity`'s LXMF delivery destination with `name`, from
/// `announcer`.
async fn announce_name(
    announcer: &leviculum_std::ReticulumNode,
    identity: leviculum_core::Identity,
    name: &str,
) {
    use leviculum_core::{Destination, DestinationType, Direction};
    let destination = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["delivery"],
    )
    .expect("a delivery destination");
    let hash = *destination.hash();
    announcer.register_destination(destination);
    let app_data = leviculum_lxmf::announce::delivery(Some(name.as_bytes()), None);
    announcer
        .announce_destination(&hash, Some(&app_data))
        .await
        .expect("announce");
}

/// 1136: the home node hears the whole network's announces. A burst of 50
/// strangers' announces wakes no client and appends nothing; a rename of
/// a peer who wrote to the home node appends one name entry, wakes no
/// client, and reaches the watching client with its next pull, the only
/// name there.
#[tokio::test(flavor = "multi_thread")]
async fn strangers_names_wake_nobody_and_a_conversation_peers_rename_rides_the_next_pull() {
    let mut mesh = Mesh::start_serving(&[]).await;
    let port = mesh.tcp_port.expect("the daemon's TCP server");
    let home = mesh.home.path().to_path_buf();
    let code = admit(&home, "phone");
    let code = loquax::home::code::HomeCode::parse(&code).expect("the code parses");
    let address = loquax::home::lxmf_address(&code.public_key).expect("the home address");
    let node = HomeNode::start(&home, &mesh.instance).await;
    let Leaf {
        phone,
        device,
        mut attached,
        link,
        events: _events,
        watching,
    } = watching_leaf(port, &code, address, &node).await;

    // A peer writes to the home node: a conversation, and its first wake.
    let sender = tempfile::tempdir().expect("sender home");
    let sender_identity =
        loquax::identity::load_or_create(&sender.path().join("identity")).expect("identity");
    let sender_address = loquax::address::to_hex(
        &loquax::engine::delivery_address(&sender_identity).expect("the sender's address"),
    );
    let sent = mesh
        .run(
            "send",
            sender.path().to_path_buf(),
            vec![
                loquax::address::to_hex(&address),
                "--timeout".to_string(),
                "40".to_string(),
            ],
            b"hallo".to_vec(),
        )
        .await;
    assert_eq!(sent.code, Some(0), "{sent}\nhome node:\n{}", node.log());
    node.wait_for("HOME archived message=", 30).await;
    node.wait_for("HOME wake device=", 10).await;
    // Past the wake's spacing, so a second one would show.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let wakes = count(&node, "HOME wake device=");
    let names = count(&node, "HOME name archived");
    let renamed = format!("HOME name archived peer={sender_address}");
    let renames = count(&node, &renamed);

    // Fifty strangers, then the peer under a new name, from one node on
    // the shared instance: its rename is heard after the burst.
    let storage = tempfile::tempdir().expect("announcer storage");
    let mut announcer = leviculum_std::driver::ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&mesh.instance)
        .storage_path(storage.path().to_path_buf())
        .build_sync()
        .expect("build the announcer");
    announcer.start().await.expect("attach the announcer");
    for n in 0..50 {
        announce_name(
            &announcer,
            leviculum_std::generate_identity(),
            &format!("Stranger {n}"),
        )
        .await;
    }
    announce_name(&announcer, sender_identity, "Renamed Peer").await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while count(&node, &renamed) == renames {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the rename was never archived:\n{}",
            node.log()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let log = node.log();
    assert_eq!(
        count(&node, "HOME name archived") - names,
        1,
        "one name entry, the rename's, none of a stranger:\n{log}"
    );
    assert_eq!(
        count(&node, "HOME wake device="),
        wakes,
        "no wake for a name:\n{log}"
    );

    // The next pull brings it, and only it.
    let pull = loquax::home::client::pull_into(
        &mut attached.outbox,
        &link,
        phone.path(),
        &code.public_key,
        &device,
        Duration::from_secs(40),
    )
    .await
    .unwrap_or_else(|error| panic!("pull: {error}\nhome node:\n{}", node.log()));
    let sender_bytes: [u8; 16] = hex::decode(&sender_address)
        .expect("hex")
        .try_into()
        .expect("16 bytes");
    assert_eq!(
        pull.names,
        [(sender_bytes, "Renamed Peer".to_string())],
        "{pull:?}"
    );
    assert_eq!(pull.messages(), 1, "the message, counted; the name is not");

    watching.abort();
    drop(link);
    attached.stop().await;
    announcer.stop().await.expect("stop the announcer");
    drop(node);
    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_watching_client_hears_new_mail_within_two_seconds_and_the_old_way_still_fetches_it() {
    let woken = wake_run(true).await;
    let latency = woken.latency.expect("a wake within 2 s of the archive");
    eprintln!("WAKE latency_ms={}", latency.as_millis());
    assert!(latency <= Duration::from_secs(2), "{latency:?}");

    // Positive control: the push off, the same wait runs out.
    let control = wake_run(false).await;
    assert_eq!(
        control.latency, None,
        "a wake came with home_wake = off: the wait above proves nothing"
    );
}

/// The home node's propagated leg end to end (#232, slice 2): the client
/// sends to a recipient that is offline and was never heard, so the direct
/// leg fails and the home node hands the same packed message to a stock
/// Python propagation node. That leg waits for the recipient's key (the
/// copy on the node is encrypted for it), which a short run of the
/// recipient's announces before it leaves again; the client then sees
/// `propagated`. The recipient comes up, collects from the node and
/// announces; the home node's direct retry gets its proof, which the
/// recipient answers for a message it already holds. The recipient keeps
/// one message, and the client sees `delivered`.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_the_recipient_cannot_take_goes_to_a_propagation_node_and_arrives_once() {
    let node_peer = PythonPeer::start();
    node_peer.lxmf_init();
    let pn_hash = node_peer
        .rpc("lxmf_enable_propagation", serde_json::json!({}))
        .get("propagation_hash")
        .and_then(|v| v.as_str())
        .expect("the Python node reports its propagation hash")
        .to_string();
    let recipient_peer = PythonPeer::start();
    let mut mesh = Mesh::start_many(&[node_peer.rns_port, recipient_peer.rns_port]).await;
    // After the mesh is up, so the announce crosses to the recipient's side.
    node_peer.rpc("lxmf_announce_propagation_node", serde_json::json!({}));
    let home = mesh.home.path().to_path_buf();
    let code = admit(&home, "laptop");
    let node = HomeNode::start_with(&home, &mesh.instance, &["--pn", &pn_hash]).await;
    let client = joined(&mesh, &node, &code).await;

    let recipient = leviculum_std::generate_identity();
    let recipient_address =
        hex::encode(loquax::engine::delivery_address(&recipient).expect("address"));
    let body = "ueber den Briefkasten";
    let sent = mesh
        .run(
            "send",
            client.path().to_path_buf(),
            vec![
                recipient_address.clone(),
                "--timeout".to_string(),
                "40".to_string(),
            ],
            body.as_bytes().to_vec(),
        )
        .await;
    assert_eq!(sent.code, Some(0), "{sent}\nhome node:\n{}", node.log());
    let message_id = sent.enqueued_id();

    // Direct fails against nobody, and the propagated leg starts.
    node.wait_for(
        &format!("HOME outbox direct failed message={message_id}"),
        120,
    )
    .await;
    node.wait_for(&format!("HOME outbox propagated message={message_id}"), 5)
        .await;
    // The recipient is heard once: a loquax run on its identity announces
    // on attach and leaves, failing on a node that does not exist rather
    // than collecting from the real one.
    let once = tempfile::tempdir().expect("the recipient's earlier home");
    std::fs::write(
        once.path().join("identity"),
        leviculum_core::identity_store::encode_identity(&recipient).expect("storable"),
    )
    .expect("the recipient's identity");
    let heard = mesh
        .run(
            "fetch",
            once.path().to_path_buf(),
            vec![
                "--pn".to_string(),
                "0123456789abcdef0123456789abcdef".to_string(),
                "--timeout".to_string(),
                "10".to_string(),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(heard.code, Some(1), "{heard}");
    node.wait_for(
        &format!("HOME outbox propagated message={message_id} seq="),
        120,
    )
    .await;
    let fetched = fetch(&mesh, &node, client.path()).await;
    assert!(
        fetched
            .stdout
            .contains(&format!("message {message_id} propagated")),
        "{fetched}"
    );

    // The recipient comes up and collects from the propagation node. Its
    // identity moves from that loquax run to a Python router, and its
    // ratchets move with it (1185): the home node encrypted the copy to
    // the ratchet the loquax run announced, and an identity that moved
    // without it could not open it. The file loquax kept is the one Python
    // loads, unchanged.
    let ratchets = std::fs::read(leviculum_std::dest_ratchet_keys_path(
        &once.path().join("storage"),
        &loquax::engine::delivery_address(&recipient).expect("address"),
    ))
    .expect("the loquax run kept its ratchets");
    let started = recipient_peer.rpc(
        "lxmf_init",
        serde_json::json!({
            "identity_prv": hex::encode(recipient.private_key_bytes().expect("private key")),
            "ratchets": hex::encode(ratchets),
        }),
    );
    assert_eq!(
        started.get("delivery_hash").and_then(|v| v.as_str()),
        Some(recipient_address.as_str())
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while let Err(error) = recipient_peer.try_rpc(
        "lxmf_set_propagation_node",
        serde_json::json!({ "dest_hash": pn_hash }),
    ) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the recipient never heard the propagation node: {error}"
        );
        node_peer.rpc("lxmf_announce_propagation_node", serde_json::json!({}));
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    recipient_peer.rpc("lxmf_request_from_propagation_node", serde_json::json!({}));
    received_by(&recipient_peer, body, 60, &node).await;
    let first = recipient_peer.received();
    let collected: Vec<_> = first
        .iter()
        .filter(|m| m["message_hash"] == message_id.as_str())
        .collect();
    assert_eq!(collected.len(), 1, "{first:?}");
    assert_eq!(collected[0]["method"], "propagated", "{first:?}");

    // It announces; the home node's direct retry is proven, and the
    // recipient keeps the copy it has.
    recipient_peer.lxmf_announce();
    node.wait_for(&format!("HOME outbox delivered message={message_id}"), 60)
        .await;
    let all = recipient_peer.received();
    assert_eq!(
        all.iter()
            .filter(|m| m["message_hash"] == message_id.as_str())
            .count(),
        1,
        "the recipient keeps one message per hash: {all:?}"
    );
    let fetched = fetch(&mesh, &node, client.path()).await;
    assert!(
        fetched
            .stdout
            .contains(&format!("message {message_id} delivered")),
        "{fetched}"
    );

    drop(node);
    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// One scripted `loquax tui` run on a client home: the script runs to its
/// end, and the run must leave with 0.
async fn tui(mesh: &Mesh, client: &std::path::Path, to: &str, script: &str, node: &HomeNode) {
    let script_file = client.join("session.script");
    std::fs::write(&script_file, script).expect("the script");
    let args: Vec<String> = vec![
        "tui".into(),
        "--instance".into(),
        mesh.instance.clone(),
        "--to".into(),
        to.into(),
        "--sync-interval".into(),
        "15m".into(),
        "--script".into(),
        script_file.display().to_string(),
        "--size".into(),
        "80x24".into(),
    ];
    let home = client.to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &home)
            .env_remove("LEVICULUM_EVENT_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax tui");
        child.wait_with_output().expect("wait for loquax tui")
    })
    .await
    .expect("the tui thread");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}\ntui.log:\n{}\nhome node:\n{}",
        String::from_utf8_lossy(&output.stderr),
        std::fs::read_to_string(client.join("tui.log")).unwrap_or_default(),
        node.log()
    );
}

/// What a client's history holds of the conversation with `peer`.
fn conversation(
    client: &std::path::Path,
    address: [u8; 16],
    peer: [u8; 16],
) -> loquax::ui::history::Restored {
    let store = loquax::store::Store::open_in(client, address).expect("the client's history");
    store
        .restore()
        .expect("restore")
        .into_iter()
        .find(|c| c.peer == peer)
        .expect("a conversation with the peer")
}

/// Slice 4 end to end (#232): read and sent state across two scripted TUI
/// clients. A stock Python sender writes to the home node. Client B pulls
/// it unread, with another conversation open. Client A pulls it, opens its
/// conversation (read) and answers through the home node. B pulls again:
/// the message is read there, and A's answer is a sent bubble B never
/// composed. The sender's announced name, heard by the home node alone,
/// reaches B's name table on its pull (1132), and the name A gives the
/// sender with the palette's `rename` reaches B's contacts file (1139).
#[tokio::test(flavor = "multi_thread")]
async fn what_one_client_reads_and_sends_shows_on_the_other() {
    let peer = PythonPeer::start();
    let sender_address = peer.lxmf_init();
    let mut mesh = Mesh::start(peer.rns_port).await;
    let home = mesh.home.path().to_path_buf();
    let codes = [admit(&home, "a"), admit(&home, "b")];
    let address = local(&home, &["address"]);
    let address = address.stdout.trim().to_string();
    let node = HomeNode::start(&home, &mesh.instance).await;
    let a = joined(&mesh, &node, &codes[0]).await;
    let b = joined(&mesh, &node, &codes[1]).await;

    // The Python sender delivers to the home node, retried until it knows
    // the home node's key from its announce.
    let body = "fuer beide Geraete";
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while let Err(error) = peer.try_rpc(
        "lxmf_send",
        serde_json::json!({
            "dest_hash": address,
            "method": "direct",
            "content": hex::encode(body),
        }),
    ) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "Python never learned the home node: {error}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    node.wait_for("HOME archived message=", 40).await;
    // The sender announces its display name, which only the home node
    // hears; it goes into the archive once.
    peer.lxmf_announce();
    node.wait_for(&format!("HOME name archived peer={sender_address}"), 30)
        .await;

    let parse = |hex_address: &str| -> [u8; 16] {
        hex::decode(hex_address)
            .expect("hex")
            .try_into()
            .expect("16 bytes")
    };
    let (home_address, sender) = (parse(&address), parse(&sender_address));
    // Another conversation open, so the message lands unread.
    let elsewhere = "00112233445566778899aabbccddeeff";

    tui(
        &mesh,
        b.path(),
        elsewhere,
        "until ready 20\nuntil synced 1 40\nkey q\n",
        &node,
    )
    .await;
    let seen = conversation(b.path(), home_address, sender);
    assert_eq!(seen.unread, 1, "B pulled it unread");
    // The sender's name, which B never heard announced: the home node
    // heard it and B took it from the archive (1132).
    let names = loquax::store::Store::open_in(b.path(), home_address)
        .expect("B's store")
        .names()
        .expect("B's names");
    assert!(
        names.contains(&(sender, "test-daemon".to_string())),
        "B knows the sender by name: {names:?}"
    );

    // A opens the sender's conversation, the newest on the list, and answers.
    tui(
        &mesh,
        a.path(),
        elsewhere,
        "until ready 20\nuntil synced 1 40\nkey g\nkey tab\nkey tab\n\
         type reply from A\nkey enter\nuntil synced 2 40\nkey esc\n\
         key :\ntype rename\nkey enter\ntype Mutti\nkey enter\nkey q\n",
        &node,
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !node.log().contains("HOME mark read") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no read mark reached the home node:\n{}\nA's tui.log:\n{}",
            node.log(),
            std::fs::read_to_string(a.path().join("tui.log")).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    node.wait_for(&format!("HOME petname peer={sender_address}"), 20)
        .await;
    let on_a = conversation(a.path(), home_address, sender);
    assert_eq!(on_a.unread, 0);
    let answer = on_a
        .messages
        .iter()
        .find(|m| m.outbound)
        .expect("A's answer");
    assert_eq!(answer.body, "reply from A");
    assert_ne!(
        answer.delivery,
        Some(leviculum_ui::delivery::DeliveryState::Queued),
        "the pull after the send moved it on"
    );

    tui(
        &mesh,
        b.path(),
        elsewhere,
        "until ready 20\nuntil synced 1 40\nkey q\n",
        &node,
    )
    .await;
    let on_b = conversation(b.path(), home_address, sender);
    assert_eq!(on_b.unread, 0, "read on A, read on B");
    // A's name for the sender, which B never typed (1139).
    let contacts = loquax::names::load_contacts(b.path()).expect("B's contacts");
    assert_eq!(
        contacts.petnames.get(&sender).map(String::as_str),
        Some("Mutti"),
        "B took A's petname: {:?}",
        std::fs::read_to_string(b.path().join(loquax::names::CONTACTS_FILE))
    );
    let shown: Vec<_> = on_b.messages.iter().filter(|m| m.outbound).collect();
    assert_eq!(shown.len(), 1, "{:?}", on_b.messages);
    assert_eq!(shown[0].body, "reply from A");
    assert_ne!(
        shown[0].delivery,
        Some(leviculum_ui::delivery::DeliveryState::Queued)
    );

    drop(node);
    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// Lew's migration on the terminal (1159): the mailbox imported a Ratspeak
/// history and its contact's name before the client's screen started, so
/// the screen's start finds nothing in its store. Its first pull writes the
/// imported rows to the store, and the same running screen lists the
/// conversation under the imported name and shows the rows when it is
/// opened: no restart.
#[tokio::test(flavor = "multi_thread")]
async fn imported_history_shows_on_the_running_screen_under_its_name() {
    let mut mesh = Mesh::start_many(&[]).await;
    let home = mesh.home.path().to_path_buf();
    let code = admit(&home, "laptop");
    let node = HomeNode::start(&home, &mesh.instance).await;
    let client = joined(&mesh, &node, &code).await;

    let peer = [0x09; 16];
    {
        let identity = loquax::identity::load_or_create(&home.join("identity")).expect("identity");
        let address = loquax::engine::delivery_address(&identity).expect("address");
        let mut store = loquax::store::Store::open_in(&home, address).expect("home store");
        let row = |id: u8, outbound: bool, at: i64, content: &str| loquax::store::ImportRow {
            id: [id; 32],
            outbound,
            timestamp: Some(at as f64),
            received_at: Some(at),
            title: Vec::new(),
            content: content.to_string(),
            fields: Vec::new(),
            delivery: None,
            signature: loquax::ui::Signature::Unverified,
            origin_verdict: None,
            held: None,
        };
        let entries: Vec<([u8; 32], i64, Vec<u8>)> = [
            row(0xa1, false, 1_000, "von Ratspeak"),
            row(0xa2, true, 2_000, "zurueck"),
        ]
        .iter()
        .map(|r| {
            let at = r.received_at.unwrap_or(0);
            (
                r.id,
                at,
                loquax::import::archive::entry("ratspeak", &peer, r),
            )
        })
        .collect();
        assert_eq!(
            store.archive_imported(&entries, 1 << 20).expect("archive"),
            2
        );
        let names = [(&peer, "Ratte")];
        assert_eq!(
            loquax::home::server::archive_petnames(&mut store, names, 3_000, 1 << 20)
                .expect("the name"),
            1
        );
    }

    // No `--to`: the list is what the store and the pull bring. The
    // imported conversation is the only one, so `enter` opens it.
    let script =
        "until ready 20\nuntil synced 1 40\nframe pulled\nkey enter\nframe opened\nkey q\n";
    let script_file = client.path().join("session.script");
    std::fs::write(&script_file, script).expect("the script");
    let args: Vec<String> = vec![
        "tui".into(),
        "--instance".into(),
        mesh.instance.clone(),
        "--sync-interval".into(),
        "15m".into(),
        "--script".into(),
        script_file.display().to_string(),
        "--size".into(),
        "80x24".into(),
    ];
    let client_home = client.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &client_home)
            .env_remove("LEVICULUM_EVENT_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax tui");
        child.wait_with_output().expect("wait for loquax tui")
    })
    .await
    .expect("the tui thread");
    let tui_log = std::fs::read_to_string(client.path().join("tui.log")).unwrap_or_default();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}\ntui.log:\n{tui_log}\nhome node:\n{}",
        String::from_utf8_lossy(&output.stderr),
        node.log()
    );
    let frames = std::fs::read_to_string(client.path().join("session.frames")).expect("the frames");
    let (pulled, opened) = frames
        .split_once(": opened ---")
        .expect("both frames drawn");
    assert!(
        pulled.contains("Ratte"),
        "the pull lists the conversation under its name:\n{frames}\ntui.log:\n{tui_log}"
    );
    assert!(
        opened.contains("von Ratspeak") && opened.contains("zurueck"),
        "the opened conversation shows the imported rows:\n{frames}"
    );

    drop(node);
    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// The path entry's timestamp the Python peer holds for `destination`, 0
/// for none: it moves when an announce of it replaced the entry.
fn path_stamp(peer: &PythonPeer, destination: &str) -> f64 {
    peer.rpc("get_path_table", serde_json::json!({}))
        .get(destination)
        .and_then(|entry| entry.get("timestamp"))
        .and_then(|t| t.as_f64())
        .unwrap_or(0.0)
}

/// The ratchet the Python peer remembers for `destination`, if any.
fn known_ratchet(peer: &PythonPeer, destination: &str) -> Option<String> {
    peer.rpc(
        "lxmf_known_ratchet",
        serde_json::json!({ "dest_hash": destination }),
    )
    .as_str()
    .map(str::to_string)
}

/// Lew's mailbox on 2026-10-31 (leviculum 1185): the identity came from an
/// app that, like every Python LXMF router (`LXMRouter.py:357`), announced
/// its delivery destination with a ratchet, and a peer that heard it
/// encrypts every single packet to that ratchet for 30 days
/// (`RNS/Identity.py:69`). The old app here is a stock Python router on the
/// mailbox's identity; it announces, the sender remembers the ratchet, and
/// the old app goes away. The mailbox then runs on the identity and
/// announces, and the sender delivers a short message OPPORTUNISTIC, one
/// packet, as Columba does.
///
/// Without ratchets of its own the mailbox announced none, which clears
/// nothing at a peer, could not decrypt the packet, and proved it anyway:
/// lost, and "delivered" at the sender. With them the announce replaces the
/// remembered ratchet (`import` false); and with the old app's ratchet file
/// imported, what the sender encrypts to the old ratchet decrypts with what
/// the mailbox holds before any announce has crossed (`import` true).
async fn stale_ratchet_round(import: bool) {
    let peer = PythonPeer::start();
    peer.lxmf_init();
    let old = PythonPeer::start();
    let mut mesh = Mesh::start_many(&[peer.rns_port, old.rns_port]).await;
    let home = mesh.home.path().to_path_buf();
    let address = local(&home, &["address"]);
    assert_eq!(address.code, Some(0), "{address}");
    let address = address.stdout.trim().to_string();
    let identity = loquax::identity::load_or_create(&home.join("identity")).expect("identity");
    let key = identity.private_key_bytes().expect("a private key");

    let init = old.rpc(
        "lxmf_init",
        serde_json::json!({ "identity_prv": hex::encode(key) }),
    );
    assert_eq!(
        init.get("delivery_hash").and_then(|v| v.as_str()),
        Some(address.as_str()),
        "the old app runs on the mailbox's identity"
    );
    let ratchets_path = init
        .get("ratchets_path")
        .and_then(|v| v.as_str())
        .expect("the old app's ratchet file")
        .to_string();
    old.lxmf_announce();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let stale = loop {
        if let Some(ratchet) = known_ratchet(&peer, &address) {
            break ratchet;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the sender never remembered the old app's ratchet"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    // A path is replaced only by an announce emitted in a later second
    // (`RNS/Transport.py:1772`, the emission time is whole seconds in the
    // random blob): the old app's and the mailbox's announces in one second
    // left the sender on the old path in one run of four.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let ratchets = home.join("old.ratchets");
    std::fs::copy(&ratchets_path, &ratchets).expect("copy the old app's ratchet file");
    drop(old);

    if import {
        let imported = local(
            &home,
            &[
                "import",
                "--ratchets",
                ratchets.to_str().expect("utf-8 path"),
            ],
        );
        assert_eq!(imported.code, Some(0), "{imported}");
        assert!(imported.stdout.contains("ratchets:"), "{imported}");
        // What the sender encrypts now, to the old app's ratchet, opens
        // with what the mailbox will load.
        let plaintext = b"an den alten Ratchet";
        let token = peer.rpc(
            "lxmf_encrypt_for",
            serde_json::json!({ "dest_hash": address, "plaintext": hex::encode(plaintext) }),
        );
        let token = hex::decode(token.as_str().expect("a token")).expect("hex");
        let held = std::fs::read(leviculum_std::dest_ratchet_keys_path(
            &home.join("storage"),
            &loquax::address::parse(&address).expect("address"),
        ))
        .expect("the mailbox's ratchet file");
        let mut destination = leviculum_lxmf::node::LxmfNode::delivery_destination(
            loquax::identity::load_or_create(&home.join("identity")).expect("identity"),
        )
        .expect("delivery destination");
        destination
            .load_ratchets_signed(&held)
            .expect("the imported file is ours and signed");
        assert_eq!(
            destination
                .decrypt(&token)
                .expect("the old ratchet decrypts"),
            plaintext
        );
    }

    let before = path_stamp(&peer, &address);
    let node = HomeNode::start(&home, &mesh.instance).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while path_stamp(&peer, &address) <= before {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the sender never heard the mailbox's announce: {before} {:?}\n{}",
            peer.rpc("get_path_table", serde_json::json!({}))
                .get(&address),
            node.log()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let now = known_ratchet(&peer, &address);
    if !import {
        assert!(
            now.is_some() && now.as_deref() != Some(stale.as_str()),
            "the mailbox's announce must replace the stale ratchet: {now:?}\n{}",
            node.log()
        );
    }

    let body = "Klaro";
    let sent = peer.rpc(
        "lxmf_send",
        serde_json::json!({
            "dest_hash": address,
            "method": "opportunistic",
            "content": hex::encode(body),
        }),
    );
    let message_hash = sent
        .get("message_hash")
        .and_then(|v| v.as_str())
        .expect("a message hash")
        .to_string();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    loop {
        let state = peer.rpc(
            "lxmf_get_outbound_status",
            serde_json::json!({ "message_hash": message_hash }),
        );
        let archived = node
            .log()
            .contains(&format!("HOME archived message={message_hash}"));
        match state.get("state").and_then(|v| v.as_str()) {
            Some("DELIVERED") => {
                assert!(
                    archived,
                    "proven but not archived: the message was lost\n{}",
                    node.log()
                );
                break;
            }
            Some("FAILED") | Some("REJECTED") => {
                panic!("the sender gave up: {state}\n{}", node.log())
            }
            _ => {}
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the sender never got DELIVERED: {state}, archived {archived}\n{}",
            node.log()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        !node.log().contains("LXMF_RX_UNDECRYPTABLE"),
        "{}",
        node.log()
    );

    drop(node);
    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sender_holding_the_old_apps_ratchet_reaches_the_mailbox_after_its_announce() {
    stale_ratchet_round(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_old_apps_imported_ratchets_open_what_the_sender_encrypts_to_them() {
    stale_ratchet_round(true).await;
}

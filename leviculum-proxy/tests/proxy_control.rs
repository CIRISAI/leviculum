use std::sync::Arc;
use std::time::Duration;

use leviculum_core::framing::kiss::{self, KissDeframeResult, KissDeframer};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio::time::Instant;

use lora_proxy::control::run_control_socket;
use lora_proxy::forward::forward_kiss_frames;
use lora_proxy::rules::{Action, Direction, Filter, RuleEngine, RuleSpec};

/// Helper: write a KISS frame to a stream
async fn write_kiss_frame<W: AsyncWriteExt + Unpin>(w: &mut W, cmd: u8, payload: &[u8]) {
    let mut buf = Vec::new();
    kiss::frame(cmd, payload, &mut buf);
    w.write_all(&buf).await.unwrap();
}

/// Helper: read exactly `count` KISS frames from a stream, with a timeout
async fn read_kiss_frames<R: AsyncReadExt + Unpin>(
    r: &mut R,
    count: usize,
    timeout: Duration,
) -> Vec<(u8, Vec<u8>)> {
    let mut deframer = KissDeframer::with_max_payload(508);
    let mut buf = [0u8; 4096];
    let mut frames = Vec::new();
    let deadline = Instant::now() + timeout;

    while frames.len() < count {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        match tokio::time::timeout(remaining, r.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                for result in deframer.process(&buf[..n]) {
                    if let KissDeframeResult::Frame { command, payload } = result {
                        frames.push((command, payload));
                    }
                }
            }
            Ok(Err(_)) => break,
            Err(_) => break, // timeout
        }
    }

    frames
}

/// A control-socket path short enough for `sun_path` (about 108 bytes):
/// the system temp directory, or `/tmp` when `TMPDIR` is so deep that a
/// bound socket there would be refused with "path must be shorter than
/// SUN_LEN" (a CI pass directory is such a TMPDIR).
fn short_sock(name: String) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(&name);
    match p.as_os_str().len() < 90 {
        true => p,
        false => std::path::PathBuf::from("/tmp").join(name),
    }
}

/// Helper: send a control command and read the response line
async fn control_cmd(sock_path: &std::path::Path, cmd: &str) -> String {
    let stream = tokio::net::UnixStream::connect(sock_path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    writer
        .write_all(format!("{cmd}\n").as_bytes())
        .await
        .unwrap();
    writer.shutdown().await.unwrap();
    let mut lines = BufReader::new(reader).lines();
    lines.next_line().await.unwrap().unwrap_or_default()
}

/// Test 1: drop with count=1 drops exactly one frame, forwards the rest
#[tokio::test]
async fn test_drop_count() {
    let engine = Arc::new(Mutex::new(RuleEngine::new()));

    let (a_proxy, mut a_test) = tokio::io::duplex(4096);
    let (b_proxy, mut b_test) = tokio::io::duplex(4096);

    let engine_clone = Arc::clone(&engine);
    let _fwd = tokio::spawn(async move {
        let _ = forward_kiss_frames(a_proxy, "a", b_proxy, "b", engine_clone).await;
    });

    // Add drop rule: drop 1 frame
    engine
        .lock()
        .await
        .add_rule(Direction::Both, Action::Drop, Filter::All, 0, 0, 0, Some(1));

    // Small yield to let the forwarding loop start
    tokio::task::yield_now().await;

    // Send 3 frames A->B
    write_kiss_frame(&mut a_test, 0x00, b"frame1").await;
    write_kiss_frame(&mut a_test, 0x00, b"frame2").await;
    write_kiss_frame(&mut a_test, 0x00, b"frame3").await;

    // Read from B side, should get exactly 2 (first was dropped)
    let received = read_kiss_frames(&mut b_test, 2, Duration::from_secs(2)).await;
    assert_eq!(
        received.len(),
        2,
        "Expected 2 frames, got {}",
        received.len()
    );

    // The dropped frame was frame1, so we should get frame2 and frame3
    assert_eq!(received[0].1, b"frame2");
    assert_eq!(received[1].1, b"frame3");

    // Check stats
    let eng = engine.lock().await;
    assert_eq!(eng.dropped, 1);
    assert_eq!(eng.forwarded, 2);
}

/// Test 2: delay adds measurable latency
#[tokio::test]
async fn test_delay() {
    let engine = Arc::new(Mutex::new(RuleEngine::new()));

    let (a_proxy, mut a_test) = tokio::io::duplex(4096);
    let (b_proxy, mut b_test) = tokio::io::duplex(4096);

    let engine_clone = Arc::clone(&engine);
    let _fwd = tokio::spawn(async move {
        let _ = forward_kiss_frames(a_proxy, "a", b_proxy, "b", engine_clone).await;
    });

    // Add 200ms delay rule (count=1)
    engine.lock().await.add_rule(
        Direction::Both,
        Action::Delay(200),
        Filter::All,
        0,
        0,
        0,
        Some(1),
    );

    tokio::task::yield_now().await;

    // Send one frame and measure delivery time
    let start = Instant::now();
    write_kiss_frame(&mut a_test, 0x00, b"delayed").await;

    let received = read_kiss_frames(&mut b_test, 1, Duration::from_secs(2)).await;
    let elapsed = start.elapsed();

    assert_eq!(received.len(), 1);
    assert_eq!(received[0].1, b"delayed");
    assert!(
        elapsed >= Duration::from_millis(180),
        "Expected >= 180ms delay, got {:?}",
        elapsed
    );

    // Second frame should be immediate (rule was count=1, auto-removed)
    let start2 = Instant::now();
    write_kiss_frame(&mut a_test, 0x00, b"fast").await;

    let received2 = read_kiss_frames(&mut b_test, 1, Duration::from_secs(2)).await;
    let elapsed2 = start2.elapsed();

    assert_eq!(received2.len(), 1);
    assert_eq!(received2[0].1, b"fast");
    assert!(
        elapsed2 < Duration::from_millis(100),
        "Expected fast delivery, got {:?}",
        elapsed2
    );

    let eng = engine.lock().await;
    assert_eq!(eng.delayed, 1);
    assert_eq!(eng.forwarded, 1);
}

/// Test 3: clear all removes rules, forwarding resumes
#[tokio::test]
async fn test_clear_all() {
    let engine = Arc::new(Mutex::new(RuleEngine::new()));

    let (a_proxy, mut a_test) = tokio::io::duplex(4096);
    let (b_proxy, mut b_test) = tokio::io::duplex(4096);

    let engine_clone = Arc::clone(&engine);
    let _fwd = tokio::spawn(async move {
        let _ = forward_kiss_frames(a_proxy, "a", b_proxy, "b", engine_clone).await;
    });

    // Add 2 rules
    engine
        .lock()
        .await
        .add_rule(Direction::Both, Action::Drop, Filter::All, 0, 0, 0, None);
    engine
        .lock()
        .await
        .add_rule(Direction::Both, Action::Corrupt, Filter::All, 0, 0, 0, None);

    tokio::task::yield_now().await;

    // Send a frame, should be dropped (first rule wins)
    write_kiss_frame(&mut a_test, 0x00, b"blocked").await;
    let received = read_kiss_frames(&mut b_test, 1, Duration::from_millis(200)).await;
    assert!(received.is_empty(), "Frame should have been dropped");

    // Clear all rules
    engine.lock().await.clear_all();
    assert_eq!(engine.lock().await.rule_count(), 0);

    // Now frames should forward normally
    write_kiss_frame(&mut a_test, 0x00, b"unblocked").await;
    let received = read_kiss_frames(&mut b_test, 1, Duration::from_secs(2)).await;
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].1, b"unblocked");
}

/// Test 4: control socket protocol works end-to-end
#[tokio::test]
async fn test_control_socket() {
    let engine = Arc::new(Mutex::new(RuleEngine::new()));
    let sock_path = short_sock(format!("proxy-test-{}.sock", std::process::id()));

    // Spawn control socket
    let engine_clone = Arc::clone(&engine);
    let sock = sock_path.clone();
    tokio::spawn(async move {
        let _ = run_control_socket(&sock, engine_clone).await;
    });

    // Wait for socket to be ready
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Add a drop rule via control socket
    let resp = control_cmd(&sock_path, "rule add drop count=5").await;
    assert!(resp.starts_with("OK "), "Expected OK, got: {resp}");

    // List rules
    let resp = control_cmd(&sock_path, "rule list").await;
    assert!(resp.starts_with("OK ["), "Expected OK [...], got: {resp}");
    assert!(resp.contains("\"action\":\"drop\""));
    assert!(resp.contains("\"remaining\":5"));

    // Stats
    let resp = control_cmd(&sock_path, "stats").await;
    assert!(resp.starts_with("OK {"), "Expected OK {{...}}, got: {resp}");
    assert!(resp.contains("\"rules\":1"));

    // Clear all
    let resp = control_cmd(&sock_path, "rule clear all").await;
    assert_eq!(resp, "OK");

    // List again, empty
    let resp = control_cmd(&sock_path, "rule list").await;
    assert_eq!(resp, "OK []");

    // Cleanup
    let _ = std::fs::remove_file(&sock_path);
}

/// Test 5: corrupt flips first payload byte
#[tokio::test]
async fn test_corrupt() {
    let engine = Arc::new(Mutex::new(RuleEngine::new()));

    let (a_proxy, mut a_test) = tokio::io::duplex(4096);
    let (b_proxy, mut b_test) = tokio::io::duplex(4096);

    let engine_clone = Arc::clone(&engine);
    let _fwd = tokio::spawn(async move {
        let _ = forward_kiss_frames(a_proxy, "a", b_proxy, "b", engine_clone).await;
    });

    engine.lock().await.add_rule(
        Direction::Both,
        Action::Corrupt,
        Filter::All,
        0,
        0,
        0,
        Some(1),
    );

    tokio::task::yield_now().await;

    write_kiss_frame(&mut a_test, 0x00, &[0xAB, 0xCD, 0xEF]).await;
    let received = read_kiss_frames(&mut b_test, 1, Duration::from_secs(2)).await;

    assert_eq!(received.len(), 1);
    assert_eq!(received[0].1[0], 0xAB ^ 0xFF); // first byte flipped
    assert_eq!(received[0].1[1], 0xCD); // rest unchanged
    assert_eq!(received[0].1[2], 0xEF);
}

/// Test 6: direction filter only affects matching direction
#[tokio::test]
async fn test_direction_filter() {
    let engine = Arc::new(Mutex::new(RuleEngine::new()));

    let (a_proxy, mut a_test) = tokio::io::duplex(4096);
    let (b_proxy, mut b_test) = tokio::io::duplex(4096);

    let engine_clone = Arc::clone(&engine);
    let _fwd = tokio::spawn(async move {
        let _ = forward_kiss_frames(a_proxy, "a", b_proxy, "b", engine_clone).await;
    });

    // Drop only A->B
    engine
        .lock()
        .await
        .add_rule(Direction::AToB, Action::Drop, Filter::All, 0, 0, 0, None);

    tokio::task::yield_now().await;

    // A->B should be dropped
    write_kiss_frame(&mut a_test, 0x00, b"a_to_b").await;
    let received = read_kiss_frames(&mut b_test, 1, Duration::from_millis(200)).await;
    assert!(received.is_empty(), "A->B should be dropped");

    // B->A should pass
    write_kiss_frame(&mut b_test, 0x00, b"b_to_a").await;
    let received = read_kiss_frames(&mut a_test, 1, Duration::from_secs(2)).await;
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].1, b"b_to_a");
}

/// Test 7: command filter only affects matching KISS command byte
#[tokio::test]
async fn test_command_filter() {
    let engine = Arc::new(Mutex::new(RuleEngine::new()));

    let (a_proxy, mut a_test) = tokio::io::duplex(4096);
    let (b_proxy, mut b_test) = tokio::io::duplex(4096);

    let engine_clone = Arc::clone(&engine);
    let _fwd = tokio::spawn(async move {
        let _ = forward_kiss_frames(a_proxy, "a", b_proxy, "b", engine_clone).await;
    });

    // Drop only CMD_DATA (0x00)
    engine.lock().await.add_rule(
        Direction::Both,
        Action::Drop,
        Filter::Command(0x00),
        0,
        0,
        0,
        None,
    );

    tokio::task::yield_now().await;

    // CMD_DATA should be dropped
    write_kiss_frame(&mut a_test, 0x00, b"data").await;
    // CMD_DETECT (0x08) should pass
    write_kiss_frame(&mut a_test, 0x08, b"detect").await;

    let received = read_kiss_frames(&mut b_test, 1, Duration::from_secs(2)).await;
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].0, 0x08); // command byte
    assert_eq!(received[0].1, b"detect");
}

/// Test 8: control socket rule add with all options
#[tokio::test]
async fn test_control_socket_full_options() {
    let engine = Arc::new(Mutex::new(RuleEngine::new()));
    let sock_path = short_sock(format!("proxy-test-opts-{}.sock", std::process::id()));

    let engine_clone = Arc::clone(&engine);
    let sock = sock_path.clone();
    tokio::spawn(async move {
        let _ = run_control_socket(&sock, engine_clone).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Add delay with all options
    let resp = control_cmd(
        &sock_path,
        "rule add delay 100 direction=a_to_b filter=cmd:0x00 count=3",
    )
    .await;
    assert!(resp.starts_with("OK "), "Got: {resp}");

    // Add corrupt with no options
    let resp = control_cmd(&sock_path, "rule add corrupt").await;
    assert!(resp.starts_with("OK "), "Got: {resp}");

    // List, should show both
    let resp = control_cmd(&sock_path, "rule list").await;
    assert!(resp.contains("\"action\":\"delay 100\""), "Got: {resp}");
    assert!(resp.contains("\"direction\":\"a_to_b\""), "Got: {resp}");
    assert!(resp.contains("\"filter\":\"cmd:0x00\""), "Got: {resp}");
    assert!(resp.contains("\"remaining\":3"), "Got: {resp}");
    assert!(resp.contains("\"action\":\"corrupt\""), "Got: {resp}");
    assert!(resp.contains("\"remaining\":null"), "Got: {resp}");

    // Clear first rule by id
    let resp = control_cmd(&sock_path, "rule clear 1").await;
    assert_eq!(resp, "OK");

    // Only corrupt rule left
    let resp = control_cmd(&sock_path, "rule list").await;
    assert!(!resp.contains("delay"), "Got: {resp}");
    assert!(resp.contains("corrupt"), "Got: {resp}");

    let _ = std::fs::remove_file(&sock_path);
}

/// Test 9: drop count=5 then forward 10 frames, stats check
#[tokio::test]
async fn test_drop_5_forward_10() {
    let engine = Arc::new(Mutex::new(RuleEngine::new()));

    let (a_proxy, mut a_test) = tokio::io::duplex(4096);
    let (b_proxy, mut b_test) = tokio::io::duplex(4096);

    let engine_clone = Arc::clone(&engine);
    let _fwd = tokio::spawn(async move {
        let _ = forward_kiss_frames(a_proxy, "a", b_proxy, "b", engine_clone).await;
    });

    engine
        .lock()
        .await
        .add_rule(Direction::Both, Action::Drop, Filter::All, 0, 0, 0, Some(5));

    tokio::task::yield_now().await;

    // Send 10 frames
    for i in 0..10u8 {
        write_kiss_frame(&mut a_test, 0x00, &[i]).await;
    }

    // Should receive exactly 5 (frames 5-9)
    let received = read_kiss_frames(&mut b_test, 5, Duration::from_secs(2)).await;
    assert_eq!(
        received.len(),
        5,
        "Expected 5 frames, got {}",
        received.len()
    );

    // Verify they are frames 5-9
    for (i, (cmd, payload)) in received.iter().enumerate() {
        assert_eq!(*cmd, 0x00);
        assert_eq!(payload[0], (i + 5) as u8);
    }

    // Check stats
    let eng = engine.lock().await;
    assert_eq!(eng.dropped, 5);
    assert_eq!(eng.forwarded, 5);
    println!("Stats: {}", eng.stats_json());
}

/// Test 11: probabilistic loss-rate rule via control socket, plus the
/// mutual-exclusivity and range guards on the parse path.
#[tokio::test]
async fn test_control_socket_rate_mode() {
    let engine = Arc::new(Mutex::new(RuleEngine::new()));
    let sock_path = short_sock(format!("proxy-test-rate-{}.sock", std::process::id()));

    let engine_clone = Arc::clone(&engine);
    let sock = sock_path.clone();
    tokio::spawn(async move {
        let _ = run_control_socket(&sock, engine_clone).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // A rate rule with an explicit seed is accepted and stored as rate mode.
    let resp = control_cmd(
        &sock_path,
        "rule add drop direction=a_to_b filter=cmd:0x00 rate=0.25 seed=99",
    )
    .await;
    assert!(resp.starts_with("OK "), "Got: {resp}");
    {
        let eng = engine.lock().await;
        let rules = eng.list_rules();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rate, Some(0.25));
        assert_eq!(rules[0].remaining, None);
    }

    // rate and count together are rejected (mutually exclusive).
    let resp = control_cmd(&sock_path, "rule add drop rate=0.5 count=3").await;
    assert!(resp.contains("mutually exclusive"), "Got: {resp}");

    // Out-of-range rate is rejected.
    let resp = control_cmd(&sock_path, "rule add drop rate=1.5").await;
    assert!(resp.contains("out of range"), "Got: {resp}");

    // Only the first (valid) rule should exist.
    let eng = engine.lock().await;
    assert_eq!(eng.list_rules().len(), 1);
    drop(eng);

    let _ = std::fs::remove_file(&sock_path);
}

/// Test 12: end-to-end reproducibility of rate mode through the forwarder.
/// Two independent proxies with the same seeded rate rule, fed an identical
/// frame sequence, drop the identical set of frames.
#[tokio::test]
async fn test_rate_mode_end_to_end_reproducible() {
    async fn run_once() -> Vec<u8> {
        let engine = Arc::new(Mutex::new(RuleEngine::new()));
        let (a_proxy, mut a_test) = tokio::io::duplex(8192);
        let (b_proxy, mut b_test) = tokio::io::duplex(8192);

        let engine_clone = Arc::clone(&engine);
        let _fwd = tokio::spawn(async move {
            let _ = forward_kiss_frames(a_proxy, "a", b_proxy, "b", engine_clone).await;
        });

        engine.lock().await.add_rule_spec(RuleSpec {
            action: Action::Drop,
            rate: Some(0.5),
            seed: 4242,
            ..RuleSpec::default()
        });
        tokio::task::yield_now().await;

        // Send 40 frames tagged 0..40 in the payload.
        for i in 0..40u8 {
            write_kiss_frame(&mut a_test, 0x00, &[i]).await;
        }

        // Collect whichever frames survive; the payload tag identifies them.
        let received = read_kiss_frames(&mut b_test, 40, Duration::from_millis(500)).await;
        received.into_iter().map(|(_, p)| p[0]).collect()
    }

    let run_a = run_once().await;
    let run_b = run_once().await;
    assert_eq!(
        run_a, run_b,
        "seeded rate mode must forward the same frames"
    );
    // Sanity: rate=0.5 dropped some but not all of the 40 frames.
    assert!(
        !run_a.is_empty() && run_a.len() < 40,
        "got {} frames",
        run_a.len()
    );
}

/// Test 10: max_size via control socket
#[tokio::test]
async fn test_control_socket_max_size() {
    let engine = Arc::new(Mutex::new(RuleEngine::new()));
    let sock_path = short_sock(format!("proxy-test-maxsz-{}.sock", std::process::id()));

    let engine_clone = Arc::clone(&engine);
    let sock = sock_path.clone();
    tokio::spawn(async move {
        let _ = run_control_socket(&sock, engine_clone).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Add a drop rule with min_size and max_size
    let resp = control_cmd(
        &sock_path,
        "rule add drop direction=a_to_b filter=cmd:0x00 min_size=50 max_size=130 count=2",
    )
    .await;
    assert!(resp.starts_with("OK "), "Got: {resp}");

    // Verify rule was added via the engine
    let eng = engine.lock().await;
    let rules = eng.list_rules();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].min_size, 50);
    assert_eq!(rules[0].max_size, 130);
    assert_eq!(rules[0].direction, Direction::AToB);
    assert!(matches!(rules[0].remaining, Some(2)));
    drop(eng);

    let _ = std::fs::remove_file(&sock_path);
}

/// Spawn the control socket at `sock_path` and wait until a `stats` round
/// trip succeeds. The answer only comes from the accept loop, which starts
/// after the socket's mode is set, so a caller may inspect the mode at once.
async fn spawn_control_ready(sock_path: &std::path::Path) -> Arc<Mutex<RuleEngine>> {
    let engine = Arc::new(Mutex::new(RuleEngine::new()));
    let engine_clone = Arc::clone(&engine);
    let sock = sock_path.to_path_buf();
    tokio::spawn(async move {
        let _ = run_control_socket(&sock, engine_clone).await;
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if tokio::net::UnixStream::connect(sock_path).await.is_ok() {
            let resp = control_cmd(sock_path, "stats").await;
            assert!(resp.starts_with("OK {"), "Got: {resp}");
            return engine;
        }
        assert!(Instant::now() < deadline, "control socket never came up");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// L-0042 (a): a regular file at the control path is not unlinked. The bind
/// refuses with an error that names the path, and the file keeps its content.
#[tokio::test]
async fn test_control_path_regular_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("results.json");
    std::fs::write(&path, b"measurement data").unwrap();

    let engine = Arc::new(Mutex::new(RuleEngine::new()));
    let outcome =
        tokio::time::timeout(Duration::from_secs(2), run_control_socket(&path, engine)).await;

    let err = match outcome {
        Ok(Err(e)) => e,
        Ok(Ok(())) => panic!("control socket returned Ok over a regular file"),
        Err(_) => panic!("control socket bound over a regular file and kept running"),
    };
    assert!(
        err.to_string().contains(&path.display().to_string()),
        "error does not name the path: {err}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"measurement data");
}

/// L-0042 (a): a stale socket left by an earlier run is removed and rebound.
#[tokio::test]
async fn test_control_path_stale_socket_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    assert!(
        path.exists(),
        "a dropped listener leaves its socket file behind"
    );

    spawn_control_ready(&path).await;
}

/// L-0042 (b): the bound socket is 0600, whatever the umask.
#[tokio::test]
async fn test_control_socket_mode_is_0600() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    spawn_control_ready(&path).await;

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "control socket mode is {mode:o}");
}

/// L-0042 (c): a line longer than the bound is refused, not buffered: the
/// peer gets an error and the connection closes without a newline ever
/// arriving. A line of exactly the bound is still a command.
#[tokio::test]
async fn test_control_line_over_bound_is_refused() {
    use lora_proxy::control::MAX_CONTROL_LINE;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    spawn_control_ready(&path).await;

    // Exactly the bound: "stats" padded with blanks, which the parser skips.
    let at_bound = format!("{:<width$}", "stats", width = MAX_CONTROL_LINE);
    let resp = control_cmd(&path, &at_bound).await;
    assert!(resp.starts_with("OK {"), "Got: {resp}");

    // One past the bound, no newline, write half left open.
    let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    writer
        .write_all(&vec![b'x'; MAX_CONTROL_LINE + 1])
        .await
        .unwrap();
    let mut lines = BufReader::new(reader).lines();
    let resp = tokio::time::timeout(Duration::from_secs(2), lines.next_line())
        .await
        .expect("an over-long line is still being buffered")
        .unwrap()
        .unwrap_or_default();
    assert!(resp.starts_with("ERR line exceeds"), "Got: {resp}");
    let after = tokio::time::timeout(Duration::from_secs(2), lines.next_line())
        .await
        .expect("connection still open after an over-long line")
        .unwrap();
    assert_eq!(after, None);
}

/// L-0054: `count=0` is refused at the control socket; no rule is stored.
#[tokio::test]
async fn test_control_socket_count_zero_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("proxy.sock");
    let engine = spawn_control_ready(&sock_path).await;

    let resp = control_cmd(&sock_path, "rule add drop count=0").await;
    assert_eq!(resp, "ERR count must be at least 1");
    assert!(engine.lock().await.list_rules().is_empty());

    let resp = control_cmd(&sock_path, "rule add drop count=1").await;
    assert!(resp.starts_with("OK "), "Got: {resp}");
}

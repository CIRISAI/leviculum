use std::collections::BTreeMap;
use std::fmt;
use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;

use crate::runner::{RunnerError, TestRunner};
use crate::topology::{scale_timeout, timeout_scale, ParallelTransferDef, Step};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum StepError {
    /// Could not resolve "node.aspect" to a hex destination hash.
    DestinationResolve { destination: String, detail: String },
    /// A step's assertion failed (wrong exit code, missing output, wrong hops).
    StepFailed {
        step_index: usize,
        action: String,
        detail: String,
    },
    /// Underlying runner/docker error.
    Runner(RunnerError),
}

impl fmt::Display for StepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StepError::DestinationResolve {
                destination,
                detail,
            } => {
                write!(f, "cannot resolve destination '{destination}': {detail}")
            }
            StepError::StepFailed {
                step_index,
                action,
                detail,
            } => {
                write!(f, "step {step_index} ({action}) failed: {detail}")
            }
            StepError::Runner(e) => write!(f, "runner error: {e}"),
        }
    }
}

impl std::error::Error for StepError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StepError::Runner(e) => Some(e),
            _ => None,
        }
    }
}

impl From<RunnerError> for StepError {
    fn from(e: RunnerError) -> Self {
        StepError::Runner(e)
    }
}

// ---------------------------------------------------------------------------
// Destination resolution
// ---------------------------------------------------------------------------

/// Parse "bob.probe" into (node_name, aspect).
fn parse_dest_spec(spec: &str) -> Result<(&str, &str), StepError> {
    match spec.find('.') {
        Some(pos) if pos > 0 && pos < spec.len() - 1 => Ok((&spec[..pos], &spec[pos + 1..])),
        _ => Err(StepError::DestinationResolve {
            destination: spec.to_string(),
            detail: "expected format 'node.aspect' (e.g. 'bob.probe')".into(),
        }),
    }
}

/// Extract the probe destination hash from a node's startup log.
///
/// Two log formats are recognized, depending on which daemon ran in the
/// container:
///
/// - lnsd (Rust):
///   `[IDENTITY] probe_destination=<HEX32> aspect=rnstransport.probe`
/// - rnsd (Python):
///   `Transport Instance will respond to probe requests on <rnstransport.probe.<HEX32>:<HEX32>>`
///   The second hex (after the colon) is the probe destination hash.
///
/// Returns the LAST match in either format (later log entries win, so
/// restarts are handled correctly).
fn extract_probe_hash_from_identity_log(logs: &str) -> Option<&str> {
    fn try_lnsd(logs: &str) -> Option<(usize, &str)> {
        let marker = "[IDENTITY] probe_destination=";
        let pos = logs.rfind(marker)?;
        let rest = &logs[pos + marker.len()..];
        let end = rest.find(|c: char| !c.is_ascii_hexdigit())?;
        let hash = &rest[..end];
        if hash.len() == 32 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            Some((pos, hash))
        } else {
            None
        }
    }
    fn try_rnsd(logs: &str) -> Option<(usize, &str)> {
        let marker = "Transport Instance will respond to probe requests on <rnstransport.probe.";
        let pos = logs.rfind(marker)?;
        let rest = &logs[pos + marker.len()..];
        // <HEX32>:<HEX32>>, skip identity hash, ':', then hash is next 32 hex
        let colon = rest.find(':')?;
        let after = &rest[colon + 1..];
        if after.len() < 32 {
            return None;
        }
        let hash = &after[..32];
        if hash.chars().all(|c| c.is_ascii_hexdigit()) {
            Some((pos, hash))
        } else {
            None
        }
    }
    match (try_lnsd(logs), try_rnsd(logs)) {
        (Some((pa, ha)), Some((pb, hb))) => Some(if pa >= pb { ha } else { hb }),
        (Some((_, h)), None) | (None, Some((_, h))) => Some(h),
        (None, None) => None,
    }
}

/// Resolve "bob.probe" to a 32-char hex destination hash by reading the
/// target node's lnsd startup log for the `[IDENTITY] probe_destination=`
/// line. Caches results in `cache` to avoid repeated docker calls.
fn resolve_destination(
    runner: &TestRunner,
    spec: &str,
    cache: &mut BTreeMap<String, String>,
) -> Result<String, StepError> {
    if let Some(cached) = cache.get(spec) {
        return Ok(cached.clone());
    }

    let (node_name, aspect) = parse_dest_spec(spec)?;

    if aspect != "probe" {
        return Err(StepError::DestinationResolve {
            destination: spec.to_string(),
            detail: format!("unsupported aspect '{aspect}', only 'probe' is implemented"),
        });
    }

    // Read the container's startup log (captured by Docker). The [IDENTITY]
    // line is emitted once by lnsd at startup when respond_to_probes=true.
    let output = runner
        .docker_logs(node_name)
        .map_err(|e| StepError::DestinationResolve {
            destination: spec.to_string(),
            detail: format!("docker logs failed on '{node_name}': {e}"),
        })?;
    // Docker logs go to stderr; merge both streams for robustness.
    let combined = alloc_combined_logs(&output.stdout, &output.stderr);

    let hash = extract_probe_hash_from_identity_log(&combined).ok_or_else(|| {
        StepError::DestinationResolve {
            destination: spec.to_string(),
            detail: format!(
                "no probe destination hash found in docker logs on '{node_name}'. \
                 Looked for lnsd '[IDENTITY] probe_destination=<hash>' and rnsd \
                 'Transport Instance will respond to probe requests on \
                 <rnstransport.probe.<id>:<hash>>'. Is the daemon running with \
                 respond_to_probes=true (lnsd) or transport_instance=Yes (rnsd)?"
            ),
        }
    })?;

    cache.insert(spec.to_string(), hash.to_string());
    Ok(hash.to_string())
}

fn alloc_combined_logs(stdout: &[u8], stderr: &[u8]) -> String {
    let a = String::from_utf8_lossy(stdout);
    let b = String::from_utf8_lossy(stderr);
    let mut out = String::with_capacity(a.len() + b.len() + 1);
    out.push_str(&a);
    if !a.ends_with('\n') && !a.is_empty() {
        out.push('\n');
    }
    out.push_str(&b);
    out
}

// ---------------------------------------------------------------------------
// Command timeout scaling
// ---------------------------------------------------------------------------

/// Scale `--discovery-timeout` and `--duration` values in a command string
/// by the `LORA_TIMEOUT_SCALE` factor. Only active when the scale != 1.0.
fn scale_command_timeouts(command: &str) -> String {
    let scale = timeout_scale();
    if (scale - 1.0).abs() < f64::EPSILON {
        return command.to_string();
    }
    let mut result = command.to_string();
    for flag in &["--discovery-timeout", "--duration"] {
        if let Some(pos) = result.find(flag) {
            let after_flag = pos + flag.len();
            let rest = &result[after_flag..];
            // Skip whitespace between flag and value
            let trimmed = rest.trim_start();
            let ws_len = rest.len() - trimmed.len();
            // Parse the numeric value
            let num_end = trimmed
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(trimmed.len());
            if num_end > 0 {
                if let Ok(val) = trimmed[..num_end].parse::<u64>() {
                    let scaled = (val as f64 * scale).ceil() as u64;
                    let value_start = after_flag + ws_len;
                    let value_end = value_start + num_end;
                    result = format!(
                        "{}{}{}",
                        &result[..value_start],
                        scaled,
                        &result[value_end..]
                    );
                }
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Step execution
// ---------------------------------------------------------------------------

/// Run a complete test lifecycle with a guaranteed timeout.
///
/// Brings containers up, waits for readiness, executes all steps, then
/// tears down, even if the timeout fires. Logs are always saved before
/// teardown. The timeout comes from `scenario.test.timeout_secs`.
///
/// This replaces the pattern of calling `runner.up()`, `execute_steps()`,
/// `runner.down()` separately in each test function. By keeping everything
/// in one place, we guarantee that `runner.down()` is always called, even
/// when a step hangs past the deadline.
pub fn run_test(runner: &mut TestRunner) -> Result<(), StepError> {
    let timeout = Duration::from_secs(runner.scenario().test.timeout_secs);
    let deadline = Instant::now() + timeout;

    runner.up().map_err(|e| StepError::StepFailed {
        step_index: 0,
        action: "up".into(),
        detail: e.to_string(),
    })?;

    let ready_timeout = 60.min(timeout.as_secs());
    runner.wait_ready(ready_timeout).map_err(|e| {
        let _ = runner.down();
        StepError::StepFailed {
            step_index: 0,
            action: "wait_ready".into(),
            detail: e.to_string(),
        }
    })?;

    let result = execute_steps_with_deadline(runner, deadline);

    // Always tear down, even after failure or timeout.
    if let Err(e) = runner.down() {
        eprintln!("[run_test] down failed: {e}");
    }

    result
}

/// Execute steps with a hard deadline. If the deadline passes between steps,
/// the remaining steps are skipped and an error is returned.
fn execute_steps_with_deadline(runner: &TestRunner, deadline: Instant) -> Result<(), StepError> {
    let steps = runner.scenario().steps.clone();
    let total = steps.len();
    let mut cache = BTreeMap::new();

    for (i, step) in steps.iter().enumerate() {
        if Instant::now() >= deadline {
            let _ = runner.collect_logs();
            return Err(StepError::StepFailed {
                step_index: i,
                action: format!("{:?}", step),
                detail: format!(
                    "test timeout ({}s) expired before step {}/{}",
                    runner.scenario().test.timeout_secs,
                    i + 1,
                    total
                ),
            });
        }

        let step_num = i + 1;
        if let Err(e) = execute_step(runner, i, step, &mut cache, step_num, total) {
            report_failure(runner, step_num, total, step, &e);
            return Err(e);
        }
    }

    let _ = runner.collect_logs();
    Ok(())
}

/// Execute all steps from the scenario against running containers.
pub fn execute_steps(runner: &TestRunner) -> Result<(), StepError> {
    let steps = runner.scenario().steps.clone();
    let total = steps.len();
    let mut cache = BTreeMap::new();

    for (i, step) in steps.iter().enumerate() {
        let step_num = i + 1;
        if let Err(e) = execute_step(runner, i, step, &mut cache, step_num, total) {
            report_failure(runner, step_num, total, step, &e);
            return Err(e);
        }
    }

    // Always collect container logs, useful for post-mortem even on success.
    let _ = runner.collect_logs();

    Ok(())
}

/// Print formatted failure report with log collection.
fn report_failure(
    runner: &TestRunner,
    step_num: usize,
    total: usize,
    step: &Step,
    error: &StepError,
) {
    let sep = "══════════════════════════════════════════════════";
    let thin = "──────────────────────────────────────────────────";

    eprintln!("\n{sep}");
    eprintln!("STEP FAILED: [{step_num}/{total}] {step:?}");
    eprintln!("{}", error);
    eprintln!("{sep}");

    match runner.collect_logs() {
        Ok(log_path) => eprintln!("Logs saved to: {}", log_path.display()),
        Err(e) => eprintln!("Failed to collect logs: {e}"),
    }
    eprintln!("{thin}");

    for name in runner.node_names() {
        match runner.container_logs_tail(name, 50) {
            Ok(tail) => {
                eprintln!("Last 50 lines from {name}:");
                for line in tail.lines() {
                    eprintln!("  {line}");
                }
            }
            Err(e) => eprintln!("Failed to get logs for {name}: {e}"),
        }
        eprintln!("{thin}");
    }

    eprintln!("{sep}\n");
}

fn execute_step(
    runner: &TestRunner,
    index: usize,
    step: &Step,
    cache: &mut BTreeMap<String, String>,
    step_num: usize,
    total: usize,
) -> Result<(), StepError> {
    match step {
        Step::WaitForPath {
            on,
            destination,
            timeout_secs,
            expect_result,
        } => {
            println!("[{step_num}/{total}] wait_for_path on {on} for {destination}...");
            execute_wait_for_path(
                runner,
                index,
                on,
                destination,
                scale_timeout(*timeout_secs),
                expect_result,
                cache,
            )
        }
        Step::RnProbe {
            from,
            to,
            expect_hops,
            expect_hops_max,
            expect_result,
            timeout_secs,
        } => {
            println!("[{step_num}/{total}] rnprobe from {from} to {to}...");
            execute_rnprobe(
                runner,
                index,
                from,
                to,
                expect_hops.as_ref().copied(),
                expect_hops_max.as_ref().copied(),
                expect_result,
                scale_timeout(*timeout_secs),
                cache,
            )
        }
        Step::Sleep { duration_secs } => {
            let scaled = scale_timeout(*duration_secs);
            println!("[{step_num}/{total}] sleep {scaled}s...");
            thread::sleep(Duration::from_secs(scaled));
            Ok(())
        }
        Step::RnPath { .. } => {
            unimplemented!("step type 'rnpath' — see Codeberg issues")
        }
        Step::RnStatus { .. } => {
            unimplemented!("step type 'rnstatus' — see Codeberg issues")
        }
        Step::Exec {
            on,
            command,
            expect_exit_code,
            expect_exit_code_any,
            expect_stdout_contains,
            env,
        } => {
            // Scale --discovery-timeout and --duration values in command strings
            // when LORA_TIMEOUT_SCALE is set (for running tests at different bitrates).
            let scaled_command = scale_command_timeouts(command);
            println!("[{step_num}/{total}] exec on {on}: {scaled_command}...");
            let args: Vec<&str> = scaled_command.split_whitespace().collect();
            let env_pairs: Vec<(&str, &str)> =
                env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let output = if env_pairs.is_empty() {
                runner.docker_exec(on, &args)?
            } else {
                runner.docker_exec_with_env(on, &args, &env_pairs)?
            };

            // Always persist exec output for post-mortem analysis.
            let label = format!("exec_{on}_{step_num}");
            let _ = runner.save_exec_output(&label, &output.stdout, &output.stderr);

            let stdout = String::from_utf8_lossy(&output.stdout);
            let code = output.status.code().unwrap_or(-1);

            if let Some(expected) = expect_exit_code {
                if code != *expected {
                    return Err(StepError::StepFailed {
                        step_index: index,
                        action: format!("exec on {on}"),
                        detail: format!(
                            "exit code {code}, expected {expected}\nstdout: {stdout}\nstderr: {}",
                            String::from_utf8_lossy(&output.stderr)
                        ),
                    });
                }
            }

            if !expect_exit_code_any.is_empty() && !expect_exit_code_any.contains(&code) {
                return Err(StepError::StepFailed {
                    step_index: index,
                    action: format!("exec on {on}"),
                    detail: format!(
                        "exit code {code}, expected one of {expect_exit_code_any:?}\nstdout: {stdout}\nstderr: {}",
                        String::from_utf8_lossy(&output.stderr)
                    ),
                });
            }

            if let Some(needle) = expect_stdout_contains {
                if !stdout.contains(needle.as_str()) {
                    return Err(StepError::StepFailed {
                        step_index: index,
                        action: format!("exec on {on}"),
                        detail: format!("stdout does not contain '{needle}'\nstdout: {stdout}"),
                    });
                }
            }

            println!("  exec ok (exit code {code})");
            Ok(())
        }
        Step::Restart { node } => {
            println!("[{step_num}/{total}] restart {node}...");
            let container = runner.container_name(node);
            let output = std::process::Command::new("docker")
                .args(["restart", &container])
                .output()
                .map_err(|e| StepError::Runner(RunnerError::Io(e)))?;
            if !output.status.success() {
                return Err(StepError::StepFailed {
                    step_index: index,
                    action: "restart".into(),
                    detail: format!(
                        "docker restart {container} failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    ),
                });
            }
            runner.wait_ready_single(node, 30)?;
            println!("  {node} restarted and ready");
            Ok(())
        }
        Step::BlockLink { from, to } => {
            println!("[{step_num}/{total}] block_link {from} -> {to}...");
            execute_iptables_rule(runner, index, from, to, "-A")
        }
        Step::RestoreLink { from, to } => {
            println!("[{step_num}/{total}] restore_link {from} -> {to}...");
            execute_iptables_rule(runner, index, from, to, "-D")
        }
        Step::ProxyRule { node, rule } => {
            println!("[{step_num}/{total}] proxy_rule on {node}: {rule}...");
            execute_proxy_cmd(runner, index, node, &format!("rule add {rule}"))
        }
        Step::ProxyRuleClear { node, id } => {
            println!("[{step_num}/{total}] proxy_rule_clear on {node}: {id}...");
            execute_proxy_cmd(runner, index, node, &format!("rule clear {id}"))
        }
        Step::ProxyStats {
            node,
            expect_dropped,
            expect_forwarded,
        } => {
            println!("[{step_num}/{total}] proxy_stats on {node}...");
            execute_proxy_stats(runner, index, node, *expect_dropped, *expect_forwarded)
        }
        Step::FileTransfer {
            sender,
            receiver,
            sender_tool,
            receiver_tool,
            file_sizes,
            direction,
            repeats,
            timeout_secs,
            mode,
            receiver_flags,
            sender_flags,
            auth_from,
            expect_result,
            fetch_path,
        } => {
            println!(
                "[{step_num}/{total}] file_transfer {sender} <-> {receiver} ({sender_tool}/{receiver_tool}, mode={mode})..."
            );
            execute_file_transfer(
                runner,
                index,
                sender,
                receiver,
                sender_tool,
                receiver_tool,
                file_sizes,
                direction,
                *repeats,
                *timeout_secs,
                mode,
                receiver_flags,
                sender_flags,
                auth_from,
                expect_result,
                fetch_path,
            )
        }
        Step::ParallelFileTransfers {
            transfers,
            timeout_secs,
            expect_result,
        } => {
            println!(
                "[{step_num}/{total}] parallel_file_transfers ({} transfers)...",
                transfers.len()
            );
            let result = execute_parallel_file_transfers(runner, index, transfers, *timeout_secs);
            let expect_fail = expect_result == "failure";
            match (expect_fail, &result) {
                (true, Err(_)) => {
                    println!("  parallel transfers failed as expected");
                    Ok(())
                }
                (true, Ok(())) => Err(StepError::StepFailed {
                    step_index: index,
                    action: "parallel_file_transfers".into(),
                    detail: "expected failure but all transfers succeeded".into(),
                }),
                (false, _) => result,
            }
        }
        Step::Benchmark {
            pairs,
            duration_secs,
            label,
        } => {
            let profile = label.as_str();
            println!(
                "[{step_num}/{total}] benchmark: {} pairs, {}s, profile={}",
                pairs.len(),
                duration_secs,
                if profile.is_empty() {
                    "default"
                } else {
                    profile
                }
            );
            execute_benchmark(runner, index, pairs, *duration_secs, label)
        }
        Step::LxmfStart {
            on,
            display_name,
            timeout_secs,
        } => {
            let dn = display_name.as_deref().unwrap_or(on);
            println!("[{step_num}/{total}] lxmf_start on {on} (display_name={dn})...");
            execute_lxmf_start(runner, index, on, dn, scale_timeout(*timeout_secs), cache)
        }
        Step::LxmfAnnounce { on } => {
            println!("[{step_num}/{total}] lxmf_announce on {on}...");
            execute_lxmf_announce(runner, index, on)
        }
        Step::LxmfWaitForPeer {
            on,
            peer,
            timeout_secs,
        } => {
            println!("[{step_num}/{total}] lxmf_wait_for_peer on {on} for {peer}...");
            execute_lxmf_wait_for_peer(runner, index, on, peer, scale_timeout(*timeout_secs), cache)
        }
        Step::LxmfSend { from, to, body } => {
            println!(
                "[{step_num}/{total}] lxmf_send {from} -> {to} ({} bytes)...",
                body.len()
            );
            execute_lxmf_send(runner, index, from, to, body, cache)
        }
        Step::LxmfAssertReceived {
            on,
            from,
            body,
            timeout_secs,
        } => {
            println!(
                "[{step_num}/{total}] lxmf_assert_received on {on} from {from} ({} bytes)...",
                body.len()
            );
            execute_lxmf_assert_received(
                runner,
                index,
                on,
                from,
                body,
                scale_timeout(*timeout_secs),
                cache,
            )
        }
        Step::LxmfStop { on } => {
            println!("[{step_num}/{total}] lxmf_stop on {on}...");
            execute_lxmf_stop(runner, index, on)
        }
        Step::StartSharedClient {
            on,
            log_path,
            timeout_secs,
        } => {
            println!("[{step_num}/{total}] start_shared_client (rnsd) on {on}...");
            execute_start_shared_client(runner, index, on, log_path, scale_timeout(*timeout_secs))
        }
    }
}

/// rnsd's warning when it attaches to an existing shared instance instead of
/// starting its own transport (vendor/Reticulum/RNS/Utilities/rnsd.py). Its
/// presence proves rnsd is a CLIENT of the running daemon.
const RNSD_ATTACHED_MARKER: &str = "connected to another shared local instance";

/// Our Rust lnsd shared-instance SERVER log when it accepts a local client
/// (leviculum-std/src/interfaces/local.rs). Its presence proves the server
/// accepted rnsd's persistent connection.
const LNSD_CLIENT_ACCEPTED_MARKER: &str = "Local client connected";

/// Launch a Python rnsd inside `on`'s container as a persistent shared-instance
/// client of the Rust lnsd already running there, then verify the attach from
/// both sides: rnsd logs it connected to the existing instance (and did NOT
/// start its own transport), and lnsd logs it accepted the client.
fn execute_start_shared_client(
    runner: &TestRunner,
    index: usize,
    on: &str,
    log_path: &str,
    timeout_secs: u64,
) -> Result<(), StepError> {
    let container = runner.container_name(on);

    // Launch rnsd detached. It uses the same config dir as lnsd, so its
    // attempt to bind the shared-instance socket fails (lnsd holds it) and RNS
    // falls back to connecting as a client. RNS_REQUIRE_SHARED=1 (set on every
    // container) makes rnsd abort loudly rather than silently run standalone if
    // the attach fails, so a green attach is unambiguous. PYTHONUNBUFFERED=1
    // keeps the log flushing line by line for the poll below.
    let launch = format!(
        "PYTHONUNBUFFERED=1 python3 -m RNS.Utilities.rnsd -v --config /root/.reticulum > {log_path} 2>&1"
    );
    let launch_output = std::process::Command::new("docker")
        .args(["exec", "-d", &container, "sh", "-c", &launch])
        .output()
        .map_err(|e| StepError::Runner(RunnerError::Io(e)))?;
    if !launch_output.status.success() {
        return Err(StepError::StepFailed {
            step_index: index,
            action: format!("start_shared_client on {on}"),
            detail: format!(
                "failed to launch rnsd: {}",
                String::from_utf8_lossy(&launch_output.stderr)
            ),
        });
    }
    println!("  rnsd launched on {on}, polling for attach (timeout {timeout_secs}s)...");

    // Poll rnsd's log until it reports the attach, or a known failure marker
    // appears, or we time out.
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut rnsd_log = String::new();
    let mut attached = false;
    while Instant::now() < deadline {
        if let Ok(out) = runner.docker_exec(on, &["cat", log_path]) {
            rnsd_log = String::from_utf8_lossy(&out.stdout).into_owned();
        }
        if rnsd_log.contains(RNSD_ATTACHED_MARKER) {
            attached = true;
            break;
        }
        // Fail fast on the unambiguous "did not attach" outcomes rather than
        // waiting out the whole timeout.
        if rnsd_log.contains("Aborting startup")
            || rnsd_log.contains("could not be connected")
            || rnsd_log.contains("Traceback")
            || rnsd_log.contains("refusing to run as standalone")
        {
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }

    let _ = runner.save_exec_output(&format!("rnsd_client_{on}"), rnsd_log.as_bytes(), &[]);

    if !attached {
        let lnsd_log = runner
            .docker_logs(on)
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        return Err(StepError::StepFailed {
            step_index: index,
            action: format!("start_shared_client on {on}"),
            detail: format!(
                "rnsd did not attach to the shared instance within {timeout_secs}s \
                 (no '{RNSD_ATTACHED_MARKER}' in its log)\n\
                 --- rnsd log ---\n{rnsd_log}\n\
                 --- lnsd ({on}) log tail ---\n{}",
                tail_lines(&lnsd_log, 40)
            ),
        });
    }
    println!("  rnsd attached as shared-instance client (rnsd log confirms)");

    // Server-side proof: lnsd accepted the persistent client connection.
    let lnsd_log = runner
        .docker_logs(on)
        .map(|o| {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            s
        })
        .unwrap_or_default();
    if !lnsd_log.contains(LNSD_CLIENT_ACCEPTED_MARKER) {
        return Err(StepError::StepFailed {
            step_index: index,
            action: format!("start_shared_client on {on}"),
            detail: format!(
                "rnsd logged attach, but lnsd never logged '{LNSD_CLIENT_ACCEPTED_MARKER}' \
                 (server did not accept the persistent client)\n\
                 --- lnsd ({on}) log tail ---\n{}",
                tail_lines(&lnsd_log, 40)
            ),
        });
    }
    println!("  lnsd accepted the persistent client (server log confirms)");
    Ok(())
}

/// Return the last `n` lines of `s`, for compact failure reports.
fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Run probe throughput benchmark: spawn parallel probe loops, collect stats.
fn execute_benchmark(
    runner: &TestRunner,
    _step_index: usize,
    pairs: &[crate::topology::BenchmarkPair],
    duration_secs: u64,
    label: &str,
) -> Result<(), StepError> {
    let radio = runner.scenario().radio.as_ref();

    // Compute min probe interval: 4x single-packet airtime accounts for
    // probe + reply airtimes + processing margin. Floor at 800ms.
    // With persistent RNS connection, each probe is 2 radio TX (no path re-request).
    let min_interval_ms = if let Some(r) = radio {
        let airtime =
            leviculum_core::rnode::airtime_ms(500, r.bandwidth, r.spreading_factor, r.coding_rate);
        (airtime * 4).max(800)
    } else {
        3000
    };
    // Per-probe timeout: 8x airtime, clamped to 10-30s. Long enough for a
    // full round-trip under contention, short enough that one lost probe
    // doesn't waste the entire benchmark window.
    let probe_timeout_s = if let Some(r) = radio {
        let airtime =
            leviculum_core::rnode::airtime_ms(500, r.bandwidth, r.spreading_factor, r.coding_rate);
        ((airtime * 8) / 1000).clamp(10, 30)
    } else {
        15
    };

    let min_interval_s = min_interval_ms as f64 / 1000.0;
    println!("  min probe interval: {min_interval_s:.1}s, probe timeout: {probe_timeout_s}s");

    // Resolve destination hashes
    let mut dest_cache = BTreeMap::new();
    let mut resolved_pairs: Vec<(&str, &str, String)> = Vec::new();
    for pair in pairs {
        let hash = resolve_destination(runner, &pair.to, &mut dest_cache)?;
        resolved_pairs.push((&pair.from, &pair.to, hash));
    }

    // Spawn benchmark threads in parallel
    let n = pairs.len();
    let (tx, rx) = mpsc::channel();
    let timeout = Duration::from_secs(duration_secs + 120);

    std::thread::scope(|s| {
        for (i, (from, _to, hash)) in resolved_pairs.iter().enumerate() {
            let tx = tx.clone();
            s.spawn(move || {
                let output = runner.docker_exec(
                    from,
                    &[
                        "python3",
                        "/opt/integ-scripts/probe_benchmark.py",
                        "/root/.reticulum",
                        hash,
                        &duration_secs.to_string(),
                        &format!("{min_interval_s:.1}"),
                        &probe_timeout_s.to_string(),
                    ],
                );
                let _ = tx.send((i, output));
            });
        }
        drop(tx);

        // Collect results
        let mut results: Vec<(usize, Option<serde_json::Value>)> = Vec::new();
        for _ in 0..n {
            match rx.recv_timeout(timeout) {
                Ok((i, Ok(output))) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    if !stderr.is_empty() {
                        eprintln!("  pair {i} stderr:\n{stderr}");
                    }
                    // Find the last JSON line in stdout
                    let json_val = stdout
                        .lines()
                        .rev()
                        .find_map(|line| serde_json::from_str::<serde_json::Value>(line).ok());
                    results.push((i, json_val));
                }
                Ok((i, Err(e))) => {
                    eprintln!("  pair {i}: docker_exec error: {e}");
                    results.push((i, None));
                }
                Err(_) => break,
            }
        }
        results.sort_by_key(|(i, _)| *i);

        // Print results table
        let profile_label = if label.is_empty() { "default" } else { label };
        println!(
            "\n=== Benchmark: probe_throughput ({}s, {}) ===",
            duration_secs, profile_label
        );
        println!(
            "{:<22} {:>5} {:>5} {:>5} {:>6} {:>9} {:>9}",
            "Pair", "Sent", "Recv", "Lost", "PDR", "Avg RTT", "P95 RTT"
        );

        let mut total_sent = 0u64;
        let mut total_recv = 0u64;
        let mut all_rtts: Vec<f64> = Vec::new();

        for (i, (_, json)) in results.iter().enumerate() {
            let from = resolved_pairs[i].0;
            let to = resolved_pairs[i].1;
            let pair_name = format!("{from} -> {to}");

            if let Some(val) = json {
                let sent = val["sent"].as_u64().unwrap_or(0);
                let recv = val["received"].as_u64().unwrap_or(0);
                let lost = val["lost"].as_u64().unwrap_or(0);
                let pdr = if sent > 0 {
                    recv as f64 / sent as f64 * 100.0
                } else {
                    0.0
                };
                let avg_rtt = val["avg_rtt_ms"].as_f64().unwrap_or(0.0);
                let p95_rtt = val["p95_rtt_ms"].as_f64().unwrap_or(0.0);

                total_sent += sent;
                total_recv += recv;
                if let Some(rtts) = val["rtts_ms"].as_array() {
                    for r in rtts {
                        if let Some(v) = r.as_f64() {
                            all_rtts.push(v);
                        }
                    }
                }

                println!(
                    "{:<22} {:>5} {:>5} {:>5} {:>5.1}% {:>7.0}ms {:>7.0}ms",
                    pair_name, sent, recv, lost, pdr, avg_rtt, p95_rtt
                );
            } else {
                println!("{:<22}  ERROR — no results", pair_name);
            }
        }

        // Total row
        if results.len() > 1 {
            let total_pdr = if total_sent > 0 {
                total_recv as f64 / total_sent as f64 * 100.0
            } else {
                0.0
            };
            all_rtts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let avg_rtt = if all_rtts.is_empty() {
                0.0
            } else {
                all_rtts.iter().sum::<f64>() / all_rtts.len() as f64
            };
            let p95_rtt = if all_rtts.is_empty() {
                0.0
            } else {
                let idx = ((all_rtts.len() as f64) * 0.95) as usize;
                all_rtts[idx.min(all_rtts.len() - 1)]
            };
            println!("{}", "-".repeat(70));
            println!(
                "{:<22} {:>5} {:>5} {:>5} {:>5.1}% {:>7.0}ms {:>7.0}ms",
                "Total",
                total_sent,
                total_recv,
                total_sent - total_recv,
                total_pdr,
                avg_rtt,
                p95_rtt
            );
        }
        println!();

        // Save JSON results
        let benchmarks_dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("benchmarks");
        let _ = std::fs::create_dir_all(&benchmarks_dir);
        let ts = chrono_timestamp();
        let filename = format!("bench_{profile_label}_{ts}.json");
        let json_results: Vec<_> = results
            .iter()
            .enumerate()
            .map(|(i, (_, json))| {
                serde_json::json!({
                    "pair": format!("{} -> {}", resolved_pairs[i].0, resolved_pairs[i].1),
                    "results": json,
                })
            })
            .collect();
        let output_json = serde_json::json!({
            "label": profile_label,
            "duration_secs": duration_secs,
            "min_interval_ms": min_interval_ms,
            "radio": radio.map(|r| serde_json::json!({
                "frequency": r.frequency,
                "bandwidth": r.bandwidth,
                "sf": r.spreading_factor,
                "cr": r.coding_rate,
                "txpower": r.tx_power,
            })),
            "pairs": json_results,
        });
        let path = benchmarks_dir.join(&filename);
        if let Ok(json_str) = serde_json::to_string_pretty(&output_json) {
            let _ = std::fs::write(&path, json_str);
            println!("  results saved to {}", path.display());
        }
    });

    // Check that at least some probes succeeded
    Ok(())
}

fn chrono_timestamp() -> String {
    use std::time::SystemTime;
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

/// Add or delete iptables DROP rules to block/restore traffic between containers.
///
/// `action` is `-A` (append/block) or `-D` (delete/restore).
/// Resolves the target container's IP via `getent hosts` since iptables
/// requires numeric IPs, not hostnames.
fn execute_iptables_rule(
    runner: &TestRunner,
    step_index: usize,
    from: &str,
    to: &str,
    action: &str,
) -> Result<(), StepError> {
    let cmd = format!(
        "iptables {action} INPUT -s $(getent hosts {to} | awk '{{print $1}}') -j DROP && \
         iptables {action} OUTPUT -d $(getent hosts {to} | awk '{{print $1}}') -j DROP"
    );
    let output = runner.docker_exec(from, &["sh", "-c", &cmd])?;
    if !output.status.success() {
        return Err(StepError::StepFailed {
            step_index,
            action: format!("iptables {action} on {from} targeting {to}"),
            detail: format!(
                "exit code {}, stderr: {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }
    let verb = if action == "-A" {
        "blocked"
    } else {
        "restored"
    };
    println!("  {verb} {from} <-> {to}");
    Ok(())
}

// ---------------------------------------------------------------------------
// LXMF step handlers
// ---------------------------------------------------------------------------

/// Cache key for the LXMF destination hash of a node's helper.
fn lxmf_cache_key(node: &str) -> String {
    format!("lxmf_hash:{node}")
}

fn lookup_peer_hash(
    cache: &BTreeMap<String, String>,
    peer: &str,
    action: &str,
    index: usize,
) -> Result<String, StepError> {
    cache
        .get(&lxmf_cache_key(peer))
        .cloned()
        .ok_or_else(|| StepError::StepFailed {
            step_index: index,
            action: action.into(),
            detail: format!("no cached LXMF hash for '{peer}' — run lxmf_start on '{peer}' first"),
        })
}

fn execute_lxmf_start(
    runner: &TestRunner,
    index: usize,
    on: &str,
    display_name: &str,
    timeout_secs: u64,
    cache: &mut BTreeMap<String, String>,
) -> Result<(), StepError> {
    let helper = runner
        .lxmf_spawn(on, display_name)
        .map_err(|e| StepError::StepFailed {
            step_index: index,
            action: format!("lxmf_start on {on}"),
            detail: e.to_string(),
        })?;

    let since = Instant::now();
    let event = helper
        .wait_for_event(
            |ev| ev.name == "lxmf_ready",
            since,
            Duration::from_secs(timeout_secs),
        )
        .ok_or_else(|| StepError::StepFailed {
            step_index: index,
            action: format!("lxmf_start on {on}"),
            detail: format!("no EVENT lxmf_ready received within {timeout_secs}s"),
        })?;

    let hash = event
        .fields
        .get("hash")
        .cloned()
        .ok_or_else(|| StepError::StepFailed {
            step_index: index,
            action: format!("lxmf_start on {on}"),
            detail: "EVENT lxmf_ready has no 'hash' field".into(),
        })?;

    cache.insert(lxmf_cache_key(on), hash.clone());
    println!("  lxmf_ready on {on}: hash={hash}");
    Ok(())
}

fn execute_lxmf_announce(runner: &TestRunner, index: usize, on: &str) -> Result<(), StepError> {
    let helper = runner.lxmf_helper(on)?;
    let since = Instant::now();
    helper
        .send_command("announce")
        .map_err(|e| StepError::StepFailed {
            step_index: index,
            action: format!("lxmf_announce on {on}"),
            detail: e.to_string(),
        })?;
    // Confirm the helper observed and emitted the announce; otherwise we'd
    // race ahead while the helper hasn't even processed the stdin line yet.
    helper
        .wait_for_event(
            |ev| ev.name == "lxmf_announce_sent",
            since,
            Duration::from_secs(5),
        )
        .ok_or_else(|| StepError::StepFailed {
            step_index: index,
            action: format!("lxmf_announce on {on}"),
            detail: "no lxmf_announce_sent event within 5s".into(),
        })?;
    println!("  announce sent on {on}");
    Ok(())
}

fn execute_lxmf_wait_for_peer(
    runner: &TestRunner,
    index: usize,
    on: &str,
    peer: &str,
    timeout_secs: u64,
    cache: &mut BTreeMap<String, String>,
) -> Result<(), StepError> {
    let action = format!("lxmf_wait_for_peer on {on} for {peer}");
    let peer_hash = lookup_peer_hash(cache, peer, &action, index)?;

    let helper = runner.lxmf_helper(on)?;
    let since = Instant::now();
    helper
        .send_command(&format!("wait_for_peer {peer_hash} {timeout_secs}"))
        .map_err(|e| StepError::StepFailed {
            step_index: index,
            action: action.clone(),
            detail: e.to_string(),
        })?;

    let peer_hash_match = peer_hash.clone();
    let event = helper
        .wait_for_event(
            move |ev| {
                (ev.name == "lxmf_wait_for_peer_ok" || ev.name == "lxmf_wait_for_peer_timeout")
                    && ev.fields.get("peer") == Some(&peer_hash_match)
            },
            since,
            Duration::from_secs(timeout_secs + 5),
        )
        .ok_or_else(|| StepError::StepFailed {
            step_index: index,
            action: action.clone(),
            detail: format!(
                "no wait_for_peer_{{ok,timeout}} event within {}s grace",
                timeout_secs + 5
            ),
        })?;

    if event.name == "lxmf_wait_for_peer_ok" {
        println!("  {on} learned {peer} ({peer_hash})");
        Ok(())
    } else {
        Err(StepError::StepFailed {
            step_index: index,
            action,
            detail: format!(
                "{on} did not learn identity AND path for {peer} ({peer_hash}) within {timeout_secs}s"
            ),
        })
    }
}

fn execute_lxmf_send(
    runner: &TestRunner,
    index: usize,
    from: &str,
    to: &str,
    body: &str,
    cache: &mut BTreeMap<String, String>,
) -> Result<(), StepError> {
    let action = format!("lxmf_send {from} -> {to}");
    let to_hash = lookup_peer_hash(cache, to, &action, index)?;
    let body_b64 = B64.encode(body.as_bytes());

    let helper = runner.lxmf_helper(from)?;
    let since = Instant::now();
    helper
        .send_command(&format!("send {to_hash} {body_b64}"))
        .map_err(|e| StepError::StepFailed {
            step_index: index,
            action: action.clone(),
            detail: e.to_string(),
        })?;

    let to_hash_match = to_hash.clone();
    let body_b64_match = body_b64.clone();
    let event = helper
        .wait_for_event(
            move |ev| {
                if ev.name == "lxmf_error" {
                    return true;
                }
                ev.name == "lxmf_msg_sent"
                    && ev.fields.get("dst") == Some(&to_hash_match)
                    && ev.fields.get("body_b64") == Some(&body_b64_match)
            },
            since,
            Duration::from_secs(10),
        )
        .ok_or_else(|| StepError::StepFailed {
            step_index: index,
            action: action.clone(),
            detail: "no lxmf_msg_sent event within 10s".into(),
        })?;

    if event.name == "lxmf_error" {
        return Err(StepError::StepFailed {
            step_index: index,
            action,
            detail: format!(
                "helper reported error: {}",
                event.fields.get("detail").cloned().unwrap_or_default()
            ),
        });
    }
    println!("  queued {} bytes for delivery to {to}", body.len());
    Ok(())
}

fn execute_lxmf_assert_received(
    runner: &TestRunner,
    index: usize,
    on: &str,
    from: &str,
    body: &str,
    timeout_secs: u64,
    cache: &mut BTreeMap<String, String>,
) -> Result<(), StepError> {
    let action = format!("lxmf_assert_received on {on} from {from}");
    let from_hash = lookup_peer_hash(cache, from, &action, index)?;
    let body_b64 = B64.encode(body.as_bytes());

    let helper = runner.lxmf_helper(on)?;

    // The matching message may have arrived before this step started; we
    // therefore scan from the helper's beginning, not from `now`. Saturate
    // to the helper's earliest possible Instant by going one hour back.
    let since = Instant::now()
        .checked_sub(Duration::from_secs(3600))
        .unwrap_or_else(Instant::now);

    let from_hash_match = from_hash.clone();
    let body_b64_match = body_b64.clone();
    helper
        .wait_for_event(
            move |ev| {
                ev.name == "lxmf_msg_received"
                    && ev.fields.get("src") == Some(&from_hash_match)
                    && ev.fields.get("body_b64") == Some(&body_b64_match)
            },
            since,
            Duration::from_secs(timeout_secs),
        )
        .ok_or_else(|| StepError::StepFailed {
            step_index: index,
            action: action.clone(),
            detail: format!(
                "no matching lxmf_msg_received event within {timeout_secs}s (src={from_hash}, body_b64={body_b64})"
            ),
        })
        .map(|event| {
            let sig = event
                .fields
                .get("sig_valid")
                .map(|s| s.as_str())
                .unwrap_or("?");
            let enc = event
                .fields
                .get("transport_encryption")
                .map(|s| s.as_str())
                .unwrap_or("?");
            println!("  {on} received from {from}: sig_valid={sig} encryption={enc}");
        })
}

fn execute_lxmf_stop(runner: &TestRunner, _index: usize, on: &str) -> Result<(), StepError> {
    if let Some(helper) = runner.lxmf_remove(on) {
        helper.shutdown();
        println!("  {on} helper stopped");
    } else {
        println!("  {on} helper already absent (no-op)");
    }
    Ok(())
}

fn execute_wait_for_path(
    runner: &TestRunner,
    index: usize,
    on: &str,
    destination: &str,
    timeout_secs: u64,
    expect_result: &str,
    cache: &mut BTreeMap<String, String>,
) -> Result<(), StepError> {
    let hash = resolve_destination(runner, destination, cache)?;

    // For success we retry up to 3 times. rnpath sends ONE path request and
    // then polls has_path() until its `-w` timeout. If the path response is
    // dropped or the tunnel isn't fully established, the wait is wasted.
    // Splitting `timeout_secs` into multiple shorter attempts re-triggers
    // a fresh path request each round, which is materially more reliable
    // in all-Python topologies where the rnsd network can take a while to
    // settle.
    //
    // For no_path we keep a single attempt with the full timeout, we want
    // to give the path the longest possible chance to NOT appear.
    let want_success = expect_result == "success";
    let attempts: u64 = if want_success { 3 } else { 1 };
    let per_attempt = (timeout_secs / attempts).max(20);

    let mut last_stdout = String::new();
    let mut last_stderr = String::new();
    let mut last_status = String::new();

    for attempt in 1..=attempts {
        let output = runner.docker_exec(
            on,
            &[
                "rnpath",
                &hash,
                "--config",
                "/root/.reticulum",
                "-w",
                &per_attempt.to_string(),
            ],
        )?;

        match (expect_result, output.status.success()) {
            ("success", true) => {
                if attempt > 1 {
                    println!("  path resolved on attempt {attempt}: {hash}");
                } else {
                    println!("  path resolved: {hash}");
                }
                return Ok(());
            }
            ("no_path", false) => {
                println!("  no path (expected): {hash}");
                return Ok(());
            }
            ("no_path", true) => {
                // The assertion is unchanged: a resolved path is still a
                // failure. We only capture forensics first, because this
                // branch is NOT a timeout and so the wedge-capture script
                // never fires here. Without this dump a rare nightly
                // no_path failure leaves no record of HOW the path arrived.
                dump_no_path_forensics(runner, index, on, destination, &hash, &output);
                return Err(StepError::StepFailed {
                    step_index: index,
                    action: "wait_for_path".into(),
                    detail: format!("expected no_path but path was resolved for {hash}"),
                });
            }
            ("success", false) => {
                last_stderr = String::from_utf8_lossy(&output.stderr).to_string();
                last_stdout = String::from_utf8_lossy(&output.stdout).to_string();
                last_status = output.status.to_string();
                if attempt < attempts {
                    println!("  path not found on attempt {attempt}/{attempts}, retrying...");
                }
            }
            _ => {
                return Err(StepError::StepFailed {
                    step_index: index,
                    action: "wait_for_path".into(),
                    detail: format!(
                        "unknown expect_result '{expect_result}', use 'success' or 'no_path'"
                    ),
                });
            }
        }
    }

    // Fallback: rnpath went through all attempts without seeing the path
    // in its LocalClient view. The daemon may still have learned the path
    //. Python rnsd's LocalInterface protocol occasionally drops path
    // responses to its own client, especially when many path requests
    // are interleaved. Authoritative source is the daemon's own log.
    if let Ok(out) = runner.docker_logs(on) {
        let combined = alloc_combined_logs(&out.stdout, &out.stderr);
        if has_known_path_line(&combined, &hash) {
            println!("  path resolved (daemon log): {hash}");
            return Ok(());
        }
    }

    Err(StepError::StepFailed {
        step_index: index,
        action: "wait_for_path".into(),
        detail: format!(
            "rnpath exited {last_status} after {attempts} attempts: {last_stdout} {last_stderr}"
        ),
    })
}

/// Scan a daemon's docker log for evidence that it has learned a path to
/// `hash`. Both lnsd's `[PATH_ADD]` line and Python rnsd's `Destination <X>
/// is now N hops away` / `Path to <X>` lines are recognized.
fn has_known_path_line(logs: &str, hash: &str) -> bool {
    // Be lenient on case (logs are lowercase hex) and on the hex
    // representation (rnsd wraps in `<>`, lnsd uses bare hex).
    let needle = hash.to_ascii_lowercase();
    for line in logs.lines() {
        let lower = line.to_ascii_lowercase();
        if !lower.contains(&needle) {
            continue;
        }
        if lower.contains("[path_add]") && lower.contains("ok=true") {
            return true;
        }
        if lower.contains("is now ") && lower.contains(" hops away") {
            return true;
        }
        if lower.contains("path to <") {
            return true;
        }
    }
    false
}

/// Substrings (case-insensitive) that mark a log line as evidence of HOW a
/// path was installed: an announce that carried it, a path_response, or the
/// table insertion itself. Used to build the focused extract for the
/// requesting node in the no_path forensic dump.
const PATH_INSTALL_MARKERS: &[&str] = &[
    "path_table_entry",
    "pathfound",
    "received announce",
    "path_response",
    "path response",
    "adding path",
    "path to <",
    "is now ",
    "[path_add]",
];

/// Return the lines in `logs` that mention `hash` (case-insensitive) AND
/// carry a [`PATH_INSTALL_MARKERS`] substring. These are the focused
/// evidence of how a path to `hash` was installed (announce, path_response,
/// or table insert) and on which interface.
fn extract_path_install_lines<'a>(logs: &'a str, hash: &str) -> Vec<&'a str> {
    let needle = hash.to_ascii_lowercase();
    logs.lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains(&needle) && PATH_INSTALL_MARKERS.iter().any(|m| lower.contains(m))
        })
        .collect()
}

/// Capture full forensics when a `no_path` step unexpectedly resolves a path.
///
/// This runs ONLY on the failing branch, so green runs are never touched.
/// Writes a durable, never-overwritten file under `logs/` holding every
/// node's docker logs plus a focused extract of the path-install lines for
/// the requesting node and the resolved hash, so the next nightly failure
/// finally shows how the requester learned the path. Best-effort: any IO
/// failure is reported to stdout but never shadows the assertion failure.
fn dump_no_path_forensics(
    runner: &TestRunner,
    index: usize,
    on: &str,
    destination: &str,
    hash: &str,
    rnpath_output: &std::process::Output,
) {
    let test_name = runner.scenario().test.name.clone();
    let logs_dir = TestRunner::logs_dir();
    if let Err(e) = std::fs::create_dir_all(&logs_dir) {
        println!("  NO_PATH forensics: cannot create logs dir {logs_dir:?}: {e}");
        return;
    }
    let path = logs_dir.join(format!(
        "{test_name}_NO_PATH_FAIL_{}.log",
        TestRunner::utc_timestamp()
    ));

    let mut buf = String::new();
    buf.push_str("=== NO_PATH FAILURE FORENSICS ===\n");
    buf.push_str(&format!("test: {test_name}\n"));
    buf.push_str(&format!("step_index: {index}\n"));
    buf.push_str(&format!("on (requesting node): {on}\n"));
    buf.push_str(&format!("destination: {destination}\n"));
    buf.push_str(&format!("resolved hash: {hash}\n"));
    buf.push_str(&format!(
        "rnpath status (reported success): {}\n",
        rnpath_output.status
    ));
    buf.push_str("--- rnpath stdout ---\n");
    buf.push_str(&String::from_utf8_lossy(&rnpath_output.stdout));
    buf.push_str("\n--- rnpath stderr ---\n");
    buf.push_str(&String::from_utf8_lossy(&rnpath_output.stderr));
    buf.push('\n');

    // Focused extract: HOW did the requesting node learn this hash, and on
    // which interface? Scan the requester's own log for path-install lines
    // mentioning the hash.
    buf.push_str(&format!(
        "\n=== FOCUSED EXTRACT (requester '{on}', hash {hash}) ===\n"
    ));
    match runner.docker_logs(on) {
        Ok(out) => {
            let combined = alloc_combined_logs(&out.stdout, &out.stderr);
            let hits = extract_path_install_lines(&combined, hash);
            if hits.is_empty() {
                buf.push_str(
                    "(no path-install marker line found for this hash on the requester; \
                     full requester log is included in the per-node dump below)\n",
                );
            } else {
                for line in hits {
                    buf.push_str(line);
                    buf.push('\n');
                }
            }
        }
        Err(e) => {
            buf.push_str(&format!("(docker logs failed for requester '{on}': {e})\n"));
        }
    }

    // Full per-node docker logs for every node in the scenario.
    let nodes: Vec<String> = runner.scenario().nodes.keys().cloned().collect();
    for node in &nodes {
        buf.push_str(&format!("\n========== DOCKER LOGS: {node} ==========\n"));
        match runner.docker_logs(node) {
            Ok(out) => {
                buf.push_str(&alloc_combined_logs(&out.stdout, &out.stderr));
                buf.push('\n');
            }
            Err(e) => {
                buf.push_str(&format!("(docker logs failed for '{node}': {e})\n"));
            }
        }
    }

    match std::fs::write(&path, &buf) {
        Ok(()) => println!("  NO_PATH forensics written: {}", path.display()),
        Err(e) => println!(
            "  NO_PATH forensics: failed to write {}: {e}",
            path.display()
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_rnprobe(
    runner: &TestRunner,
    index: usize,
    from: &str,
    to: &str,
    expect_hops: Option<u32>,
    expect_hops_max: Option<u32>,
    expect_result: &str,
    timeout_secs: u64,
    cache: &mut BTreeMap<String, String>,
) -> Result<(), StepError> {
    if expect_hops.is_some() && expect_hops_max.is_some() {
        return Err(StepError::StepFailed {
            step_index: index,
            action: "rnprobe".into(),
            detail: "expect_hops and expect_hops_max are mutually exclusive".into(),
        });
    }

    let hash = resolve_destination(runner, to, cache)?;
    let timeout_str = timeout_secs.to_string();
    let expect_fail = expect_result == "failure" || expect_result == "fail";

    // For expected-success probes: retry up to 3 times with 2s delay.
    // LoRa probes are fire-and-forget (no transport-level retry), so a
    // single lost packet due to half-duplex collision causes failure.
    let max_attempts = if expect_fail { 1 } else { 3 };

    let mut last_stdout = String::new();
    let mut last_stderr = String::new();
    let mut last_status = None;

    for attempt in 1..=max_attempts {
        if attempt > 1 {
            println!("  probe attempt {attempt}/{max_attempts}...");
            thread::sleep(Duration::from_secs(2));
        }

        let output = runner.docker_exec(
            from,
            &[
                "rnprobe",
                "rnstransport.probe",
                &hash,
                "--config",
                "/root/.reticulum",
                "-t",
                &timeout_str,
            ],
        )?;

        last_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        last_status = output.status.code();

        if output.status.success() {
            // Success, check hops and return
            return check_probe_hops(index, expect_hops, expect_hops_max, &last_stdout, &hash);
        }

        if attempt < max_attempts {
            println!("  probe attempt {attempt}/{max_attempts} failed, retrying...");
        }
    }

    // All attempts exhausted (or single attempt for expect_fail)
    if expect_fail {
        if last_status != Some(0) {
            println!("  probe failed (expected): {hash}");
            return Ok(());
        }
        return Err(StepError::StepFailed {
            step_index: index,
            action: "rnprobe".into(),
            detail: "expected failure but rnprobe exited successfully".into(),
        });
    }

    Err(StepError::StepFailed {
        step_index: index,
        action: "rnprobe".into(),
        detail: format!(
            "expected success but rnprobe failed after {max_attempts} attempts: {last_stdout} {last_stderr}",
        ),
    })
}

fn check_probe_hops(
    index: usize,
    expect_hops: Option<u32>,
    expect_hops_max: Option<u32>,
    stdout: &str,
    hash: &str,
) -> Result<(), StepError> {
    if let Some(expected) = expect_hops {
        let actual = parse_hops_from_output(stdout);
        match actual {
            Some(hops) if hops == expected => {
                println!("  probe ok: {hops} hop(s) to {hash}");
            }
            Some(hops) => {
                return Err(StepError::StepFailed {
                    step_index: index,
                    action: "rnprobe".into(),
                    detail: format!("expected {expected} hop(s) but got {hops}"),
                });
            }
            None => {
                return Err(StepError::StepFailed {
                    step_index: index,
                    action: "rnprobe".into(),
                    detail: format!("could not parse hop count from output: {stdout}"),
                });
            }
        }
    } else if let Some(max) = expect_hops_max {
        let actual = parse_hops_from_output(stdout);
        match actual {
            Some(hops) if hops <= max => {
                println!("  probe ok: {hops} hop(s) (≤ {max}) to {hash}");
            }
            Some(hops) => {
                return Err(StepError::StepFailed {
                    step_index: index,
                    action: "rnprobe".into(),
                    detail: format!("expected at most {max} hop(s) but got {hops}"),
                });
            }
            None => {
                return Err(StepError::StepFailed {
                    step_index: index,
                    action: "rnprobe".into(),
                    detail: format!("could not parse hop count from output: {stdout}"),
                });
            }
        }
    } else {
        println!("  probe ok: {hash}");
    }
    Ok(())
}

/// Send a command to a node's lora-proxy control socket and return the response.
fn proxy_control_cmd(runner: &TestRunner, node: &str, cmd: &str) -> Result<String, StepError> {
    let sock_path = runner
        .proxy_socket(node)
        .ok_or_else(|| StepError::StepFailed {
            step_index: 0,
            action: "proxy_cmd".into(),
            detail: format!("node '{node}' has no proxy control socket"),
        })?;

    let stream = std::os::unix::net::UnixStream::connect(sock_path).map_err(|e| {
        StepError::Runner(RunnerError::ProxyError(format!(
            "connect to proxy socket for '{node}': {e}"
        )))
    })?;

    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| StepError::Runner(RunnerError::Io(e)))?;

    let mut writer = std::io::BufWriter::new(&stream);
    writer
        .write_all(format!("{cmd}\n").as_bytes())
        .map_err(|e| StepError::Runner(RunnerError::Io(e)))?;
    writer
        .flush()
        .map_err(|e| StepError::Runner(RunnerError::Io(e)))?;
    // Shut down write half so the proxy knows we're done sending.
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|e| StepError::Runner(RunnerError::Io(e)))?;

    let mut reader = std::io::BufReader::new(&stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| StepError::Runner(RunnerError::Io(e)))?;
    Ok(line.trim().to_string())
}

/// Execute a proxy command and verify the response starts with "OK".
fn execute_proxy_cmd(
    runner: &TestRunner,
    step_index: usize,
    node: &str,
    cmd: &str,
) -> Result<(), StepError> {
    let resp = proxy_control_cmd(runner, node, cmd)?;
    if resp.starts_with("OK") {
        println!("  proxy: {resp}");
        Ok(())
    } else {
        Err(StepError::StepFailed {
            step_index,
            action: format!("proxy cmd on {node}"),
            detail: format!("expected OK, got: {resp}"),
        })
    }
}

/// Query proxy stats and optionally assert on dropped/forwarded counters.
fn execute_proxy_stats(
    runner: &TestRunner,
    step_index: usize,
    node: &str,
    expect_dropped: Option<u64>,
    expect_forwarded: Option<u64>,
) -> Result<(), StepError> {
    let resp = proxy_control_cmd(runner, node, "stats")?;
    if !resp.starts_with("OK ") {
        return Err(StepError::StepFailed {
            step_index,
            action: format!("proxy stats on {node}"),
            detail: format!("expected OK {{...}}, got: {resp}"),
        });
    }
    let json = &resp[3..]; // strip "OK "
    println!("  proxy stats: {json}");

    if let Some(expected) = expect_dropped {
        let actual = parse_json_u64(json, "dropped");
        if actual != Some(expected) {
            return Err(StepError::StepFailed {
                step_index,
                action: format!("proxy stats on {node}"),
                detail: format!("expected dropped={expected}, got {:?} in {json}", actual),
            });
        }
    }

    if let Some(expected) = expect_forwarded {
        let actual = parse_json_u64(json, "forwarded");
        if actual != Some(expected) {
            return Err(StepError::StepFailed {
                step_index,
                action: format!("proxy stats on {node}"),
                detail: format!("expected forwarded={expected}, got {:?} in {json}", actual),
            });
        }
    }

    Ok(())
}

/// Simple JSON number extraction: find `"key":N` and parse N.
fn parse_json_u64(json: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{}\":", key);
    let pos = json.find(&needle)? + needle.len();
    let rest = &json[pos..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

// ---------------------------------------------------------------------------
// File transfer
// ---------------------------------------------------------------------------

struct TransferResult {
    direction: String,
    size: u64,
    repeat: u32,
    duration: Duration,
    sender_tool: String,
    receiver_tool: String,
}

/// Parse destination hash from tool `-p` output.
///
/// lncp: bare hex on stdout.
/// rncp: "Listening on : <hex>" format with prettyhexrep angle brackets.
/// Fallback: scan all lines for a 32-char hex string.
fn parse_dest_hash_from_print_identity(stdout: &str) -> Result<String, String> {
    // Try rncp format: "Listening on : <hash>"
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("Listening on : ") {
            let trimmed = rest.trim();
            let hash = trimmed.trim_start_matches('<').trim_end_matches('>');
            if hash.len() == 32 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
                return Ok(hash.to_string());
            }
        }
    }

    // Fallback: scan for any 32-char hex string (lncp bare hash or other format)
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.len() == 32 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(trimmed.to_string());
        }
    }

    Err(format!(
        "could not parse destination hash from -p output:\n{stdout}"
    ))
}

/// Parse identity hash from tool `-p` output.
///
/// lncp format: `"Identity  : aabbccdd..."` (line 2)
/// rncp format:  `"Identity     : <aabbccdd...>"` (line 1, angle-bracketed)
///
/// Finds the line starting with "Identity", strips prefix/colon/whitespace/angle
/// brackets, and validates 32 hex chars.
fn parse_identity_hash_from_print_identity(stdout: &str) -> Result<String, String> {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Identity") {
            // Strip "Identity" prefix, then optional whitespace, then ':'
            let rest = trimmed.strip_prefix("Identity").unwrap_or("");
            let rest = rest.trim_start();
            let rest = rest.strip_prefix(':').unwrap_or(rest);
            let rest = rest.trim();
            // Strip angle brackets (rncp prettyhexrep format)
            let hash = rest.trim_start_matches('<').trim_end_matches('>');
            if hash.len() == 32 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
                return Ok(hash.to_string());
            }
        }
    }
    Err(format!(
        "could not parse identity hash from -p output:\n{stdout}"
    ))
}

/// Format a file size for display.
fn format_size(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{}MB", bytes / 1_048_576)
    } else if bytes >= 1024 {
        format!("{}KB", bytes / 1024)
    } else {
        format!("{}B", bytes)
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_file_transfer(
    runner: &TestRunner,
    step_index: usize,
    sender: &str,
    receiver: &str,
    sender_tool: &str,
    receiver_tool: &str,
    file_sizes: &[u64],
    direction: &str,
    repeats: u32,
    timeout_secs: u64,
    mode: &str,
    receiver_flags: &str,
    sender_flags: &str,
    auth_from: &str,
    expect_result: &str,
    fetch_path: &str,
) -> Result<(), StepError> {
    // Resolve auth identity hash if auth_from is set
    let auth_identity_hash = if !auth_from.is_empty() {
        let auth_node_def =
            runner
                .scenario()
                .nodes
                .get(auth_from)
                .ok_or_else(|| StepError::StepFailed {
                    step_index,
                    action: "file_transfer".into(),
                    detail: format!("auth_from node '{auth_from}' not found in scenario"),
                })?;
        let auth_tool = match auth_node_def.node_type.as_str() {
            "rust" => "lncp",
            _ => "rncp",
        };
        let print_output = runner.docker_exec_with_env(
            auth_from,
            &["timeout", "30", auth_tool, "-p"],
            &[("PYTHONUNBUFFERED", "1")],
        )?;
        if !print_output.status.success() {
            return Err(StepError::StepFailed {
                step_index,
                action: "file_transfer".into(),
                detail: format!(
                    "{auth_tool} -p on {auth_from} failed (exit {}): {}",
                    print_output.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&print_output.stderr)
                ),
            });
        }
        let stdout = String::from_utf8_lossy(&print_output.stdout);
        let hash = parse_identity_hash_from_print_identity(&stdout).map_err(|e| {
            StepError::StepFailed {
                step_index,
                action: "file_transfer".into(),
                detail: format!("failed to parse identity hash from {auth_tool} -p: {e}"),
            }
        })?;
        println!("  auth identity ({auth_from}): {hash}");
        Some(hash)
    } else {
        None
    };

    // Treat any node attached to LoRa hardware (RNode OR LNode firmware
    // over USB-Serial) as "has_rnode" for sender-timeout purposes — both
    // pump packets over the same low-bandwidth LoRa link and need the
    // longer establishment + retry budget.
    let has_rnode = runner
        .scenario()
        .nodes
        .values()
        .any(|n| n.rnode || n.rnode_interfaces.is_some() || n.serial);
    let mut results = Vec::new();

    match direction {
        "a_to_b" => {
            execute_transfer_direction(
                runner,
                step_index,
                sender,
                receiver,
                sender_tool,
                receiver_tool,
                file_sizes,
                repeats,
                timeout_secs,
                mode,
                receiver_flags,
                sender_flags,
                auth_identity_hash.as_deref(),
                expect_result,
                fetch_path,
                &mut results,
                has_rnode,
            )?;
        }
        "b_to_a" => {
            execute_transfer_direction(
                runner,
                step_index,
                receiver,
                sender,
                receiver_tool,
                sender_tool,
                file_sizes,
                repeats,
                timeout_secs,
                mode,
                receiver_flags,
                sender_flags,
                auth_identity_hash.as_deref(),
                expect_result,
                fetch_path,
                &mut results,
                has_rnode,
            )?;
        }
        "both" => {
            execute_transfer_direction(
                runner,
                step_index,
                sender,
                receiver,
                sender_tool,
                receiver_tool,
                file_sizes,
                repeats,
                timeout_secs,
                mode,
                receiver_flags,
                sender_flags,
                auth_identity_hash.as_deref(),
                expect_result,
                fetch_path,
                &mut results,
                has_rnode,
            )?;
            execute_transfer_direction(
                runner,
                step_index,
                receiver,
                sender,
                receiver_tool,
                sender_tool,
                file_sizes,
                repeats,
                timeout_secs,
                mode,
                receiver_flags,
                sender_flags,
                auth_identity_hash.as_deref(),
                expect_result,
                fetch_path,
                &mut results,
                has_rnode,
            )?;
        }
        other => {
            return Err(StepError::StepFailed {
                step_index,
                action: "file_transfer".into(),
                detail: format!("unknown direction '{other}', use 'a_to_b', 'b_to_a', or 'both'"),
            });
        }
    }

    print_transfer_results(&results);
    save_transfer_results_json(runner, &results);

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn execute_transfer_direction(
    runner: &TestRunner,
    step_index: usize,
    send_node: &str,
    recv_node: &str,
    send_tool: &str,
    recv_tool: &str,
    file_sizes: &[u64],
    repeats: u32,
    timeout_secs: u64,
    mode: &str,
    receiver_flags: &str,
    sender_flags: &str,
    auth_identity_hash: Option<&str>,
    expect_result: &str,
    fetch_path: &str,
    results: &mut Vec<TransferResult>,
    has_rnode: bool,
) -> Result<(), StepError> {
    let is_fetch = mode == "fetch";
    let expect_failure = expect_result == "failure";
    let direction_label = format!("{send_node} -> {recv_node}");
    println!("  === {direction_label} ({send_tool} -> {recv_tool}, mode={mode}) ===");

    // 1. Get receiver destination hash
    //    PYTHONUNBUFFERED=1 is required for Python rncp: its RNS.exit() calls
    //    os._exit() which doesn't flush stdout buffers. In docker exec (non-TTY),
    //    Python's stdout is block-buffered, so print() output would be lost.
    let print_output = runner.docker_exec_with_env(
        recv_node,
        &["timeout", "30", recv_tool, "-p"],
        &[("PYTHONUNBUFFERED", "1")],
    )?;
    if !print_output.status.success() {
        return Err(StepError::StepFailed {
            step_index,
            action: "file_transfer".into(),
            detail: format!(
                "{recv_tool} -p on {recv_node} failed (exit {}): {}",
                print_output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&print_output.stderr)
            ),
        });
    }
    let stdout = String::from_utf8_lossy(&print_output.stdout);
    let dest_hash =
        parse_dest_hash_from_print_identity(&stdout).map_err(|e| StepError::StepFailed {
            step_index,
            action: "file_transfer".into(),
            detail: format!("failed to parse dest hash from {recv_tool} -p: {e}"),
        })?;
    println!("  receiver hash: {dest_hash}");

    // 2. Create save directory. The listener always needs /tmp/received (-s flag),
    //    and in fetch mode the sender also needs it for saving fetched files.
    runner.docker_exec(recv_node, &["mkdir", "-p", "/tmp/received"])?;
    if is_fetch {
        runner.docker_exec(send_node, &["mkdir", "-p", "/tmp/received"])?;
    }

    // 3. Build and start listener on receiver (detached)
    //    docker exec -d runs detached, the Docker client returns immediately.
    //    Use sh -c to redirect output to a log file for debugging.
    let container = runner.container_name(recv_node);
    let listener_cmd = build_listener_cmd(recv_tool, receiver_flags, auth_identity_hash);
    println!("  listener cmd: {listener_cmd}");
    let listener_output = std::process::Command::new("docker")
        .args(["exec", "-d", &container, "sh", "-c", &listener_cmd])
        .output()
        .map_err(|e| StepError::Runner(RunnerError::Io(e)))?;
    if !listener_output.status.success() {
        return Err(StepError::StepFailed {
            step_index,
            action: "file_transfer".into(),
            detail: format!(
                "failed to start listener on {recv_node}: {}",
                String::from_utf8_lossy(&listener_output.stderr)
            ),
        });
    }
    println!("  listener started on {recv_node}");

    // 4. Wait for announce propagation using rnpath on sender.
    //    Give the announce time to propagate before rnpath sends a path request.
    //    Without this delay, rnpath's path request arrives at the relay before the
    //    relay has learned the destination from the listener's announce, and the
    //    relay ignores it. Python rnpath only sends one path request and then polls
    //    has_path(), so if the relay never sent a path response, rnpath depends on
    //    the announce rebroadcast reaching the sender's daemon, which doesn't
    //    always happen reliably with all-Python setups.
    //    Non-LoRa: 15s. Python interpreter cold-start + RNS init + initial
    //    announce + relay rebroadcast jitter (~3s) totals ~10-12s in
    //    all-Python topologies; 5s left rncp_baseline-style tests racing.
    //    LoRa: 30s, announce airtime (~490ms) + jitter (up to ~3s) + propagation.
    let announce_wait = if has_rnode { 30 } else { 15 };
    thread::sleep(Duration::from_secs(announce_wait));
    println!("  waiting for path to {dest_hash} on {send_node}...");
    let path_output = runner.docker_exec(
        send_node,
        &[
            "rnpath",
            &dest_hash,
            "--config",
            "/root/.reticulum",
            "-w",
            "60",
        ],
    )?;
    if !path_output.status.success() {
        // Dump the listener's log for debugging
        if let Ok(out) = runner.docker_exec(recv_node, &["cat", "/tmp/recv_tool.log"]) {
            let log = String::from_utf8_lossy(&out.stdout);
            let lines: Vec<&str> = log.lines().collect();
            println!(
                "  --- {recv_tool} log on {recv_node} ({} lines, on rnpath failure) ---",
                lines.len()
            );
            for line in &lines {
                println!("  {line}");
            }
            println!("  --- end of {recv_tool} log ---");
        }
        return Err(StepError::StepFailed {
            step_index,
            action: "file_transfer".into(),
            detail: format!(
                "announce from {recv_node} did not reach {send_node} (rnpath failed): {}",
                String::from_utf8_lossy(&path_output.stderr)
            ),
        });
    }
    println!("  path found");

    // 5. For each file size, for each repeat: create, send, verify
    //
    // Per-transfer shell `timeout` wrapper. For LoRa scenarios this is
    // size-based and deliberately ultra-generous (Codeberg #53): the
    // TOML `timeout_secs` was sized when `lncp -w` still capped the
    // transfer phase; with `-w` now unlimited, the shell `timeout` is
    // the per-transfer backstop and must not false-kill a healthy-but-
    // slow LoRa transfer. Non-LoRa (fast TCP) keeps the TOML value.
    let timeout_str = if has_rnode {
        let max_size = file_sizes.iter().copied().max().unwrap_or(0);
        crate::timeout::lncp_lora_wrapper_secs(max_size, 1).to_string()
    } else {
        timeout_secs.to_string()
    };
    // For fetch mode, determine the remote file path
    let remote_file_path = if !fetch_path.is_empty() {
        fetch_path.to_string()
    } else {
        "/tmp/fetchable/test_transfer.bin".to_string()
    };

    for &size in file_sizes {
        for repeat in 1..=repeats {
            let size_label = format_size(size);
            println!("  [{size_label} run {repeat}/{repeats}]");

            // 5a. Create test file
            let (bs, count) = if size >= 1_048_576 {
                ("1048576", size / 1_048_576)
            } else {
                ("1024", size / 1024)
            };
            let count_str = count.to_string();

            // In fetch mode, create the file on recv_node (server).
            // In push mode, create on send_node (client).
            let file_create_node = if is_fetch { recv_node } else { send_node };
            let file_create_path = if is_fetch {
                remote_file_path.clone()
            } else {
                "/tmp/test_transfer.bin".to_string()
            };

            // Ensure parent directory exists
            if let Some(parent) = std::path::Path::new(&file_create_path).parent() {
                let parent_str = parent.to_string_lossy();
                if !parent_str.is_empty() {
                    runner.docker_exec(file_create_node, &["mkdir", "-p", &parent_str])?;
                }
            }

            let dd_output = runner.docker_exec(
                file_create_node,
                &[
                    "dd",
                    "if=/dev/urandom",
                    &format!("of={file_create_path}"),
                    &format!("bs={bs}"),
                    &format!("count={count_str}"),
                ],
            )?;
            if !dd_output.status.success() {
                return Err(StepError::StepFailed {
                    step_index,
                    action: "file_transfer".into(),
                    detail: format!(
                        "dd failed on {file_create_node}: {}",
                        String::from_utf8_lossy(&dd_output.stderr)
                    ),
                });
            }

            // Get expected md5
            let md5_output =
                runner.docker_exec(file_create_node, &["md5sum", &file_create_path])?;
            let expected_md5 = String::from_utf8_lossy(&md5_output.stdout)
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();

            // 5b. Transfer file
            //
            // No `-w` is passed to lncp: as of the Codeberg-#53 fix, `lncp -w`
            // defaults to unlimited (a 15 s default silently aborted slow LoRa
            // transfers — Bug #53). The transfer runs to completion; the only
            // broken-test-abort mechanisms are the per-transfer shell `timeout`
            // (`timeout_str`, size-based for LoRa scenarios — see step 5 above)
            // and the test-level `run_with_timeout` wrapper.
            let start = Instant::now();
            let send_output = if is_fetch {
                // Fetch mode: sender runs `<tool> -f <remote_path> <dest_hash> -s /tmp/received`
                let mut args: Vec<String> = vec![
                    "timeout".into(),
                    timeout_str.clone(),
                    send_tool.into(),
                    "-f".into(),
                    remote_file_path.clone(),
                    dest_hash.clone(),
                    "-s".into(),
                    "/tmp/received".into(),
                ];
                // Append extra sender flags
                for flag in sender_flags.split_whitespace() {
                    args.push(flag.into());
                }
                let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                runner.docker_exec(send_node, &args_ref)?
            } else {
                // Push mode: sender runs `<tool> <file> <dest_hash>`
                let mut args: Vec<String> = vec![
                    "timeout".into(),
                    timeout_str.clone(),
                    send_tool.into(),
                    "/tmp/test_transfer.bin".into(),
                    dest_hash.clone(),
                ];
                for flag in sender_flags.split_whitespace() {
                    args.push(flag.into());
                }
                let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                runner.docker_exec(send_node, &args_ref)?
            };
            let elapsed = start.elapsed();

            if expect_failure {
                // We expect the transfer to fail
                if send_output.status.success() {
                    return Err(StepError::StepFailed {
                        step_index,
                        action: "file_transfer".into(),
                        detail: format!(
                            "transfer unexpectedly succeeded ({size_label} run {repeat}), expected failure"
                        ),
                    });
                }
                println!(
                    "    {size_label} run {repeat}: transfer correctly failed (exit {})",
                    send_output.status.code().unwrap_or(-1)
                );
                continue;
            }

            if !send_output.status.success() {
                let stderr = String::from_utf8_lossy(&send_output.stderr);
                let stdout = String::from_utf8_lossy(&send_output.stdout);
                // Dump receiver tool log on failure for debugging
                if let Ok(out) = runner.docker_exec(recv_node, &["cat", "/tmp/recv_tool.log"]) {
                    let log = String::from_utf8_lossy(&out.stdout);
                    let lines: Vec<&str> = log.lines().collect();
                    let start_line = 0;
                    println!("  --- last {} lines of {recv_tool} log on {recv_node} (on send failure) ---", lines.len() - start_line);
                    for line in &lines[start_line..] {
                        println!("  {line}");
                    }
                    println!("  --- end of {recv_tool} log ---");
                }
                return Err(StepError::StepFailed {
                    step_index,
                    action: "file_transfer".into(),
                    detail: format!(
                        "send failed on {send_node} ({size_label} run {repeat}): exit {}\nstdout: {stdout}\nstderr: {stderr}",
                        send_output.status.code().unwrap_or(-1)
                    ),
                });
            }

            // 5c. Verify received file
            // In fetch mode, the file lands on send_node at /tmp/received/test_transfer.bin
            // In push mode, the file lands on recv_node at /tmp/received/test_transfer.bin
            let verify_node = if is_fetch { send_node } else { recv_node };
            let verify_output =
                runner.docker_exec(verify_node, &["md5sum", "/tmp/received/test_transfer.bin"])?;
            let actual_md5 = String::from_utf8_lossy(&verify_output.stdout)
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();

            if actual_md5 != expected_md5 {
                return Err(StepError::StepFailed {
                    step_index,
                    action: "file_transfer".into(),
                    detail: format!(
                        "md5 mismatch ({size_label} run {repeat}): expected {expected_md5}, got {actual_md5}"
                    ),
                });
            }

            // Cleanup for next repeat
            runner.docker_exec(verify_node, &["rm", "/tmp/received/test_transfer.bin"])?;

            // 5d. Record result
            println!(
                "    {size_label} run {repeat}: {:.2}s (md5 ok)",
                elapsed.as_secs_f64()
            );
            results.push(TransferResult {
                direction: direction_label.clone(),
                size,
                repeat,
                duration: elapsed,
                sender_tool: send_tool.to_string(),
                receiver_tool: recv_tool.to_string(),
            });
        }
    }

    // 6. Stop listener
    let _ = runner.docker_exec(recv_node, &["pkill", "-f", &format!("{recv_tool} -l")]);
    println!("  listener stopped on {recv_node}");

    Ok(())
}

/// Run multiple file transfers simultaneously in separate threads.
///
/// Each transfer runs in its own thread via `std::thread::scope()`.
/// Results are collected via `mpsc::channel` with a hard deadline.
/// All nodes must be disjoint, no node appears in more than one transfer.
fn execute_parallel_file_transfers(
    runner: &TestRunner,
    step_index: usize,
    transfers: &[ParallelTransferDef],
    timeout_secs: u64,
) -> Result<(), StepError> {
    // Validate: all nodes must be disjoint
    let mut seen: BTreeMap<String, (usize, &str)> = BTreeMap::new();
    for (i, t) in transfers.iter().enumerate() {
        for (role, name) in [("sender", &t.sender), ("receiver", &t.receiver)] {
            if let Some((prev_i, prev_role)) = seen.insert(name.clone(), (i, role)) {
                return Err(StepError::StepFailed {
                    step_index,
                    action: "parallel_file_transfers".into(),
                    detail: format!(
                        "node '{}' appears as {} in transfer {} and {} in transfer {} \
                         — shared containers cause file path races",
                        name, prev_role, prev_i, role, i
                    ),
                });
            }
        }
    }

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    // Treat any node attached to LoRa hardware (RNode OR LNode firmware
    // over USB-Serial) as "has_rnode" for sender-timeout purposes — both
    // pump packets over the same low-bandwidth LoRa link and need the
    // longer establishment + retry budget.
    let has_rnode = runner
        .scenario()
        .nodes
        .values()
        .any(|n| n.rnode || n.rnode_interfaces.is_some() || n.serial);
    let n = transfers.len();
    let (tx, rx) = mpsc::channel();

    let collected = std::thread::scope(|s| {
        // Spawn all transfer threads
        for (i, transfer) in transfers.iter().enumerate() {
            let tx = tx.clone();
            s.spawn(move || {
                println!(
                    "  [{i}] starting: {} -> {}",
                    transfer.sender, transfer.receiver
                );
                let mut results = Vec::new();
                let r = execute_transfer_direction(
                    runner,
                    step_index,
                    &transfer.sender,
                    &transfer.receiver,
                    &transfer.sender_tool,
                    &transfer.receiver_tool,
                    &[transfer.file_size],
                    1,
                    transfer.timeout_secs,
                    &transfer.mode,
                    "",
                    "",
                    None,
                    "success",
                    "",
                    &mut results,
                    has_rnode,
                );
                let _ = tx.send((i, r.map(|_| results)));
            });
        }
        drop(tx);

        // Collect results with hard deadline
        let mut collected: Vec<(usize, Result<Vec<TransferResult>, StepError>)> = Vec::new();
        for _ in 0..n {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining) {
                Ok(result) => collected.push(result),
                Err(_) => break,
            }
        }
        collected
    });

    // Report all results
    let mut any_failed = false;
    let collected_indices: Vec<usize> = collected.iter().map(|(i, _)| *i).collect();

    for (i, result) in &collected {
        let t = &transfers[*i];
        match result {
            Ok(transfer_results) => {
                for tr in transfer_results {
                    println!(
                        "  [{i}] {} -> {}: {:.2}s ok",
                        t.sender,
                        t.receiver,
                        tr.duration.as_secs_f64()
                    );
                }
            }
            Err(e) => {
                println!("  [{i}] {} -> {}: FAILED: {e}", t.sender, t.receiver);
                any_failed = true;
            }
        }
    }

    // Report timed-out transfers
    for (i, t) in transfers.iter().enumerate() {
        if !collected_indices.contains(&i) {
            println!(
                "  [{i}] {} -> {}: TIMEOUT (no result within {timeout_secs}s)",
                t.sender, t.receiver
            );
            any_failed = true;
        }
    }

    if any_failed {
        Err(StepError::StepFailed {
            step_index,
            action: "parallel_file_transfers".into(),
            detail: "one or more parallel transfers failed".into(),
        })
    } else {
        Ok(())
    }
}

/// Build the listener command string with proper flag handling.
///
/// Decision table:
/// | auth_hash | receiver_flags | Listener gets               |
/// |-----------|----------------|-----------------------------|
/// | None      | empty          | `-l -n -s /tmp/received -b 0` (backward compat) |
/// | None      | "-F -n"        | `-l -F -n -s /tmp/received -b 0` |
/// | Some(h)   | empty          | `-l -a <h> -s /tmp/received -b 0` |
/// | Some(h)   | "-F"           | `-l -a <h> -F -s /tmp/received -b 0` |
fn build_listener_cmd(
    recv_tool: &str,
    receiver_flags: &str,
    auth_identity_hash: Option<&str>,
) -> String {
    let mut parts = vec![format!("RUST_LOG=debug {recv_tool} -l")];

    // Auth or no-auth flag
    if let Some(hash) = auth_identity_hash {
        parts.push(format!("-a {hash}"));
    } else if receiver_flags.is_empty() || !receiver_flags.contains("-n") {
        // Backward compat: auto-add -n when no auth and -n not in receiver_flags
        parts.push("-n".into());
    }

    // Extra receiver flags
    if !receiver_flags.is_empty() {
        parts.push(receiver_flags.to_string());
    }

    parts.push("-s /tmp/received -b 0 > /tmp/recv_tool.log 2>&1".into());
    parts.join(" ")
}

fn print_transfer_results(results: &[TransferResult]) {
    if results.is_empty() {
        return;
    }

    println!("\n{}", "=".repeat(70));
    println!("File Transfer Results");
    println!("{}", "=".repeat(70));
    println!(
        "{:<20} {:>8} {:>10} {:>10}  {:<20}",
        "Direction", "Size", "Run", "Time", "Tools"
    );
    println!("{}", "-".repeat(70));

    // Group by (direction, size) for averages
    let mut groups: BTreeMap<(String, u64), Vec<Duration>> = BTreeMap::new();

    for r in results {
        let tool_pair = format!("{} -> {}", r.sender_tool, r.receiver_tool);
        println!(
            "{:<20} {:>8} {:>10} {:>9.2}s  {:<20}",
            r.direction,
            format_size(r.size),
            format!(
                "{}/{}",
                r.repeat,
                results
                    .iter()
                    .filter(|x| x.direction == r.direction && x.size == r.size)
                    .count()
            ),
            r.duration.as_secs_f64(),
            tool_pair,
        );
        groups
            .entry((r.direction.clone(), r.size))
            .or_default()
            .push(r.duration);
    }

    println!("{}", "-".repeat(70));
    println!("Averages:");
    for ((direction, size), durations) in &groups {
        let avg = durations.iter().map(|d| d.as_secs_f64()).sum::<f64>() / durations.len() as f64;
        println!(
            "  {direction:<20} {:>8}  avg {:.2}s ({} runs)",
            format_size(*size),
            avg,
            durations.len()
        );
    }
    println!("{}\n", "=".repeat(70));
}

fn save_transfer_results_json(runner: &TestRunner, results: &[TransferResult]) {
    let logs_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("logs");
    if std::fs::create_dir_all(&logs_dir).is_err() {
        return;
    }
    let path = logs_dir.join(format!(
        "{}_transfer_results.json",
        runner.scenario().test.name
    ));

    let mut json = String::from("[\n");
    for (i, r) in results.iter().enumerate() {
        if i > 0 {
            json.push_str(",\n");
        }
        json.push_str(&format!(
            "  {{\"direction\":\"{}\",\"size\":{},\"repeat\":{},\"duration_secs\":{:.3},\"sender_tool\":\"{}\",\"receiver_tool\":\"{}\"}}",
            r.direction, r.size, r.repeat, r.duration.as_secs_f64(), r.sender_tool, r.receiver_tool
        ));
    }
    json.push_str("\n]\n");
    let _ = std::fs::write(&path, &json);
}

/// Parse hop count from rnprobe output. Looks for `over N hop` pattern.
fn parse_hops_from_output(output: &str) -> Option<u32> {
    // rnprobe output: "Probe reply received ... over 1 hops"
    let marker = "over ";
    let start = output.find(marker)? + marker.len();
    let rest = &output[start..];
    // Take digits until non-digit.
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let digits = &rest[..end];
    digits.parse().ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Create a TestRunner, skipping (return) if devices are missing.
    /// Panics on any other error from TestRunner::new().
    ///
    /// The skip is LOUD and structured: one `SCENARIO_SKIPPED` line with
    /// the runner's `reason=...` payload. run-tier3-hw.sh tallies these
    /// lines into the run summary, so a device shortfall is neither a
    /// silent green nor a red. Plugging the missing device back
    /// re-enables the scenario with zero config changes (discovery is
    /// per-run).
    macro_rules! require_runner {
        ($scenario:expr) => {{
            let scenario = $scenario;
            let scenario_name = scenario.test.name.clone();
            match TestRunner::new(scenario) {
                Ok(r) => r,
                Err(RunnerError::InsufficientRNodes(ref msg)) => {
                    let line = format!("SCENARIO_SKIPPED scenario={scenario_name} {msg}");
                    eprintln!("{line}");
                    // libtest swallows captured output of green tests, so
                    // the eprintln alone would make this skip silent again.
                    // run-tier3-hw.sh sets LEVICULUM_SKIP_LOG and tallies
                    // this file into the run summary.
                    if let Ok(path) = std::env::var("LEVICULUM_SKIP_LOG") {
                        use std::io::Write;
                        if let Ok(mut f) = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&path)
                        {
                            let _ = writeln!(f, "{line}");
                        }
                    }
                    return;
                }
                Err(e) => panic!("TestRunner::new failed: {e}"),
            }
        }};
    }

    /// Regression guard for the integ-lock release-on-panic invariant.
    ///
    /// After `TestRunner::new()` acquires the process-wide lock, any
    /// panic in the test must still close the lock file via OS unwind /
    /// process exit, next cargo-test invocation must succeed. This test
    /// exercises the acquire + panic path; the "does subsequent
    /// invocation succeed" half is validated manually in verification
    /// check 9, not here. Staying in the suite so a future refactor that
    /// swaps `OnceLock<File>` for something with a non-trivial Drop can't
    /// silently break release-on-panic.
    ///
    /// Uses a TCP-only scenario → no hardware required, and
    /// `TestRunner::new()` returns before any container is spawned, so
    /// this is fast and hermetic.
    #[test]
    #[should_panic(expected = "deliberate panic for lock-release regression test")]
    fn lock_released_on_panic() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/basic_probe.toml"
        ))
        .expect("basic_probe.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");
        let _runner = require_runner!(scenario);
        panic!("deliberate panic for lock-release regression test");
    }

    #[test]
    fn extract_path_install_lines_picks_marker_lines_for_hash() {
        let hash = "a1b2c3d4e5f6";
        let logs = "\
unrelated startup line\n\
[ANNOUNCE] received announce for a1b2c3d4e5f6 via TCPInterface[middle]\n\
[PATH_ADD] a1b2c3d4e5f6 ok=true iface=TCPInterface[middle]\n\
received announce for deadbeef via AutoInterface\n\
some path_response for A1B2C3D4E5F6 on TCPInterface[bob]\n\
plain mention of a1b2c3d4e5f6 with no marker keyword\n";
        let lines = extract_path_install_lines(logs, hash);
        // Three lines mention the hash AND carry a marker; case-insensitive
        // on both the hash and the marker.
        assert_eq!(lines.len(), 3, "got: {lines:?}");
        assert!(lines
            .iter()
            .any(|l| l.contains("received announce for a1b2c3d4e5f6")));
        assert!(lines.iter().any(|l| l.contains("[PATH_ADD]")));
        assert!(lines
            .iter()
            .any(|l| l.contains("path_response for A1B2C3D4E5F6")));
        // The other-hash announce and the marker-less mention are excluded.
        assert!(!lines.iter().any(|l| l.contains("deadbeef")));
        assert!(!lines.iter().any(|l| l.contains("no marker keyword")));
    }

    #[test]
    fn extract_path_install_lines_empty_when_no_match() {
        let logs = "nothing here\nrandom line about cafebabe\n";
        assert!(extract_path_install_lines(logs, "a1b2c3d4e5f6").is_empty());
    }

    #[test]
    fn parse_dest_spec_valid() {
        let (node, aspect) = parse_dest_spec("bob.probe").unwrap();
        assert_eq!(node, "bob");
        assert_eq!(aspect, "probe");
    }

    #[test]
    fn parse_dest_spec_multi_part() {
        let (node, aspect) = parse_dest_spec("alice.custom_aspect").unwrap();
        assert_eq!(node, "alice");
        assert_eq!(aspect, "custom_aspect");
    }

    #[test]
    fn parse_dest_spec_no_dot() {
        let result = parse_dest_spec("bob");
        assert!(result.is_err());
        match result.unwrap_err() {
            StepError::DestinationResolve { destination, .. } => {
                assert_eq!(destination, "bob");
            }
            other => panic!("expected DestinationResolve, got: {other:?}"),
        }
    }

    #[test]
    fn parse_dest_spec_trailing_dot() {
        let result = parse_dest_spec("bob.");
        assert!(result.is_err());
    }

    #[test]
    fn parse_dest_spec_leading_dot() {
        let result = parse_dest_spec(".probe");
        assert!(result.is_err());
    }

    #[test]
    fn extract_probe_hash_from_identity_log_basic() {
        let logs = r#"
2026-04-12T12:57:24 INFO Loaded transport identity: d6f22eb20a37434a
2026-04-12T12:57:24 INFO [IDENTITY] node=d6f22eb20a37434a422c1da996ae8179
2026-04-12T12:57:24 INFO Probe responder at <72a8e7db7e8b8c851ce34fb7bb972685> active
2026-04-12T12:57:24 INFO [IDENTITY] probe_destination=72a8e7db7e8b8c851ce34fb7bb972685 aspect=rnstransport.probe
"#;
        let hash = extract_probe_hash_from_identity_log(logs).unwrap();
        assert_eq!(hash, "72a8e7db7e8b8c851ce34fb7bb972685");
    }

    #[test]
    fn extract_probe_hash_from_identity_log_takes_last() {
        // Simulates lnsd restart: two [IDENTITY] lines, pick the most recent.
        let logs = r#"
[IDENTITY] probe_destination=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa aspect=rnstransport.probe
... some logs ...
[IDENTITY] probe_destination=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb aspect=rnstransport.probe
"#;
        let hash = extract_probe_hash_from_identity_log(logs).unwrap();
        assert_eq!(hash, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    }

    #[test]
    fn extract_probe_hash_from_identity_log_not_present() {
        let logs = "Reticulum started, respond_to_probes=false\n";
        assert!(extract_probe_hash_from_identity_log(logs).is_none());
    }

    #[test]
    fn extract_probe_hash_from_identity_log_truncated() {
        let logs = "[IDENTITY] probe_destination=c46bbee6 aspect=rnstransport.probe\n";
        assert!(extract_probe_hash_from_identity_log(logs).is_none());
    }

    #[test]
    fn extract_probe_hash_from_python_rnsd_log() {
        let logs = r#"
[2026-04-13 11:58:44] [Notice]   Transport Instance will respond to probe requests on <rnstransport.probe.13dc1efa4fa4aea3801565809bae7e1d:e6e0c4ba320d31cf79d97cdd152ec2db>
[2026-04-13 11:58:44] [Verbose]  Transport instance <13dc1efa4fa4aea3801565809bae7e1d> started
"#;
        let hash = extract_probe_hash_from_identity_log(logs).unwrap();
        assert_eq!(hash, "e6e0c4ba320d31cf79d97cdd152ec2db");
    }

    #[test]
    fn extract_probe_hash_prefers_later_format() {
        // lnsd line first, rnsd line second, rnsd wins (later position).
        let logs = "\n[IDENTITY] probe_destination=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa aspect=rnstransport.probe\nTransport Instance will respond to probe requests on <rnstransport.probe.bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:cccccccccccccccccccccccccccccccc>\n";
        let hash = extract_probe_hash_from_identity_log(logs).unwrap();
        assert_eq!(hash, "cccccccccccccccccccccccccccccccc");
    }

    #[test]
    fn parse_hops_single() {
        assert_eq!(parse_hops_from_output("Probe reply over 1 hops"), Some(1));
    }

    #[test]
    fn parse_hops_multi() {
        assert_eq!(
            parse_hops_from_output("Probe reply received over 3 hops in 234ms"),
            Some(3)
        );
    }

    #[test]
    fn parse_hops_none() {
        assert_eq!(parse_hops_from_output("Probe timed out"), None);
    }

    #[test]
    fn parse_dest_hash_rncp_format() {
        let output = "Identity     : <abc123>\nListening on : <c46bbee6a9437963a27ad5d18ce4a87c>\n";
        assert_eq!(
            parse_dest_hash_from_print_identity(output).unwrap(),
            "c46bbee6a9437963a27ad5d18ce4a87c"
        );
    }

    #[test]
    fn parse_dest_hash_lncp_bare() {
        let output = "c46bbee6a9437963a27ad5d18ce4a87c\n";
        assert_eq!(
            parse_dest_hash_from_print_identity(output).unwrap(),
            "c46bbee6a9437963a27ad5d18ce4a87c"
        );
    }

    #[test]
    fn parse_dest_hash_empty_fails() {
        assert!(parse_dest_hash_from_print_identity("").is_err());
    }

    #[test]
    fn parse_identity_hash_lncp_format() {
        // lncp -p output: line 1 = dest hash, line 2 = "Identity  : <hex>"
        let output =
            "c46bbee6a9437963a27ad5d18ce4a87c\nIdentity  : aabbccdd11223344aabbccdd11223344\n";
        assert_eq!(
            parse_identity_hash_from_print_identity(output).unwrap(),
            "aabbccdd11223344aabbccdd11223344"
        );
    }

    #[test]
    fn parse_identity_hash_rncp_format() {
        // rncp -p output: "Identity     : <hex>" with angle brackets
        let output = "Identity     : <aabbccdd11223344aabbccdd11223344>\nListening on : <c46bbee6a9437963a27ad5d18ce4a87c>\n";
        assert_eq!(
            parse_identity_hash_from_print_identity(output).unwrap(),
            "aabbccdd11223344aabbccdd11223344"
        );
    }

    #[test]
    fn parse_identity_hash_missing() {
        assert!(parse_identity_hash_from_print_identity("no identity here\n").is_err());
    }

    #[test]
    fn format_size_display() {
        assert_eq!(format_size(102400), "100KB");
        assert_eq!(format_size(1048576), "1MB");
        assert_eq!(format_size(10485760), "10MB");
        assert_eq!(format_size(512), "512B");
    }

    #[test]
    fn step_error_display_destination_resolve() {
        let err = StepError::DestinationResolve {
            destination: "bob.probe".into(),
            detail: "not found".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("bob.probe"));
        assert!(msg.contains("not found"));
    }

    #[test]
    fn step_error_display_step_failed() {
        let err = StepError::StepFailed {
            step_index: 2,
            action: "rnprobe".into(),
            detail: "expected 1 hop(s) but got 3".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("step 2"));
        assert!(msg.contains("rnprobe"));
        assert!(msg.contains("expected 1 hop(s) but got 3"));
    }

    #[test]
    fn step_error_display_runner() {
        let runner_err = RunnerError::Compose {
            action: "up".into(),
            stderr: "fail".into(),
        };
        let err = StepError::Runner(runner_err);
        let msg = err.to_string();
        assert!(msg.contains("runner error"));
        assert!(msg.contains("up"));
    }

    // -----------------------------------------------------------------------
    // Integration test (requires Docker)
    // -----------------------------------------------------------------------

    #[test]
    #[serial(docker)]
    fn basic_probe_end_to_end() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/basic_probe.toml"
        ))
        .expect("basic_probe.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn probe_through_relay() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/probe_through_relay.toml"
        ))
        .expect("probe_through_relay.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn path_self_healing() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/path_self_healing.toml"
        ))
        .expect("path_self_healing.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn node_restart_path_recovery() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/node_restart_path_recovery.toml"
        ))
        .expect("node_restart_path_recovery.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    // Non-Docker guard: the c-api scenario stays well-formed and keeps
    // exercising a c-api node through a fault. Runs in the normal test pass, so
    // a broken scenario is caught without bringing up containers.
    #[test]
    fn c_api_restart_recovery_scenario_is_well_formed() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/c_api_restart_recovery.toml"
        ))
        .expect("c_api_restart_recovery.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");
        assert!(
            scenario.nodes.values().any(|n| n.node_type == "c-api"),
            "scenario must exercise a c-api node"
        );
        assert!(
            scenario
                .steps
                .iter()
                .any(|s| matches!(s, Step::Restart { .. })),
            "scenario must inject a restart fault"
        );
    }

    #[test]
    #[serial(docker)]
    fn c_api_restart_recovery() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/c_api_restart_recovery.toml"
        ))
        .expect("c_api_restart_recovery.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn announce_replacement() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/announce_replacement.toml"
        ))
        .expect("announce_replacement.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn four_node_chain() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/four_node_chain.toml"
        ))
        .expect("four_node_chain.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn rust_relay_python_endpoints() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/rust_relay_python_endpoints.toml"
        ))
        .expect("rust_relay_python_endpoints.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn double_restart_identity_persistence() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/double_restart_identity_persistence.toml"
        ))
        .expect("double_restart_identity_persistence.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn rnstatus_transport_info() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/rnstatus_transport_info.toml"
        ))
        .expect("rnstatus_transport_info.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn lns_diag() {
        let toml_str =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/lns_diag.toml"))
                .expect("lns_diag.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn rnpath_conformance() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/rnpath_conformance.toml"
        ))
        .expect("rnpath_conformance.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn rnprobe_conformance() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/rnprobe_conformance.toml"
        ))
        .expect("rnprobe_conformance.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn rnid_retain() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/rnid_retain.toml"
        ))
        .expect("rnid_retain.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn rnsd_attaches_to_lnsd() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/rnsd_attaches_to_lnsd.toml"
        ))
        .expect("rnsd_attaches_to_lnsd.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn rust_probe_through_python_relay() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/rust_probe_through_python_relay.toml"
        ))
        .expect("rust_probe_through_python_relay.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn five_node_mesh() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/five_node_mesh.toml"
        ))
        .expect("five_node_mesh.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn link_failure_recovery() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/link_failure_recovery.toml"
        ))
        .expect("link_failure_recovery.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn non_transport_no_relay() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/non_transport_no_relay.toml"
        ))
        .expect("non_transport_no_relay.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn selftest_ratchet_direct() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/selftest_ratchet_direct.toml"
        ))
        .expect("selftest_ratchet_direct.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn selftest_ratchet_chain() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/selftest_ratchet_chain.toml"
        ))
        .expect("selftest_ratchet_chain.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn selftest_ratchet_mixed() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/selftest_ratchet_mixed.toml"
        ))
        .expect("selftest_ratchet_mixed.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[ignore]
    // Requires RNode hardware at /dev/ttyACM0 and /dev/ttyACM1.
    // Build lnsd with: cargo build --release --bin lnsd --features serial
    // If wait_for_path times out, check container logs for
    // "configured" or "configuration failed", wait_ready() succeeds
    // even if the RNode device is not detected (reconnect runs in background).
    #[serial(lora)]
    fn lora_direct_rust() {
        crate::timeout::run_with_timeout("lora_direct_rust", 300, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_direct_rust.toml"
            ))
            .expect("lora_direct_rust.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore]
    // Requires RNode hardware at /dev/ttyACM0 and /dev/ttyACM1.
    // Build lnsd with: cargo build --release --bin lnsd --features serial
    // Docker image must have pyserial installed for Python RNodeInterface.
    // If wait_for_path times out, check container logs for
    // "configured"/"configuration failed" (Rust) or RNodeInterface errors (Python).
    #[serial(lora)]
    fn lora_interop_rust_python() {
        crate::timeout::run_with_timeout("lora_interop_rust_python", 300, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_interop_rust_python.toml"
            ))
            .expect("lora_interop_rust_python.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore]
    // Requires RNode hardware at /dev/ttyACM0 and /dev/ttyACM1.
    // Build lnsd with: cargo build --release --bin lnsd --features serial
    // Build lnstest with: cargo build --release --bin lnstest
    #[serial(lora)]
    fn lora_link_rust() {
        crate::timeout::run_with_timeout("lora_link_rust", 1800, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_link_rust.toml"
            ))
            .expect("lora_link_rust.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore]
    // Python-RNS counterpart to `lora_link_rust`, for Bug #25
    // controlled A/B/A comparison. Both nodes run Python rnsd; the
    // driver is the same `lnstest selftest` binary as `lora_link_rust` —
    // the only difference between the three LoRa link-test TOMLs is
    // the `[nodes.*].type` field.
    // Expected-marginal, non-gating: the python relay stack does not recover a
    // lost LinkRequest/LRPROOF in-window over slow LoRa (~60% red, vs lora_link_rust
    // and lora_link_interop green). On the EXPECTED_MARGINAL allowlist in
    // scripts/run-tier3-hw.sh, the single source of truth: a failure is reported
    // and counted but does not flip the tier3 verdict RED.
    #[serial(lora)]
    fn lora_link_python() {
        crate::timeout::run_with_timeout("lora_link_python", 1800, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_link_python.toml"
            ))
            .expect("lora_link_python.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore]
    // Requires RNode hardware at /dev/ttyACM0 and /dev/ttyACM1.
    // Build lnsd with: cargo build --release --bin lnsd --features serial
    // Build lnstest with: cargo build --release --bin lnstest
    #[serial(lora)]
    fn lora_link_interop() {
        crate::timeout::run_with_timeout("lora_link_interop", 1800, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_link_interop.toml"
            ))
            .expect("lora_link_interop.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_tcp_bridge_rust_relay() {
        crate::timeout::run_with_timeout("lora_tcp_bridge_rust_relay", 600, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_tcp_bridge_rust_relay.toml"
            ))
            .expect("lora_tcp_bridge_rust_relay.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_tcp_bridge_python_relay() {
        crate::timeout::run_with_timeout("lora_tcp_bridge_python_relay", 600, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_tcp_bridge_python_relay.toml"
            ))
            .expect("lora_tcp_bridge_python_relay.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_tcp_bridge_python_selftest() {
        crate::timeout::run_with_timeout("lora_tcp_bridge_python_selftest", 1200, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_tcp_bridge_python_selftest.toml"
            ))
            .expect("lora_tcp_bridge_python_selftest.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_late_announce_2node() {
        // 900 s budget = 410-450 s TOML scenario time + ~2× margin.
        // Empirically uncalibrated; bump generously on false timeout. (Codeberg #50)
        crate::timeout::run_with_timeout("lora_late_announce_2node", 1200, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_late_announce_2node.toml"
            ))
            .expect("lora_late_announce_2node.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_late_announce_4node() {
        // 900 s budget = 410-450 s TOML scenario time + ~2× margin.
        // Empirically uncalibrated; bump generously on false timeout. (Codeberg #50)
        crate::timeout::run_with_timeout("lora_late_announce_4node", 1800, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_late_announce_4node.toml"
            ))
            .expect("lora_late_announce_4node.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_late_announce_6node() {
        // 900 s budget = 410-450 s TOML scenario time + ~2× margin.
        // Empirically uncalibrated; bump generously on false timeout. (Codeberg #50)
        crate::timeout::run_with_timeout("lora_late_announce_6node", 1800, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_late_announce_6node.toml"
            ))
            .expect("lora_late_announce_6node.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_late_announce_8node() {
        // 900 s budget = 410-450 s TOML scenario time + ~2× margin.
        // Empirically uncalibrated; bump generously on false timeout. (Codeberg #50)
        crate::timeout::run_with_timeout("lora_late_announce_8node", 1800, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_late_announce_8node.toml"
            ))
            .expect("lora_late_announce_8node.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_late_announce_10node() {
        // 1200 s budget = 480-570 s TOML scenario time + ~2× margin.
        // Empirically uncalibrated; bump generously on false timeout. (Codeberg #50)
        crate::timeout::run_with_timeout("lora_late_announce_10node", 1800, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_late_announce_10node.toml"
            ))
            .expect("lora_late_announce_10node.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_rpc_after_selftest() {
        crate::timeout::run_with_timeout("lora_rpc_after_selftest", 1200, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_rpc_after_selftest.toml"
            ))
            .expect("lora_rpc_after_selftest.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires 4 RNode devices
    #[serial(lora)]
    fn lora_multihop_transfer() {
        crate::timeout::run_with_timeout("lora_multihop_transfer", 1200, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_multihop_transfer.toml"
            ))
            .expect("lora_multihop_transfer.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires 4 RNode devices
    #[serial(lora)]
    fn lora_multihop_bidir() {
        crate::timeout::run_with_timeout("lora_multihop_bidir", 1800, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_multihop_bidir.toml"
            ))
            .expect("lora_multihop_bidir.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires 4 RNode devices
    #[serial(lora)]
    fn lora_multihop_link_loss() {
        crate::timeout::run_with_timeout("lora_multihop_link_loss", 1800, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_multihop_link_loss.toml"
            ))
            .expect("lora_multihop_link_loss.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_ratchet_basic() {
        crate::timeout::run_with_timeout("lora_ratchet_basic", 600, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_ratchet_basic.toml"
            ))
            .expect("lora_ratchet_basic.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_ratchet_enforced() {
        crate::timeout::run_with_timeout("lora_ratchet_enforced", 600, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_ratchet_enforced.toml"
            ))
            .expect("lora_ratchet_enforced.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_ratchet_rotation() {
        crate::timeout::run_with_timeout("lora_ratchet_rotation", 1200, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_ratchet_rotation.toml"
            ))
            .expect("lora_ratchet_rotation.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_path_recovery() {
        crate::timeout::run_with_timeout("lora_path_recovery", 300, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_path_recovery.toml"
            ))
            .expect("lora_path_recovery.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lncp_proxy_50kb() {
        // 480 s budget = 90-120 s TOML scenario time + ~4× margin.
        // Empirically uncalibrated; bump generously on false timeout. (Codeberg #50)
        crate::timeout::run_with_timeout(
            "lora_lncp_proxy_50kb",
            crate::timeout::lncp_lora_wrapper_secs(51200, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_proxy_50kb.toml"
                ))
                .expect("lora_lncp_proxy_50kb.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_link_teardown() {
        // 420 s budget = 25 s init + 2 × 135 s (95th percentile under
        // contention) + 5 s + ~30 s runner cleanup = ~330 s + ~90 s
        // headroom. The 300 s wrapper used by sibling lora_link_*
        // tests does not fit because teardown runs TWO selftest
        // cycles where siblings run one. Audit-confirmed empirically
        // 2026-05-07; pre-fix 0/10 PASS, post-fix-only-TOML 6/10 PASS,
        // post-combo target 8-10/10 PASS. Codeberg #52.
        crate::timeout::run_with_timeout("lora_link_teardown", 1200, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_link_teardown.toml"
            ))
            .expect("lora_link_teardown.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lncp_auth_fetch() {
        crate::timeout::run_with_timeout(
            "lora_lncp_auth_fetch",
            crate::timeout::lncp_lora_wrapper_secs(1024, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_auth_fetch.toml"
                ))
                .expect("lora_lncp_auth_fetch.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    #[test]
    #[ignore] // Requires 4 RNode devices
    #[serial(lora)]
    fn lora_4node_contention_rust() {
        crate::timeout::run_with_timeout("lora_4node_contention_rust", 1200, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_4node_contention_rust.toml"
            ))
            .expect("lora_4node_contention_rust.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires 4 RNode devices
    #[serial(lora)]
    fn lora_4node_contention_python() {
        crate::timeout::run_with_timeout("lora_4node_contention_python", 1200, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_4node_contention_python.toml"
            ))
            .expect("lora_4node_contention_python.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires 4 RNode devices
    #[serial(lora)]
    fn lora_4node_contention_mixed() {
        crate::timeout::run_with_timeout("lora_4node_contention_mixed", 1200, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_4node_contention_mixed.toml"
            ))
            .expect("lora_4node_contention_mixed.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires 4 RNode devices
    #[serial(lora)]
    fn lora_4node_sequential_python() {
        crate::timeout::run_with_timeout("lora_4node_sequential_python", 1200, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_4node_sequential_python.toml"
            ))
            .expect("lora_4node_sequential_python.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_dual_cluster_rust() {
        // 480 s budget = 13-step topology TOML scenario time + ~4× margin.
        // Empirically uncalibrated; bump generously on false timeout. (Codeberg #50)
        crate::timeout::run_with_timeout("lora_dual_cluster_rust", 1800, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_dual_cluster_rust.toml"
            ))
            .expect("lora_dual_cluster_rust.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_dual_cluster_mixed() {
        // 480 s budget = 13-step topology TOML scenario time + ~4× margin.
        // Empirically uncalibrated; bump generously on false timeout. (Codeberg #50)
        crate::timeout::run_with_timeout("lora_dual_cluster_mixed", 1800, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_dual_cluster_mixed.toml"
            ))
            .expect("lora_dual_cluster_mixed.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware + lora-proxy binary
    #[serial(lora)]
    fn lora_proxy_loss() {
        crate::timeout::run_with_timeout("lora_proxy_loss", 600, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_proxy_loss.toml"
            ))
            .expect("lora_proxy_loss.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lncp_push() {
        crate::timeout::run_with_timeout(
            "lora_lncp_push",
            crate::timeout::lncp_lora_wrapper_secs(1024, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_push.toml"
                ))
                .expect("lora_lncp_push.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lncp_fetch() {
        crate::timeout::run_with_timeout(
            "lora_lncp_fetch",
            crate::timeout::lncp_lora_wrapper_secs(1024, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_fetch.toml"
                ))
                .expect("lora_lncp_fetch.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lncp_auth() {
        crate::timeout::run_with_timeout(
            "lora_lncp_auth",
            crate::timeout::lncp_lora_wrapper_secs(1024, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_auth.toml"
                ))
                .expect("lora_lncp_auth.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    #[test]
    #[ignore] // Requires RNode hardware + lora-proxy binary
    #[serial(lora)]
    fn lora_lncp_proxy() {
        crate::timeout::run_with_timeout(
            "lora_lncp_proxy",
            crate::timeout::lncp_lora_wrapper_secs(2048, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_proxy.toml"
                ))
                .expect("lora_lncp_proxy.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lncp_size_sweep() {
        // 900 s budget = 360-420 s TOML scenario time + ~2× margin.
        // Empirically uncalibrated; bump generously on false timeout. (Codeberg #50)
        crate::timeout::run_with_timeout(
            "lora_lncp_size_sweep",
            crate::timeout::lncp_lora_wrapper_secs(64 + 512 + 2048 + 5120 + 10240 + 51200, 6),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_size_sweep.toml"
                ))
                .expect("lora_lncp_size_sweep.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lncp_proxy_4drop() {
        crate::timeout::run_with_timeout(
            "lora_lncp_proxy_4drop",
            crate::timeout::lncp_lora_wrapper_secs(10240, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_proxy_4drop.toml"
                ))
                .expect("lora_lncp_proxy_4drop.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lncp_proxy_6drop() {
        crate::timeout::run_with_timeout(
            "lora_lncp_proxy_6drop",
            crate::timeout::lncp_lora_wrapper_secs(10240, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_proxy_6drop.toml"
                ))
                .expect("lora_lncp_proxy_6drop.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lncp_link_loss() {
        crate::timeout::run_with_timeout(
            "lora_lncp_link_loss",
            crate::timeout::lncp_lora_wrapper_secs(2048, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_link_loss.toml"
                ))
                .expect("lora_lncp_link_loss.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lncp_link_retry() {
        crate::timeout::run_with_timeout(
            "lora_lncp_link_retry",
            crate::timeout::lncp_lora_wrapper_secs(2048, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_link_retry.toml"
                ))
                .expect("lora_lncp_link_retry.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lncp_proof_retry() {
        crate::timeout::run_with_timeout(
            "lora_lncp_proof_retry",
            crate::timeout::lncp_lora_wrapper_secs(2048, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_proof_retry.toml"
                ))
                .expect("lora_lncp_proof_retry.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lrncp_resource_proof_retry() {
        crate::timeout::run_with_timeout("lora_lrncp_resource_proof_retry", 1200, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_lrncp_resource_proof_retry.toml"
            ))
            .expect("lora_lrncp_resource_proof_retry.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lncp_bidir() {
        crate::timeout::run_with_timeout(
            "lora_lncp_bidir",
            crate::timeout::lncp_lora_wrapper_secs(2048, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_bidir.toml"
                ))
                .expect("lora_lncp_bidir.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    #[test]
    #[ignore] // Requires two LNode-firmware boards (T114 + RAK4631 or similar)
    #[serial(lora)]
    fn lora_lnode_lncp_bidir() {
        // 480 s budget = 32 KB bidi TOML scenario time + ~4× margin.
        // Empirically uncalibrated; bump generously on false timeout. (Codeberg #50)
        crate::timeout::run_with_timeout(
            "lora_lnode_lncp_bidir",
            crate::timeout::lncp_lora_wrapper_secs(32768, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lnode_lncp_bidir.toml"
                ))
                .expect("lora_lnode_lncp_bidir.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    #[test]
    #[ignore] // Requires two LNode-firmware boards (T114 + RAK4631 or similar)
    #[serial(lora)]
    fn lora_lnode_lncp_bidir_slow() {
        // SF10 LNode<->LNode bidirectional transfer: regression gate for the
        // #23 post-TX RX window fix. A regression that restarves the SX1262
        // RX after TX fails to complete the transfer. SF10 airtime is much
        // slower than SF7, so the per-test budget is the full TOML timeout.
        crate::timeout::run_with_timeout("lora_lnode_lncp_bidir_slow", 900, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_lnode_lncp_bidir_slow.toml"
            ))
            .expect("lora_lnode_lncp_bidir_slow.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lncp_bridge() {
        crate::timeout::run_with_timeout(
            "lora_lncp_bridge",
            crate::timeout::lncp_lora_wrapper_secs(1024, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_bridge.toml"
                ))
                .expect("lora_lncp_bridge.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    // Python-to-Python LoRa tests

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_rncp_push() {
        crate::timeout::run_with_timeout("lora_rncp_push", 900, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_rncp_push.toml"
            ))
            .expect("lora_rncp_push.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_rncp_fetch() {
        crate::timeout::run_with_timeout("lora_rncp_fetch", 900, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_rncp_fetch.toml"
            ))
            .expect("lora_rncp_fetch.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_rncp_auth() {
        crate::timeout::run_with_timeout("lora_rncp_auth", 900, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_rncp_auth.toml"
            ))
            .expect("lora_rncp_auth.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware + lora-proxy binary
    #[serial(lora)]
    fn lora_rncp_proxy() {
        crate::timeout::run_with_timeout("lora_rncp_proxy", 1200, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_rncp_proxy.toml"
            ))
            .expect("lora_rncp_proxy.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_rncp_bridge() {
        crate::timeout::run_with_timeout("lora_rncp_bridge", 1200, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_rncp_bridge.toml"
            ))
            .expect("lora_rncp_bridge.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    // Cross-implementation LoRa tests

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lncp_push_to_python() {
        crate::timeout::run_with_timeout(
            "lora_lncp_push_to_python",
            crate::timeout::lncp_lora_wrapper_secs(1024, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_push_to_python.toml"
                ))
                .expect("lora_lncp_push_to_python.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_rncp_push_to_rust() {
        crate::timeout::run_with_timeout("lora_rncp_push_to_rust", 900, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_rncp_push_to_rust.toml"
            ))
            .expect("lora_rncp_push_to_rust.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lncp_fetch_from_python() {
        crate::timeout::run_with_timeout(
            "lora_lncp_fetch_from_python",
            crate::timeout::lncp_lora_wrapper_secs(1024, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_fetch_from_python.toml"
                ))
                .expect("lora_lncp_fetch_from_python.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_rncp_fetch_from_rust() {
        crate::timeout::run_with_timeout("lora_rncp_fetch_from_rust", 900, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_rncp_fetch_from_rust.toml"
            ))
            .expect("lora_rncp_fetch_from_rust.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lncp_auth_to_python() {
        crate::timeout::run_with_timeout(
            "lora_lncp_auth_to_python",
            crate::timeout::lncp_lora_wrapper_secs(1024, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_auth_to_python.toml"
                ))
                .expect("lora_lncp_auth_to_python.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    #[test]
    #[ignore] // Requires RNode hardware
    #[serial(lora)]
    fn lora_lncp_bridge_python_relay() {
        crate::timeout::run_with_timeout(
            "lora_lncp_bridge_python_relay",
            crate::timeout::lncp_lora_wrapper_secs(1024, 1),
            || {
                let toml_str = std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/lora_lncp_bridge_python_relay.toml"
                ))
                .expect("lora_lncp_bridge_python_relay.toml not found");
                let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

                let mut runner = require_runner!(scenario);

                run_test(&mut runner).expect("test failed");
            },
        );
    }

    // 3-node shared medium LoRa tests (require 3 RNode devices)

    #[test]
    #[ignore] // Requires 3 RNode hardware devices
    #[serial(lora)]
    fn lora_3node_transfer() {
        crate::timeout::run_with_timeout("lora_3node_transfer", 1200, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_3node_transfer.toml"
            ))
            .expect("lora_3node_transfer.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires 3 RNode hardware devices
    #[serial(lora)]
    fn lora_3node_contention() {
        crate::timeout::run_with_timeout("lora_3node_contention", 1200, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_3node_contention.toml"
            ))
            .expect("lora_3node_contention.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires 3 RNode hardware devices
    #[serial(lora)]
    fn lora_3node_bidir() {
        crate::timeout::run_with_timeout("lora_3node_bidir", 1200, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_3node_bidir.toml"
            ))
            .expect("lora_3node_bidir.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode + T114 hardware
    #[serial(lora)]
    fn lora_lnode_serial_relay() {
        crate::timeout::run_with_timeout("lora_lnode_serial_relay", 300, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_lnode_serial_relay.toml"
            ))
            .expect("lora_lnode_serial_relay.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode + T114 hardware
    #[serial(lora)]
    fn lora_lnode_probe() {
        crate::timeout::run_with_timeout("lora_lnode_probe", 300, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_lnode_probe.toml"
            ))
            .expect("lora_lnode_probe.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode + T114 hardware
    #[serial(lora)]
    fn lora_lnode_probe_ca() {
        crate::timeout::run_with_timeout("lora_lnode_probe_ca", 300, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_lnode_probe_ca.toml"
            ))
            .expect("lora_lnode_probe_ca.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[ignore] // Requires RNode + T114 hardware
    #[serial(lora)]
    fn lora_lnode_split() {
        crate::timeout::run_with_timeout("lora_lnode_split", 600, || {
            let toml_str = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/lora_lnode_split.toml"
            ))
            .expect("lora_lnode_split.toml not found");
            let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

            let mut runner = require_runner!(scenario);

            run_test(&mut runner).expect("test failed");
        });
    }

    #[test]
    #[serial(docker)]
    fn selftest_bulk() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/selftest_bulk.toml"
        ))
        .expect("selftest_bulk.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn ifac_basic_probe() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/ifac_basic_probe.toml"
        ))
        .expect("ifac_basic_probe.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn ifac_mixed_links() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/ifac_mixed_links.toml"
        ))
        .expect("ifac_mixed_links.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn ifac_through_relay() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/ifac_through_relay.toml"
        ))
        .expect("ifac_through_relay.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn ifac_rust_relay() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/ifac_rust_relay.toml"
        ))
        .expect("ifac_rust_relay.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn lncp_baseline() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/lncp_baseline.toml"
        ))
        .expect("lncp_baseline.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn tcp_blackhole_giveup() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/tcp_blackhole_giveup.toml"
        ))
        .expect("tcp_blackhole_giveup.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn lncp_silent() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/lncp_silent.toml"
        ))
        .expect("lncp_silent.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn lncp_rust_sender() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/lncp_rust_sender.toml"
        ))
        .expect("lncp_rust_sender.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn lncp_rust_edges() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/lncp_rust_edges.toml"
        ))
        .expect("lncp_rust_edges.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn lncp_full_rust() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/lncp_full_rust.toml"
        ))
        .expect("lncp_full_rust.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn e30_repro() {
        let toml_str =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/e30_repro.toml"))
                .expect("e30_repro.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn e31_repro() {
        let toml_str =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/e31_repro.toml"))
                .expect("e31_repro.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn lncp_fetch() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/lncp_fetch.toml"
        ))
        .expect("lncp_fetch.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn lncp_auth() {
        let toml_str =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/lncp_auth.toml"))
                .expect("lncp_auth.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn lncp_auth_reject() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/lncp_auth_reject.toml"
        ))
        .expect("lncp_auth_reject.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn lncp_fetch_auth() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/lncp_fetch_auth.toml"
        ))
        .expect("lncp_fetch_auth.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn lncp_fetch_jail() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/lncp_fetch_jail.toml"
        ))
        .expect("lncp_fetch_jail.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn lncp_fetch_cross() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/lncp_fetch_cross.toml"
        ))
        .expect("lncp_fetch_cross.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");

        let mut runner = require_runner!(scenario);

        run_test(&mut runner).expect("test failed");
    }

    // Benchmark tests
    #[test]
    #[ignore] // Requires RNode + T114 hardware
    #[serial(lora)]
    fn bench_single_pair_fast() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/bench_single_pair_fast.toml"
        ))
        .expect("bench_single_pair_fast.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");
        let mut runner = require_runner!(scenario);
        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[ignore] // Requires RNode + T114 hardware
    #[serial(lora)]
    fn bench_single_pair_medium() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/bench_single_pair_medium.toml"
        ))
        .expect("bench_single_pair_medium.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");
        let mut runner = require_runner!(scenario);
        run_test(&mut runner).expect("test failed");
    }

    // Expected-marginal, non-gating PDR. Same SF10/CR8 mixed SX1262 LNode to
    // SX127x RNode chip interop margin corner as bench_single_pair_slow_ca (same
    // rnode_lnode_pair profile); shares its disposition. On the EXPECTED_MARGINAL
    // allowlist in scripts/run-tier3-hw.sh: a failure is reported and counted but
    // does not flip the tier3 verdict RED. Runnable for diagnostics.
    #[test]
    #[ignore] // Requires RNode + T114 hardware
    #[serial(lora)]
    fn bench_single_pair_slow() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/bench_single_pair_slow.toml"
        ))
        .expect("bench_single_pair_slow.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");
        let mut runner = require_runner!(scenario);
        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[ignore] // Requires 2x RNode + 2x T114 hardware
    #[serial(lora)]
    fn bench_dual_pair_fast() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/bench_dual_pair_fast.toml"
        ))
        .expect("bench_dual_pair_fast.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");
        let mut runner = require_runner!(scenario);
        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[ignore] // Requires 2x RNode + 2x T114 hardware
    #[serial(lora)]
    fn bench_dual_pair_medium() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/bench_dual_pair_medium.toml"
        ))
        .expect("bench_dual_pair_medium.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");
        let mut runner = require_runner!(scenario);
        run_test(&mut runner).expect("test failed");
    }

    // Expected-marginal, non-gating PDR. Same SF10/CR8 mixed SX1262 LNode to
    // SX127x RNode chip interop margin corner as bench_dual_pair_slow_ca (same
    // dual_pair_mixed profile); shares its disposition. On the EXPECTED_MARGINAL
    // allowlist in scripts/run-tier3-hw.sh: a failure is reported and counted but
    // does not flip the tier3 verdict RED. Runnable for diagnostics.
    #[test]
    #[ignore] // Requires 2x RNode + 2x T114 hardware
    #[serial(lora)]
    fn bench_dual_pair_slow() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/bench_dual_pair_slow.toml"
        ))
        .expect("bench_dual_pair_slow.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");
        let mut runner = require_runner!(scenario);
        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[ignore]
    #[serial(lora)]
    fn bench_single_pair_medium_ca() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/bench_single_pair_medium_ca.toml"
        ))
        .expect("bench_single_pair_medium_ca.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");
        let mut runner = require_runner!(scenario);
        run_test(&mut runner).expect("test failed");
    }

    // Expected-marginal, non-gating PDR. SF10/CR8 over a mixed SX1262 LNode to
    // SX127x RNode pair is a characterized chip interop margin corner at the
    // longest packet. Frequency was refuted by direct FEI (about 275 Hz, 0.32
    // ppm); the residual is a TX modulation and spectral interop effect.
    // Excluded from the 100% PDR lab target. SF10/CR5 is the robust mixed chip
    // limit and same chip pairs (bench_single_pair_slow_ca_rnode_only) pass
    // SF10/CR8; both stay gating. Runnable for diagnostics; PDR here is
    // reported but not gating. The tier3 gating carve-out is the
    // EXPECTED_MARGINAL allowlist in scripts/run-tier3-hw.sh, the single source
    // of truth: a failure of this bench is reported and counted but does not
    // flip the verdict RED. The non-`_ca` sibling bench_single_pair_slow shares
    // this disposition (same SF10/CR8 over the same mixed rnode_lnode_pair
    // profile) and is on the same allowlist.
    #[test]
    #[ignore]
    #[serial(lora)]
    fn bench_single_pair_slow_ca() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/bench_single_pair_slow_ca.toml"
        ))
        .expect("bench_single_pair_slow_ca.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");
        let mut runner = require_runner!(scenario);
        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[ignore]
    #[serial(lora)]
    fn bench_single_pair_medium_ca_rnode_only() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/bench_single_pair_medium_ca_rnode_only.toml"
        ))
        .expect("bench_single_pair_medium_ca_rnode_only.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");
        let mut runner = require_runner!(scenario);
        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[ignore]
    #[serial(lora)]
    fn bench_single_pair_medium_rnode_only() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/bench_single_pair_medium_rnode_only.toml"
        ))
        .expect("bench_single_pair_medium_rnode_only.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");
        let mut runner = require_runner!(scenario);
        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[ignore]
    #[serial(lora)]
    fn bench_single_pair_slow_ca_rnode_only() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/bench_single_pair_slow_ca_rnode_only.toml"
        ))
        .expect("bench_single_pair_slow_ca_rnode_only.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");
        let mut runner = require_runner!(scenario);
        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[ignore]
    #[serial(lora)]
    fn bench_dual_pair_medium_ca() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/bench_dual_pair_medium_ca.toml"
        ))
        .expect("bench_dual_pair_medium_ca.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");
        let mut runner = require_runner!(scenario);
        run_test(&mut runner).expect("test failed");
    }

    // Expected-marginal, non-gating PDR. Same SF10/CR8 mixed SX1262 LNode to
    // SX127x RNode chip interop margin corner as bench_single_pair_slow_ca, run
    // as a dual pair. Excluded from the 100% PDR lab target. SF10/CR5 is the
    // robust mixed chip limit and same chip pairs pass SF10/CR8; both stay
    // gating. Runnable for diagnostics; PDR here is reported but not gating.
    // The tier3 gating carve-out is the EXPECTED_MARGINAL allowlist in
    // scripts/run-tier3-hw.sh, the single source of truth: a failure of this
    // bench is reported and counted but does not flip the verdict RED. The
    // non-`_ca` sibling bench_dual_pair_slow shares this disposition (same
    // SF10/CR8 over the same mixed dual_pair_mixed profile) and is on the same
    // allowlist.
    #[test]
    #[ignore]
    #[serial(lora)]
    fn bench_dual_pair_slow_ca() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/bench_dual_pair_slow_ca.toml"
        ))
        .expect("bench_dual_pair_slow_ca.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");
        let mut runner = require_runner!(scenario);
        run_test(&mut runner).expect("test failed");
    }

    // -----------------------------------------------------------------------
    // LXMF integration tests
    // -----------------------------------------------------------------------

    #[test]
    #[serial(docker)]
    fn lxmf_two_node_basic() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/lxmf_two_node_basic.toml"
        ))
        .expect("lxmf_two_node_basic.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");
        let mut runner = require_runner!(scenario);
        run_test(&mut runner).expect("test failed");
    }

    #[test]
    #[serial(docker)]
    fn lxmf_two_node_auto() {
        let toml_str = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/lxmf_two_node_auto.toml"
        ))
        .expect("lxmf_two_node_auto.toml not found");
        let scenario = crate::topology::parse_scenario(&toml_str).expect("parse failed");
        let mut runner = require_runner!(scenario);
        run_test(&mut runner).expect("test failed");
    }
}

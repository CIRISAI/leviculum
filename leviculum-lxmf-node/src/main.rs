//! `lxmf-node` — I/O wiring only. The protocol lives in
//! [`leviculum_lxmf_node::protocol`], the stack in
//! [`leviculum_lxmf_node::processor`].
//!
//! Usage, as periculum spawns it:
//!
//! ```sh
//! docker exec -i <container> lxmf-node --config /root/.reticulum [display_name]
//! ```
//!
//! Reads commands from stdin (one per line), emits `EVENT …` lines on stdout
//! (one per line, flushed), diagnostics on stderr. Identical to
//! `periculum/assets/scripts/lxmf_node.py` at all three.
//!
//! # `LXMF_STORAGE` on a deployment
//!
//! The default, `/tmp/lxmf-state`, is right for the test corpus and wrong for
//! anything standing: a scenario's container is destroyed with the scenario, so
//! a fresh directory per run is what makes consecutive runs independent peers.
//! A deployment — a field base, a standing endpoint someone writes to — must
//! point `LXMF_STORAGE` at a durable directory instead, because the LXMF
//! address this helper answers at is derived from the identity kept there
//! (`identity::load_or_create`). On `/tmp` a reboot takes that file with it and
//! the node comes up at an address nobody has. It did, on 2026-09-26: the field
//! base was restarted and announced `8f35c8d5…` where its operator had been
//! given `4c64d723…` (#322).
//!
//! Stderr carries two kinds of line: this helper's own `[lxmf-node] …`
//! diagnostics, written by the emitter thread, and the `tracing` output of
//! `leviculum-core` / `leviculum-std` underneath it, written by the global
//! subscriber this installs (see [`run`]). Until #330 the second kind reached
//! nobody — no subscriber was installed at all — so a scenario could not
//! assert on anything the stack itself said, only on what the helper chose to
//! repeat. Stdout is untouched by both: it is the driver's data channel.
//!
//! The helper connects as a shared-instance client to the daemon running in
//! its container (`lnsd` or `rnsd` — the IPC and config-file formats are the
//! same, which is the whole point). All LXMF traffic therefore flows through
//! the daemon under test, exercising its real link / announce / delivery code
//! paths. A daemon that is not there is a hard startup failure, not a silent
//! fallback to an own transport: `connect_to_shared_instance` has no local
//! interfaces to fall back to, so `build`/`start` fails and the process exits
//! non-zero, which is what `RNS_REQUIRE_SHARED=1` buys on the Python side.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use leviculum_lxmf::CooperativeStamper;
use leviculum_lxmf_node::identity::{load_or_create, Provenance, IDENTITY_FILE};
use leviculum_lxmf_node::processor::{
    run_build_worker, BuildJob, Emitter, HelperConfig, Input, LxmfHelperProcessor, Out, Shutdown,
    StampJob,
};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::Config;

/// Where the daemon's config lives, and therefore which shared instance to
/// join. Default matches every other client tool in the tree
/// (`Config::default_config_dir`); periculum's containers put it at
/// `/root/.reticulum`.
const CONFIG_FLAG: &str = "--config";

/// Node storage for the helper's own client state (learned identities, paths).
///
/// Deliberately NOT the daemon's storage directory. `lncp` and `lnstatus`
/// share it safely because a client with `enable_transport(false)` writes no
/// paths, announces or packet hashes — this helper registers a destination and
/// learns identities, so it gets its own. `LXMF_STORAGE` is the variable
/// Python's helper already reads for the same purpose (`periculum/assets/scripts/lxmf_node.py:66`).
///
/// Since #322 it also holds the helper's LXMF identity, and therefore its
/// address, so the default below is a *test* default: see the module header for
/// why a deployment has to override it with something durable.
const STORAGE_ENV: &str = "LXMF_STORAGE";
const STORAGE_DEFAULT: &str = "/tmp/lxmf-state";

/// `argv[1]` when none is given (`periculum/assets/scripts/lxmf_node.py:65`).
const DEFAULT_DISPLAY_NAME: &str = "lxmf-test";

/// Hand outbound Resource builds to the build worker instead of running them
/// inside the router's tick, i.e. under the daemon connection's core lock
/// (Codeberg #196). Off by default, matching `RouterConfig`; periculum sets it
/// per node via `defer_resource_builds = true` on its `lxmf_start` step.
const DEFER_FLAG: &str = "--defer-resource-builds";

struct Args {
    display_name: String,
    config_dir: PathBuf,
    defer_resource_builds: bool,
    /// `-v` / `--verbose` occurrences, `lnsd`'s and `lnpnd`'s spelling
    /// (`leviculum-cli/src/lnsd.rs:85-90`, `lnpnd/src/main.rs:80-83`).
    verbose: u8,
    /// `-q` / `--quiet` occurrences.
    quiet: u8,
}

/// The verbosity a single argv token contributes, as `(verbose, quiet)`, or
/// `None` when the token is not a verbosity flag.
///
/// Accepts the two long spellings and uniform short clusters (`-v`, `-vv`,
/// `-qq`); a mixed cluster like `-vq` is not a verbosity flag and falls
/// through to the caller's normal handling, which keeps a display name that
/// happens to start with `-` behaving exactly as before.
fn verbosity_flag(arg: &str) -> Option<(u8, u8)> {
    match arg {
        "--verbose" => return Some((1, 0)),
        "--quiet" => return Some((0, 1)),
        _ => {}
    }
    let short = arg.strip_prefix('-').filter(|rest| !rest.is_empty())?;
    let count = short.len().min(u8::MAX as usize) as u8;
    if short.chars().all(|c| c == 'v') {
        Some((count, 0))
    } else if short.chars().all(|c| c == 'q') {
        Some((0, count))
    } else {
        None
    }
}

/// RNS log level used when the daemon's config names none: 4 = info
/// (`reference/Reticulum/RNS/__init__.py:66-73`).
///
/// Info, not debug, for the same reason `lnsd` defaults there: `warn!` — the
/// level of the one line that says this terminus adopted a proof's hop count
/// (`node/link_management.rs:1144`) — must be visible without being asked
/// for, while the `event=` debug lines are volume and are asked for. Asking
/// is what every periculum node already does, twice over: the rendered config
/// carries `[logging] loglevel = 5` (`periculum/src/topology.rs:9186`) and the
/// container environment carries `RUST_LOG=debug`
/// (`periculum/src/compose.rs:291-294`), so a scenario gets `PATH_REBALANCE`
/// (`transport.rs:8431`) without touching the argv.
const DEFAULT_LOGLEVEL: u8 = 4;

/// Map an RNS log level (0-7) to a tracing env-filter directive.
///
/// tracing has no notice/verbose/extreme, so notice folds into info, verbose
/// into debug and extreme into trace. The third copy of this ladder
/// (`leviculum-cli/src/lnsd.rs:296`, `lnpnd/src/config.rs:458`); kept local
/// because this batch is scoped to this crate, and cited from here so a later
/// consolidation into `leviculum-std` finds all three.
fn loglevel_filter(level: u8) -> &'static str {
    match level {
        0 | 1 => "error",
        2 => "warn",
        3 | 4 => "info",
        5 | 6 => "debug",
        _ => "trace",
    }
}

/// The config level shifted by the CLI's net verbosity, clamped to the RNS
/// range — `lnpnd`'s arithmetic (`lnpnd/src/main.rs:256`). `RUST_LOG` is not
/// consulted here: `install_global_subscriber` gives it precedence over
/// whatever default we hand it.
fn effective_level(config_loglevel: Option<u8>, verbose: u8, quiet: u8) -> u8 {
    let base = i16::from(config_loglevel.unwrap_or(DEFAULT_LOGLEVEL));
    (base + i16::from(verbose) - i16::from(quiet)).clamp(0, 7) as u8
}

fn parse_args() -> Result<Args, String> {
    let mut display_name = None;
    let mut config_dir = None;
    let mut defer_resource_builds = false;
    let mut verbose = 0u8;
    let mut quiet = 0u8;
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        if let Some((v, q)) = verbosity_flag(&arg) {
            verbose = verbose.saturating_add(v);
            quiet = quiet.saturating_add(q);
            continue;
        }
        match arg.as_str() {
            CONFIG_FLAG => {
                config_dir = Some(PathBuf::from(
                    argv.next()
                        .ok_or_else(|| format!("{CONFIG_FLAG} needs a directory"))?,
                ));
            }
            DEFER_FLAG => defer_resource_builds = true,
            other if other.starts_with("--") => {
                return Err(format!("unknown option {other}"));
            }
            positional => {
                if display_name.replace(positional.to_string()).is_some() {
                    return Err("only one display name may be given".into());
                }
            }
        }
    }
    Ok(Args {
        display_name: display_name.unwrap_or_else(|| DEFAULT_DISPLAY_NAME.to_string()),
        config_dir: config_dir.unwrap_or_else(Config::default_config_dir),
        defer_resource_builds,
        verbose,
        quiet,
    })
}

/// What this helper takes from the daemon's config file.
///
/// The instance name decides the abstract socket (`\0rns/{name}`) the
/// daemon listens on, so it has to come from the daemon's own config —
/// same derivation `lncp` and `lnstatus` use. `max_links` and
/// `keepalive_interval` ride along (#388): the helper is the process
/// that TERMINATES delivery links on this node (a shared-instance
/// client runs its own stack), so a config-file link cap that bound
/// only the daemon would bind nothing a peer can open. Reading them
/// here makes the one config file govern every leviculum stack on the
/// node — which is also what the board is, in one process.
struct DaemonConfigKeys {
    instance_name: String,
    max_links: Option<usize>,
    keepalive_interval: Option<u64>,
    /// `[logging] loglevel`, the same key `lnsd` folds into its default
    /// tracing filter. The helper is one more leviculum stack on the node, so
    /// the node's one config file sets its level too.
    loglevel: Option<u8>,
}

fn daemon_config_keys(config_dir: &std::path::Path) -> DaemonConfigKeys {
    let config_file = config_dir.join("config");
    if config_file.exists() {
        if let Ok(config) = Config::load(&config_file) {
            return DaemonConfigKeys {
                instance_name: config.reticulum.instance_name,
                max_links: config.reticulum.max_links,
                keepalive_interval: config.reticulum.keepalive_interval,
                loglevel: config.reticulum.loglevel,
            };
        }
    }
    DaemonConfigKeys {
        instance_name: "default".to_string(),
        max_links: None,
        keepalive_interval: None,
        loglevel: None,
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("[lxmf-node] {e}");
            eprintln!(
                "usage: lxmf-node [{CONFIG_FLAG} <dir>] [{DEFER_FLAG}] [-v|-q]... [display_name]"
            );
            eprintln!(
                "  {STORAGE_ENV} (default {STORAGE_DEFAULT}) holds this node's LXMF identity, \
                 and therefore its address: a deployment must point it at a \
                 durable directory, or a restart comes up at an address nobody has."
            );
            return ExitCode::from(2);
        }
    };
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[lxmf-node] {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), String> {
    let daemon_keys = daemon_config_keys(&args.config_dir);
    // The stack's own voice, on stderr: exactly the subscriber `lnsd` installs
    // (`leviculum-cli/src/lnsd.rs:136`), so `RUST_LOG` wins, `-v`/`-q` shift
    // the config's level, and `LEVICULUM_EVENT_LOG` picks up the structured
    // event layer if it is set. Installed before anything else in `run` so no
    // startup line is lost; `parse_args` failures above it stay plain
    // `eprintln!`, having no level to obey yet.
    leviculum_std::event_log::install_global_subscriber(loglevel_filter(effective_level(
        daemon_keys.loglevel,
        args.verbose,
        args.quiet,
    )));

    let storage_dir =
        PathBuf::from(std::env::var_os(STORAGE_ENV).unwrap_or_else(|| STORAGE_DEFAULT.into()));
    std::fs::create_dir_all(&storage_dir)
        .map_err(|e| format!("could not create {}: {e}", storage_dir.display()))?;

    // The address, before anything that could publish it. A corrupt record is
    // fatal here and leaves through `main`'s error arm rather than becoming a
    // silent new address (`identity::load_or_create`).
    let identity_path = storage_dir.join(IDENTITY_FILE);
    let (identity, provenance) = load_or_create(&identity_path).map_err(|e| e.to_string())?;

    // The writer thread owns both output streams. Everything upstream of it —
    // including the hooks, which run under the core mutex — only pushes onto
    // an unbounded queue, so no line of output can ever block the node.
    let (lines_tx, lines_rx) = mpsc::channel::<Out>();
    let writer = thread::spawn(move || {
        let stdout = std::io::stdout();
        for line in lines_rx {
            match line {
                Out::Event(text) => {
                    let mut handle = stdout.lock();
                    // Flush per line: the driver reads them as they arrive and
                    // times steps off the arrival.
                    let _ = writeln!(handle, "{text}");
                    let _ = handle.flush();
                }
                Out::Log(text) => eprintln!("{text}"),
            }
        }
    });

    let emitter = Emitter::new(lines_tx, Instant::now());
    let (inputs_tx, inputs_rx) = mpsc::channel::<Input>();
    let (stamps_tx, mut stamps_rx) = tokio::sync::mpsc::unbounded_channel::<StampJob>();
    let (builds_tx, builds_rx) = mpsc::channel::<BuildJob>();
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::unbounded_channel::<Shutdown>();

    let instance = daemon_keys.instance_name.clone();
    emitter.log(format!(
        "[lxmf-node] starting display_name={} storage={} instance={instance}",
        args.display_name,
        storage_dir.display()
    ));
    // Which of the two happened is the operator's one chance to notice that a
    // deployment is minting where it should have loaded — the symptom of #322
    // was invisible until two addresses were compared by hand.
    emitter.log(match provenance {
        Provenance::Loaded => format!(
            "[lxmf-node] lxmf identity loaded from {}",
            identity_path.display()
        ),
        Provenance::Created => format!(
            "[lxmf-node] no lxmf identity at {}, so a NEW address was created; \
             a deployment sets {STORAGE_ENV} to a durable directory",
            identity_path.display()
        ),
    });

    let processor = LxmfHelperProcessor::new(
        HelperConfig {
            display_name: args.display_name.clone().into_bytes(),
            defer_resource_builds: args.defer_resource_builds,
            pn_store_dir: storage_dir.join("pn-messagestore"),
        },
        identity,
        emitter.clone(),
        inputs_rx,
        stamps_tx,
        builds_tx,
        shutdown_tx.clone(),
    );

    let mut builder = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&instance)
        .max_links(daemon_keys.max_links)
        .storage_path(storage_dir)
        .core_processor(processor);
    if let Some(secs) = daemon_keys.keepalive_interval {
        builder = builder.link_keepalive(secs);
    }
    let mut node = builder
        .build()
        .await
        .map_err(|e| format!("could not attach to the shared instance '{instance}': {e}"))?;
    let mut events = node
        .take_event_receiver()
        .ok_or("the node has no event receiver")?;
    node.start()
        .await
        .map_err(|e| format!("could not start the node: {e}"))?;
    emitter.log("[lxmf-node] shared-instance client connected");

    // The application side of the control plane. Two jobs: drain it so the
    // bounded channel cannot overflow, and notice the one event that means the
    // helper has silently stopped being an LXMF node.
    let event_emitter = emitter.clone();
    let event_shutdown = shutdown_tx.clone();
    let event_drain = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let leviculum_std::NodeEvent::CoreProcessorPanicked { hook } = event {
                // The driver has detached the processor for good and carries on
                // serving the mesh without it. From the driver's point of view
                // this helper is now a node that will never answer again, so
                // say so and stop rather than time out every later step.
                event_emitter.error(&format!(
                    "lxmf processor panicked in {hook} and was detached"
                ));
                let _ = event_shutdown.send(Shutdown::Quit);
            }
        }
    });

    // Proof-of-work, off the core lock. Dormant in every scenario whose peers
    // advertise no stamp cost, which is all of them today — Python's helper
    // registers with `stamp_cost=0`.
    //
    // Its own thread with its own current-thread runtime, not `tokio::spawn`:
    // mining is pure CPU that can run for minutes at a peer-chosen cost, so it
    // stays off the runtime's workers. `StampExecutor::generate` is `Send`
    // (Codeberg #203), so `tokio::spawn` would now accept it — this thread is a
    // scheduling choice, no longer a `!Send` workaround.
    let stamp_inputs = inputs_tx.clone();
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(e) => {
                eprintln!("[lxmf-node] no runtime for the stamp executor: {e}");
                return;
            }
        };
        runtime.block_on(async move {
            while let Some(job) = stamps_rx.recv().await {
                let mut executor = CooperativeStamper::cooperative(rand_core::OsRng);
                // No cancellation handle: this helper's protocol has no
                // cancel command, so nothing here can withdraw a stamp. The
                // grind is still at a cost the peer announced, so this worker
                // has the Codeberg #185 shape and would take the handle the
                // day the protocol grows a cancel.
                let cancel = leviculum_lxmf::StampCancel::new();
                let input = match job {
                    StampJob::Delivery(request) => {
                        match request.generate_with(&mut executor, &cancel).await {
                            Ok(stamp) => Input::StampReady { request, stamp },
                            Err(e) => Input::StampFailed {
                                request,
                                detail: format!("{e:?}"),
                            },
                        }
                    }
                    StampJob::Propagation(request) => {
                        match request.generate_with(&mut executor, &cancel).await {
                            Ok(stamp) => Input::PropagationStampReady { request, stamp },
                            Err(e) => Input::PropagationStampFailed {
                                request,
                                detail: format!("{e:?}"),
                            },
                        }
                    }
                };
                if stamp_inputs.send(input).is_err() {
                    return;
                }
            }
        });
    });

    // Resource builds, off the core lock (Codeberg #196). A plain thread with
    // a blocking receive: the build is synchronous CPU work (2–60 ms), so
    // there is nothing to await. Deliberately NOT the stamp worker's queue —
    // a build behind a minutes-long stamp mine would delay a delivery by the
    // mine, and the two workloads share nothing but "runs off the lock".
    // Dormant unless the processor was built with `--defer-resource-builds`.
    let build_inputs = inputs_tx.clone();
    thread::spawn(move || run_build_worker(builds_rx, build_inputs));

    // stdin is a blocking read on a real thread, not a tokio task: the reader
    // must keep running while the runtime is busy, and `quit` has to be seen
    // even if the node is wedged.
    let stdin_inputs = inputs_tx;
    let stdin_shutdown = shutdown_tx;
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let quit = line.trim() == "quit";
            if stdin_inputs.send(Input::Line(line)).is_err() {
                return;
            }
            // Python stops reading after `quit` (`periculum/assets/scripts/lxmf_node.py:118-119`); the
            // processor emits `lxmf_shutdown` and signals the runtime.
            if quit {
                return;
            }
        }
        let _ = stdin_inputs.send(Input::Eof);
        // Belt and braces: if the processor is already detached, `Input::Eof`
        // reaches nobody and only this gets the process out.
        let _ = stdin_shutdown.send(Shutdown::Eof);
    });

    let reason = shutdown_rx.recv().await.unwrap_or(Shutdown::Eof);
    emitter.log(format!("[lxmf-node] shutting down ({reason:?})"));
    event_drain.abort();

    let _ = node.stop().await;
    drop(emitter);
    // The writer flushes every line the moment it arrives, so all that can be
    // outstanding here is the tail of the queue. This waits for that rather
    // than joining the thread: the other `Out` senders live inside the
    // driver's event-loop task and the aborted drain, which are dropped
    // asynchronously, and a join would be waiting on task teardown rather
    // than on output.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    drop(writer);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The argv the driver sends today carries no verbosity flag, and a
    /// display name must keep arriving as a display name.
    #[test]
    fn only_verbosity_flags_are_verbosity_flags() {
        assert_eq!(verbosity_flag("-v"), Some((1, 0)));
        assert_eq!(verbosity_flag("-vvv"), Some((3, 0)));
        assert_eq!(verbosity_flag("--verbose"), Some((1, 0)));
        assert_eq!(verbosity_flag("-q"), Some((0, 1)));
        assert_eq!(verbosity_flag("-qq"), Some((0, 2)));
        assert_eq!(verbosity_flag("--quiet"), Some((0, 1)));
        assert_eq!(verbosity_flag("alice"), None);
        assert_eq!(verbosity_flag(CONFIG_FLAG), None);
        assert_eq!(verbosity_flag(DEFER_FLAG), None);
        assert_eq!(verbosity_flag("-"), None);
        assert_eq!(verbosity_flag("--"), None);
        // Not a recognised cluster, so it stays what it was before: a
        // positional. Nothing in the tree passes one.
        assert_eq!(verbosity_flag("-vq"), None);
    }

    /// What a periculum node gets without a flag: the rendered config's
    /// `loglevel = 5` (`periculum/src/topology.rs`), which is where the
    /// `event=` debug lines the hop-asymmetry cells assert on live.
    #[test]
    fn the_scenarios_config_alone_reaches_debug() {
        assert_eq!(loglevel_filter(effective_level(Some(5), 0, 0)), "debug");
    }

    /// No config, no flags: info, so `warn!` — the level of the LRPROOF
    /// hop-asymmetry line — is visible without being asked for.
    #[test]
    fn the_bare_default_still_shows_warnings() {
        assert_eq!(loglevel_filter(effective_level(None, 0, 0)), "info");
        assert_eq!(effective_level(None, 0, 0), DEFAULT_LOGLEVEL);
    }

    /// `-v`/`-q` shift the config's level and clamp at the ends of the RNS
    /// range, `lnpnd`'s arithmetic.
    #[test]
    fn verbosity_shifts_and_clamps() {
        assert_eq!(loglevel_filter(effective_level(None, 1, 0)), "debug");
        assert_eq!(loglevel_filter(effective_level(None, 3, 0)), "trace");
        assert_eq!(loglevel_filter(effective_level(None, 0, 1)), "info");
        assert_eq!(loglevel_filter(effective_level(None, 0, 2)), "warn");
        assert_eq!(loglevel_filter(effective_level(None, 0, 4)), "error");
        // Clamped, not wrapped, at both ends.
        assert_eq!(effective_level(Some(5), 9, 0), 7);
        assert_eq!(effective_level(Some(2), 0, 9), 0);
        assert_eq!(effective_level(None, 255, 255), DEFAULT_LOGLEVEL);
    }

    /// The RNS ladder this shares with `lnsd` and `lnpnd`; a divergence here
    /// would make the same config file mean two things on one node.
    #[test]
    fn the_rns_ladder_matches_the_daemons() {
        assert_eq!(loglevel_filter(0), "error");
        assert_eq!(loglevel_filter(1), "error");
        assert_eq!(loglevel_filter(2), "warn");
        assert_eq!(loglevel_filter(3), "info");
        assert_eq!(loglevel_filter(4), "info");
        assert_eq!(loglevel_filter(5), "debug");
        assert_eq!(loglevel_filter(6), "debug");
        assert_eq!(loglevel_filter(7), "trace");
    }
}

//! Long-running LXMF helper process driver.
//!
//! Each `LxmfHelper` wraps a `docker exec -i` invocation of
//! `scripts/lxmf_node.py`. The runner spawns one helper per node that
//! participates in an LXMF test. Commands are sent over the exec's
//! stdin (newline-terminated). The helper emits structured `EVENT …`
//! lines on stdout, which a background thread parses and accumulates
//! into a shared event buffer that step handlers can poll.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct EventLine {
    pub name: String,
    pub fields: BTreeMap<String, String>,
    pub recv_at: Instant,
}

impl EventLine {
    pub fn parse(line: &str) -> Option<EventLine> {
        let line = line.trim();
        let mut iter = line.split_whitespace();
        if iter.next()? != "EVENT" {
            return None;
        }
        let name = iter.next()?.to_string();
        let mut fields = BTreeMap::new();
        for tok in iter {
            if let Some((k, v)) = tok.split_once('=') {
                fields.insert(k.to_string(), v.to_string());
            }
        }
        Some(EventLine {
            name,
            fields,
            recv_at: Instant::now(),
        })
    }
}

pub struct LxmfHelper {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    events: Arc<Mutex<Vec<EventLine>>>,
    container: String,
}

impl LxmfHelper {
    /// Spawn a new helper process inside `container`.
    ///
    /// `display_name` is passed as `argv[1]` to lxmf_node.py and shown
    /// in announces. `log_path` receives a tee of the helper's stdout
    /// (every line, EVENT or not). Stderr is sent to a sibling file
    /// with `.stderr.log` appended.
    pub fn spawn(container: String, display_name: &str, log_path: PathBuf) -> io::Result<Self> {
        if let Some(parent) = log_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let mut cmd = Command::new("docker");
        cmd.args(["exec", "-i"])
            .arg(&container)
            .args([
                "python3",
                "-u",
                "/opt/integ-scripts/lxmf_node.py",
                display_name,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let events: Arc<Mutex<Vec<EventLine>>> = Arc::new(Mutex::new(Vec::new()));
        let events_thread = Arc::clone(&events);
        let stdout_log_path = log_path.clone();

        thread::spawn(move || {
            let mut log_file = fs::File::create(&stdout_log_path).ok();
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(_) => break,
                };
                if let Some(f) = log_file.as_mut() {
                    let _ = writeln!(f, "{line}");
                    let _ = f.flush();
                }
                if let Some(event) = EventLine::parse(&line) {
                    if let Ok(mut guard) = events_thread.lock() {
                        guard.push(event);
                    }
                }
            }
        });

        let stderr_log_path = log_path.with_extension("stderr.log");
        thread::spawn(move || {
            let mut log_file = fs::File::create(&stderr_log_path).ok();
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if let Some(f) = log_file.as_mut() {
                    let _ = writeln!(f, "{line}");
                    let _ = f.flush();
                }
            }
        });

        Ok(LxmfHelper {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            events,
            container,
        })
    }

    /// Container this helper is talking to (for diagnostics).
    pub fn container(&self) -> &str {
        &self.container
    }

    /// Send a single command line to the helper. Newline is appended.
    /// Fails loudly if the helper has already exited.
    pub fn send_command(&self, cmd: &str) -> io::Result<()> {
        if let Ok(mut child) = self.child.lock() {
            if let Ok(Some(status)) = child.try_wait() {
                return Err(io::Error::other(format!(
                    "lxmf helper for {} exited before command '{cmd}' (status: {status})",
                    self.container
                )));
            }
        }
        let mut stdin = self.stdin.lock().unwrap();
        stdin.write_all(cmd.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    /// Wait for the first event matching `predicate` whose `recv_at`
    /// is at or after `since`. Polls every 100ms up to `timeout`.
    pub fn wait_for_event<F>(
        &self,
        predicate: F,
        since: Instant,
        timeout: Duration,
    ) -> Option<EventLine>
    where
        F: Fn(&EventLine) -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let guard = self.events.lock().unwrap();
                for ev in guard.iter() {
                    if ev.recv_at >= since && predicate(ev) {
                        return Some(ev.clone());
                    }
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Snapshot of all events received so far.
    pub fn events_snapshot(&self) -> Vec<EventLine> {
        self.events.lock().unwrap().clone()
    }

    /// Send `quit`, wait up to 3s, then SIGKILL on timeout.
    pub fn shutdown(&self) {
        let _ = self.send_command("quit");
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let exited = self
                .child
                .lock()
                .ok()
                .and_then(|mut c| c.try_wait().ok().flatten())
                .is_some();
            if exited {
                return;
            }
            if Instant::now() >= deadline {
                if let Ok(mut child) = self.child.lock() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for LxmfHelper {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Build the per-test, per-node log file path for an LXMF helper.
pub fn helper_log_path(logs_dir: &Path, test_name: &str, node: &str, ts: &str) -> PathBuf {
    logs_dir.join(format!("{test_name}_{node}_lxmf_{ts}.log"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_event() {
        let ev = EventLine::parse(
            "EVENT lxmf_msg_received src=aabb body_b64=aGVsbG8= sig_valid=true t=1234",
        )
        .expect("parse");
        assert_eq!(ev.name, "lxmf_msg_received");
        assert_eq!(ev.fields.get("src").unwrap(), "aabb");
        assert_eq!(ev.fields.get("body_b64").unwrap(), "aGVsbG8=");
        assert_eq!(ev.fields.get("sig_valid").unwrap(), "true");
        assert_eq!(ev.fields.get("t").unwrap(), "1234");
    }

    #[test]
    fn parse_minimal_event() {
        let ev = EventLine::parse("EVENT lxmf_ready hash=ff t=1").expect("parse");
        assert_eq!(ev.name, "lxmf_ready");
        assert_eq!(ev.fields.get("hash").unwrap(), "ff");
    }

    #[test]
    fn parse_rejects_non_event() {
        assert!(EventLine::parse("[lxmf_node] starting display_name=Alice").is_none());
        assert!(EventLine::parse("").is_none());
    }
}

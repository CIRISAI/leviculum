//! A minimal harness that spawns the shared Python RNS test daemon
//! (`scripts/test_daemon.py`) and drives it over its JSON-RPC command port,
//! reusing the exact protocol of `leviculum-std`'s `rnsd_interop` harness.
//!
//! Tests use this to assert that the C API interoperates with the Python
//! Reticulum reference. If Python RNS is not available the daemon exits and
//! [`PyDaemon::start`] returns `None`, so the test skips cleanly.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use super::free_port;

/// Path to the shared daemon script (sibling `scripts/` of the repo root).
fn daemon_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("scripts")
        .join("test_daemon.py")
}

pub struct PyDaemon {
    child: Child,
    pub rns_port: u16,
    pub cmd_port: u16,
}

impl PyDaemon {
    /// Spawn the daemon and wait for its READY handshake. Returns `None`
    /// (the test should skip) if Python RNS is unavailable or it never readies.
    pub fn start() -> Option<PyDaemon> {
        Self::start_with_args(&[])
    }

    /// Like [`start`], with extra daemon flags (e.g. `--echo-channel`).
    pub fn start_with_args(extra: &[&str]) -> Option<PyDaemon> {
        let rns_port = free_port();
        let cmd_port = free_port();
        let script = daemon_script();
        if !script.exists() {
            eprintln!("skipping interop: {} not found", script.display());
            return None;
        }

        let mut child = Command::new("python3")
            .arg(&script)
            .arg("--rns-port")
            .arg(rns_port.to_string())
            .arg("--cmd-port")
            .arg(cmd_port.to_string())
            .args(extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .ok()?;

        // Read stdout on a thread, signalling when "READY" appears or stdout
        // closes (the daemon exited, e.g. RNS not importable).
        let stdout = child.stdout.take()?;
        let (tx, rx) = mpsc::channel::<bool>();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) if l.starts_with("READY ") => {
                        let _ = tx.send(true);
                        return;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            let _ = tx.send(false);
        });

        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(true) => {
                // Let interfaces settle, matching the reference harness.
                std::thread::sleep(Duration::from_millis(300));
                Some(PyDaemon {
                    child,
                    rns_port,
                    cmd_port,
                })
            }
            _ => {
                eprintln!("skipping interop: Python RNS daemon did not become ready");
                let _ = child.kill();
                None
            }
        }
    }

    /// Send one JSON-RPC command and return the parsed response value.
    fn query(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let cmd = serde_json::json!({ "method": method, "params": params });
        let mut stream =
            TcpStream::connect(("127.0.0.1", self.cmd_port)).expect("connect daemon cmd port");
        stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
        stream
            .write_all(cmd.to_string().as_bytes())
            .expect("write command");
        stream
            .shutdown(std::net::Shutdown::Write)
            .expect("shutdown write");
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).expect("read response");
        serde_json::from_slice(&buf).expect("parse JSON response")
    }

    fn result(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let v = self.query(method, params);
        if let Some(err) = v.get("error") {
            panic!("daemon {method} error: {err}");
        }
        v.get("result").cloned().unwrap_or(serde_json::Value::Null)
    }

    pub fn rns_addr(&self) -> String {
        format!("127.0.0.1:{}", self.rns_port)
    }

    /// Register a destination on the daemon; returns (hash, signing_key) as hex.
    pub fn register_destination(&self, app_name: &str, aspects: &[&str]) -> (String, String) {
        let r = self.result(
            "register_destination",
            serde_json::json!({ "app_name": app_name, "aspects": aspects }),
        );
        (
            r["hash"].as_str().unwrap().to_string(),
            r["signing_key"].as_str().unwrap().to_string(),
        )
    }

    pub fn announce_destination(&self, hash_hex: &str, app_data_hex: &str) {
        self.result(
            "announce_destination",
            serde_json::json!({ "hash": hash_hex, "app_data": app_data_hex }),
        );
    }

    pub fn has_path(&self, hash_hex: &str) -> bool {
        self.result("has_path", serde_json::json!({ "hash": hash_hex }))
            .as_bool()
            .unwrap_or(false)
    }

    /// Set the resource acceptance strategy for one of the daemon's
    /// destinations (`"accept_all"` / `"accept_none"`); apply before a link is
    /// established.
    pub fn set_resource_strategy(&self, dest_hash: &str, strategy: &str) {
        self.result(
            "set_resource_strategy",
            serde_json::json!({ "dest_hash": dest_hash, "strategy": strategy }),
        );
    }

    /// The resources the daemon has received, each a JSON object with `data`
    /// (hex), `metadata`, and `status`.
    pub fn received_resources(&self) -> Vec<serde_json::Value> {
        self.result("get_received_resources", serde_json::json!({}))
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// Verify an Ed25519 signature with the reference implementation. All
    /// arguments are hex; the public key is the 64-byte identity public key.
    pub fn verify_signature(&self, public_key: &str, message: &str, signature: &str) -> bool {
        self.result(
            "verify_signature",
            serde_json::json!({
                "public_key": public_key,
                "message": message,
                "signature": signature,
            }),
        )
        .get("valid")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    }

    /// Link hashes currently known to the daemon (hex).
    pub fn link_hashes(&self) -> Vec<String> {
        let r = self.result("get_links", serde_json::json!({}));
        r.as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Hex payloads of packets the daemon received over links.
    pub fn received_link_packets(&self) -> Vec<String> {
        let r = self.result("get_received_packets", serde_json::json!({}));
        r.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| p["data"].as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn send_on_link(&self, link_hash_hex: &str, data_hex: &str) {
        self.result(
            "send_on_link",
            serde_json::json!({ "link_hash": link_hash_hex, "data": data_hex }),
        );
    }
}

impl Drop for PyDaemon {
    fn drop(&mut self) {
        let _ = self.query("shutdown", serde_json::json!({}));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

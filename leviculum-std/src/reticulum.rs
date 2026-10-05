//! Main Reticulum instance
//!
//! High-level entry point that wires together configuration, storage,
//! core NodeCore, and the async runtime (via `ReticulumNode`).

use crate::config::Config;
use crate::driver::{EventReceiver, ReticulumNode, ReticulumNodeBuilder};
use crate::error::Result;

/// Main Reticulum instance
///
/// Wraps a `ReticulumNode` with configuration-driven setup.
pub struct Reticulum {
    /// Configuration
    config: Config,
    /// The underlying node
    node: ReticulumNode,
}

impl Reticulum {
    /// Create a new Reticulum instance with default configuration
    pub fn new() -> Result<Self> {
        let config_path = Config::default_config_path();
        let config = if config_path.exists() {
            Config::load(&config_path)?
        } else {
            Config::default()
        };

        Self::with_config(config)
    }

    /// Create a new Reticulum instance with custom configuration
    ///
    /// The builder reads enable_transport and interface configurations
    /// from the provided config automatically.
    pub fn with_config(config: Config) -> Result<Self> {
        let builder = ReticulumNodeBuilder::new().config(config.clone());

        Ok(Self {
            config,
            node: builder.build_sync()?,
        })
    }

    /// Create a new Reticulum instance in daemon-mode.
    ///
    /// Identical to `with_config` except the application event channel
    /// is not constructed. Use this for daemon-style processes (`lnsd`)
    /// that have no application code consuming `NodeEvent`s. Forwarding
    /// (broadcasts, directed sends, local-client routing) is unaffected,
    /// it runs entirely on `output.actions`.
    ///
    /// `resource_window_policy` selects the resource receive-window
    /// adaptation algorithm (Codeberg #85); the daemon binary reads it from
    /// the `LEVICULUM_RESOURCE_WINDOW_POLICY` environment variable via
    /// [`crate::resource_policy::resource_window_policy_from_env`].
    ///
    /// After this constructor, `take_event_receiver()` returns `None`.
    pub fn with_config_daemon(
        config: Config,
        resource_window_policy: leviculum_core::resource::WindowPolicy,
    ) -> Result<Self> {
        let builder = ReticulumNodeBuilder::new()
            .config(config.clone())
            .resource_window_policy(resource_window_policy)
            .without_events();

        Ok(Self {
            config,
            node: builder.build_sync()?,
        })
    }

    /// Start the Reticulum instance (spawns the event loop)
    pub async fn start(&mut self) -> Result<()> {
        self.node.start().await?;
        Ok(())
    }

    /// Stop the Reticulum instance
    pub async fn stop(&mut self) -> Result<()> {
        self.node.stop().await?;
        tracing::info!("Reticulum stopped");
        Ok(())
    }

    /// Check if the instance is running
    pub fn is_running(&self) -> bool {
        self.node.is_running()
    }

    /// Get the configuration
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Check if transport mode is enabled
    pub fn is_transport_enabled(&self) -> bool {
        self.node.is_transport_enabled()
    }

    /// Return a diagnostic dump of memory usage including process RSS
    pub fn diagnostic_dump(&self) -> String {
        let mut dump = self.node.diagnostic_dump();

        // The event-log sink's hand-off queue is std-side state the core
        // census cannot see (Codeberg #399). Bounded at 8192 lines, so a
        // large pending count is a stalled writer, not a leak; `dropped`
        // is the running loss the file also reports as EVENT_LOG_DROPPED.
        match crate::event_log::sink_status() {
            Some((pending, dropped)) => {
                // ~96 bytes per formatted line, the writer thread's own
                // batching estimate (`writer_loop`).
                dump.push_str(&format!(
                    "event_log_sink: {} pending lines, {} dropped, estimated {} bytes (queue cap 8192)\n",
                    pending,
                    dropped,
                    pending * 96
                ));
            }
            None => dump.push_str("event_log_sink: inactive\n"),
        }

        // Read RSS from /proc/self/statm (Linux only)
        if let Ok(statm) = std::fs::read_to_string("/proc/self/statm") {
            if let Some(rss_pages) = statm.split_whitespace().nth(1) {
                if let Ok(pages) = rss_pages.parse::<u64>() {
                    let rss_bytes = pages * 4096;
                    dump.push_str(&format!("=== Process RSS: {} bytes ===\n", rss_bytes));
                    // The number #399 was opened on: what the resident set
                    // holds that no census line accounts for. Printed by
                    // the dump itself so a soak journal says where the
                    // bytes are NOT, instead of only that they exist.
                    if let Some(estimated) = parse_estimated_total(&dump) {
                        dump.push_str(&format!(
                            "=== Census gap (RSS - estimated): {} bytes ===\n",
                            rss_bytes.saturating_sub(estimated)
                        ));
                    }
                }
            }
        }
        dump
    }

    /// Take the event receiver (can only be called once)
    pub fn take_event_receiver(&mut self) -> Option<EventReceiver> {
        self.node.take_event_receiver()
    }
}

/// Pull the number out of the core dump's `=== Total estimated: N bytes ===`
/// line, so the RSS line below it can be followed by the gap between the two.
fn parse_estimated_total(dump: &str) -> Option<u64> {
    let rest = dump.split("=== Total estimated: ").nth(1)?;
    rest.split_whitespace().next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_instance() {
        let td = tempfile::tempdir().expect("tempdir");
        let mut config = Config::default();
        config.reticulum.storage_path = Some(td.path().to_path_buf());
        let mut rns = Reticulum::with_config(config).unwrap();

        // Start the node
        rns.start().await.unwrap();
        assert!(rns.is_running());
        assert!(rns.is_transport_enabled());

        // Can take event receiver
        let rx = rns.take_event_receiver();
        assert!(rx.is_some());
        assert!(rns.take_event_receiver().is_none()); // Second call returns None

        rns.stop().await.unwrap();
        assert!(!rns.is_running());
    }

    /// Codeberg #399: the dump must say what the RSS holds beyond the
    /// census — the gap line — and must name the event-log sink's queue,
    /// the one std-side buffer between an emitted event and the file.
    #[tokio::test]
    async fn diagnostic_dump_prints_sink_line_and_census_gap() {
        let td = tempfile::tempdir().expect("tempdir");
        let mut config = Config::default();
        config.reticulum.storage_path = Some(td.path().to_path_buf());
        let rns = Reticulum::with_config(config).unwrap();
        let dump = rns.diagnostic_dump();
        assert!(
            dump.contains("event_log_sink: "),
            "sink line present (inactive counts): {dump}"
        );
        assert!(
            dump.contains("tunnels: "),
            "core tunnel census line reaches the composed dump: {dump}"
        );
        // The RSS, and so the gap, is read from /proc/self/statm, which only
        // Linux has; elsewhere the dump carries neither line (CIRIS fork: the
        // macOS and Windows lanes).
        #[cfg(target_os = "linux")]
        assert!(
            dump.contains("=== Census gap (RSS - estimated): "),
            "gap line follows the RSS line: {dump}"
        );
    }

    #[tokio::test]
    async fn test_transport_disabled_via_config() {
        let td = tempfile::tempdir().expect("tempdir");
        let mut config = Config::default();
        config.reticulum.enable_transport = false;
        config.reticulum.storage_path = Some(td.path().to_path_buf());
        let rns = Reticulum::with_config(config).unwrap();
        assert!(!rns.is_transport_enabled());
    }
}

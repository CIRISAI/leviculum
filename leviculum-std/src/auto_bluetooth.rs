//! The automatic layer's Bluetooth medium (#471 step 6,
//! `docs/src/concepts/zero-config.md`, "BLE").
//!
//! With `[auto] bluetooth = yes` lnsd asks BlueZ over D-Bus for an adapter
//! before it builds the node. An adapter present (powered, or powered by
//! lnsd on the spot) gives one `[[Auto Bluetooth]]` section, origin
//! `default`: a `BLEInterface` with the interface's shipped defaults, the
//! Columba room as every configured BLE interface joins it. No adapter, no
//! reachable BlueZ, or an adapter lnsd cannot power is one warning, and the
//! start goes on.
//!
//! The adapter rule is the BLE interface's own: the adapter
//! [`ENV_BLUETOOTH_ADAPTER`] names, else BlueZ's default adapter (`hci0`
//! when there is one, else the lowest name; `bluer::Session::default_adapter`).
//! The section names no adapter in the second case, so the interface takes
//! the same default when its session starts, and again after a loss.
//!
//! A configuration file with an enabled `BLEInterface` section of its own
//! keeps that section and gets none from this medium (`reason=configured`).
//! A build without the `ble` feature names the medium and attaches nothing
//! (`reason=not_built`).
//!
//! An adapter that is missing at the start is asked for again every
//! [`RESCAN`], and the section is handed to the running node once it
//! appears. After the attach the interface's own retry loop owns the
//! adapter: a loss and a return are its business
//! (`interfaces::ble`, `ADAPTER_RETRY` and the lost-adapter re-resolve).
//!
//! Every verdict is one `AUTO_BLUETOOTH` line, once, and again only when it
//! changes. The decision sits in front of the core, as the other media's do.

use std::time::Duration;

use crate::auto_layer::{yes_no, AutoLayer};
use crate::config::{Config, InterfaceConfig, InterfaceOrigin};

/// The environment knob naming the one adapter the medium may take. Unset
/// or empty is BlueZ's default adapter. A cell names its own controller
/// with it, so a configless lnsd on a rig host never takes the host's real
/// adapter; a name that does not exist is "no adapter".
pub const ENV_BLUETOOTH_ADAPTER: &str = "LEVICULUM_AUTO_BLUETOOTH_ADAPTER";

/// The section the medium adds.
pub const SECTION: &str = "Auto Bluetooth";

/// How often a missing or powered-off adapter is asked for again.
pub const RESCAN: Duration = Duration::from_secs(5);

/// The longest one question to BlueZ may take. bluetoothd answers in
/// milliseconds; a bus that does not answer must not hold the start.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(3);

/// The `reason=` of an `AUTO_BLUETOOTH` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// An adapter is there and powered: the section is attached.
    Present,
    /// No adapter, or no BlueZ to ask.
    NoAdapter,
    /// An adapter that is off and that lnsd could not power (rfkill).
    PoweredOff,
    /// The configuration has a BLE interface of its own.
    Configured,
    /// This lnsd is built without the BLE interface.
    NotBuilt,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::NoAdapter => "no_adapter",
            Self::PoweredOff => "powered_off",
            Self::Configured => "configured",
            Self::NotBuilt => "not_built",
        }
    }

    /// Whether the verdict stands for the rest of the run: nothing is asked
    /// again after it.
    pub fn settled(self) -> bool {
        !matches!(self, Self::NoAdapter | Self::PoweredOff)
    }
}

/// What one question to BlueZ found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// The adapter, powered (by lnsd, if it was off).
    Present { adapter: String },
    /// The adapter is off and stays off.
    PoweredOff { adapter: String, why: String },
    /// None to take, and why.
    NoAdapter { why: String },
}

/// The medium's verdict, as its line states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub adapter: Option<String>,
    pub reason: Reason,
    pub section: Option<String>,
}

/// The Bluetooth medium's state across the start and its rescans.
#[derive(Debug)]
pub struct AutoBluetooth {
    /// The adapter [`ENV_BLUETOOTH_ADAPTER`] names, `None` for BlueZ's
    /// default.
    wanted: Option<String>,
    /// The configuration's own enabled BLE section, if any, and its adapter.
    configured: Option<Option<String>>,
    built: bool,
    verdict: Option<Verdict>,
    /// How many `AUTO_BLUETOOTH` lines this medium logged.
    lines: usize,
}

impl AutoBluetooth {
    /// The medium for a node whose configuration is `config`. `built` is
    /// false in a build without the BLE interface.
    pub fn new(wanted: Option<String>, config: &Config, built: bool) -> Self {
        let configured = config
            .interfaces
            .values()
            .find(|i| i.enabled && i.interface_type == "BLEInterface")
            .map(|i| i.device.clone());
        Self {
            wanted,
            configured,
            built,
            verdict: None,
            lines: 0,
        }
    }

    /// Whether BlueZ needs asking: neither a configured section nor a
    /// missing build answers the question, and no adapter was taken yet.
    pub fn wants_probe(&self) -> bool {
        if self.configured.is_some() || !self.built {
            return false;
        }
        self.verdict.as_ref().is_none_or(|v| !v.reason.settled())
    }

    /// The verdict without a probe, for a medium that needs none
    /// ([`Self::wants_probe`] false). Logged once.
    pub fn settle(&mut self) {
        if self.verdict.is_some() {
            return;
        }
        let verdict = match &self.configured {
            Some(adapter) => Verdict {
                adapter: adapter.clone(),
                reason: Reason::Configured,
                section: None,
            },
            None => Verdict {
                adapter: None,
                reason: Reason::NotBuilt,
                section: None,
            },
        };
        self.record(verdict);
    }

    /// Take one probe's answer: the section to attach when an adapter was
    /// found, `None` otherwise. Logs the line when the verdict changed, and
    /// a warning with it when the adapter is missing or off.
    pub fn observe(&mut self, probe: Probe) -> Option<InterfaceConfig> {
        let (verdict, warning) = match probe {
            Probe::Present { adapter } => (
                Verdict {
                    adapter: Some(adapter),
                    reason: Reason::Present,
                    section: Some(SECTION.to_string()),
                },
                None,
            ),
            Probe::PoweredOff { adapter, why } => (
                Verdict {
                    adapter: Some(adapter.clone()),
                    reason: Reason::PoweredOff,
                    section: None,
                },
                Some(format!(
                    "[auto] bluetooth = yes, but the adapter {adapter} is powered off and \
                     lnsd could not power it ({why}); no BLE interface, asked again every {} s",
                    RESCAN.as_secs()
                )),
            ),
            Probe::NoAdapter { why } => (
                Verdict {
                    adapter: None,
                    reason: Reason::NoAdapter,
                    section: None,
                },
                Some(format!(
                    "[auto] bluetooth = yes, but there is no Bluetooth adapter to take ({why}); \
                     no BLE interface, asked again every {} s",
                    RESCAN.as_secs()
                )),
            ),
        };
        if self.verdict.as_ref() != Some(&verdict) {
            if let Some(w) = warning {
                tracing::warn!("{}", w);
            }
        }
        let section = verdict.section.as_ref().map(|_| self.section());
        self.record(verdict);
        section
    }

    fn record(&mut self, verdict: Verdict) {
        if self.verdict.as_ref() != Some(&verdict) {
            log_verdict(&verdict);
            self.lines += 1;
            self.verdict = Some(verdict);
        }
    }

    /// The section an adapter gives: the BLE interface's shipped defaults,
    /// on the adapter the knob names or on BlueZ's default.
    pub fn section(&self) -> InterfaceConfig {
        InterfaceConfig {
            name: SECTION.to_string(),
            interface_type: "BLEInterface".to_string(),
            enabled: true,
            origin: InterfaceOrigin::Default,
            device: self.wanted.clone(),
            ..Default::default()
        }
    }

    /// The medium's verdict so far.
    pub fn verdict(&self) -> Option<&Verdict> {
        self.verdict.as_ref()
    }

    /// How many `AUTO_BLUETOOTH` lines were logged.
    pub fn lines(&self) -> usize {
        self.lines
    }
}

fn log_verdict(v: &Verdict) {
    use crate::event_log::Scalar;
    tracing::info!(
        event = "AUTO_BLUETOOTH",
        adapter = %Scalar(v.adapter.as_deref().unwrap_or("none")),
        reason = v.reason.as_str(),
        attached = %yes_no(v.section.is_some()),
        section = %Scalar(v.section.as_deref().unwrap_or("none")),
    );
}

/// The adapter [`ENV_BLUETOOTH_ADAPTER`] names, `None` when unset or empty.
pub fn wanted_adapter() -> Option<String> {
    std::env::var(ENV_BLUETOOTH_ADAPTER)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The medium for this start, or `None` when `[auto] bluetooth` is off.
pub fn for_layer(layer: AutoLayer, config: &Config) -> Option<AutoBluetooth> {
    if !layer.media.bluetooth {
        return None;
    }
    let built = cfg!(all(feature = "ble", target_os = "linux"));
    if !built {
        tracing::warn!(
            "[auto] bluetooth = yes, but this lnsd is built without the BLE interface; \
             no BLE interface is started"
        );
    }
    Some(AutoBluetooth::new(wanted_adapter(), config, built))
}

/// Ask BlueZ once, bounded by [`QUERY_TIMEOUT`], and return the section to
/// attach, if any. A medium that needs no probe states its verdict instead.
pub async fn check(medium: &mut AutoBluetooth) -> Option<InterfaceConfig> {
    if !medium.wants_probe() {
        medium.settle();
        return None;
    }
    let probe = match tokio::time::timeout(QUERY_TIMEOUT, probe(medium.wanted.clone())).await {
        Ok(p) => p,
        Err(_) => Probe::NoAdapter {
            why: format!("BlueZ did not answer within {} s", QUERY_TIMEOUT.as_secs()),
        },
    };
    medium.observe(probe)
}

/// Run the startup check and add its section to `config`.
pub async fn apply(medium: &mut AutoBluetooth, config: &mut Config) {
    if let Some(section) = check(medium).await {
        if config.interfaces.contains_key(&section.name) {
            tracing::warn!(
                "[auto] bluetooth = yes, but the section [[{}]] is the configuration's own; \
                 no BLE interface is added",
                section.name
            );
            return;
        }
        config.interfaces.insert(section.name.clone(), section);
    }
}

#[cfg(all(feature = "ble", target_os = "linux"))]
async fn probe(wanted: Option<String>) -> Probe {
    let session = match bluer::Session::new().await {
        Ok(s) => s,
        Err(e) => {
            return Probe::NoAdapter {
                why: format!("BlueZ not reachable: {e}"),
            }
        }
    };
    let adapter = match &wanted {
        Some(name) => match session.adapter_names().await {
            Ok(names) if names.iter().any(|n| n == name) => session.adapter(name),
            Ok(_) => {
                return Probe::NoAdapter {
                    why: format!("BlueZ has no adapter {name}"),
                }
            }
            Err(e) => Err(e),
        },
        None => session.default_adapter().await,
    };
    let adapter = match adapter {
        Ok(a) => a,
        Err(e) if e.kind == bluer::ErrorKind::NotFound => {
            return Probe::NoAdapter {
                why: "BlueZ lists none".to_string(),
            }
        }
        Err(e) => {
            return Probe::NoAdapter {
                why: format!("BlueZ: {e}"),
            }
        }
    };
    let name = adapter.name().to_string();
    match adapter.is_powered().await {
        Ok(true) => Probe::Present { adapter: name },
        Ok(false) => match adapter.set_powered(true).await {
            Ok(()) => Probe::Present { adapter: name },
            Err(e) => Probe::PoweredOff {
                adapter: name,
                why: e.to_string(),
            },
        },
        Err(e) => Probe::NoAdapter {
            why: format!("BlueZ: {e}"),
        },
    }
}

#[cfg(not(all(feature = "ble", target_os = "linux")))]
async fn probe(_wanted: Option<String>) -> Probe {
    Probe::NoAdapter {
        why: "built without the BLE interface".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(sections: &[InterfaceConfig]) -> Config {
        let mut config = Config::default();
        for s in sections {
            config.interfaces.insert(s.name.clone(), s.clone());
        }
        config
    }

    fn ble(name: &str, device: Option<&str>) -> InterfaceConfig {
        InterfaceConfig {
            name: name.to_string(),
            interface_type: "BLEInterface".to_string(),
            enabled: true,
            device: device.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn an_adapter_present_gives_one_default_section_on_the_default_adapter() {
        let mut m = AutoBluetooth::new(None, &Config::default(), true);
        assert!(m.wants_probe());
        let s = m
            .observe(Probe::Present {
                adapter: "hci0".into(),
            })
            .expect("a section");
        assert_eq!(s.name, SECTION);
        assert_eq!(s.interface_type, "BLEInterface");
        assert_eq!(s.origin, InterfaceOrigin::Default);
        assert_eq!(s.device, None, "the interface takes BlueZ's default itself");
        assert!(s.enabled);
        // Everything else is the interface's shipped default.
        assert_eq!(s.max_connections, None);
        assert_eq!(s.initiate_only, None);
        assert_eq!(s.accept_only, None);
        assert_eq!(
            m.verdict(),
            Some(&Verdict {
                adapter: Some("hci0".into()),
                reason: Reason::Present,
                section: Some(SECTION.into()),
            })
        );
        assert!(!m.wants_probe(), "an attached adapter is the interface's");
    }

    #[test]
    fn the_knob_names_the_adapter_of_the_section() {
        let m = AutoBluetooth::new(Some("hci7".into()), &Config::default(), true);
        assert_eq!(m.section().device.as_deref(), Some("hci7"));
    }

    #[test]
    fn a_missing_adapter_is_asked_again_and_logged_once_until_it_appears() {
        let mut m = AutoBluetooth::new(None, &Config::default(), true);
        let none = || Probe::NoAdapter {
            why: "BlueZ lists none".into(),
        };
        assert!(m.observe(none()).is_none());
        assert!(m.observe(none()).is_none());
        assert_eq!(m.lines(), 1, "the same verdict is logged once");
        assert!(m.wants_probe(), "a missing adapter is asked for again");
        assert_eq!(m.verdict().map(|v| v.reason), Some(Reason::NoAdapter));
        // The adapter appears later: the next rescan attaches it.
        let s = m.observe(Probe::Present {
            adapter: "hci1".into(),
        });
        assert_eq!(s.map(|s| s.name), Some(SECTION.to_string()));
        assert_eq!(m.lines(), 2);
        assert!(!m.wants_probe());
    }

    #[test]
    fn a_powered_off_adapter_is_a_warning_and_asked_again() {
        let mut m = AutoBluetooth::new(None, &Config::default(), true);
        let off = Probe::PoweredOff {
            adapter: "hci0".into(),
            why: "rfkill".into(),
        };
        assert!(m.observe(off.clone()).is_none());
        assert!(m.observe(off).is_none());
        assert_eq!(m.lines(), 1);
        assert_eq!(
            m.verdict(),
            Some(&Verdict {
                adapter: Some("hci0".into()),
                reason: Reason::PoweredOff,
                section: None,
            })
        );
        assert!(m.wants_probe());
    }

    #[test]
    fn a_configured_ble_section_wins_without_a_probe() {
        let config = config_with(&[ble("phones", Some("hci1"))]);
        let mut m = AutoBluetooth::new(None, &config, true);
        assert!(!m.wants_probe());
        m.settle();
        m.settle();
        assert_eq!(m.lines(), 1);
        assert_eq!(
            m.verdict(),
            Some(&Verdict {
                adapter: Some("hci1".into()),
                reason: Reason::Configured,
                section: None,
            })
        );
    }

    #[test]
    fn a_disabled_ble_section_does_not_count_as_configured() {
        let mut off = ble("phones", None);
        off.enabled = false;
        let m = AutoBluetooth::new(None, &config_with(&[off]), true);
        assert!(m.wants_probe());
    }

    #[test]
    fn a_build_without_ble_names_the_medium_and_attaches_nothing() {
        let mut m = AutoBluetooth::new(None, &Config::default(), false);
        assert!(!m.wants_probe());
        m.settle();
        assert_eq!(m.verdict().map(|v| v.reason), Some(Reason::NotBuilt));
        assert_eq!(m.verdict().and_then(|v| v.section.clone()), None);
    }

    #[test]
    fn reasons_spell_as_the_line_does() {
        let all = [
            (Reason::Present, "present"),
            (Reason::NoAdapter, "no_adapter"),
            (Reason::PoweredOff, "powered_off"),
            (Reason::Configured, "configured"),
            (Reason::NotBuilt, "not_built"),
        ];
        for (r, s) in all {
            assert_eq!(r.as_str(), s);
        }
    }

    #[tokio::test]
    async fn the_startup_check_adds_nothing_for_a_configured_section() {
        let mut config = config_with(&[ble("phones", None)]);
        let mut m = AutoBluetooth::new(None, &config, true);
        apply(&mut m, &mut config).await;
        assert_eq!(config.interfaces.len(), 1);
        assert!(!config.interfaces.contains_key(SECTION));
    }
}

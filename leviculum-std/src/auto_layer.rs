//! The automatic layer's switches (#471 step 2, `docs/src/concepts/zero-config.md`).
//!
//! A fresh lnsd brings up what the machine offers; a configuration file is
//! the opt-out, per medium, through its `[auto]` section. This module holds
//! the four switches, the rule that turns a file, the host marker and the
//! environment into one decision, and the medium it moves itself: the LAN
//! (an AutoInterface). The servers medium is [`crate::auto_servers`] (step
//! 3), serial is [`crate::auto_serial`] (step 5), Bluetooth is
//! [`crate::auto_bluetooth`] (step 6).
//!
//! The decision sits in front of the core. It edits the interface list a
//! [`Config`] hands the builder, and the core never learns that an
//! interface was started automatically.

use serde::{Deserialize, Serialize};

use crate::config::{Config, ConfigSource, InterfaceOrigin};

/// The host marker: a file at this path turns the automatic layer off on
/// every start, with or without a configuration file. Installed on the rig
/// and coder hosts by `scripts/install-ci.sh`.
pub const HOST_MARKER: &str = "/etc/leviculum/no-auto";

/// The environment switch. `1` (or any spelling of yes) turns the automatic
/// layer off; `0` (or any spelling of no) lifts the host marker, which is
/// how a cell measures the automatic layer on a marked host.
pub const ENV_NO_AUTO: &str = "LEVICULUM_NO_AUTO";

/// The `[auto]` section: which media lnsd may start on its own.
///
/// `Default` is every key `no`, which is what a file without the section
/// means. [`AutoMedia::SHIPPED`] is what a start without a file means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoMedia {
    /// An AutoInterface when the configuration names none.
    pub lan: bool,
    /// The shipped server list (step 3, [`crate::auto_servers`]).
    pub servers: bool,
    /// BLE when an adapter is present (step 6, [`crate::auto_bluetooth`]).
    pub bluetooth: bool,
    /// LNodes and RNodes on USB serial ports (step 5, [`crate::auto_serial`]).
    pub serial: bool,
}

impl AutoMedia {
    /// Every medium off: a file without `[auto]`, the marker, the
    /// environment switch.
    pub const NONE: Self = Self {
        lan: false,
        servers: false,
        bluetooth: false,
        serial: false,
    };

    /// The shipped defaults, what a start without a file runs and what
    /// `lnsd --writeconfig` writes. Each medium turns `yes` here when its
    /// step lands.
    pub const SHIPPED: Self = Self {
        lan: true,
        servers: true,
        bluetooth: true,
        serial: true,
    };

    /// Whether any medium may start.
    pub fn any(self) -> bool {
        self.lan || self.servers || self.bluetooth || self.serial
    }
}

/// The `[auto]` section `lnsd --writeconfig` appends to rnsd's first-start
/// file, with [`AutoMedia::SHIPPED`] and a comment line per key. rnsd and the
/// stock tools read `[logging]`, `[reticulum]` and `[interfaces]` only
/// (`Reticulum.py:469-545`, `Reticulum.py:701-702`), so the file stays
/// theirs to read. Indented the way ConfigObj writes a section.
pub const AUTO_SECTION_DEFAULT: &str = include_str!("auto_section_default");

/// Why the automatic layer is in the state it is in: the `reason=` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoReason {
    /// The file's `[auto]` section, or its absence.
    Config,
    /// The host marker, [`HOST_MARKER`].
    Marker,
    /// The environment switch, [`ENV_NO_AUTO`].
    Env,
    /// No file: the shipped defaults.
    Default,
}

impl AutoReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Marker => "marker",
            Self::Env => "env",
            Self::Default => "default",
        }
    }
}

/// The decision lnsd takes once per start and states in its `AUTO_LAYER`
/// line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoLayer {
    pub media: AutoMedia,
    pub reason: AutoReason,
}

impl AutoLayer {
    /// `on` when any medium may start, else `off`.
    pub fn state(self) -> &'static str {
        if self.media.any() {
            "on"
        } else {
            "off"
        }
    }

    /// The decision as `lnstatus` shows it, the `AUTO_LAYER` line's fields
    /// after `state=`.
    pub fn summary(self) -> String {
        format!(
            "{} reason={} lan={} servers={} bluetooth={} serial={}",
            self.state(),
            self.reason.as_str(),
            yes_no(self.media.lan),
            yes_no(self.media.servers),
            yes_no(self.media.bluetooth),
            yes_no(self.media.serial),
        )
    }

    /// The `AUTO_LAYER` line, once per start, after a plain line naming the
    /// marker or the variable when one of them turned the layer off.
    pub fn log(self) {
        match self.reason {
            AutoReason::Marker => tracing::info!(
                "The automatic layer is off: the host marker {} exists \
                 ({}=0 lifts it)",
                HOST_MARKER,
                ENV_NO_AUTO
            ),
            AutoReason::Env => tracing::info!(
                "The automatic layer is off: {} is set in the environment",
                ENV_NO_AUTO
            ),
            AutoReason::Config | AutoReason::Default => {}
        }
        tracing::info!(
            event = "AUTO_LAYER",
            state = %self.state(),
            reason = %self.reason.as_str(),
            lan = %yes_no(self.media.lan),
            servers = %yes_no(self.media.servers),
            bluetooth = %yes_no(self.media.bluetooth),
            serial = %yes_no(self.media.serial),
        );
    }
}

/// The token a key is written with in `[auto]`, and logged with.
pub fn yes_no(on: bool) -> &'static str {
    if on {
        "yes"
    } else {
        "no"
    }
}

/// What the environment switch says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvSwitch {
    /// Unset or empty.
    Unset,
    /// Turns the automatic layer off.
    Off,
    /// Lifts the host marker.
    LiftMarker,
}

impl EnvSwitch {
    /// Read a value of [`ENV_NO_AUTO`]. A value that is no spelling of no
    /// turns the layer off: an operator who set the variable at all meant
    /// to keep the automatic layer out, and a typo must not let it in.
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(|v| v.trim().to_ascii_lowercase()) {
            None => Self::Unset,
            Some(v) if v.is_empty() => Self::Unset,
            Some(v) if matches!(v.as_str(), "0" | "no" | "false" | "off") => Self::LiftMarker,
            Some(_) => Self::Off,
        }
    }
}

/// The per-medium rule: which automatic interfaces may start.
///
/// | configuration (`file_media`) | marker  | `LEVICULUM_NO_AUTO` | media           | reason    |
/// |------------------------------|---------|---------------------|-----------------|-----------|
/// | asks for a medium            | any     | `1`                 | none            | `env`     |
/// | asks for a medium            | present | unset               | none            | `marker`  |
/// | no file: the shipped defaults | absent, or lifted by `0` | unset or `0` | [`AutoMedia::SHIPPED`] | `default` |
/// | a file: its `[auto]` keys     | absent, or lifted by `0` | unset or `0` | the keys that are `yes` | `config` |
/// | a file asking for nothing     | any     | any                 | none            | `config`  |
///
/// A medium starts only when its key is `yes` and neither the marker nor
/// the variable turns the layer off; the variable wins over the marker in
/// both directions. A file with interfaces and no `[auto]` section asks
/// for nothing, so it starts nothing automatic, and its reason is `config`
/// on a marked host too: the marker and the variable are named only when
/// they turned off something the configuration asked for. That keeps every
/// periculum cell's line `state=off reason=config` on every host, which is
/// what `periculum auto-layer --require-line` holds them to.
///
/// `file_media` is the configuration's `[auto]` section as parsed:
/// [`AutoMedia::NONE`] for a file without one, the shipped defaults for a
/// start without a file.
pub fn decide(
    source: ConfigSource,
    file_media: AutoMedia,
    env: EnvSwitch,
    marker: bool,
) -> AutoLayer {
    let own = match source {
        ConfigSource::File => AutoReason::Config,
        ConfigSource::Default => AutoReason::Default,
    };
    let override_reason = match (env, marker) {
        (EnvSwitch::Off, _) => Some(AutoReason::Env),
        (EnvSwitch::Unset, true) => Some(AutoReason::Marker),
        _ => None,
    };
    match override_reason {
        Some(reason) if file_media.any() => AutoLayer {
            media: AutoMedia::NONE,
            reason,
        },
        _ => AutoLayer {
            media: file_media,
            reason: own,
        },
    }
}

/// [`decide`] with this host's inputs: [`ENV_NO_AUTO`] from the process
/// environment and whether [`HOST_MARKER`] exists.
pub fn decide_for_host(source: ConfigSource, file_media: AutoMedia) -> AutoLayer {
    let env = std::env::var(ENV_NO_AUTO).ok();
    let marker = std::path::Path::new(HOST_MARKER).exists();
    decide(source, file_media, EnvSwitch::parse(env.as_deref()), marker)
}

/// The section name of the AutoInterface the LAN medium adds, the one in
/// rnsd's first-start file.
pub const LAN_SECTION: &str = "Default Interface";

/// Apply `layer` to `config`'s interfaces and record it on the config.
///
/// The LAN is the one medium this step moves. With `lan` on and no
/// AutoInterface section in the configuration (enabled or not: a section
/// the operator wrote is theirs), the AutoInterface of rnsd's first-start
/// file is added, with origin `default`. With `lan` off, every interface of
/// origin `default` goes: on a start without a file that is the
/// AutoInterface the shipped defaults hold, and a file's own sections are
/// never touched.
pub fn apply(config: &mut Config, layer: AutoLayer) -> crate::error::Result<()> {
    if layer.media.lan {
        let named = config
            .interfaces
            .values()
            .any(|i| i.interface_type == "AutoInterface");
        if !named {
            if config.interfaces.contains_key(LAN_SECTION) {
                tracing::warn!(
                    "[auto] lan = yes, but the section [[{}]] is not an AutoInterface; \
                     no AutoInterface is added",
                    LAN_SECTION
                );
            } else {
                let mut default = Config::rnsd_default()?;
                if let Some(lan) = default.interfaces.remove(LAN_SECTION) {
                    config.interfaces.insert(LAN_SECTION.to_string(), lan);
                }
            }
        }
    } else {
        config
            .interfaces
            .retain(|_, i| i.origin != InterfaceOrigin::Default);
    }
    config.auto_layer = Some(layer);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAN: AutoMedia = AutoMedia {
        lan: true,
        servers: false,
        bluetooth: false,
        serial: false,
    };

    /// Every row of the table in [`decide`]'s doc comment.
    #[test]
    fn the_rule_table() {
        use ConfigSource::{Default as NoFile, File};
        use EnvSwitch::{LiftMarker, Off, Unset};
        let none = AutoMedia::NONE;
        let shipped = AutoMedia::SHIPPED;
        #[rustfmt::skip]
        let rows: &[(ConfigSource, AutoMedia, EnvSwitch, bool, AutoMedia, AutoReason)] = &[
            // The environment off wins over everything.
            (NoFile, shipped, Off, false, none, AutoReason::Env),
            (NoFile, shipped, Off, true, none, AutoReason::Env),
            (File, LAN, Off, false, none, AutoReason::Env),
            // The marker, unless the environment lifts it.
            (NoFile, shipped, Unset, true, none, AutoReason::Marker),
            (File, LAN, Unset, true, none, AutoReason::Marker),
            (NoFile, shipped, LiftMarker, true, shipped, AutoReason::Default),
            (File, LAN, LiftMarker, true, LAN, AutoReason::Config),
            // No file: the shipped defaults.
            (NoFile, shipped, Unset, false, shipped, AutoReason::Default),
            // A file: its [auto] keys, all `no` without the section.
            (File, none, Unset, false, none, AutoReason::Config),
            (File, LAN, Unset, false, LAN, AutoReason::Config),
            (File, none, LiftMarker, false, none, AutoReason::Config),
            // A file asking for nothing is the config's `off`, marker or not.
            (File, none, Unset, true, none, AutoReason::Config),
            (File, none, Off, false, none, AutoReason::Config),
            (File, none, Off, true, none, AutoReason::Config),
        ];
        for &(source, file, env, marker, media, reason) in rows {
            assert_eq!(
                decide(source, file, env, marker),
                AutoLayer { media, reason },
                "{source:?} {file:?} {env:?} marker={marker}"
            );
        }
    }

    #[test]
    fn the_environment_switch_spellings() {
        assert_eq!(EnvSwitch::parse(None), EnvSwitch::Unset);
        assert_eq!(EnvSwitch::parse(Some("")), EnvSwitch::Unset);
        for off in ["1", "yes", "true", "on", " 1 ", "bogus"] {
            assert_eq!(EnvSwitch::parse(Some(off)), EnvSwitch::Off, "{off}");
        }
        for lift in ["0", "no", "False", "off"] {
            assert_eq!(
                EnvSwitch::parse(Some(lift)),
                EnvSwitch::LiftMarker,
                "{lift}"
            );
        }
    }

    #[test]
    fn the_line_names_state_reason_and_every_key() {
        let layer = AutoLayer {
            media: AutoMedia::NONE,
            reason: AutoReason::Config,
        };
        assert_eq!(
            layer.summary(),
            "off reason=config lan=no servers=no bluetooth=no serial=no"
        );
        let layer = AutoLayer {
            media: AutoMedia::SHIPPED,
            reason: AutoReason::Default,
        };
        assert_eq!(
            layer.summary(),
            "on reason=default lan=yes servers=yes bluetooth=yes serial=yes"
        );
    }

    fn lan_interface() -> crate::config::InterfaceConfig {
        let mut config = Config::rnsd_default().expect("default parses");
        config
            .interfaces
            .remove(LAN_SECTION)
            .expect("the default AutoInterface")
    }

    fn tcp(name: &str) -> crate::config::InterfaceConfig {
        crate::config::InterfaceConfig {
            name: name.to_string(),
            interface_type: "TCPClientInterface".to_string(),
            ..Default::default()
        }
    }

    /// `lan = yes` in a file that names no AutoInterface adds the default
    /// one; a file that names one keeps exactly its own.
    #[test]
    fn lan_adds_an_auto_interface_only_when_the_file_names_none() {
        let on = AutoLayer {
            media: LAN,
            reason: AutoReason::Config,
        };
        let mut config = Config::default();
        config.interfaces.insert("tcp".into(), tcp("tcp"));
        apply(&mut config, on).expect("apply");
        let added = config.interfaces.get(LAN_SECTION).expect("added");
        assert_eq!(added.interface_type, "AutoInterface");
        assert_eq!(added.origin, InterfaceOrigin::Default);
        assert_eq!(config.interfaces.len(), 2);
        assert_eq!(config.auto_layer, Some(on));

        let mut config = Config::default();
        let mut own = lan_interface();
        own.name = "My LAN".into();
        own.origin = InterfaceOrigin::Configured;
        own.enabled = false;
        config.interfaces.insert("My LAN".into(), own);
        apply(&mut config, on).expect("apply");
        assert_eq!(
            config.interfaces.keys().collect::<Vec<_>>(),
            vec!["My LAN"],
            "a disabled AutoInterface the file names is still the file's"
        );
    }

    /// `lan = no` removes the default AutoInterface of a start without a
    /// file and leaves a file's sections alone.
    #[test]
    fn lan_off_removes_only_default_interfaces() {
        let off = AutoLayer {
            media: AutoMedia::NONE,
            reason: AutoReason::Env,
        };
        let mut config = Config::rnsd_default().expect("default parses");
        apply(&mut config, off).expect("apply");
        assert!(config.interfaces.is_empty());

        let mut config = Config::default();
        let mut own = lan_interface();
        own.origin = InterfaceOrigin::Configured;
        config.interfaces.insert(LAN_SECTION.into(), own);
        apply(&mut config, off).expect("apply");
        assert_eq!(config.interfaces.len(), 1);
    }
}

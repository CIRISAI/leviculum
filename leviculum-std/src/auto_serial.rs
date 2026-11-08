//! The automatic layer's serial medium (#471 step 5,
//! `docs/src/concepts/zero-config.md`, "Serial: LNodes by their name, RNodes
//! by silence, then a probe").
//!
//! With `[auto] serial = yes` lnsd brings up every USB serial board it can
//! name, and writes to nothing it cannot. It reads the by-id directory
//! ([`ENV_SERIAL_DIR`], default [`DEFAULT_SERIAL_DIR`]) and sorts each entry
//! by its USB ids, read from sysfs, or, where the link leads to no USB device
//! (a cell's pty), by the strings udev built its name from:
//!
//! | ids | name | what it is | what lnsd does |
//! |---|---|---|---|
//! | a `bootloader_usb` id of `lnflash/catalogue.toml` | or `bootloader` in the name | a board in its bootloader | nothing, not opened |
//! | a `1209:*` `candidate_usb` id of the catalogue | and `usb-leviculum_...` | an LNode (our firmware sets the strings) | `if02`: a `SerialInterface`; `if00` (debug log): nothing |
//! | [`RNODE_USB`] | or one of its name fragments | an RNode candidate | listens [`SILENCE`]; silent: one `CMD_DETECT`; `DETECT_RESP`: an `RNodeInterface` |
//! | anything else | | a stranger | nothing, not opened |
//!
//! A port another process holds (an fd on it in `/proc`, or `EBUSY` under
//! `TIOCEXCL`) is skipped and asked again on the next scan. An LNode needs
//! no open at all; a candidate is opened under `TIOCEXCL` for its silence
//! and its probe, and closed before the interface opens it.
//!
//! Each board gets an `[[Auto Serial n]]` section with the shipped LoRa
//! defaults ([`LORA_FREQUENCY`], [`LORA_BANDWIDTH`], [`LORA_SPREADING_FACTOR`],
//! [`LORA_CODING_RATE`]), origin `default`, its port the by-id link, so a board
//! that leaves and comes back is found again by the interface's own reconnect
//! loop. A board that appears later is found by the next scan, every
//! [`RESCAN`]. Every verdict is one `AUTO_SERIAL` line, once per entry and
//! again only when it changes.
//!
//! The decision sits in front of the core: this module edits the interface
//! list a configuration hands the builder, or hands a section to the running
//! node, and the core never learns where it came from.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use crate::auto_layer::AutoLayer;
use crate::config::{Config, InterfaceConfig, InterfaceOrigin};

/// The environment knob naming the directory lnsd reads boards from. A cell
/// points it at ptys; unset is [`DEFAULT_SERIAL_DIR`].
pub const ENV_SERIAL_DIR: &str = "LEVICULUM_AUTO_SERIAL_DIR";

/// Where udev links every USB serial port under the strings of its device.
pub const DEFAULT_SERIAL_DIR: &str = "/dev/serial/by-id";

/// How long an RNode candidate must stay silent before it is asked. An RNode
/// firmware says nothing until it is asked; a GPS speaks NMEA every second,
/// a printer its status.
pub const SILENCE: Duration = Duration::from_secs(1);

/// How long a probed candidate has to answer `DETECT_RESP`. The
/// RNodeInterface's own detect waits 200 ms (`interfaces::rnode`); a
/// first-time probe gets more, it may meet a board just through its boot.
pub const PROBE_ANSWER: Duration = Duration::from_millis(500);

/// The rescan period: a board plugged in after the start is found this long
/// after its link appears at most, plus its silence.
pub const RESCAN: Duration = Duration::from_secs(5);

/// How often a candidate that talked during its silence is listened to again
/// before it counts as a stranger. A booting ESP32 prints its ROM log once;
/// a GPS never stops.
pub const TALKING_LISTENS: u32 = 3;

/// The section names: `Auto Serial 1`, `Auto Serial 2`, ...
pub const SECTION_PREFIX: &str = "Auto Serial";

/// The shipped LoRa defaults every automatic interface starts on: what a
/// board boots on standalone (`project_lora_default_phy`).
pub const LORA_FREQUENCY: u64 = 869_463_000;
/// See [`LORA_FREQUENCY`].
pub const LORA_BANDWIDTH: u32 = 125_000;
/// See [`LORA_FREQUENCY`].
pub const LORA_SPREADING_FACTOR: u8 = 8;
/// See [`LORA_FREQUENCY`].
pub const LORA_CODING_RATE: u8 = 5;

/// The line speed an LNode's transport port is opened at. A CDC ACM port
/// ignores it; it is the value every LNode section in the corpus names.
pub const LNODE_SPEED: u32 = 115_200;

/// The line speed an RNode is probed at, the RNode firmware's.
pub const RNODE_SPEED: u32 = 115_200;

/// The `bInterfaceNumber` of an LNode's transport port; `if00` is its debug
/// log (`leviculum-nrf/src/usb.rs`).
pub const LNODE_DATA_INTERFACE: u8 = 2;

/// A USB `vid:pid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UsbId {
    pub vid: u16,
    pub pid: u16,
}

impl UsbId {
    pub const fn new(vid: u16, pid: u16) -> Self {
        Self { vid, pid }
    }

    /// `vid:pid` as lsusb writes it, the way the catalogue does.
    pub fn parse(s: &str) -> Option<Self> {
        let (vid, pid) = s.trim().split_once(':')?;
        Some(Self {
            vid: u16::from_str_radix(vid.trim(), 16).ok()?,
            pid: u16::from_str_radix(pid.trim(), 16).ok()?,
        })
    }
}

impl std::fmt::Display for UsbId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04x}:{:04x}", self.vid, self.pid)
    }
}

/// The USB bridges RNodes sit on, measured on the rig, each with the
/// fragment of the by-id name udev builds from its strings. A bridge is
/// carried by a thousand other devices too, which is why a candidate is
/// asked only after its silence.
///
/// `1a86:7522` (CH340K) is deliberately absent: it is the ThinkNode M9's
/// bridge, and the M9 runs our firmware, not RNode's.
pub const RNODE_USB: &[(UsbId, &str, &str)] = &[
    // Heltec V3 (and T-Beam v1.1, LoRa32): CP2102/CP2104.
    (UsbId::new(0x10c4, 0xea60), "cp210x", "Silicon_Labs_CP210"),
    // The rig's T-Beams: CH9102.
    (
        UsbId::new(0x1a86, 0x55d4),
        "ch9102",
        "1a86_USB_Single_Serial",
    ),
    // Heltec V4 (and T-Beam Supreme, T3S3): the ESP32-S3's own USB.
    (
        UsbId::new(0x303a, 0x1001),
        "esp32-usb-jtag",
        "Espressif_USB_JTAG_serial_debug_unit",
    ),
];

/// The vendor id of the pid.codes TEST ids our firmware enumerates with
/// (`project_usb_id_registration_open`).
pub const PID_CODES_VID: u16 = 0x1209;

/// The USB ids the board catalogue knows: the bootloaders (never probed,
/// never a node) and our firmware's application ids. Read from
/// `lnflash/catalogue.toml`, the file lnflash compiles in, so a new board is
/// one entry there for both tools.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogueIds {
    pub bootloader: Vec<UsbId>,
    pub lnode: Vec<UsbId>,
}

const CATALOGUE: &str = include_str!("../../lnflash/catalogue.toml");

impl CatalogueIds {
    /// Parse a catalogue's text: every `board.*.flashing.identify.bootloader_usb`
    /// and every `board.*.candidate_usb` id under [`PID_CODES_VID`].
    pub fn parse(text: &str) -> Result<Self, String> {
        let value: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut ids = Self::default();
        let Some(boards) = value.get("board").and_then(|b| b.as_table()) else {
            return Err("no [board] table".to_string());
        };
        let list = |v: Option<&toml::Value>| -> Vec<UsbId> {
            v.and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().and_then(UsbId::parse))
                        .collect()
                })
                .unwrap_or_default()
        };
        for board in boards.values() {
            ids.lnode.extend(
                list(board.get("candidate_usb"))
                    .into_iter()
                    .filter(|id| id.vid == PID_CODES_VID),
            );
            let identify = board.get("flashing").and_then(|f| f.get("identify"));
            ids.bootloader
                .extend(list(identify.and_then(|i| i.get("bootloader_usb"))));
        }
        Ok(ids)
    }

    /// The catalogue compiled into this build.
    pub fn builtin() -> &'static Self {
        static IDS: OnceLock<CatalogueIds> = OnceLock::new();
        IDS.get_or_init(|| match Self::parse(CATALOGUE) {
            Ok(ids) => ids,
            Err(e) => {
                // Without the catalogue no LNode is named and no bootloader
                // is known by its id; the name rules below still hold.
                tracing::warn!("the board catalogue does not parse: {}", e);
                Self::default()
            }
        })
    }
}

/// What a by-id entry is, before anything is opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Class {
    /// A board in its bootloader: never probed, never a node.
    Bootloader,
    /// An LNode's transport port: attached without a probe.
    Lnode { board: String },
    /// An LNode's other port (its debug log): left alone.
    LnodeDebug { board: String },
    /// An RNode candidate: listened to, then probed.
    RnodeCandidate { bridge: &'static str },
    /// Anything else: never opened.
    Stranger,
}

/// The `-ifNN` interface number of a by-id name.
pub fn interface_number(name: &str) -> Option<u8> {
    let at = name.rfind("-if")?;
    let digits: String = name[at + 3..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    u8::from_str_radix(&digits, 16).ok()
}

/// The board of an LNode by-id name, `usb-leviculum_leviculum_<Board>_<Serial>-ifNN`
/// (and the older `usb-leviculum_<Board>_<Serial>-ifNN`). `None` when the
/// name is not ours.
pub fn lnode_board(name: &str) -> Option<String> {
    let rest = name.strip_prefix("usb-leviculum_")?;
    let rest = rest.strip_prefix("leviculum_").unwrap_or(rest);
    let base = &rest[..rest.rfind("-if").unwrap_or(rest.len())];
    let (board, _serial) = base.rsplit_once('_')?;
    (!board.is_empty()).then(|| board.to_string())
}

/// Sort one by-id entry. `ids` are its USB ids where sysfs has them; without
/// them the name decides alone.
pub fn classify(name: &str, ids: Option<UsbId>, catalogue: &CatalogueIds) -> Class {
    if let Some(id) = ids {
        if catalogue.bootloader.contains(&id) {
            return Class::Bootloader;
        }
    }
    if name.to_ascii_lowercase().contains("bootloader") {
        return Class::Bootloader;
    }
    if let Some(board) = lnode_board(name) {
        let ours = match ids {
            Some(id) => catalogue.lnode.contains(&id),
            None => true,
        };
        if ours {
            return if interface_number(name) == Some(LNODE_DATA_INTERFACE) {
                Class::Lnode { board }
            } else {
                Class::LnodeDebug { board }
            };
        }
    }
    let bridge = match ids {
        Some(id) => RNODE_USB
            .iter()
            .find(|(usb, _, _)| *usb == id)
            .map(|(_, b, _)| *b),
        None => RNODE_USB
            .iter()
            .find(|(_, _, fragment)| name.contains(fragment))
            .map(|(_, b, _)| *b),
    };
    match bridge {
        Some(bridge) => Class::RnodeCandidate { bridge },
        None => Class::Stranger,
    }
}

/// The `kind=` of a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Lnode,
    Rnode,
    Bootloader,
    Other,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lnode => "lnode",
            Self::Rnode => "rnode",
            Self::Bootloader => "bootloader",
            Self::Other => "other",
        }
    }
}

/// Why an entry was or was not attached, the `reason=` of its line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// An LNode, by its strings.
    Name,
    /// An RNode, by its `DETECT_RESP`.
    Detect,
    /// An LNode's debug port.
    DebugPort,
    /// A bootloader.
    Bootloader,
    /// No candidate: never opened.
    NotCandidate,
    /// Another process holds the port.
    Held,
    /// The candidate spoke during its silence.
    Talking,
    /// The candidate was asked and did not answer.
    NoAnswer,
    /// The configuration names this port already.
    Configured,
    /// lnsd may not open the port (permissions).
    NoAccess,
    /// The open failed otherwise.
    OpenFailed,
    /// This build cannot attach the board (no `serial` feature).
    NotBuilt,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Detect => "detect",
            Self::DebugPort => "debug_port",
            Self::Bootloader => "bootloader",
            Self::NotCandidate => "not_candidate",
            Self::Held => "held",
            Self::Talking => "talking",
            Self::NoAnswer => "no_answer",
            Self::Configured => "configured",
            Self::NoAccess => "no_access",
            Self::OpenFailed => "open_failed",
            Self::NotBuilt => "not_built",
        }
    }

    /// Whether the entry is settled: not asked again while its link stays.
    /// A held port, a talker below [`TALKING_LISTENS`] and a port lnsd could
    /// not open are asked again on the next scan.
    fn settled(self) -> bool {
        !matches!(self, Self::Held | Self::NoAccess | Self::OpenFailed)
    }
}

/// One entry's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub kind: Kind,
    pub board: Option<String>,
    pub reason: Reason,
    /// The section lnsd attached, when it did.
    pub section: Option<String>,
}

/// What a probe of a candidate found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Rnode,
    Talking,
    NoAnswer,
    Held,
    NoAccess,
    OpenFailed,
}

/// Whether `bytes` carry a `DETECT_RESP` frame.
pub fn has_detect_response(bytes: &[u8]) -> bool {
    use leviculum_core::framing::kiss::FEND;
    use leviculum_core::rnode::{CMD_DETECT, DETECT_RESP};
    bytes
        .windows(3)
        .any(|w| w == [FEND, CMD_DETECT, DETECT_RESP])
}

/// The one frame a candidate is ever sent: `CMD_DETECT` with `DETECT_REQ`.
pub fn detect_request() -> [u8; 4] {
    use leviculum_core::framing::kiss::FEND;
    use leviculum_core::rnode::{CMD_DETECT, DETECT_REQ};
    [FEND, CMD_DETECT, DETECT_REQ, FEND]
}

/// The section an LNode gets: its transport port, the shipped PHY, and
/// `interface_mode = gateway`, because the board behind the line asks paths
/// for itself and only a discovering mode looks for them onwards.
pub fn lnode_section(name: &str, port: &Path) -> InterfaceConfig {
    InterfaceConfig {
        name: name.to_string(),
        interface_type: "SerialInterface".to_string(),
        enabled: true,
        origin: InterfaceOrigin::Default,
        mode: Some("gateway".to_string()),
        port: Some(port.display().to_string()),
        speed: Some(LNODE_SPEED),
        frequency: Some(LORA_FREQUENCY),
        bandwidth: Some(LORA_BANDWIDTH),
        spreading_factor: Some(LORA_SPREADING_FACTOR),
        coding_rate: Some(LORA_CODING_RATE),
        ..Default::default()
    }
}

/// The section an RNode gets: its port and the shipped PHY; the transmit
/// power left to the builder, which takes the board's maximum capped by the
/// lawful limit for the frequency.
pub fn rnode_section(name: &str, port: &Path) -> InterfaceConfig {
    InterfaceConfig {
        name: name.to_string(),
        interface_type: "RNodeInterface".to_string(),
        enabled: true,
        origin: InterfaceOrigin::Default,
        port: Some(port.display().to_string()),
        frequency: Some(LORA_FREQUENCY),
        bandwidth: Some(LORA_BANDWIDTH),
        spreading_factor: Some(LORA_SPREADING_FACTOR),
        coding_rate: Some(LORA_CODING_RATE),
        ..Default::default()
    }
}

/// What the scanner needs from the machine, so the decisions can be tested
/// without one.
pub trait Host {
    /// The by-id names in the directory, unsorted.
    fn entries(&self, dir: &Path) -> Vec<String>;
    /// The USB ids behind a link, when it leads to a USB device.
    fn usb_ids(&self, link: &Path) -> Option<UsbId>;
    /// Another process holding the port, as `pid comm`.
    fn holder(&self, link: &Path) -> Option<String>;
    /// The device a link leads to, for comparing with configured ports.
    fn device(&self, path: &Path) -> Option<PathBuf>;
    /// Listen for [`SILENCE`], then ask once.
    fn probe(&self, link: &Path) -> Probe;
}

#[derive(Debug, Clone)]
struct Seen {
    verdict: Verdict,
    talking: u32,
}

/// The serial medium's state across scans: what each link was found to be.
pub struct AutoSerial<H: Host> {
    host: H,
    dir: PathBuf,
    /// Devices the configuration's own sections name.
    configured: Vec<PathBuf>,
    /// Section names taken, by the configuration or by this medium.
    taken: Vec<String>,
    seen: HashMap<String, Seen>,
    next: usize,
    attach: bool,
}

impl<H: Host + Sync> AutoSerial<H> {
    /// A scanner over `dir` for a node whose configuration is `config`.
    /// `attach` is false in a build without the `serial` feature: the boards
    /// are named, and none is attached.
    pub fn new(host: H, dir: PathBuf, config: &Config, attach: bool) -> Self {
        let configured = config
            .interfaces
            .values()
            .filter_map(|i| i.port.as_deref())
            .filter_map(|p| host.device(Path::new(p)))
            .collect();
        Self {
            host,
            dir,
            configured,
            taken: config.interfaces.keys().cloned().collect(),
            seen: HashMap::new(),
            next: 1,
            attach,
        }
    }

    fn next_section(&mut self) -> String {
        loop {
            let name = format!("{} {}", SECTION_PREFIX, self.next);
            self.next += 1;
            if !self.taken.contains(&name) {
                self.taken.push(name.clone());
                return name;
            }
        }
    }

    /// One pass over the directory: every entry not yet settled is sorted,
    /// candidates are listened to and probed (in parallel, so a start with
    /// several waits one silence), and every board found is returned as a
    /// section to attach. An entry whose link is gone is forgotten, so a
    /// device that comes back is asked again.
    pub fn scan(&mut self) -> Vec<InterfaceConfig> {
        let mut names = self.host.entries(&self.dir);
        names.sort();
        self.seen.retain(|n, _| names.contains(n));
        let pending: Vec<String> = names
            .into_iter()
            .filter(|n| match self.seen.get(n) {
                None => true,
                Some(s) if s.verdict.reason == Reason::Talking => s.talking < TALKING_LISTENS,
                Some(s) => !s.verdict.reason.settled(),
            })
            .collect();

        let catalogue = CatalogueIds::builtin();
        let host = &self.host;
        let dir = &self.dir;
        let configured = &self.configured;
        let attach = self.attach;
        let judged: Vec<(String, Class, Option<Probe>, Option<Reason>)> =
            std::thread::scope(|scope| {
                let handles: Vec<_> = pending
                    .iter()
                    .map(|name| {
                        scope.spawn(move || {
                            let link = dir.join(name);
                            let class = classify(name, host.usb_ids(&link), catalogue);
                            let wants_port =
                                matches!(class, Class::Lnode { .. } | Class::RnodeCandidate { .. });
                            if !wants_port {
                                return (name.clone(), class, None, None);
                            }
                            if host.device(&link).is_some_and(|d| configured.contains(&d)) {
                                return (name.clone(), class, None, Some(Reason::Configured));
                            }
                            if !attach {
                                return (name.clone(), class, None, Some(Reason::NotBuilt));
                            }
                            if let Some(holder) = host.holder(&link) {
                                tracing::info!(
                                    "{} is held by {}; not touched, asked again in {} s",
                                    link.display(),
                                    holder,
                                    RESCAN.as_secs()
                                );
                                return (name.clone(), class, None, Some(Reason::Held));
                            }
                            let probe = match class {
                                Class::RnodeCandidate { .. } => Some(host.probe(&link)),
                                _ => None,
                            };
                            (name.clone(), class, probe, None)
                        })
                    })
                    .collect();
                handles.into_iter().filter_map(|h| h.join().ok()).collect()
            });

        let mut found = Vec::new();
        for (name, class, probe, early) in judged {
            let link = self.dir.join(&name);
            let (kind, board) = match &class {
                Class::Bootloader => (Kind::Bootloader, None),
                Class::Lnode { board } | Class::LnodeDebug { board } => {
                    (Kind::Lnode, Some(board.clone()))
                }
                Class::RnodeCandidate { .. } => (Kind::Rnode, None),
                Class::Stranger => (Kind::Other, None),
            };
            let reason = match (&class, early, probe) {
                (_, Some(r), _) => r,
                (Class::Bootloader, ..) => Reason::Bootloader,
                (Class::Stranger, ..) => Reason::NotCandidate,
                (Class::LnodeDebug { .. }, ..) => Reason::DebugPort,
                (Class::Lnode { .. }, ..) => Reason::Name,
                (Class::RnodeCandidate { .. }, _, Some(Probe::Rnode)) => Reason::Detect,
                (Class::RnodeCandidate { .. }, _, Some(Probe::Talking)) => Reason::Talking,
                (Class::RnodeCandidate { .. }, _, Some(Probe::Held)) => Reason::Held,
                (Class::RnodeCandidate { .. }, _, Some(Probe::NoAccess)) => Reason::NoAccess,
                (Class::RnodeCandidate { .. }, _, Some(Probe::OpenFailed)) => Reason::OpenFailed,
                (Class::RnodeCandidate { .. }, _, Some(Probe::NoAnswer) | None) => Reason::NoAnswer,
            };
            // A candidate that talked is a stranger to the line: an RNode
            // that never answered is no RNode.
            let kind = match (kind, reason) {
                (Kind::Rnode, Reason::Detect) => Kind::Rnode,
                (Kind::Rnode, _) => Kind::Other,
                (k, _) => k,
            };
            if reason == Reason::NoAccess {
                tracing::warn!(
                    "lnsd may not open {}: is the user it runs as in the dialout group?",
                    link.display()
                );
            }
            let section = match reason {
                Reason::Name | Reason::Detect => {
                    let section = self.next_section();
                    found.push(match reason {
                        Reason::Name => lnode_section(&section, &link),
                        _ => rnode_section(&section, &link),
                    });
                    Some(section)
                }
                _ => None,
            };
            let verdict = Verdict {
                kind,
                board,
                reason,
                section,
            };
            let talking = self.seen.get(&name).map_or(0, |s| s.talking)
                + u32::from(reason == Reason::Talking);
            let changed = self.seen.get(&name).map(|s| &s.verdict) != Some(&verdict);
            if changed {
                log_verdict(&name, &verdict);
            }
            self.seen.insert(name, Seen { verdict, talking });
        }
        found
    }

    /// The verdict on one by-id name, for tests and diagnostics.
    pub fn verdict(&self, name: &str) -> Option<&Verdict> {
        self.seen.get(name).map(|s| &s.verdict)
    }
}

fn log_verdict(name: &str, v: &Verdict) {
    use crate::event_log::Scalar;
    let section = v.section.clone().unwrap_or_else(|| "none".to_string());
    tracing::info!(
        event = "AUTO_SERIAL",
        board = %Scalar(v.board.as_deref().unwrap_or("none")),
        port = %Scalar(name),
        kind = v.kind.as_str(),
        reason = v.reason.as_str(),
        attached = %crate::auto_layer::yes_no(v.section.is_some()),
        section = %Scalar(&section),
    );
}

/// The directory this host's boards are read from: [`ENV_SERIAL_DIR`], else
/// [`DEFAULT_SERIAL_DIR`].
pub fn serial_dir() -> PathBuf {
    match std::env::var_os(ENV_SERIAL_DIR) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(DEFAULT_SERIAL_DIR),
    }
}

/// The scanner for this start, or `None` when the serial medium is off.
/// On a host that cannot scan (not Linux) the medium being on is said once.
pub fn for_layer(layer: AutoLayer, config: &Config) -> Option<AutoSerial<SystemHost>> {
    if !layer.media.serial {
        return None;
    }
    if !cfg!(target_os = "linux") {
        tracing::warn!("[auto] serial = yes, but lnsd scans for boards on Linux only");
        return None;
    }
    let attach = cfg!(feature = "serial");
    if !attach {
        tracing::warn!(
            "[auto] serial = yes, but this lnsd is built without the serial interfaces; \
             boards are named and none is attached"
        );
    }
    Some(AutoSerial::new(SystemHost, serial_dir(), config, attach))
}

/// Run the startup scan and add its sections to `config`.
pub fn apply(scanner: &mut AutoSerial<SystemHost>, config: &mut Config) {
    for section in scanner.scan() {
        config.interfaces.insert(section.name.clone(), section);
    }
}

/// The machine itself: sysfs for ids, `/proc` for holders, termios for the
/// probe. Off Linux it finds nothing.
pub struct SystemHost;

#[cfg(not(target_os = "linux"))]
impl Host for SystemHost {
    fn entries(&self, _dir: &Path) -> Vec<String> {
        Vec::new()
    }
    fn usb_ids(&self, _link: &Path) -> Option<UsbId> {
        None
    }
    fn holder(&self, _link: &Path) -> Option<String> {
        None
    }
    fn device(&self, path: &Path) -> Option<PathBuf> {
        Some(path.to_path_buf())
    }
    fn probe(&self, _link: &Path) -> Probe {
        Probe::OpenFailed
    }
}

#[cfg(target_os = "linux")]
impl Host for SystemHost {
    fn entries(&self, dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn usb_ids(&self, link: &Path) -> Option<UsbId> {
        let dev = std::fs::canonicalize(link).ok()?;
        let tty = dev.file_name()?;
        let mut at =
            std::fs::canonicalize(Path::new("/sys/class/tty").join(tty).join("device")).ok()?;
        for _ in 0..6 {
            let vid = std::fs::read_to_string(at.join("idVendor"));
            let pid = std::fs::read_to_string(at.join("idProduct"));
            if let (Ok(vid), Ok(pid)) = (vid, pid) {
                return UsbId::parse(&format!("{}:{}", vid.trim(), pid.trim()));
            }
            at = at.parent()?.to_path_buf();
        }
        None
    }

    fn holder(&self, link: &Path) -> Option<String> {
        linux::holder(link)
    }

    fn device(&self, path: &Path) -> Option<PathBuf> {
        std::fs::canonicalize(path).ok()
    }

    fn probe(&self, link: &Path) -> Probe {
        linux::probe(link)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    use std::path::Path;
    use std::time::Instant;

    use super::{detect_request, has_detect_response, Probe, PROBE_ANSWER, RNODE_SPEED, SILENCE};

    /// Another process with an fd on the device `link` leads to, as
    /// `pid comm`. Processes whose fds this one may not read are not seen;
    /// `TIOCEXCL` at the open covers those that hold the port exclusively.
    pub(super) fn holder(link: &Path) -> Option<String> {
        let meta = std::fs::metadata(link).ok()?;
        if !meta.file_type().is_char_device() {
            return None;
        }
        let rdev = meta.rdev();
        let own = std::process::id().to_string();
        for proc_entry in std::fs::read_dir("/proc").ok()?.flatten() {
            let pid = proc_entry.file_name();
            let Some(pid) = pid.to_str() else { continue };
            if pid == own || !pid.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let Ok(fds) = std::fs::read_dir(proc_entry.path().join("fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                let Ok(m) = std::fs::metadata(fd.path()) else {
                    continue;
                };
                if m.file_type().is_char_device() && m.rdev() == rdev {
                    let comm =
                        std::fs::read_to_string(proc_entry.path().join("comm")).unwrap_or_default();
                    return Some(format!("{} {}", pid, comm.trim()));
                }
            }
        }
        None
    }

    struct Port(libc::c_int);

    impl Drop for Port {
        fn drop(&mut self) {
            // SAFETY: the fd is this struct's own, opened in `open` and
            // closed nowhere else. TIOCNXCL first, so the interface that
            // opens the port next is not refused by our exclusivity.
            unsafe {
                libc::ioctl(self.0, libc::TIOCNXCL);
                libc::close(self.0);
            }
        }
    }

    fn open(link: &Path) -> Result<Port, Probe> {
        let Ok(path) = CString::new(link.as_os_str().as_bytes()) else {
            return Err(Probe::OpenFailed);
        };
        // SAFETY: a NUL-terminated path; the flags never block.
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            return Err(match err.raw_os_error() {
                Some(libc::EBUSY) => Probe::Held,
                Some(libc::EACCES) | Some(libc::EPERM) => Probe::NoAccess,
                _ => {
                    tracing::info!("could not open {}: {}", link.display(), err);
                    Probe::OpenFailed
                }
            });
        }
        let port = Port(fd);
        // SAFETY: `fd` is open; `tio` is fully written by tcgetattr before
        // it is read.
        unsafe {
            if libc::ioctl(fd, libc::TIOCEXCL) != 0 {
                return Err(Probe::Held);
            }
            let mut tio: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut tio) != 0 {
                return Err(Probe::OpenFailed);
            }
            libc::cfmakeraw(&mut tio);
            tio.c_cflag |= libc::CLOCAL | libc::CREAD;
            let speed = match RNODE_SPEED {
                115_200 => libc::B115200,
                _ => libc::B9600,
            };
            libc::cfsetispeed(&mut tio, speed);
            libc::cfsetospeed(&mut tio, speed);
            if libc::tcsetattr(fd, libc::TCSANOW, &tio) != 0 {
                return Err(Probe::OpenFailed);
            }
            // Only what arrives from now on counts as talking.
            libc::tcflush(fd, libc::TCIFLUSH);
        }
        Ok(port)
    }

    /// Read what arrives within `window`, at most `cap` bytes.
    fn read_for(port: &Port, window: std::time::Duration, cap: usize) -> Vec<u8> {
        let mut got = Vec::new();
        let deadline = Instant::now() + window;
        let mut buf = [0u8; 256];
        while got.len() < cap {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let mut pfd = libc::pollfd {
                fd: port.0,
                events: libc::POLLIN,
                revents: 0,
            };
            let ms = left.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
            // SAFETY: one valid pollfd.
            let ready = unsafe { libc::poll(&mut pfd, 1, ms) };
            if ready <= 0 {
                continue;
            }
            if pfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
                && pfd.revents & libc::POLLIN == 0
            {
                break;
            }
            // SAFETY: `buf` is valid for its length.
            let n = unsafe { libc::read(port.0, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                got.extend_from_slice(&buf[..n as usize]);
            } else if n == 0 {
                break;
            }
        }
        got
    }

    pub(super) fn probe(link: &Path) -> Probe {
        let port = match open(link) {
            Ok(port) => port,
            Err(p) => return p,
        };
        if !read_for(&port, SILENCE, 1).is_empty() {
            return Probe::Talking;
        }
        let request = detect_request();
        // SAFETY: `request` is valid for its length.
        let n = unsafe { libc::write(port.0, request.as_ptr().cast(), request.len()) };
        if n != request.len() as isize {
            return Probe::OpenFailed;
        }
        let answer = read_for(&port, PROBE_ANSWER, 4096);
        if has_detect_response(&answer) {
            Probe::Rnode
        } else {
            Probe::NoAnswer
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn catalogue() -> &'static CatalogueIds {
        CatalogueIds::builtin()
    }

    #[test]
    fn the_catalogue_names_our_ids_and_the_bootloaders() {
        let ids = catalogue();
        for id in ["1209:0001", "1209:0002", "1209:0003", "1209:0004"] {
            assert!(ids.lnode.contains(&UsbId::parse(id).unwrap()), "{id}");
        }
        for id in ["239a:0071", "239a:0029", "2886:0057"] {
            assert!(ids.bootloader.contains(&UsbId::parse(id).unwrap()), "{id}");
        }
        // Stock firmware's application ids are no LNode.
        assert!(!ids.lnode.contains(&UsbId::new(0x239a, 0x8029)));
    }

    /// The rig's by-id directory on 2026-11-08, with the ids sysfs gives.
    #[test]
    fn the_rig_sorts_as_measured() {
        let c = catalogue();
        let lnode = classify(
            "usb-leviculum_leviculum_T1000-E_DD6AB3FCACCE95DA-if02",
            Some(UsbId::new(0x1209, 0x0004)),
            c,
        );
        assert_eq!(
            lnode,
            Class::Lnode {
                board: "T1000-E".into()
            }
        );
        assert_eq!(
            classify(
                "usb-leviculum_leviculum_T114_183004F712B4A7FE-if00",
                Some(UsbId::new(0x1209, 0x0001)),
                c
            ),
            Class::LnodeDebug {
                board: "T114".into()
            }
        );
        assert_eq!(
            classify(
                "usb-Espressif_USB_JTAG_serial_debug_unit_44:1B:F6:69:77:24-if00",
                Some(UsbId::new(0x303a, 0x1001)),
                c
            ),
            Class::RnodeCandidate {
                bridge: "esp32-usb-jtag"
            }
        );
        assert_eq!(
            classify(
                "usb-1a86_USB_Single_Serial_5896000022-if00",
                Some(UsbId::new(0x1a86, 0x55d4)),
                c
            ),
            Class::RnodeCandidate { bridge: "ch9102" }
        );
        // The M9's CH340K is no candidate.
        assert_eq!(
            classify(
                "usb-1a86_USB_Serial-if00-port0",
                Some(UsbId::new(0x1a86, 0x7522)),
                c
            ),
            Class::Stranger
        );
        // A bootloader by its id, whatever its name says.
        assert_eq!(
            classify(
                "usb-Seeed_T1000-E-BOOT_X-if00",
                Some(UsbId::new(0x2886, 0x0057)),
                c
            ),
            Class::Bootloader
        );
        // Our strings on an id that is not ours: not named an LNode.
        assert_eq!(
            classify(
                "usb-leviculum_leviculum_T114_X-if02",
                Some(UsbId::new(0x239a, 0x8071)),
                c
            ),
            Class::Stranger
        );
    }

    /// The cell's ptys have no ids: the name decides.
    #[test]
    fn a_link_without_ids_sorts_by_its_name() {
        let c = catalogue();
        assert_eq!(
            classify(
                "usb-Prolific_Technology_Inc._USB-Serial_Controller-if00-port0",
                None,
                c
            ),
            Class::Stranger
        );
        assert_eq!(
            classify(
                "usb-Silicon_Labs_CP2102_USB_to_UART_Bridge_Controller_0001-if00-port0",
                None,
                c
            ),
            Class::RnodeCandidate { bridge: "cp210x" }
        );
        assert_eq!(
            classify("usb-leviculum_T114_183004F712B4A7FE-if02", None, c),
            Class::Lnode {
                board: "T114".into()
            }
        );
        assert_eq!(
            classify("usb-Adafruit_nRF52_Bootloader_X-if00", None, c),
            Class::Bootloader
        );
    }

    #[test]
    fn a_detect_response_is_found_in_a_stream() {
        assert!(has_detect_response(&[0x00, 0xc0, 0x08, 0x46, 0xc0]));
        assert!(!has_detect_response(&[0xc0, 0x08, 0x73, 0xc0]));
        assert_eq!(detect_request(), [0xc0, 0x08, 0x73, 0xc0]);
    }

    #[test]
    fn the_sections_carry_the_shipped_phy() {
        let l = lnode_section("Auto Serial 1", Path::new("/dev/serial/by-id/x-if02"));
        assert_eq!(l.interface_type, "SerialInterface");
        assert_eq!(l.mode.as_deref(), Some("gateway"));
        assert_eq!(l.origin, InterfaceOrigin::Default);
        let r = rnode_section("Auto Serial 2", Path::new("/dev/serial/by-id/y"));
        for s in [&l, &r] {
            assert_eq!(s.frequency, Some(869_463_000));
            assert_eq!(s.bandwidth, Some(125_000));
            assert_eq!(s.spreading_factor, Some(8));
            assert_eq!(s.coding_rate, Some(5));
        }
        assert_eq!(r.interface_type, "RNodeInterface");
        assert_eq!(r.tx_power, None);
    }

    /// A host made of a table: names, ids, holders, probe answers, and a
    /// record of every port the scanner probed.
    #[derive(Default)]
    struct FakeHost {
        entries: Mutex<Vec<String>>,
        held: Vec<String>,
        probes: HashMap<String, Probe>,
        probed: Mutex<Vec<String>>,
    }

    impl Host for FakeHost {
        fn entries(&self, _dir: &Path) -> Vec<String> {
            self.entries.lock().unwrap().clone()
        }
        fn usb_ids(&self, _link: &Path) -> Option<UsbId> {
            None
        }
        fn holder(&self, link: &Path) -> Option<String> {
            let name = link.file_name()?.to_str()?;
            self.held
                .iter()
                .any(|h| h == name)
                .then(|| "42 minicom".into())
        }
        fn device(&self, path: &Path) -> Option<PathBuf> {
            Some(path.to_path_buf())
        }
        fn probe(&self, link: &Path) -> Probe {
            let name = link.file_name().unwrap().to_str().unwrap().to_string();
            self.probed.lock().unwrap().push(name.clone());
            self.probes.get(&name).copied().unwrap_or(Probe::NoAnswer)
        }
    }

    const QUIET: &str = "usb-Prolific_Technology_Inc._USB-Serial_Controller-if00-port0";
    const TALKER: &str = "usb-Silicon_Labs_CP2102_USB_to_UART_Bridge_Controller_0001-if00-port0";
    const RNODE: &str = "usb-Espressif_USB_JTAG_serial_debug_unit_AA:BB:CC:DD:EE:FF-if00";
    const LNODE: &str = "usb-leviculum_leviculum_T114_0123456789ABCDEF-if02";
    const LNODE_DEBUG: &str = "usb-leviculum_leviculum_T114_0123456789ABCDEF-if00";

    fn scanner(host: FakeHost, config: &Config) -> AutoSerial<FakeHost> {
        AutoSerial::new(host, PathBuf::from("/by-id"), config, true)
    }

    #[test]
    fn strangers_are_never_probed_and_boards_are_attached() {
        let host = FakeHost {
            entries: Mutex::new(
                [QUIET, TALKER, RNODE, LNODE, LNODE_DEBUG]
                    .map(String::from)
                    .to_vec(),
            ),
            probes: HashMap::from([
                (TALKER.to_string(), Probe::Talking),
                (RNODE.to_string(), Probe::Rnode),
            ]),
            ..Default::default()
        };
        let mut s = scanner(host, &Config::default());
        let found = s.scan();
        let mut probed = s.host.probed.lock().unwrap().clone();
        probed.sort();
        let mut want = vec![TALKER.to_string(), RNODE.to_string()];
        want.sort();
        assert_eq!(probed, want);
        let kinds: Vec<_> = found
            .iter()
            .map(|c| (c.name.as_str(), c.interface_type.as_str()))
            .collect();
        // Sorted by name: the Espressif link before ours.
        assert_eq!(
            kinds,
            vec![
                ("Auto Serial 1", "RNodeInterface"),
                ("Auto Serial 2", "SerialInterface")
            ]
        );
        assert_eq!(s.verdict(QUIET).unwrap().reason, Reason::NotCandidate);
        assert_eq!(s.verdict(TALKER).unwrap().reason, Reason::Talking);
        assert_eq!(s.verdict(TALKER).unwrap().kind, Kind::Other);
        assert_eq!(s.verdict(LNODE_DEBUG).unwrap().reason, Reason::DebugPort);

        // The next scan attaches nothing twice and probes only the talker,
        // which is listened to again until TALKING_LISTENS.
        assert!(s.scan().is_empty());
        assert!(s.scan().is_empty());
        assert!(s.scan().is_empty());
        let talker_listens = s
            .host
            .probed
            .lock()
            .unwrap()
            .iter()
            .filter(|n| *n == TALKER)
            .count();
        assert_eq!(talker_listens, TALKING_LISTENS as usize);
        let rnode_probes = s
            .host
            .probed
            .lock()
            .unwrap()
            .iter()
            .filter(|n| *n == RNODE)
            .count();
        assert_eq!(rnode_probes, 1);
    }

    #[test]
    fn a_held_port_is_skipped_and_asked_again() {
        let host = FakeHost {
            entries: Mutex::new(vec![LNODE.to_string(), RNODE.to_string()]),
            held: vec![LNODE.to_string(), RNODE.to_string()],
            ..Default::default()
        };
        let mut s = scanner(host, &Config::default());
        assert!(s.scan().is_empty());
        assert!(s.host.probed.lock().unwrap().is_empty());
        assert_eq!(s.verdict(LNODE).unwrap().reason, Reason::Held);
        s.host.held.clear();
        let found = s.scan();
        assert_eq!(found.len(), 1, "the LNode, the RNode does not answer here");
        assert_eq!(s.verdict(LNODE).unwrap().reason, Reason::Name);
    }

    #[test]
    fn a_configured_port_is_left_to_its_section() {
        let mut config = Config::default();
        config.interfaces.insert(
            "Auto Serial 1".into(),
            InterfaceConfig {
                name: "Auto Serial 1".into(),
                interface_type: "SerialInterface".into(),
                port: Some(format!("/by-id/{LNODE}")),
                ..Default::default()
            },
        );
        let host = FakeHost {
            entries: Mutex::new(vec![LNODE.to_string(), RNODE.to_string()]),
            probes: HashMap::from([(RNODE.to_string(), Probe::Rnode)]),
            ..Default::default()
        };
        let mut s = scanner(host, &config);
        let found = s.scan();
        assert_eq!(s.verdict(LNODE).unwrap().reason, Reason::Configured);
        // The configuration's own section name is not taken over.
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Auto Serial 2");
    }

    #[test]
    fn a_device_that_comes_back_is_asked_again() {
        let host = FakeHost {
            entries: Mutex::new(vec![RNODE.to_string()]),
            ..Default::default()
        };
        let mut s = scanner(host, &Config::default());
        assert!(s.scan().is_empty());
        assert!(s.scan().is_empty());
        assert_eq!(s.host.probed.lock().unwrap().len(), 1);
        s.host.entries.lock().unwrap().clear();
        s.scan();
        s.host.entries.lock().unwrap().push(RNODE.to_string());
        s.scan();
        assert_eq!(s.host.probed.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_build_without_serial_attaches_nothing_and_writes_nothing() {
        let host = FakeHost {
            entries: Mutex::new(vec![LNODE.to_string(), RNODE.to_string()]),
            probes: HashMap::from([(RNODE.to_string(), Probe::Rnode)]),
            ..Default::default()
        };
        let mut s = AutoSerial::new(host, PathBuf::from("/by-id"), &Config::default(), false);
        assert!(s.scan().is_empty());
        assert!(s.host.probed.lock().unwrap().is_empty());
        assert_eq!(s.verdict(LNODE).unwrap().reason, Reason::NotBuilt);
    }

    /// The probe on a real pty: a silent device that answers is an RNode, a
    /// talker is refused before anything is written to it.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_probe_on_a_pty() {
        use std::io::{Read, Write};
        use std::os::fd::FromRawFd;

        fn pty() -> (std::fs::File, PathBuf) {
            let mut master = 0;
            let mut slave = 0;
            // SAFETY: openpty writes two fds; the name pointer may be null.
            let rc = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            };
            assert_eq!(rc, 0);
            let path = std::fs::read_link(format!("/proc/self/fd/{slave}")).unwrap();
            // SAFETY: the slave fd is ours; the probe opens it by path.
            unsafe { libc::close(slave) };
            // SAFETY: the master fd is ours and moves into the File.
            (unsafe { std::fs::File::from_raw_fd(master) }, path)
        }

        // The answering RNode.
        let (mut master, path) = pty();
        let answerer = std::thread::spawn(move || {
            let mut got = Vec::new();
            let mut buf = [0u8; 64];
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                match master.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        got.extend_from_slice(&buf[..n]);
                        if got.ends_with(&detect_request()) {
                            master.write_all(&[0xc0, 0x08, 0x46, 0xc0]).unwrap();
                            // The master goes back with the bytes: closing
                            // it here would hang the slave up under the
                            // probe and could take the answer with it.
                            return (got, master);
                        }
                    }
                    // EIO while no slave is open.
                    _ => std::thread::sleep(Duration::from_millis(10)),
                }
            }
            (got, master)
        });
        assert_eq!(linux::probe(&path), Probe::Rnode);
        let (got, _master) = answerer.join().unwrap();
        assert_eq!(got, detect_request());

        // The talker.
        let (mut master, path) = pty();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let talker = std::thread::spawn(move || {
            let mut got = Vec::new();
            let mut buf = [0u8; 64];
            while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = master.write_all(b"$GPGGA\r\n");
                // SAFETY: a poll on the master's own fd.
                let mut pfd = libc::pollfd {
                    fd: std::os::fd::AsRawFd::as_raw_fd(&master),
                    events: libc::POLLIN,
                    revents: 0,
                };
                if unsafe { libc::poll(&mut pfd, 1, 100) } > 0 && pfd.revents & libc::POLLIN != 0 {
                    if let Ok(n) = master.read(&mut buf) {
                        got.extend_from_slice(&buf[..n]);
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            got
        });
        assert_eq!(linux::probe(&path), Probe::Talking);
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(
            talker.join().unwrap().is_empty(),
            "nothing written to a talker"
        );
    }
}

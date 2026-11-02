//! `loquax mailbox setup` (1266, walk finding 4): the onboarding draft's
//! wizard (`docs/drafts/loquax-onboarding-2026-10.md`, section 6, "The
//! wizard") as one command, so a computer mailbox is one command and one
//! scan instead of four commands, one of them with root and one with an
//! address typed by hand.
//!
//! The five steps, each a screen of the text this module writes:
//!
//! 1. **Where to store**: the system mailbox under [`SYSTEM_DIR`], run by
//!    the packaged service, or the mailbox in the user's own Loquax
//!    directory, started by hand.
//! 2. **Start on boot**: the system mailbox's service is enabled and
//!    started; that needs root once, and the screen says why before sudo
//!    asks.
//! 3. **Reachability**: the rendezvous of 1196 (the default, nothing to
//!    set up on a router) or a fixed address, which the config keeps as
//!    `home_address` with `rendezvous_dial = off` and every later code
//!    carries.
//! 4. **The code**, as text and as a terminal QR code
//!    (`super::qr::lines`, the drawing `loquax mailbox admit` prints).
//! 5. **On the phone**: the two buttons to press.
//!
//! Running it again changes nothing that is already as asked, says so, and
//! shows a new code: a code is single-use, so "show the code" is a new one.
//!
//! This module holds what can be decided without touching the system: the
//! choices, the config edit, the command lines the binary runs, and the
//! texts. The binary (`loquax/src/main.rs`, `run_setup`) does the effects.

use std::path::{Path, PathBuf};

use super::phone_server;
use crate::config::{self, Config};

/// The system mailbox's state directory, `StateDirectory=` of
/// `packaging/loquax/loquax-mailbox.service`.
pub const SYSTEM_DIR: &str = "/var/lib/loquax-mailbox";
/// The account the system service runs as (`User=` of the unit; the
/// package's postinst makes it).
pub const SYSTEM_USER: &str = "loquax";
/// The system Reticulum config the service reads (`--config` in the
/// unit's `ExecStart=`).
pub const SYSTEM_RETICULUM: &str = "/etc/reticulum";
/// The packaged service.
pub const UNIT: &str = "loquax-mailbox.service";
/// Where a package puts the unit.
const UNIT_FILES: &[&str] = &[
    "/usr/lib/systemd/system/loquax-mailbox.service",
    "/lib/systemd/system/loquax-mailbox.service",
    "/etc/systemd/system/loquax-mailbox.service",
];

/// Whether the packaged service is installed on this host.
pub fn unit_installed() -> bool {
    UNIT_FILES.iter().any(|path| Path::new(path).exists())
}

/// Step 1: where the mailbox keeps the identity and the mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Store {
    /// [`SYSTEM_DIR`], run by the packaged service from boot on.
    System,
    /// The user's own Loquax directory (`LNMSG_HOME`, else
    /// `~/.config/loquax`), run by `loquax mailbox run`.
    Account,
}

impl Store {
    /// `--store`'s value.
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.trim() {
            "system" => Ok(Self::System),
            "account" => Ok(Self::Account),
            other => Err(format!("{other:?} is not system or account")),
        }
    }

    /// The word `--store` takes.
    pub fn flag(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Account => "account",
        }
    }

    /// The default: the system mailbox where the package installed its
    /// service, the account's otherwise (a checkout, a tarball).
    pub fn default_here() -> Self {
        match unit_installed() {
            true => Self::System,
            false => Self::Account,
        }
    }
}

/// Step 3: how a phone reaches the mailbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// Both dial the rendezvous (1196); the code carries its list.
    Rendezvous,
    /// The phone dials this `host:port`, the mailbox's phone port
    /// ([`Answers::phone_port`]) as the world reaches it: a router's port
    /// forward may put another port in front of it, so the two are given
    /// apart.
    Fixed(String),
}

impl Reach {
    /// What a mailbox's config says: a fixed address only with the
    /// rendezvous switched off, as [`Reach::apply`] writes it.
    pub fn of(config: &Config) -> Self {
        match (config.rendezvous_dial, &config.home_address) {
            (Some(false), Some(address)) => Self::Fixed(address.clone()),
            _ => Self::Rendezvous,
        }
    }

    /// `--reach`'s word.
    pub fn flag(&self) -> &'static str {
        match self {
            Self::Rendezvous => "rendezvous",
            Self::Fixed(_) => "fixed",
        }
    }

    /// The config text with this choice in it: a fixed address is
    /// `home_address` with `rendezvous_dial = off`, which the codes carry
    /// alone; the rendezvous is `rendezvous_dial = on` and no
    /// `home_address`. Every other line stays.
    pub fn apply(&self, text: &str) -> String {
        match self {
            Self::Rendezvous => {
                config::unset(&config::set(text, "rendezvous_dial", "on"), "home_address")
            }
            Self::Fixed(address) => config::set(
                &config::set(text, "rendezvous_dial", "off"),
                "home_address",
                address,
            ),
        }
    }

    /// One line for the screen.
    pub fn describe(&self) -> String {
        match self {
            Self::Rendezvous => "from anywhere, through leviculum.network".to_string(),
            Self::Fixed(address) => format!("at the fixed address {address}"),
        }
    }
}

/// `--address`'s check: `host:port`, as the code and the config take it.
pub fn check_address(text: &str) -> Result<String, String> {
    let text = text.trim();
    crate::engine::parse_server(text)?;
    Ok(text.to_string())
}

/// Write `reach` into `home/config` when it is not there yet; `true` when
/// the file changed, so a running mailbox needs a restart to dial by it.
pub fn save_reach(home: &Path, reach: &Reach) -> std::io::Result<bool> {
    let text = config::read_text(home)?;
    let next = reach.apply(&text);
    if next == text {
        return Ok(false);
    }
    config::save_text(home, &next)?;
    Ok(true)
}

/// This host's address on its local network, the one a packet to the
/// outside would leave from. A UDP socket connected to a documentation
/// address sends nothing; it only picks the route. `None` offline.
pub fn lan_address() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("192.0.2.1", 9)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

/// The fixed address offered when the user picks one: the LAN address on
/// the phone port, which is right for a phone at home and the start of a
/// router's port forward.
pub fn default_fixed() -> Option<String> {
    lan_address().map(|ip| format!("{ip}:{}", phone_server::PORT))
}

/// The answers, as the binary hands them from the questions (or the
/// flags) to the steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answers {
    pub store: Store,
    pub reach: Reach,
    /// The name the device is admitted under; `None` is admit's default.
    pub name: Option<String>,
    /// The port the daemon's section for phones listens on, for a fixed
    /// address: [`phone_server::PORT`] unless `--phone-port` says another.
    pub phone_port: u16,
}

impl Answers {
    /// The flags that give these answers without a question, for the run
    /// under sudo.
    pub fn flags(&self) -> Vec<String> {
        let mut out = vec![
            "--store".to_string(),
            self.store.flag().to_string(),
            "--reach".to_string(),
            self.reach.flag().to_string(),
        ];
        if let Reach::Fixed(address) = &self.reach {
            out.extend(["--address".to_string(), address.clone()]);
            if self.phone_port != phone_server::PORT {
                out.extend(["--phone-port".to_string(), self.phone_port.to_string()]);
            }
        }
        if let Some(name) = &self.name {
            out.extend(["--name".to_string(), name.clone()]);
        }
        out.push("--yes".to_string());
        out
    }
}

/// The command line that runs the system steps with root: this program
/// again under sudo, every question answered, so sudo asks for the
/// password once and nothing is asked after it.
pub fn sudo_argv(exe: &Path, answers: &Answers) -> Vec<String> {
    let mut out = vec![
        "sudo".to_string(),
        exe.display().to_string(),
        "mailbox".to_string(),
        "setup".to_string(),
    ];
    out.extend(answers.flags());
    out
}

/// The command line that issues the code as the service's own account,
/// so every file it makes (the identity, the store) belongs to it.
pub fn admit_argv(exe: &Path, dir: &Path, name: Option<&str>) -> Vec<String> {
    let mut out = vec![
        "runuser".to_string(),
        "-u".to_string(),
        SYSTEM_USER.to_string(),
        "--".to_string(),
        "env".to_string(),
        format!("{}={}", crate::identity::HOME_ENV, dir.display()),
        exe.display().to_string(),
        "mailbox".to_string(),
        "admit".to_string(),
        "--code-only".to_string(),
    ];
    if let Some(name) = name {
        out.extend(["--name".to_string(), name.to_string()]);
    }
    out
}

/// The system mailbox's directory.
pub fn system_dir() -> PathBuf {
    PathBuf::from(SYSTEM_DIR)
}

/// The title line.
pub const TITLE: &str = "Loquax Mailbox setup";

/// Step 1's question, with the account's directory for the second
/// choice.
pub fn store_question(account_dir: &Path, unit: bool) -> String {
    let system = match unit {
        true => "",
        false => " (the loquax package is not installed here)",
    };
    format!(
        "1/5  Where your mailbox keeps your mail\n     \
         1) On this computer from every boot on, in {SYSTEM_DIR}{system}\n     \
         2) In your own account, in {}; you start it yourself",
        account_dir.display()
    )
}

/// Step 1's answer.
pub fn store_done(store: Store, dir: &Path) -> String {
    let how = match store {
        Store::System => "run by the system service",
        Store::Account => "in your account",
    };
    format!(
        "1/5  Where your mailbox keeps your mail: {} ({how})",
        dir.display()
    )
}

/// Step 2's explanation before sudo asks for a password.
pub fn sudo_why() -> String {
    format!(
        "2/5  Start on boot\n     \
         Turning on the service that starts your mailbox with the computer \
         needs administrator rights, once. sudo may ask for your password now: \
         it enables {UNIT}, opens the port for phones when you chose a fixed \
         address, and writes the mailbox's settings as its own account, \
         {SYSTEM_USER}."
    )
}

/// What step 2 found or did on the system mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// Enabled and running before, settings unchanged: nothing done.
    AlreadyOn,
    /// Enabled and started now.
    Started,
    /// Running, and restarted for the changed settings.
    Restarted,
}

/// Step 2's line; `changed` says the settings moved, which a mailbox in
/// the account picks up only when it is started again.
pub fn service_done(service: Option<Service>, changed: bool) -> String {
    match service {
        Some(Service::AlreadyOn) => "2/5  Start on boot: already on, the service runs.".to_string(),
        Some(Service::Started) => {
            "2/5  Start on boot: on; the service runs now and with every boot.".to_string()
        }
        Some(Service::Restarted) => {
            "2/5  Start on boot: on; the service restarted for the new settings.".to_string()
        }
        None => {
            let restart = match changed {
                true => " (if it runs already, stop and start it for the new settings)",
                false => "",
            };
            format!(
                "2/5  Start on boot: not as a service, since it lives in your account.\n     \
                 Keep it running with: loquax mailbox run{restart}"
            )
        }
    }
}

/// Step 3's question, with the address offered for a fixed one.
pub fn reach_question(offered: Option<&str>) -> String {
    let offered = match offered {
        Some(address) => format!(", for instance {address}"),
        None => String::new(),
    };
    format!(
        "3/5  How your phone reaches it\n     \
         1) From anywhere, through leviculum.network (nothing to set up on your router)\n     \
         2) At a fixed address you give{offered} (a public address or a port forward)"
    )
}

/// Step 3's line, with what it changed. `port` is what the fixed
/// address's phone port, asked on `phone_port`, came to, `None` for the
/// rendezvous; the system
/// mailbox's setup restarts the daemon for a new port, an account's
/// leaves that to the user.
pub fn reach_done(
    store: Store,
    reach: &Reach,
    changed: bool,
    port: Option<&phone_server::Added>,
    phone_port: u16,
) -> String {
    let changed = match changed {
        true => "",
        false => " (as before)",
    };
    let mut out = format!(
        "3/5  How your phone reaches it: {}{changed}",
        reach.describe()
    );
    if let (Reach::Fixed(_), Some(added)) = (reach, port) {
        out.push_str("\n     ");
        out.push_str(&match added {
            phone_server::Added::Written(path) => format!(
                "Added port {phone_port} for phones to {}; {}.",
                path.display(),
                match store {
                    Store::System => "lnsd restarted to open it",
                    Store::Account => "restart the Reticulum daemon (lnsd or rnsd) to open it",
                }
            ),
            phone_server::Added::Present(_, Some(listening)) => {
                format!("The port for phones, {listening}, is open.")
            }
            phone_server::Added::Present(path, None) => {
                format!("The port for phones is in {}.", path.display())
            }
            phone_server::Added::NoConfig(path) => format!(
                "No port for phones yet: {} does not exist; the Reticulum daemon writes it \
                 on its first start. Then run this setup again.",
                path.display()
            ),
            phone_server::Added::Unwritable(path, error) => format!(
                "No port for phones yet: {} could not be written ({error}). Put this into \
                 its [interfaces] section and restart the Reticulum daemon:\n\n{}",
                path.display(),
                phone_server::block(phone_port)
            ),
            phone_server::Added::PortTaken(path) => format!(
                "No port for phones: port {phone_port} is taken on this host or named in {}. \
                 Run this setup again with --phone-port and a free port, and give the address \
                 a phone reaches that port at.",
                path.display()
            ),
        });
    }
    if matches!(reach, Reach::Rendezvous) && super::rendezvous::defaults_held_back() {
        out.push_str(
            "\n     This host holds the shipped rendezvous back (the lab marker or \
             LEVICULUM_NO_AUTO), so the code carries no address: choose a fixed one.",
        );
    }
    out
}

/// Step 4's head, with how long the code stays good
/// (`home_token_hours`).
pub fn code_head(hours: u64) -> String {
    format!("4/5  Your code, good for one device, once, for {hours} hours:")
}

/// Step 5: what to do on the phone.
pub const PHONE_LINE: &str =
    "5/5  On your phone, open Loquax and choose \"On a computer at home\", \
     then \"Scan the code\".";

/// The line a second run starts with, when the mailbox has an identity
/// already.
pub fn already(address: &str, devices: Option<usize>) -> String {
    let devices = match devices {
        Some(1) => ", 1 device admitted".to_string(),
        Some(n) => format!(", {n} devices admitted"),
        None => String::new(),
    };
    format!("This mailbox is set up already: address {address}{devices}. Here is its state and a new code.")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed address is two keys, the rendezvous takes the address out
    /// again, and either applied twice changes nothing more: a second run
    /// with the same answer does not restart the service.
    #[test]
    fn the_reach_is_two_keys_and_applying_it_twice_is_a_no_op() {
        let fixed = Reach::Fixed("192.168.1.20:4243".to_string());
        let text = fixed.apply("theme = light\n");
        assert_eq!(
            text,
            "theme = light\nrendezvous_dial = off\nhome_address = 192.168.1.20:4243\n"
        );
        assert_eq!(fixed.apply(&text), text);
        let config = config::parse(&text).expect("parses");
        assert_eq!(Reach::of(&config), fixed);

        let back = Reach::Rendezvous.apply(&text);
        assert_eq!(back, "theme = light\nrendezvous_dial = on\n");
        assert_eq!(Reach::Rendezvous.apply(&back), back);
        assert_eq!(
            Reach::of(&config::parse(&back).expect("parses")),
            Reach::Rendezvous
        );
        // An address left from a hand edit with the rendezvous on is not
        // the fixed choice.
        let mixed = config::parse("home_address = a.example:1\n").expect("parses");
        assert_eq!(Reach::of(&mixed), Reach::Rendezvous);
    }

    #[test]
    fn saving_the_reach_says_whether_the_file_changed() {
        let dir = tempfile::tempdir().expect("tmp");
        let fixed = Reach::Fixed("10.0.0.2:4243".to_string());
        assert!(save_reach(dir.path(), &fixed).expect("first"));
        assert!(!save_reach(dir.path(), &fixed).expect("second"));
        assert!(save_reach(dir.path(), &Reach::Rendezvous).expect("back"));
        let config = config::load(dir.path()).expect("loads");
        assert_eq!(config.home_address, None);
        assert_eq!(config.rendezvous_dial, Some(true));
    }

    /// The run under sudo asks nothing: every answer is a flag, `--yes`
    /// last, and the code is issued as the service's account.
    #[test]
    fn the_root_run_and_the_admit_run_carry_every_answer() {
        let exe = Path::new("/usr/bin/loquax");
        let answers = Answers {
            store: Store::System,
            reach: Reach::Fixed("example.org:4243".to_string()),
            name: Some("phone".to_string()),
            phone_port: phone_server::PORT,
        };
        assert_eq!(
            sudo_argv(exe, &answers).join(" "),
            "sudo /usr/bin/loquax mailbox setup --store system --reach fixed \
             --address example.org:4243 --name phone --yes"
        );
        let rendezvous = Answers {
            store: Store::System,
            reach: Reach::Rendezvous,
            name: None,
            phone_port: phone_server::PORT,
        };
        assert_eq!(
            rendezvous.flags().join(" "),
            "--store system --reach rendezvous --yes"
        );
        let forwarded = Answers {
            reach: Reach::Fixed("example.org:443".to_string()),
            phone_port: 5000,
            ..answers
        };
        assert_eq!(
            forwarded.flags().join(" "),
            "--store system --reach fixed --address example.org:443 --phone-port 5000 \
             --name phone --yes"
        );
        assert_eq!(
            admit_argv(exe, &system_dir(), Some("phone")).join(" "),
            "runuser -u loquax -- env LNMSG_HOME=/var/lib/loquax-mailbox \
             /usr/bin/loquax mailbox admit --code-only --name phone"
        );
    }

    #[test]
    fn the_flags_read_back() {
        assert_eq!(Store::parse("system"), Ok(Store::System));
        assert_eq!(Store::parse(" account "), Ok(Store::Account));
        assert!(Store::parse("home").is_err());
        assert_eq!(
            check_address(" 10.0.0.2:4243 "),
            Ok("10.0.0.2:4243".to_string())
        );
        assert!(check_address("no-port").is_err());
    }

    /// The step-5 line names the buttons the app shows (`where_computer`
    /// and `scan_code` in `loquax-android/android/res/values/strings.xml`).
    #[test]
    fn the_phone_line_names_the_apps_buttons() {
        let strings = include_str!("../../../loquax-android/android/res/values/strings.xml");
        for (name, label) in [
            ("where_computer", "On a computer at home"),
            ("scan_code", "Scan the code"),
        ] {
            assert!(
                strings.contains(&format!("<string name=\"{name}\">{label}</string>")),
                "{name} is no longer {label:?}"
            );
            assert!(PHONE_LINE.contains(&format!("\"{label}\"")));
        }
    }
}

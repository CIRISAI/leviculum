//! `loquax` binary: the LXMF messenger described in
//! `docs/src/concepts/loquax.md`. With no subcommand it is the window
//! (`gui`, with the `gui` feature) when a graphical session is there, and
//! the conversation view in the terminal (`tui`, with the `tui` feature)
//! otherwise ([`front`]); the subcommands are the paths for scripts:
//!
//! ```sh
//! echo "disk 91%" | loquax send <address> --title "backup"
//! loquax fetch
//! ```
//!
//! It attaches to a running `lnsd` or `rnsd` shared instance the way `lnomad`
//! does, runs one command through its own LXMF router and exits. A send that
//! worked prints nothing at all; errors go to stderr. It never starts a
//! Reticulum stack of its own, and it never claims a message was delivered.
//!
//! Exit codes extend `lnomad`'s convention (`lnomad/src/main.rs:174`, `:188`,
//! `:222`): 0 success, 1 operational failure, 2 argument error — plus 3,
//! "a propagation node holds the message", which `--via auto`'s fallback and
//! `--via propagated` return so a script can tell the two successes apart
//! (`loquax/src/send.rs`, `EXIT_PROPAGATED`).

#[cfg(any(feature = "tui", feature = "gui"))]
mod browsing;
mod cpu;
#[cfg(any(feature = "tui", feature = "gui"))]
mod desktop;
mod front;
#[cfg(feature = "gui")]
mod gui;
#[cfg(any(feature = "tui", feature = "gui"))]
mod mic;
#[cfg(any(feature = "tui", feature = "gui"))]
mod shell;
#[cfg(feature = "tui")]
mod tui;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};

use leviculum_std::config::Config;

use loquax::clock::Tokio;
use loquax::effects::Terminal;
use loquax::engine::{attach, AttachConfig};
use loquax::fetch::{run_fetch, FetchError, FetchOptions};
use loquax::mailbox_state::{self, MailboxRun, Route};
use loquax::outbox::{MailboxInput, SendRequest, Via};
use loquax::seen::SeenStore;
use loquax::send::{
    run_send, run_send_auto, run_send_propagated, AutoError, AutoSent, SendOptions, EXIT_FAILURE,
    EXIT_PROPAGATED, EXIT_USAGE,
};
use loquax::{address, body, display_name, events, identity};

use front::{Ask, Front};

/// Everything after attaching gets this many seconds by default: becoming
/// ready, learning a route to the destination, queueing, and getting the
/// message onto the network. A cron job needs to know when the command is
/// guaranteed to be gone more than it needs a slow path to succeed. In
/// `--via auto` the direct leg and the fallback leg each get one budget.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// `loquax fetch`'s default budget: a drain crosses the link twice for the
/// list round alone and the payload can be a mailbox-full, so it gets twice
/// the send budget before it is called stuck.
const DEFAULT_FETCH_TIMEOUT_SECS: u64 = 60;

/// `--version` names the v3 build (1469), so a person, a test and
/// scripts/verify-deb-packaging.sh can tell which of the two answered.
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
macro_rules! cpu_suffix {
    () => {
        " (x86-64-v3)"
    };
}
#[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
macro_rules! cpu_suffix {
    () => {
        ""
    };
}

#[derive(Parser, Debug)]
#[command(
    name = "loquax",
    version = concat!(env!("LEVICULUM_VERSION"), cpu_suffix!()),
    about = "the Leviculum messenger; with no arguments it opens the window, or the terminal view \
             where there is no display",
    long_about = "the Leviculum messenger; with no arguments it opens the window, or the terminal view \
                  where there is no display\n\n\
                  The window opens when a graphical session is there (WAYLAND_DISPLAY or \
                  DISPLAY set) and this build carries it; otherwise the terminal view \
                  starts, saying so on stderr when the window could have opened. `loquax \
                  gui` insists on the window and fails without a display, `loquax tui` \
                  insists on the terminal view. A flag only the terminal view takes \
                  (--script, --to, --ascii, ...) asks for it too.\n\n\
                  `send`, `fetch` and `address` are the commands for scripts. \
                  Loquax attaches to a daemon that is already running (lnsd, or \
                  Python's rnsd) the same way lnomad does. It does not start a \
                  Reticulum stack of its own, so a daemon has to be running.\n\n\
                  A send that worked prints nothing and exits 0, which means \
                  the message was queued cleanly -- not that it was delivered. \
                  Exit 3 means a propagation node holds the message for later \
                  collection. Failures explain themselves on stderr. Set \
                  LEVICULUM_EVENT_LOG=<path> for the full run as structured \
                  events, including the message id."
)]
// No subcommand is the messenger (Lew, 2026-10-26; the window first since
// 2026-11-02, 1342), so the subcommand is an `Option` and the messenger's
// options sit at the top level; `tui` stays as a subcommand so every script
// and cell that names it keeps working.
// [`parse_args`] keeps the two forms apart: `loquax --to X send` is an
// argument error, not a send with a stray flag. `--config` alone may go
// before a subcommand (1119), which clap's `args_conflicts_with_subcommands`
// cannot allow: it exempts no argument, global ones included.
struct Args {
    /// Run Loquax Mailbox, as `loquax mailbox run` does; kept for the
    /// scripts and units that name it. Takes the messenger's --instance,
    /// --config, --pn, --sync-interval and --timeout.
    #[cfg(feature = "tui")]
    #[arg(long)]
    daemon: bool,

    #[cfg(feature = "tui")]
    #[command(flatten)]
    tui: TuiArgs,

    /// When no daemon answers, print what the stack said (the instance, its
    /// socket, the daemons that would serve it) instead of the one sentence
    /// that names the command to start it. Taken before or after a
    /// subcommand.
    #[arg(long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<SubCommand>,
}

#[derive(Subcommand, Debug)]
enum SubCommand {
    /// Queue one message for an LXMF address.
    Send(SendArgs),
    /// Drain the propagation node's mailbox and print what is new.
    Fetch(FetchArgs),
    /// Print this machine's own LXMF address.
    Address,
    /// Announce this machine's position to the mesh once, on `lev.position`
    /// beside its delivery address: every Loquax in range shows it on the
    /// map, a contact's pin or a node nearby, with no mailbox and no
    /// message.
    Announce(AnnounceArgs),
    /// Bring over the identity, contacts and history of another messenger:
    /// NomadNet's tree, a raw identity file of any app, a pasted Base32 key,
    /// or a loquax export. Without a path it looks in the apps' default
    /// places. Asks before it writes; the identity only on a typed `yes`.
    #[cfg(feature = "history")]
    Import(ImportArgs),
    /// Write this loquax's identity, contacts and history into a new
    /// directory in NomadNet's shape, which NomadNet and `loquax import`
    /// read. The directory holds the private key, with mode 0600.
    #[cfg(feature = "history")]
    Export {
        /// The directory to write; it must not exist or be empty.
        dir: PathBuf,
    },
    /// The terminal messenger, as plain `loquax` where there is no display:
    /// conversations, messages and a compose line.
    #[cfg(feature = "tui")]
    Tui(TuiArgs),
    /// The messenger in a window, as plain `loquax` on a desktop, attached as
    /// the terminal one is. Fails when there is no display.
    #[cfg(feature = "gui")]
    Gui(GuiArgs),
    /// Loquax Mailbox, the always-on part that receives and keeps your
    /// mail while your devices are off: run it, enable its system service,
    /// admit, list and revoke devices, join one from a device. `home` is
    /// the old spelling and still works.
    #[cfg(feature = "tui")]
    #[command(subcommand, alias = "home")]
    Mailbox(MailboxCommand),
}

/// The phones' port of a mailbox on its own node (1354): where a daemon's
/// `[[Loquax Mailbox TCP]]` section would listen, `0.0.0.0:4243`, since no
/// daemon serves one. Taken by something else, the port is a loopback one
/// the system picks: the mailbox then reaches its phones through its entry
/// servers alone, which its log says.
#[cfg(feature = "tui")]
fn own_screens() -> Result<std::net::TcpListener, String> {
    let port = loquax::home::phone_server::PORT;
    std::net::TcpListener::bind(("0.0.0.0", port)).or_else(|error| {
        tracing::warn!("HOME phones' port {port} not taken: {error}");
        std::net::TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("no port for the phones: {error}"))
    })
}

/// `loquax mailbox ...` (also `loquax home ...`).
#[cfg(feature = "tui")]
#[derive(Subcommand, Debug)]
enum MailboxCommand {
    /// Set up this computer as your mailbox, in five steps: where it keeps
    /// your mail, start on boot, how your phone reaches it, the code to
    /// scan, and what to press on the phone. Asks what no flag answers;
    /// running it again keeps what is set and shows a new code.
    Setup(SetupArgs),
    /// Run Loquax Mailbox in the foreground until SIGINT or SIGTERM:
    /// receive for this identity, archive every message, answer the
    /// admitted devices, collect from the propagation node. `loquax
    /// --daemon` is the same. The packaged system service runs this.
    Run(RunArgs),
    /// Enable and start the packaged system service,
    /// `loquax-mailbox.service`, through systemctl; needs root.
    Enable,
    /// Stop the packaged system service and disable it; needs root. Its
    /// state under /var/lib/loquax-mailbox stays.
    Disable,
    /// On the mailbox: open a single-use admission for one device and
    /// print its code, as text and as a QR code.
    Admit {
        /// The name the device is listed and revoked under.
        #[arg(long)]
        name: Option<String>,
        /// The mailbox's TCP server as a phone reaches it, `host:port`:
        /// written into the code (`lqxh1...@host:port`) ahead of the
        /// rendezvous list the mailbox dials itself, so the phone needs one
        /// scan or one paste and tries this address first.
        #[arg(long)]
        server: Option<String>,
        /// Print the code and its QR code only, for `mailbox setup`.
        #[arg(long, hide = true)]
        code_only: bool,
    },
    /// On the new device: redeem a code from `mailbox admit` and become a
    /// client of that Loquax Mailbox.
    Join(JoinArgs),
    /// On the mailbox: list the admitted devices.
    Devices,
    /// On the mailbox: remove a device, by its name or its hash (four hex
    /// characters of it are enough when they are unique).
    Revoke { device: String },
    /// On the mailbox: list the admission codes not yet used and not yet
    /// expired (`home_token_hours` in the config, 24 by default).
    Tokens,
    /// On the mailbox: withdraw an admission code not yet used, by the
    /// start of its hash as `mailbox tokens` lists it (four hex characters
    /// are enough when they are unique).
    RevokeToken { prefix: String },
    /// On the mailbox: print the archive, one entry a line, `id kind at`
    /// (the message id in hex, the entry's kind, Unix seconds), oldest
    /// first. Two mailboxes of one identity that hold the same lines hold
    /// the same history.
    Archive,
    /// On the mailbox: print the ratchet ring of the delivery address,
    /// `ratchets=<count> ring=<sha256 of the public keys>`, or `ratchets=0`
    /// before it has one. Two mailboxes with one ring open the same
    /// packets.
    Ratchets,
}

/// `loquax mailbox setup`'s answers (`loquax::home::setup`).
#[cfg(feature = "tui")]
#[derive(clap::Args, Debug)]
struct SetupArgs {
    /// Where the mailbox keeps your mail: `system`, in
    /// /var/lib/loquax-mailbox, run by the packaged service from every boot
    /// on (the default where the loquax package is installed), or
    /// `account`, in your own Loquax directory, started with `loquax
    /// mailbox run`.
    #[arg(long, value_name = "WHERE")]
    store: Option<String>,

    /// How a phone reaches the mailbox: `rendezvous` (the default), through
    /// leviculum.network with nothing to set up on a router, or `fixed`,
    /// at --address.
    #[arg(long, value_name = "HOW")]
    reach: Option<String>,

    /// The fixed address, `host:port`, as a phone reaches this computer's
    /// port for phones (--phone-port); implies `--reach fixed`.
    #[arg(long, value_name = "HOST:PORT")]
    address: Option<String>,

    /// The port for phones the Reticulum daemon listens on, for a fixed
    /// address: 4243 unless another is given. The address may name another
    /// port when a router forwards it here.
    #[arg(long, value_name = "PORT")]
    phone_port: Option<u16>,

    /// The name the device is admitted under.
    #[arg(long)]
    name: Option<String>,

    /// Ask nothing: every question no flag answers takes its default.
    #[arg(long)]
    yes: bool,

    /// Reticulum config directory, where the port for phones goes for a
    /// fixed address on a mailbox in your account (default: the platform
    /// default; the system mailbox's is /etc/reticulum).
    #[arg(long)]
    config: Option<PathBuf>,
}

#[cfg(feature = "tui")]
#[derive(clap::Args, Debug)]
struct JoinArgs {
    /// The code `loquax mailbox admit` printed on the mailbox.
    code: String,

    /// Shared-instance name to connect to (overrides the config file's).
    #[arg(long)]
    instance: Option<String>,

    /// Reticulum config directory (default: the platform default).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Seconds for the path, the link and the answer.
    #[arg(long, default_value_t = DEFAULT_FETCH_TIMEOUT_SECS)]
    timeout: u64,
}

/// `loquax mailbox run`, and what `loquax --daemon` takes of the
/// messenger's options.
#[cfg(feature = "tui")]
#[derive(clap::Args, Debug)]
struct RunArgs {
    /// The propagation node to collect from, as `send --pn`.
    #[arg(long, value_name = "HASH")]
    pn: Option<String>,

    /// Shared-instance name to connect to (overrides the config file's).
    #[arg(long)]
    instance: Option<String>,

    /// Reticulum config directory (default: the platform default).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Seconds each send's leg may take, as `send --timeout`; a fetch gets
    /// twice this.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    timeout: u64,

    /// The longest the propagation node goes unsynced, as the messenger's
    /// `--sync-interval`: `15m` by default, `off` for none.
    #[arg(long, value_name = "INTERVAL")]
    sync_interval: Option<String>,
}

#[cfg(feature = "gui")]
#[derive(clap::Args, Debug)]
struct GuiArgs {
    /// The propagation node for the fallback and for fetch, as `send --pn`.
    #[arg(long, value_name = "HASH")]
    pn: Option<String>,

    /// Shared-instance name to connect to (overrides the config file's).
    #[arg(long)]
    instance: Option<String>,

    /// Reticulum config directory (default: the platform default).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Seconds each send's leg may take, as `send --timeout`; a fetch gets
    /// twice this.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    timeout: u64,

    /// The longest the mailbox goes unsynced, as the terminal's
    /// `--sync-interval`: `15m` by default, `off` for none.
    #[arg(long, value_name = "DURATION")]
    sync_interval: Option<String>,

    /// Open the window, draw its first frame and exit 0, without attaching
    /// to a daemon: the check that the window starts on this display.
    #[arg(long)]
    exit_after_first_frame: bool,

    /// Measure the window's frames: frames per second and a frame's time
    /// with its parts in the top right corner, and a WINDOW_FRAME line a
    /// second in the log while it draws. `LOQUAX_FPS=1` does the same.
    #[arg(long)]
    fps: bool,

    /// Fail when no shared instance answers instead of starting lnsd as a
    /// child, which goes when loquax does (zero-config, #471).
    #[arg(long)]
    no_start: bool,
}

#[cfg(feature = "tui")]
#[derive(clap::Args, Debug)]
struct TuiArgs {
    /// A correspondent to list from the start: 32 hex characters, repeatable.
    /// The history restores every conversation already held, so this is for
    /// writing first to someone it does not hold yet.
    #[arg(long, value_name = "ADDRESS")]
    to: Vec<String>,

    /// The propagation node for the fallback and for fetch, as `send --pn`.
    #[arg(long, value_name = "HASH")]
    pn: Option<String>,

    /// Shared-instance name to connect to (overrides the config file's).
    #[arg(long)]
    instance: Option<String>,

    /// Reticulum config directory (default: the platform default). Taken
    /// before a subcommand too, for every subcommand: `loquax --config X
    /// mailbox join CODE` as well as `loquax mailbox join CODE --config X`
    /// (1119).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Seconds each send's leg may take, as `send --timeout`; a fetch gets
    /// twice this.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    timeout: u64,

    /// The longest the mailbox goes unsynced: `15m` (the default, over the
    /// config file's `sync_interval`), `90s`, `1h`, minutes without a unit;
    /// `off` or `0` syncs only at start and on Ctrl-r. After a message in or
    /// out it syncs more often for a while, never less often.
    #[arg(long, value_name = "INTERVAL")]
    sync_interval: Option<String>,

    /// Fail when no shared instance answers instead of starting lnsd as a
    /// child, which goes when loquax does (zero-config, #471).
    #[arg(long)]
    no_start: bool,

    /// Draw the delivery marks in ASCII (`.` `*` `o` `^` `v` `x`).
    #[arg(long)]
    ascii: bool,

    /// No colours: the monochrome screen, reverse video for the selection
    /// and the toast. `NO_COLOR` set and not empty does the same.
    #[arg(long)]
    no_color: bool,

    /// The colour tier: `auto` climbs the ladder (`COLORTERM` truecolor or
    /// 24bit, `TERM` with 256color, none on a dumb or unset `TERM`, else the
    /// sixteen); `truecolor`, `256` and `16` force one. `--no-color` and
    /// `NO_COLOR` still win.
    #[arg(long, value_enum, default_value_t = ColorArg::Auto)]
    color: ColorArg,

    /// Light or dark colours: `auto` asks the terminal for its background
    /// (dark when it does not answer). Over the config file's `theme` key.
    #[arg(long, value_enum)]
    theme: Option<ThemeArg>,

    /// Leave the mouse to the terminal, so its own selection and copy work;
    /// the palette's `mouse` captures it again.
    #[arg(long)]
    no_mouse: bool,

    /// How the map is drawn: `auto` picks by itself, asking the terminal
    /// and climbing the ladder, kitty or sixel, else half-blocks in colour,
    /// else braille, which is also what it takes behind mosh; the others
    /// force one.
    #[arg(long, value_enum, default_value_t = tui::map::GraphicsFlag::Auto)]
    graphics: tui::map::GraphicsFlag,

    /// Run a key script instead of reading the keyboard, and write every
    /// `frame` it asks for to --frames. For tests and demos.
    #[arg(long, value_name = "FILE")]
    script: Option<PathBuf>,

    /// Where a script's frames go (default: the script's path with
    /// `.frames`).
    #[arg(long, value_name = "FILE", requires = "script")]
    frames: Option<PathBuf>,

    /// A script's screen size, columns x rows.
    #[arg(long, value_name = "WxH", default_value = "80x24", requires = "script")]
    size: String,
}

#[derive(clap::Args, Debug)]
struct SendArgs {
    /// Recipient's LXMF address: 32 hex characters, with or without the
    /// `lxmf@` prefix lnomad copies to the clipboard.
    address: String,

    /// Message body. Omitted, or given as `-`, the body is read from stdin —
    /// which is the form a script uses: `echo "disk 91%" | loquax send <addr>`.
    /// One trailing newline is removed.
    body: Option<String>,

    /// Message title (LXMF carries one; it may be empty).
    #[arg(long)]
    title: Option<String>,

    /// Send a position with it: the Telemeter of this place in degrees,
    /// timed now, as Sideband and Columba send one. Without a body the
    /// message is a position report, no text, which a receiving Loquax
    /// shows as a state and raises no notification for (1209); stdin is
    /// then not read.
    #[arg(long, value_name = "LAT,LON", allow_hyphen_values = true)]
    position: Option<String>,

    /// Send this file with it, as Columba and Sideband attach one (1221):
    /// the LXMF file attachments field, its name and bytes unchanged. A
    /// file over 990 KB is refused, the most a receiver takes. Without a
    /// body the message is the file alone and stdin is not read.
    #[arg(long, value_name = "FILE")]
    file: Option<PathBuf>,

    /// Send this picture with it, as Columba and Sideband send a photo
    /// (1220): turned upright, scaled by `--photo-size`, its EXIF (the
    /// place, the camera) removed. Without a body the message is the
    /// photo alone and stdin is not read.
    #[cfg(feature = "photos")]
    #[arg(long, value_name = "FILE")]
    photo: Option<PathBuf>,

    /// How far `--photo` is scaled: `small` (800 px), `phone` (1600 px,
    /// the default) or `original`. Without it the config's `photo_size`
    /// decides.
    #[cfg(feature = "photos")]
    #[arg(long, value_name = "SIZE", requires = "photo")]
    photo_size: Option<String>,

    /// The name recipients see as the sender. Defaults to the account name of
    /// the user running loquax; `LNMSG_DISPLAY_NAME` sets it where there is no
    /// command line to edit, and this flag wins over it.
    #[arg(long, value_name = "NAME")]
    from: Option<String>,

    /// How the message travels. `auto` tries the direct delivery link first
    /// and falls back to a propagation node when none comes up; `direct` and
    /// `propagated` are the explicit choices.
    #[arg(long, value_enum, default_value_t = ViaArg::Auto)]
    via: ViaArg,

    /// The propagation node to use: 32 hex characters, remembered as your
    /// choice. Without it, the `propagation_node` key in loquax's config
    /// decides; without either, the mailbox loquax adopted on an earlier run,
    /// or a newly adopted one: the most recently heard node that does not
    /// run on this device.
    #[arg(long, value_name = "HASH")]
    pn: Option<String>,

    /// Shared-instance name to connect to (overrides the config file's).
    #[arg(long)]
    instance: Option<String>,

    /// Reticulum config directory (default: the platform default, like `lncp`).
    /// Only its instance name is read; loquax keeps its own state elsewhere.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Seconds to spend, after connecting, on getting the message onto the
    /// network. This is not a wait for delivery: the command returns as soon
    /// as the stack has taken the message. In `--via auto` each leg gets
    /// this budget.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    timeout: u64,
}

#[derive(clap::Args, Debug)]
struct AnnounceArgs {
    /// The place to announce, `LAT,LON` in degrees, timed now.
    #[arg(long)]
    position: String,

    /// The name the delivery address announces beside it (default: as
    /// `send --from`).
    #[arg(long)]
    from: Option<String>,

    /// Shared-instance name to connect to (overrides the config file's).
    #[arg(long)]
    instance: Option<String>,

    /// Reticulum config directory (default: the platform default, like `lncp`).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Seconds to spend, after connecting, on getting the announce out.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    timeout: u64,
}

#[cfg(feature = "history")]
#[derive(clap::Args, Debug)]
struct ImportArgs {
    /// List what would be imported, per conversation with counts of new,
    /// already held and unreadable messages, and write nothing.
    #[arg(long)]
    dry_run: bool,

    /// Read a Base32 identity key, as Sideband, Columba, MeshChatX and
    /// Ratspeak export it, from the first line of stdin; it is not taken on
    /// the command line, where it would stay in the shell's history.
    #[arg(long, conflicts_with = "path")]
    key: bool,

    /// A NomadNet directory, a loquax export, a raw identity file, a file
    /// with a Base32 key, or a copy of Ratspeak's `ratspeak.db` (schema 33 or 45).
    /// Without it, the apps' default places are scanned; a Ratspeak database
    /// is never looked for.
    path: Option<PathBuf>,

    /// The ratchet file of the app the identity came from, for the identity
    /// Loquax answers on (after the import, when it adopts one): Python
    /// LXMF's `<destination hash>.ratchets` (NomadNet, Sideband) or
    /// Ratspeak's `identities/<hash>/ratchets/ring`, told apart by content.
    /// Peers that heard that app's announce encrypt short messages to its
    /// ratchet for up to 30 days. NomadNet's and Ratspeak's are taken
    /// along by themselves when the identity is adopted from their place.
    #[arg(long, value_name = "FILE")]
    ratchets: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct FetchArgs {
    /// The propagation node to drain: 32 hex characters. Resolution order is
    /// the same as `send --pn`'s.
    #[arg(long, value_name = "HASH")]
    pn: Option<String>,

    /// Shared-instance name to connect to (overrides the config file's).
    #[arg(long)]
    instance: Option<String>,

    /// Reticulum config directory (default: the platform default).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Seconds for the whole drain: selecting the node, the link, the
    /// list/fetch/confirm round.
    #[arg(long, default_value_t = DEFAULT_FETCH_TIMEOUT_SECS)]
    timeout: u64,
}

/// The `--via` choice. `auto` is frontend orchestration over the seam's
/// [`Via`] values, so it has no engine-side mirror.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum ViaArg {
    /// Direct link first; a propagation node when none comes up.
    Auto,
    /// Straight to the recipient over the mesh, never a mailbox.
    Direct,
    /// Through a propagation node's mailbox, never a direct attempt.
    Propagated,
}

/// The `--color` choice.
#[cfg(feature = "tui")]
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum ColorArg {
    /// Climb the ladder.
    Auto,
    /// 24-bit true colour.
    Truecolor,
    /// The xterm-256 palette.
    #[value(name = "256")]
    Ansi256,
    /// The sixteen ANSI colours.
    #[value(name = "16")]
    Ansi16,
}

#[cfg(feature = "tui")]
impl From<ColorArg> for leviculum_ui::ColorFlag {
    fn from(arg: ColorArg) -> Self {
        match arg {
            ColorArg::Auto => Self::Auto,
            ColorArg::Truecolor => Self::Truecolor,
            ColorArg::Ansi256 => Self::Ansi256,
            ColorArg::Ansi16 => Self::Ansi16,
        }
    }
}

/// The `--theme` choice.
#[cfg(feature = "tui")]
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum ThemeArg {
    /// Ask the terminal.
    Auto,
    /// Dark colours.
    Dark,
    /// Light colours.
    Light,
}

#[cfg(feature = "tui")]
impl From<ThemeArg> for leviculum_ui::ThemeFlag {
    fn from(arg: ThemeArg) -> Self {
        match arg {
            ThemeArg::Auto => Self::Auto,
            ThemeArg::Dark => Self::Dark,
            ThemeArg::Light => Self::Light,
        }
    }
}

fn usage(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("loquax: {message}");
    events::done(None, "usage", EXIT_USAGE);
    ExitCode::from(EXIT_USAGE)
}

fn failure(message: impl std::fmt::Display, outcome: &str) -> ExitCode {
    eprintln!("loquax: {message}");
    events::done(None, outcome, EXIT_FAILURE);
    ExitCode::from(EXIT_FAILURE)
}

/// `--verbose`, set once from the command line before any subcommand runs.
static VERBOSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Attaching failed: one sentence and the command that starts the daemon
/// (1265, walk finding 5), or with `--verbose` what the stack said.
fn no_daemon(error: &loquax::engine::AttachError) -> ExitCode {
    match VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
        true => failure(error, "no-daemon"),
        false => failure(
            error.brief(
                loquax::engine::locale(),
                loquax::engine::start_command_here(),
            ),
            "no-daemon",
        ),
    }
}

/// `$LNMSG_HOME/reticulum` when `LNMSG_HOME` is set.
#[cfg(any(feature = "tui", feature = "gui"))]
fn moved_daemon_dir() -> Option<std::path::PathBuf> {
    std::env::var_os(identity::HOME_ENV)
        .filter(|home| !home.is_empty())
        .map(|home| std::path::PathBuf::from(home).join(DAEMON_DIR))
}

/// Where the instance name the window and the terminal ask for came from,
/// as the `INSTANCE` line's `source=` says it.
#[cfg(any(feature = "tui", feature = "gui"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NameSource {
    /// `--instance`, or the config `--config` names.
    Flag,
    /// The config under `$LNMSG_HOME/reticulum`.
    Home,
    /// The config under the user's `~/.reticulum`.
    DefaultDir,
    /// No config: `default`.
    None,
}

#[cfg(any(feature = "tui", feature = "gui"))]
impl NameSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            NameSource::Flag => "flag",
            NameSource::Home => "home",
            NameSource::DefaultDir => "default-dir",
            NameSource::None => "none",
        }
    }
}

/// One daemon to look for: its config directory (its transport, and the
/// RPC key a client asks it with) and the instance name.
#[cfg(any(feature = "tui", feature = "gui"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Look {
    pub dir: std::path::PathBuf,
    pub instance: String,
    pub source: NameSource,
}

/// What the window and the terminal look for before they start a daemon,
/// and what they start (1598). The running daemon is looked for, in order,
/// by `--instance`, by the config under `$LNMSG_HOME/reticulum`, by the one
/// under `~/.reticulum`, and, where a config named another, as `default`;
/// `--instance` or `--config` is the one look there is. Only when none
/// answers is a daemon started, under the name `--instance` or that
/// directory's config gives, in `--config`, else `$LNMSG_HOME/reticulum`
/// when [`LNMSG_HOME`](identity::HOME_ENV) moves Loquax's state, else the
/// user's `~/.reticulum`: a Loquax whose state lives elsewhere (a test, a
/// second identity) starts its daemon there too, never in the user's own
/// directory (1570: a test's started lnsd ran 20 s on the coder host's
/// `~/.reticulum`, its AutoInterface up). Before 1598 the
/// name came from the start directory alone, so a Loquax with `LNMSG_HOME`
/// set asked for `default`, missed the user's daemon with its own name, and
/// started a second one beside it.
#[cfg(any(feature = "tui", feature = "gui"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DaemonLookup {
    /// The running daemons to try, in order.
    pub looks: Vec<Look>,
    /// The daemon to start when none of them answers.
    pub start: Look,
}

#[cfg(any(feature = "tui", feature = "gui"))]
pub(crate) fn daemon_lookup(
    config: Option<std::path::PathBuf>,
    instance: Option<String>,
) -> DaemonLookup {
    lookup_in(
        config,
        instance,
        moved_daemon_dir(),
        Config::default_config_dir(),
    )
}

/// [`daemon_lookup`] with its directories given, for the tests.
#[cfg(any(feature = "tui", feature = "gui"))]
fn lookup_in(
    config: Option<std::path::PathBuf>,
    instance: Option<String>,
    moved: Option<std::path::PathBuf>,
    default_dir: std::path::PathBuf,
) -> DaemonLookup {
    let has_config = |dir: &std::path::Path| dir.join("config").is_file();
    let explicit = config.is_some() || instance.is_some();
    let start_dir = config
        .clone()
        .or_else(|| moved.clone())
        .unwrap_or_else(|| default_dir.clone());
    let start = match (&instance, explicit) {
        (Some(name), _) => Look {
            dir: start_dir.clone(),
            instance: name.clone(),
            source: NameSource::Flag,
        },
        (None, true) => Look {
            instance: instance_name(&start_dir),
            dir: start_dir.clone(),
            source: NameSource::Flag,
        },
        (None, false) => Look {
            instance: instance_name(&start_dir),
            source: match (has_config(&start_dir), moved.is_some()) {
                (false, _) => NameSource::None,
                (true, true) => NameSource::Home,
                (true, false) => NameSource::DefaultDir,
            },
            dir: start_dir.clone(),
        },
    };
    if explicit {
        // What the flags name is what is asked for, and nothing else.
        let dir = match &config {
            Some(dir) => dir.clone(),
            None => [moved.clone(), Some(default_dir.clone())]
                .into_iter()
                .flatten()
                .find(|dir| has_config(dir))
                .unwrap_or_else(|| start_dir.clone()),
        };
        return DaemonLookup {
            looks: vec![Look {
                dir,
                ..start.clone()
            }],
            start,
        };
    }
    let mut looks: Vec<Look> = Vec::new();
    let configured = moved
        .iter()
        .map(|dir| (dir, NameSource::Home))
        .chain([(&default_dir, NameSource::DefaultDir)]);
    for (dir, source) in configured {
        if has_config(dir) {
            looks.push(Look {
                dir: dir.clone(),
                instance: instance_name(dir),
                source,
            });
        }
    }
    if !looks.iter().any(|look| look.instance == "default") {
        // A default daemon is most likely the user's own, whose RPC key
        // lies under `~/.reticulum` when anywhere.
        looks.push(Look {
            dir: default_dir.clone(),
            instance: "default".to_string(),
            source: NameSource::None,
        });
    }
    let mut seen = std::collections::HashSet::new();
    looks.retain(|look| seen.insert(look.instance.clone()));
    DaemonLookup { looks, start }
}

/// The daemon's directory under a moved `LNMSG_HOME` ([`daemon_lookup`]).
/// Not the state directory itself: Loquax's own `config` lies there.
#[cfg(any(feature = "tui", feature = "gui"))]
const DAEMON_DIR: &str = "reticulum";

/// No daemon to attach to and none could be started. An lnsd that is not
/// there to start is the missing daemon the one sentence is for (1265), so
/// it reads as [`no_daemon`] does; a start that failed otherwise (it exited,
/// it never answered) is said as it is, with its log.
#[cfg(any(feature = "tui", feature = "gui"))]
fn no_start(error: leviculum_std::instance::InstanceError, instance: &str) -> ExitCode {
    use leviculum_std::instance::InstanceError;
    match &error {
        InstanceError::Spawn { error: io, .. } if io.kind() == std::io::ErrorKind::NotFound => {
            no_daemon(&loquax::engine::AttachError::NoDaemon {
                instance: instance.to_string(),
                detail: error.to_string(),
            })
        }
        _ => failure(error, "no-daemon"),
    }
}

/// The instance name decides the abstract socket (`\0rns/{name}`) the daemon
/// listens on, so it comes from the daemon's own config file. Same derivation
/// `lncp`, `lnstatus` and `leviculum-lxmf-node` use.
fn instance_name(config_dir: &std::path::Path) -> String {
    let config_file = config_dir.join("config");
    if config_file.exists() {
        if let Ok(config) = Config::load(&config_file) {
            return config.reticulum.instance_name;
        }
    }
    "default".to_string()
}

/// Resolve the propagation-node choice: the `--pn` flag, then loquax's own
/// config. `None` with `"adopted"` means "let the adoption rule decide"
/// (`loquax::mailbox`). The source word is what `LNMSG_PN` logs.
fn resolve_pn(
    flag: Option<&str>,
    home: &std::path::Path,
) -> Result<(Option<[u8; 16]>, &'static str), String> {
    if let Some(text) = flag {
        return match address::parse(text) {
            Ok(hash) => Ok((Some(hash), "flag")),
            Err(error) => Err(format!("--pn {}: {error}", text.trim())),
        };
    }
    match loquax::config::load(home) {
        Ok(config) => match config.propagation_node {
            Some(hash) => Ok((Some(hash), "config")),
            None => Ok((None, "adopted")),
        },
        Err(error) => Err(error.to_string()),
    }
}

/// How this run selects its mailbox. A run that names a node with `--pn` or
/// the config needs no input; one that does not reads the daemon's path
/// table once, here, because the engine runs under the core lock and may not
/// make a round trip. `excluded` are the nodes that stopped answering, which
/// only the TUI, running sync after sync, keeps; a CLI run has none.
async fn mailbox_run(
    home: &Path,
    config_dir: &Path,
    instance: &str,
    preferred: Option<[u8; 16]>,
    excluded: Vec<[u8; 16]>,
) -> MailboxRun {
    let input = match preferred {
        Some(_) => None,
        None => Some(MailboxInput {
            routes: read_routes(config_dir, instance).await,
            previous: mailbox_state::load(home),
            excluded,
        }),
    };
    MailboxRun { input }
}

/// The client node's storage directory under `home`, created. One that
/// cannot be made fails the run with the outcome a missing daemon has, as it
/// did while `attach` made it.
fn storage_dir(home: &Path) -> Result<PathBuf, String> {
    let dir = home.join("storage");
    std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    Ok(dir)
}

/// The daemon's path table over the shared-instance RPC, keyed the way
/// `lnstatus` and `lnpath` key themselves (`leviculum_std::daemon_rpc`).
async fn read_routes(config_dir: &Path, instance: &str) -> Result<Vec<Route>, String> {
    let config_file = config_dir.join("config");
    let config = config_file
        .exists()
        .then(|| Config::load(&config_file).ok())
        .flatten();
    let (authkey, _) = leviculum_std::daemon_rpc::resolve_authkey(config_dir, config.as_ref())
        .inspect_err(|error| tracing::info!("RPC path_table authkey failed: {error}"))?;
    // Every RPC call loquax makes is this one, and each says how it went: a
    // shared instance that logs "digest received was wrong" for a client
    // can then be checked against this line (leviculum 1002).
    let table = leviculum_std::rpc_query(instance, &authkey, "path_table")
        .await
        .map_err(|error| error.to_string())
        .inspect_err(|error| {
            tracing::info!("RPC path_table instance={instance} failed: {error}")
        })?;
    let routes = mailbox_state::routes_from_path_table(&table);
    tracing::info!(
        "RPC path_table instance={instance} ok entries={}",
        routes.len()
    );
    Ok(routes)
}

/// [`Args`] from the command line, refusing a subcommand together with any of
/// the messenger's own options except `--config`, which every subcommand
/// takes before it as well as after it (1119); and, for no subcommand, what
/// it asks of [`front::front`].
fn parse_args() -> (Args, Ask) {
    use clap::{CommandFactory as _, FromArgMatches as _};

    let mut command = Args::command();
    let matches = command.get_matches_mut();
    let ask = ask_of(&command, &matches);
    let args = Args::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    (args, ask)
}

/// What plain `loquax` with these flags asks: the terminal view when one of
/// them is the terminal's own (anything but [`front::SHARED`]), else
/// Loquax's choice. Exits on a subcommand given with one of the messenger's
/// options, as [`parse_args`] says.
fn ask_of(command: &clap::Command, matches: &clap::ArgMatches) -> Ask {
    use clap::parser::ValueSource;

    let given = |id: &str| matches.value_source(id) == Some(ValueSource::CommandLine);
    if let Some((name, _)) = matches.subcommand() {
        // The command's own arguments, not `matches.ids()`: that also holds
        // the group id the flattened `TuiArgs` brings, which counts as given.
        let stray = command
            .get_arguments()
            .filter(|arg| arg.get_id() != "config" && arg.get_id() != "verbose")
            .find(|arg| given(arg.get_id().as_str()))
            .map(ToString::to_string);
        if let Some(flag) = stray {
            command
                .clone()
                .error(
                    clap::error::ErrorKind::ArgumentConflict,
                    format!("the subcommand '{name}' cannot be used with '{flag}'"),
                )
                .exit();
        }
    }
    let terminal = command
        .get_arguments()
        .map(|arg| arg.get_id().as_str())
        .filter(|id| !front::SHARED.contains(id))
        .any(given);
    match terminal {
        true => Ask::Tui,
        false => Ask::Default,
    }
}

/// The frontend [`front::front`] picks for `ask` on this host and build.
fn pick(ask: Ask) -> Front {
    front::front(
        ask,
        front::has_display(|name| std::env::var_os(name)),
        cfg!(feature = "gui"),
    )
}

/// Plain `loquax`: the window, or the terminal view, as [`pick`] says.
async fn run_default(args: Args, ask: Ask) -> ExitCode {
    match pick(ask) {
        #[cfg(feature = "gui")]
        Front::Gui => return run_gui(gui_args(&args)).await,
        Front::Tui(Some(note)) => eprintln!("loquax: {note}"),
        _ => {}
    }
    #[cfg(feature = "tui")]
    {
        run_tui(args.tui).await
    }
    #[cfg(not(feature = "tui"))]
    {
        let _ = args;
        eprintln!(
            "loquax: the messenger needs the `tui` feature, which this build lacks; \
             see loquax --help for the commands it has"
        );
        ExitCode::from(EXIT_USAGE)
    }
}

/// The window's options from plain `loquax`'s: the ones both frontends take.
#[cfg(feature = "gui")]
fn gui_args(args: &Args) -> GuiArgs {
    #[cfg(feature = "tui")]
    {
        let tui = &args.tui;
        GuiArgs {
            pn: tui.pn.clone(),
            instance: tui.instance.clone(),
            config: tui.config.clone(),
            timeout: tui.timeout,
            sync_interval: tui.sync_interval.clone(),
            no_start: tui.no_start,
            exit_after_first_frame: false,
            fps: false,
        }
    }
    #[cfg(not(feature = "tui"))]
    {
        let _ = args;
        GuiArgs {
            pn: None,
            instance: None,
            config: None,
            timeout: DEFAULT_TIMEOUT_SECS,
            sync_interval: None,
            no_start: false,
            exit_after_first_frame: false,
            fps: false,
        }
    }
}

/// Measured live heap for the mailbox census, under the default-off
/// `heap-accounting` feature (order 1585): lnpnd's shim
/// (`lnpnd/src/main.rs`), counting and forwarding to `System`.
#[cfg(feature = "heap-accounting")]
#[global_allocator]
static ALLOC: leviculum_std::heap_accounting::CountingAllocator =
    leviculum_std::heap_accounting::CountingAllocator;

/// The starter first (1469): on an x86-64-v3 CPU the packaged baseline
/// build hands the process to the v3 build before a thread or a runtime
/// exists ([`cpu`]); everything else is [`messenger`].
fn main() -> ExitCode {
    cpu::start();
    messenger()
}

#[tokio::main]
async fn messenger() -> ExitCode {
    let (mut args, ask) = parse_args();
    VERBOSE.store(args.verbose, std::sync::atomic::Ordering::Relaxed);
    // Once, before any subcommand resolves the state directory: a first
    // start after the rename must find the address it had as lnmsg.
    match identity::migrate_legacy_home() {
        Ok(identity::Migration::NotNeeded) => {}
        Ok(identity::Migration::Moved { from, to }) => {
            eprintln!("loquax: moved {} to {}", from.display(), to.display());
        }
        Err(error) => {
            eprintln!("loquax: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    }
    // The propagation node's selection under its name since 1153, once.
    // Silent when it works, as a send that works is; not fatal when it does
    // not, because `load` then reads the old name where it is.
    if let Some(home) = identity::home_path().ok().filter(|home| home.is_dir()) {
        if let Err(error) = mailbox_state::migrate(&home) {
            eprintln!(
                "loquax: {}: {error}",
                home.join(mailbox_state::LEGACY_MAILBOX_FILE).display()
            );
        }
    }
    // `LEVICULUM_EVENT_LOG=<path>` turns the structured events into an
    // append-only file in the documented format; without it this is the plain
    // fmt subscriber and the events cost a filtered-out call. `warn` rather
    // than `info` as the default: a successful run says nothing on any stream,
    // so an `info`-level line on stderr would be the only thing a cron job
    // ever saw from a send that worked. Whoever wants the detail asks for the
    // event log, which is where the message id lives.
    #[cfg(feature = "tui")]
    if args.daemon {
        let tui = args.tui;
        return run_daemon(RunArgs {
            pn: tui.pn,
            instance: tui.instance,
            config: tui.config,
            timeout: tui.timeout,
            sync_interval: tui.sync_interval,
        })
        .await;
    }
    let command = match args.command.take() {
        #[cfg(feature = "gui")]
        Some(SubCommand::Gui(gui)) => {
            return match pick(Ask::Gui) {
                Front::Refuse(why) => {
                    eprintln!("loquax: {why}");
                    ExitCode::from(EXIT_FAILURE)
                }
                _ => run_gui(gui).await,
            }
        }
        #[cfg(feature = "tui")]
        Some(SubCommand::Tui(tui)) => return run_tui(tui).await,
        // Before the subscriber below, as `--daemon`: the mailbox installs
        // its own, which journals its info lines.
        #[cfg(feature = "tui")]
        Some(SubCommand::Mailbox(MailboxCommand::Run(run))) => return run_daemon(run).await,
        None => return run_default(args, ask).await,
        Some(command) => command,
    };
    leviculum_std::event_log::install_global_subscriber("warn");

    match command {
        SubCommand::Send(send) => run(send).await,
        SubCommand::Announce(args) => run_announce(args).await,
        SubCommand::Fetch(fetch) => run_fetch_command(fetch).await,
        SubCommand::Address => run_address(),
        #[cfg(feature = "history")]
        SubCommand::Import(import) => run_import(import),
        #[cfg(feature = "history")]
        SubCommand::Export { dir } => run_export(&dir),
        #[cfg(feature = "tui")]
        SubCommand::Mailbox(mailbox) => run_mailbox(mailbox).await,
        // Dispatched above, before the subscriber writes to stderr.
        #[cfg(feature = "tui")]
        SubCommand::Tui(_) => ExitCode::SUCCESS,
        #[cfg(feature = "gui")]
        SubCommand::Gui(_) => ExitCode::SUCCESS,
    }
}

/// `loquax gui`: parse what the shell needs, then hand over.
#[cfg(feature = "gui")]
async fn run_gui(args: GuiArgs) -> ExitCode {
    if args.fps || std::env::var_os("LOQUAX_FPS").is_some_and(|v| v == "1") {
        gui::measure_frames();
    }
    if args.exit_after_first_frame {
        return gui::probe();
    }
    let sync_interval = match args
        .sync_interval
        .as_deref()
        .map(loquax::sync::parse_interval)
    {
        None => None,
        Some(Ok(bound)) => Some(bound),
        Some(Err(error)) => return usage(format!("--sync-interval {error}")),
    };
    gui::run(shell::ShellOptions {
        sync_interval,
        to: Vec::new(),
        pn: args.pn,
        instance: args.instance,
        config: args.config,
        timeout: args.timeout,
        ascii: false,
        no_mouse: false,
        scripted: false,
        log_file: "gui.log",
        no_start: args.no_start,
    })
    .await
}

/// `loquax` (or `loquax tui`): parse what the shell needs, then hand over.
#[cfg(feature = "tui")]
async fn run_tui(args: TuiArgs) -> ExitCode {
    let mut to = Vec::new();
    for text in &args.to {
        match address::parse(text) {
            Ok(peer) => to.push(peer),
            Err(error) => return usage(format!("--to {}: {error}", text.trim())),
        }
    }
    let size = args
        .size
        .split_once('x')
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .filter(|&(w, h): &(u16, u16)| w >= 20 && h >= 8);
    let Some(size) = size else {
        return usage(format!("--size {}: want WxH, at least 20x8", args.size));
    };
    if args.script.is_none() {
        first_start_import();
    }
    let sync_interval = match args
        .sync_interval
        .as_deref()
        .map(loquax::sync::parse_interval)
    {
        None => None,
        Some(Ok(bound)) => Some(bound),
        Some(Err(error)) => return usage(format!("--sync-interval {error}")),
    };
    tui::run(tui::TuiOptions {
        sync_interval,
        to,
        pn: args.pn,
        instance: args.instance,
        config: args.config,
        timeout: args.timeout,
        ascii: args.ascii,
        no_color: args.no_color,
        color: args.color.into(),
        theme: args.theme.map(Into::into),
        no_mouse: args.no_mouse,
        graphics: args.graphics,
        script: args.script,
        frames: args.frames,
        size,
        no_start: args.no_start,
    })
    .await
}

/// `loquax import`: find, list, ask, import.
#[cfg(feature = "history")]
fn run_import(args: ImportArgs) -> ExitCode {
    use loquax::import;
    let home = match identity::home_path() {
        Ok(home) => home,
        Err(error) => return failure(error, "no-home"),
    };
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    if args.ratchets.is_some() && args.path.is_none() && !args.key {
        return import_ratchets(&home, args.ratchets.as_deref(), args.dry_run);
    }
    let found = if args.key {
        let mut line = String::new();
        if let Err(error) = std::io::BufRead::read_line(&mut input, &mut line) {
            return failure(format!("stdin: {error}"), "import-failed");
        }
        match import::key::from_base32(&line) {
            Some(key) => vec![import::pasted("pasted", Path::new("(stdin)"), key)],
            None => return usage("--key: the first line of stdin is not a Base32 identity key"),
        }
    } else if let Some(path) = &args.path {
        match import::sniff(path) {
            Ok(found) => vec![found],
            Err(error) => return failure(error, "import-failed"),
        }
    } else {
        let Some(user_home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return failure("HOME is not set, so there is nowhere to look", "no-home");
        };
        import::scan(&user_home, true)
    };
    if found.is_empty() {
        println!("nothing to import found");
        return ExitCode::SUCCESS;
    }
    for source in &found {
        println!("{}", source.line());
    }
    if args.dry_run {
        let mut failed = false;
        for source in found.iter().filter(|f| f.importable()) {
            let choices = import::Choices {
                contacts: true,
                history: true,
                identity: false,
            };
            println!("\n{}", source.path.display());
            match import::import(source, &home, choices, true) {
                Ok(report) => report.lines(true).iter().for_each(|l| println!("{l}")),
                Err(error) => {
                    failed = true;
                    eprintln!("loquax: {error}");
                }
            }
            if source.address.is_some() && source.address != import::current_address(&home) {
                println!("identity: offered, adopted only on a typed yes");
            }
        }
        if failed {
            return ExitCode::from(EXIT_FAILURE);
        }
        return import_ratchets(&home, args.ratchets.as_deref(), true);
    }
    let mut output = std::io::stdout();
    match import::ask_and_import(&found, &home, &mut input, &mut output) {
        Ok(reports) if reports.iter().all(Result::is_ok) => {
            import_ratchets(&home, args.ratchets.as_deref(), false)
        }
        Ok(_) => ExitCode::from(EXIT_FAILURE),
        Err(error) => failure(error, "import-failed"),
    }
}

#[cfg(feature = "history")]
/// `loquax import --ratchets FILE`: the ratchets of the app the identity came
/// from, for the identity Loquax answers on now. Nothing to do without one.
fn import_ratchets(home: &Path, file: Option<&Path>, dry_run: bool) -> ExitCode {
    let Some(file) = file else {
        return ExitCode::SUCCESS;
    };
    match loquax::home::client::load_client(home) {
        Ok(Some(_)) => {
            return failure(
                "this Loquax is a client of a home node and answers on its address; \
                 ratchets are imported on the home node",
                "import-failed",
            )
        }
        Ok(None) => {}
        Err(error) => return failure(error, "no-home"),
    }
    match loquax::import::take_ratchets(home, file, dry_run) {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(error) => failure(error, "import-failed"),
    }
}

/// `loquax export <dir>`.
#[cfg(feature = "history")]
fn run_export(dir: &Path) -> ExitCode {
    let home = match identity::home_path() {
        Ok(home) => home,
        Err(error) => return failure(error, "no-home"),
    };
    match loquax::export::export(&home, dir) {
        Ok(done) => {
            println!(
                "exported <{}>: {} messages in {} conversations, {} contacts, and the identity \
                 (private, mode 0600) to {}",
                address::to_hex(&done.address),
                done.messages,
                done.conversations,
                done.contacts,
                dir.display()
            );
            ExitCode::SUCCESS
        }
        Err(error) => failure(error, "export-failed"),
    }
}

/// On the very first start, before an identity is minted: look in the
/// apps' default places and, when something is there, ask on the terminal
/// before the screen opens. Nothing found is no question at all.
#[cfg(feature = "tui")]
fn first_start_import() {
    use std::io::IsTerminal;
    let Ok(home) = identity::home_path() else {
        return;
    };
    if !loquax::import::first_start(&home) || !std::io::stdin().is_terminal() {
        return;
    }
    let Some(user_home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let found = loquax::import::scan(&user_home, true);
    if !found.iter().any(|f| f.importable()) {
        return;
    }
    println!("Loquax starts for the first time here, and found another messenger's data:");
    for source in &found {
        println!("{}", source.line());
    }
    println!("(Enter takes the default; nothing is written without an answer.)");
    let stdin = std::io::stdin();
    let mut output = std::io::stdout();
    if let Err(error) =
        loquax::import::ask_and_import(&found, &home, &mut stdin.lock(), &mut output)
    {
        eprintln!("loquax: import: {error}");
    }
}

/// `loquax address`: print our own LXMF address. No daemon involved — the
/// address is a pure function of the persistent identity, minted on first
/// use, so this is also how a fresh machine learns (and creates) the address
/// a correspondent should write down.
fn run_address() -> ExitCode {
    use std::io::IsTerminal;
    let home = match identity::home_dir() {
        Ok(home) => home,
        Err(error) => return failure(error, "no-home"),
    };
    // A client's address is its home node's.
    let public_key = match loquax::home::client::load_client(&home) {
        Ok(Some(public_key)) => public_key,
        Ok(None) => match identity::load_or_create(&home.join("identity")) {
            Ok(identity) => identity.public_key_bytes(),
            Err(error) => return failure(error, "no-identity"),
        },
        Err(error) => return failure(error, "no-home"),
    };
    match address_lines(&public_key, std::io::stdout().is_terminal()) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => failure(error, "no-identity"),
    }
}

/// What `loquax address` prints: the address alone for a script, which
/// reads the one line as before; at a terminal the contact code under it,
/// the text a friend pastes to add us (1279, walk finding 23).
fn address_lines(
    public_key: &[u8; loquax::lxma::KEY_BYTES],
    terminal: bool,
) -> Result<Vec<String>, String> {
    let address = loquax::home::lxmf_address(public_key)?;
    let mut lines = vec![address::to_hex(&address)];
    if terminal {
        lines.push(loquax::lxma::encode(public_key)?);
    }
    Ok(lines)
}

async fn run(args: SendArgs) -> ExitCode {
    let destination = match address::parse(&args.address) {
        Ok(destination) => destination,
        Err(error) => return usage(format!("{}: {error}", args.address.trim())),
    };
    // Before the body is read: an unusable name is an argument error, and a
    // script piping into us should hear about it without first having its
    // stdin consumed.
    let display_name = match display_name::from_process(args.from.as_deref()) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };
    events::sender(&display_name.name, display_name.source.as_str());
    let fields = match args.position.as_deref().map(position_fields).transpose() {
        Ok(fields) => fields,
        Err(error) => return usage(error),
    };
    #[cfg(feature = "photos")]
    let photo = match args.photo.as_deref() {
        None => None,
        Some(path) => match send_photo(path, args.photo_size.as_deref()) {
            Ok(fields) => Some(fields),
            Err(error) => return usage(error),
        },
    };
    #[cfg(not(feature = "photos"))]
    let photo: Option<Vec<leviculum_lxmf::Field>> = None;
    let file = match args.file.as_deref() {
        None => None,
        Some(path) => match send_file(path) {
            Ok(fields) => Some(fields),
            Err(error) => return usage(error),
        },
    };
    let fields = match (fields, photo, file) {
        (None, None, None) => None,
        (position, photo, file) => Some(
            position
                .into_iter()
                .chain(photo)
                .chain(file)
                .flatten()
                .collect::<Vec<_>>(),
        ),
    };
    // A report, a photo or a file has no body, and reads none from stdin.
    let body = match (&fields, args.body.as_deref()) {
        (Some(_), None) => Vec::new(),
        (_, body) => match body::resolve(body, &mut std::io::stdin().lock()) {
            Ok(body) => body,
            Err(error) => return usage(error),
        },
    };
    let fields = fields.unwrap_or_default();
    let title = args.title.unwrap_or_default().into_bytes();

    let home = match identity::home_dir() {
        Ok(home) => home,
        Err(error) => return failure(error, "no-home"),
    };
    match loquax::home::client::load_client(&home) {
        Ok(None) => {}
        Ok(Some(public_key)) => {
            let message = ClientMessage {
                destination,
                title,
                body,
                fields,
            };
            let reach = (args.instance, args.config, args.timeout);
            return run_client_send(home, public_key, message, reach).await;
        }
        Err(error) => return failure(error, "no-home"),
    }
    // The send registers the delivery destination for its run: never
    // beside the home node that answers for the same identity.
    let _lock = match loquax::home::lock::DeliveryLock::share(&home) {
        Ok(lock) => lock,
        Err(error) => return failure(error, "locked"),
    };
    // Resolved before attaching for the same reason the display name is:
    // a bad --pn or a corrupt config line is an argument-shaped problem and
    // should not cost a daemon connection first.
    let (preferred_pn, pn_source) = match resolve_pn(args.pn.as_deref(), &home) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };
    let identity = match identity::load_or_create(&home.join("identity")) {
        Ok(identity) => identity,
        Err(error) => return failure(error, "no-identity"),
    };

    let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
    let instance = args.instance.unwrap_or_else(|| instance_name(&config_dir));

    let storage_dir = match storage_dir(&home) {
        Ok(dir) => dir,
        Err(error) => return failure(error, "no-daemon"),
    };
    let mut attached = match attach(AttachConfig {
        instance: instance.clone(),
        config_dir: Some(config_dir.clone()),
        storage_dir,
        identity,
        display_name: display_name.name.into_bytes(),
        notices: Terminal::new(home.clone()),
    })
    .await
    {
        Ok(attached) => attached,
        Err(error) => return no_daemon(&error),
    };

    let mut options = SendOptions::new(instance.clone(), Duration::from_secs(args.timeout));
    let mut effects = Terminal::new(home.clone());
    let clock = Tokio::new();
    // `--via auto` reads the path table only if its direct leg fails.
    if args.via == ViaArg::Propagated {
        options.mailbox =
            mailbox_run(&home, &config_dir, &instance, preferred_pn, Vec::new()).await;
    }
    let request = |via: Via| SendRequest {
        destination,
        title: title.clone(),
        body: body.clone(),
        fields: fields.clone(),
        via,
    };

    let code = match args.via {
        ViaArg::Direct => {
            let outcome =
                run_send(&mut attached.outbox, request(Via::Direct), &options, &clock).await;
            match outcome {
                Ok(queued) => {
                    events::done(Some(&queued.message_id), "queued", 0);
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("loquax: {error}");
                    events::done(None, "failed", EXIT_FAILURE);
                    ExitCode::from(EXIT_FAILURE)
                }
            }
        }
        ViaArg::Propagated => {
            let outcome = run_send_propagated(
                &mut attached.outbox,
                request(Via::Propagated),
                preferred_pn,
                pn_source,
                None,
                &options,
                &mut effects,
                &clock,
            )
            .await;
            match outcome {
                Ok(queued) => {
                    events::via("propagated", "requested");
                    events::done(Some(&queued.message_id), "propagated", EXIT_PROPAGATED);
                    ExitCode::from(EXIT_PROPAGATED)
                }
                Err(error) => {
                    eprintln!("loquax: {error}");
                    events::done(None, "failed", EXIT_FAILURE);
                    ExitCode::from(EXIT_FAILURE)
                }
            }
        }
        ViaArg::Auto => {
            let outcome = run_send_auto(
                &mut attached.outbox,
                request(Via::Link),
                preferred_pn,
                pn_source,
                &mut options,
                || mailbox_run(&home, &config_dir, &instance, preferred_pn, Vec::new()),
                &mut effects,
                &clock,
            )
            .await;
            match outcome {
                Ok(AutoSent::Direct(queued)) => {
                    events::done(Some(&queued.message_id), "queued", 0);
                    ExitCode::SUCCESS
                }
                Ok(AutoSent::Propagated(queued)) => {
                    events::done(Some(&queued.message_id), "propagated", EXIT_PROPAGATED);
                    ExitCode::from(EXIT_PROPAGATED)
                }
                Err(AutoError::Direct(direct_error)) => {
                    eprintln!("loquax: {direct_error}");
                    events::done(None, "failed", EXIT_FAILURE);
                    ExitCode::from(EXIT_FAILURE)
                }
                Err(AutoError::Both { direct, fallback }) => {
                    eprintln!("loquax: direct: {direct}\nloquax: propagated: {fallback}");
                    events::done(None, "failed", EXIT_FAILURE);
                    ExitCode::from(EXIT_FAILURE)
                }
            }
        }
    };
    attached.stop().await;
    code
}

async fn run_fetch_command(args: FetchArgs) -> ExitCode {
    let home = match identity::home_dir() {
        Ok(home) => home,
        Err(error) => return failure(error, "no-home"),
    };
    match loquax::home::client::load_client(&home) {
        Ok(None) => {}
        Ok(Some(public_key)) => return run_client_fetch(home, public_key, args).await,
        Err(error) => return failure(error, "no-home"),
    }
    // As `loquax send`: the home node collects for its own identity.
    let _lock = match loquax::home::lock::DeliveryLock::share(&home) {
        Ok(lock) => lock,
        Err(error) => return failure(error, "locked"),
    };
    let (preferred_pn, pn_source) = match resolve_pn(args.pn.as_deref(), &home) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };
    let identity = match identity::load_or_create(&home.join("identity")) {
        Ok(identity) => identity,
        Err(error) => return failure(error, "no-identity"),
    };
    let mut seen = match SeenStore::load(&home) {
        Ok(seen) => seen,
        Err(error) => return failure(format!("seen store: {error}"), "no-home"),
    };

    let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
    let instance = args.instance.unwrap_or_else(|| instance_name(&config_dir));

    // The display name travels in the delivery announce a fetch also sends;
    // there is no --from here because a fetch is not authored.
    let display_name = match display_name::from_process(None) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };

    let storage_dir = match storage_dir(&home) {
        Ok(dir) => dir,
        Err(error) => return failure(error, "no-daemon"),
    };
    let mut attached = match attach(AttachConfig {
        instance: instance.clone(),
        config_dir: Some(config_dir.clone()),
        storage_dir,
        identity,
        display_name: display_name.name.into_bytes(),
        notices: Terminal::new(home.clone()),
    })
    .await
    {
        Ok(attached) => attached,
        Err(error) => return no_daemon(&error),
    };

    let mut options = FetchOptions::new(instance.clone(), Duration::from_secs(args.timeout));
    let mut effects = Terminal::new(home.clone());
    options.mailbox = mailbox_run(&home, &config_dir, &instance, preferred_pn, Vec::new()).await;
    // `Terminal::print` deliberately does NOT hold `stdout().lock()` across
    // the run: the engine's driver thread logs through tracing, and with
    // `RUST_LOG=debug` (how the conformance rig runs every tool) the fmt
    // subscriber writes to this same stdout. A lock held across the whole
    // fetch deadlocked the first engine tick against it for the entire run.
    // `run_fetch` renders each message into one buffer and hands it over as
    // a single write, so output stays whole without anyone holding the
    // stream.
    let outcome = run_fetch(
        &mut attached.outbox,
        preferred_pn,
        pn_source,
        &options,
        &mut effects,
        &mut seen,
        &Tokio::new(),
    )
    .await;

    let code = match outcome {
        Ok(fetched) => {
            events::done(None, if fetched.new > 0 { "fetched" } else { "empty" }, 0);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("loquax: {error}");
            match error {
                FetchError::NoPath(attempts) => {
                    events::done_without_path(EXIT_FAILURE, attempts.requests, attempts.waited_ms)
                }
                _ => events::done(None, "failed", EXIT_FAILURE),
            }
            ExitCode::from(EXIT_FAILURE)
        }
    };
    attached.stop().await;
    code
}

/// One message as `loquax send` read it, for a client to hand its home node.
struct ClientMessage {
    destination: [u8; 16],
    title: Vec<u8>,
    body: Vec<u8>,
    fields: Vec<leviculum_lxmf::Field>,
}

/// `--file`'s name and bytes as the file attachments field, refused past
/// [`loquax::ui::files::LARGEST`] with both sizes said.
fn send_file(path: &std::path::Path) -> Result<Vec<leviculum_lxmf::Field>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("--file {}: {e}", path.display()))?;
    if let Some(why) = loquax::ui::files::refusal(bytes.len()) {
        return Err(format!(
            "--file {}: {}",
            path.display(),
            why.resolve(loquax::ui::strings::Lang::En)
        ));
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(loquax::ui::files::file_fields(&name, bytes))
}

/// `--photo`'s picture prepared at `size`, else at the config's
/// `photo_size`, else at `phone` ([`loquax::photo::prepare`]), as the
/// image field.
#[cfg(feature = "photos")]
fn send_photo(
    path: &std::path::Path,
    size: Option<&str>,
) -> Result<Vec<leviculum_lxmf::Field>, String> {
    use loquax::ui::photos::PhotoSize;
    let size = match size {
        Some(word) => PhotoSize::from_word(word)
            .ok_or_else(|| format!("--photo-size {word}: want small, phone or original"))?,
        None => identity::home_dir()
            .ok()
            .and_then(|home| loquax::config::load(&home).ok())
            .and_then(|config| config.photo_size)
            .unwrap_or_default(),
    };
    let bytes = std::fs::read(path).map_err(|e| format!("--photo {}: {e}", path.display()))?;
    let prepared = loquax::photo::prepare(&bytes, size)
        .map_err(|e| format!("--photo {}: {e}", path.display()))?;
    Ok(loquax::ui::photos::photo_fields(
        prepared.format,
        prepared.data,
    ))
}

/// `--position`'s `LAT,LON` in degrees as the fields of a position report
/// timed now ([`loquax::ui::position_fields`]).
fn position_fields(text: &str) -> Result<Vec<leviculum_lxmf::Field>, String> {
    position_location(text).map(loquax::ui::position_fields)
}

/// `--position`'s `LAT,LON` in degrees as a reading timed now.
fn position_location(text: &str) -> Result<leviculum_lxmf::telemetry::Location, String> {
    let bad = || format!("--position {text}: expected LAT,LON in degrees, as 52.5163,13.3777");
    let (lat, lon) = text.split_once(',').ok_or_else(bad)?;
    let degrees = |part: &str, limit: f64| {
        part.trim()
            .parse::<f64>()
            .ok()
            .filter(|d| d.is_finite() && d.abs() <= limit)
            .map(|d| (d * 1e6).round() as i32)
            .ok_or_else(bad)
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    Ok(leviculum_lxmf::telemetry::Location {
        latitude_e6: degrees(lat, 90.0)?,
        longitude_e6: degrees(lon, 180.0)?,
        altitude_e2: 0,
        speed_e2: 0,
        bearing_e2: 0,
        accuracy_e2: 0,
        last_update: now,
    })
}

/// `loquax announce`: attach as `send` does, announce the position once on
/// `lev.position` (1302), and wait until the engine took it, then a moment
/// for the driver to put it on the wire.
async fn run_announce(args: AnnounceArgs) -> ExitCode {
    let location = match position_location(&args.position) {
        Ok(location) => location,
        Err(error) => return usage(error),
    };
    let display_name = match display_name::from_process(args.from.as_deref()) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };
    // Plain and named, so every node in range shows it (1313).
    let position = loquax::mesh_position::manual(&location, Some(display_name.name.clone()));
    let home = match identity::home_dir() {
        Ok(home) => home,
        Err(error) => return failure(error, "no-home"),
    };
    // The announce registers our destinations for its run, as a send does.
    let _lock = match loquax::home::lock::DeliveryLock::share(&home) {
        Ok(lock) => lock,
        Err(error) => return failure(error, "locked"),
    };
    let identity = match identity::load_or_create(&home.join("identity")) {
        Ok(identity) => identity,
        Err(error) => return failure(error, "no-identity"),
    };
    let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
    let instance = args.instance.unwrap_or_else(|| instance_name(&config_dir));
    let storage_dir = match storage_dir(&home) {
        Ok(dir) => dir,
        Err(error) => return failure(error, "no-daemon"),
    };
    let mut attached = match attach(AttachConfig {
        instance,
        config_dir: Some(config_dir),
        storage_dir,
        identity,
        display_name: display_name.name.into_bytes(),
        notices: Terminal::new(home.clone()),
    })
    .await
    {
        Ok(attached) => attached,
        Err(error) => return no_daemon(&error),
    };
    use loquax::outbox::{Command, Outbox, OutboxEvent};
    const TOKEN: u64 = 1302;
    let submitted = attached
        .outbox
        .submit(Command::AnnouncePosition { position })
        .and_then(|()| attached.outbox.submit(Command::Fence { token: TOKEN }));
    if let Err(gone) = submitted {
        eprintln!("loquax: {gone}");
        return ExitCode::from(EXIT_FAILURE);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(args.timeout);
    loop {
        match attached.outbox.try_next_event() {
            Ok(Some(OutboxEvent::Fenced { token: TOKEN })) => break,
            Ok(Some(_)) => continue,
            Ok(None) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok(None) => {
                eprintln!(
                    "loquax: the engine did not take the announce within {}s",
                    args.timeout
                );
                return ExitCode::from(EXIT_FAILURE);
            }
            Err(gone) => {
                eprintln!("loquax: {gone}");
                return ExitCode::from(EXIT_FAILURE);
            }
        }
    }
    // The packet left the engine with the fence; the driver writes it to
    // the daemon's socket on its own task.
    tokio::time::sleep(Duration::from_millis(500)).await;
    println!("announced {}", args.position.trim());
    ExitCode::SUCCESS
}

/// `loquax send` on a client of a home node: hand the message to the home
/// node with `/send`, which signs it, keeps it in its outbox and delivers it
/// (`docs/src/concepts/loquax-home-node.md`). Done once the home node
/// answered; `--via` and `--pn` are the home node's business, not read
/// here. Then one pull, as `loquax fetch` runs it: the message's `accepted`
/// line, and whatever else came, is printed now rather than at the next
/// fetch, since the device has one cursor and what a pull took is shown or
/// lost. A pull that fails is said and changes nothing: the home node has
/// the message.
async fn run_client_send(
    home: PathBuf,
    public_key: [u8; 64],
    message: ClientMessage,
    (instance, config, timeout): (Option<String>, Option<PathBuf>, u64),
) -> ExitCode {
    let address = match loquax::home::lxmf_address(&public_key) {
        Ok(address) => address,
        Err(error) => return failure(error, "no-identity"),
    };
    let device =
        match identity::load_or_create(&home.join(loquax::home::client::DEVICE_IDENTITY_FILE)) {
            Ok(identity) => identity,
            Err(error) => return failure(error, "no-identity"),
        };
    let config_dir = config.unwrap_or_else(Config::default_config_dir);
    let instance = instance.unwrap_or_else(|| instance_name(&config_dir));
    let storage_dir = match storage_dir(&home) {
        Ok(dir) => dir,
        Err(error) => return failure(error, "no-daemon"),
    };
    let mut seen = match SeenStore::load(&home) {
        Ok(seen) => seen,
        Err(error) => return failure(format!("seen store: {error}"), "no-home"),
    };
    let mut attached = match loquax::engine::attach_as(
        AttachConfig {
            instance,
            config_dir: Some(config_dir.clone()),
            storage_dir,
            identity: device.clone(),
            display_name: Vec::new(),
            notices: Terminal::new(home.clone()),
        },
        loquax::engine::Role::Client { address },
    )
    .await
    {
        Ok(attached) => attached,
        Err(error) => return no_daemon(&error),
    };
    let node = attached.node();
    let bytes = message.body.len();
    let submission = loquax::home::wire::Submission {
        client_id: loquax::home::client::client_id(),
        destination: message.destination,
        title: message.title,
        content: message.body,
        fields: message.fields,
    };
    let accepted = loquax::home::client::send(
        &node,
        &public_key,
        &device,
        &submission,
        Duration::from_secs(timeout),
    )
    .await;
    if accepted.is_ok() {
        let mut effects = Terminal::new(home.clone());
        if let Err(error) = loquax::home::client::fetch(
            &mut attached.outbox,
            &node,
            &home,
            &public_key,
            &device,
            Duration::from_secs(timeout),
            &mut effects,
            &mut seen,
        )
        .await
        {
            eprintln!("loquax: the mailbox took the message; the pull after it: {error}");
        }
    }
    drop(node);
    attached.stop().await;
    match accepted {
        Ok(id) => {
            events::enqueued(&id, &message.destination, bytes, "home");
            events::done(Some(&id), "home", 0);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("loquax: the mailbox did not take the message: {error}");
            events::done(None, "failed", EXIT_FAILURE);
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// `loquax mailbox run` and `loquax --daemon`: Loquax Mailbox.
#[cfg(feature = "tui")]
async fn run_daemon(args: RunArgs) -> ExitCode {
    // The journal under systemd: loquax's own lines (archived, admitted,
    // refused) and every warning. `RUST_LOG` replaces the filter.
    leviculum_std::event_log::install_global_subscriber("warn,loquax=info");
    let home = match identity::home_dir() {
        Ok(home) => home,
        Err(error) => return failure(error, "no-home"),
    };
    match loquax::home::client::load_client(&home) {
        Ok(None) => {}
        Ok(Some(_)) => {
            return failure(
                "this device is a client of a Loquax Mailbox, so it cannot be one; \
                 the mailbox runs where the identity is",
                "client",
            )
        }
        Err(error) => return failure(error, "no-home"),
    }
    let (preferred_pn, pn_source) = match resolve_pn(args.pn.as_deref(), &home) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };
    let config = match loquax::config::load(&home) {
        Ok(config) => config,
        Err(error) => return usage(error.to_string()),
    };
    let sync_bound_ms = match args
        .sync_interval
        .as_deref()
        .map(loquax::sync::parse_interval)
    {
        None => config
            .sync_interval
            .unwrap_or(Some(loquax::sync::DEFAULT_BOUND_MS)),
        Some(Ok(bound)) => bound,
        Some(Err(error)) => return usage(format!("--sync-interval {error}")),
    };
    // A mailbox with no identity yet is a new install (1176).
    let first_start = !home.join("identity").exists();
    let identity = match identity::load_or_create(&home.join("identity")) {
        Ok(identity) => identity,
        Err(error) => return failure(error, "no-identity"),
    };
    let display_name = match display_name::from_process(None) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };
    let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
    let instance = args.instance.unwrap_or_else(|| instance_name(&config_dir));
    // Beside the daemon or on a node of its own (1354): the config's
    // `mailbox_upstream`, else whether a daemon answers on its shared
    // instance, where its own config says it listens.
    let (way, why) = loquax::home::upstream::decide(&config, || {
        let reticulum = leviculum_std::daemon_rpc::load_config(&config_dir);
        reticulum
            .map(|c| leviculum_std::interfaces::SharedInstanceTransport::from_config(&c.reticulum))
            .unwrap_or_default()
            .answers(&instance)
    });
    // The phones' port, in access point mode, on a new install beside a
    // daemon only: a config that already runs is never rewritten (1176).
    // A mailbox on its own node serves the phones itself ([`own_screens`]).
    if first_start && way == loquax::home::upstream::Way::Shared {
        let port = loquax::home::phone_server::PORT;
        let added = loquax::home::phone_server::add(&config_dir, port);
        println!(
            "{}",
            loquax::home::phone_server::first_start_lines(&added, port)
        );
    }
    let storage_dir = match storage_dir(&home) {
        Ok(dir) => dir,
        Err(error) => return failure(error, "no-daemon"),
    };
    let archive_cap_mb = config
        .archive_cap_mb
        .unwrap_or(loquax::home::DEFAULT_ARCHIVE_CAP_MB);
    let listed = |list: &[String]| match list.is_empty() {
        true => "none".to_string(),
        false => list.join(","),
    };
    let upstream = match way {
        loquax::home::upstream::Way::Shared => {
            let mailbox: loquax::home::daemon::MailboxSource = {
                let (home, config_dir, instance) =
                    (home.clone(), config_dir.clone(), instance.clone());
                Box::new(move || {
                    let (home, config_dir, instance) =
                        (home.clone(), config_dir.clone(), instance.clone());
                    Box::pin(async move {
                        mailbox_run(&home, &config_dir, &instance, preferred_pn, Vec::new()).await
                    })
                })
            };
            // The rendezvous the phones meet the mailbox on (1196): dialled
            // from the mailbox's own node, one at a time, as a peer.
            let (rendezvous, from) = loquax::home::rendezvous::mailbox_list(&config);
            tracing::warn!("HOME upstream shared instance={instance} why={why}");
            tracing::warn!("HOME rendezvous list={} from={from}", listed(&rendezvous));
            loquax::home::daemon::DaemonUpstream::Shared {
                instance,
                config_dir,
                mailbox,
                rendezvous,
                tcp: config.tcp,
            }
        }
        loquax::home::upstream::Way::Mesh { servers, from } => {
            let screens = match own_screens() {
                Ok(listener) => listener,
                Err(error) => return failure(error, "no-port"),
            };
            tracing::warn!(
                "HOME upstream mesh servers={} from={from} why={why} phones={}",
                listed(&servers),
                screens
                    .local_addr()
                    .map_or_else(|e| e.to_string(), |a| a.to_string())
            );
            // A peer of the entry servers, as the rendezvous dial beside a
            // daemon is: their keepalive, not a phone's long idle.
            loquax::home::daemon::DaemonUpstream::Mesh {
                servers,
                screens,
                tcp: config.tcp,
                timing: loquax::home::rendezvous::Timing::default(),
            }
        }
    };
    let outcome = loquax::home::daemon::run(loquax::home::daemon::DaemonOptions {
        home,
        upstream,
        storage_dir,
        identity,
        display_name: display_name.name.into_bytes(),
        archive_cap_bytes: archive_cap_mb.saturating_mul(1 << 20),
        token_ttl_secs: loquax::home::token_ttl_secs(&config),
        policy: loquax::home::policy(&config),
        preferred_pn,
        pn_source,
        sync_bound_ms,
        fetch_budget: Duration::from_secs(args.timeout.saturating_mul(2)),
        wake: config.home_wake.unwrap_or(true),
        stop: None,
        // A desktop or server mailbox has no platform that says a network
        // change; its connection's keepalive (1176) finds a dead one.
        network: Default::default(),
        lends: Vec::new(),
    })
    .await;
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => failure(error, "failed"),
    }
}

/// `loquax mailbox ...` (also `loquax home ...`).
#[cfg(feature = "tui")]
async fn run_mailbox(command: MailboxCommand) -> ExitCode {
    match command {
        MailboxCommand::Run(args) => return run_daemon(args).await,
        MailboxCommand::Setup(args) => return Box::pin(run_setup(args)).await,
        // Through systemctl, before anything is made under this user's home.
        MailboxCommand::Enable => return systemctl(&["enable", "--now", MAILBOX_UNIT]),
        MailboxCommand::Disable => return systemctl(&["disable", "--now", MAILBOX_UNIT]),
        MailboxCommand::Admit { .. }
        | MailboxCommand::Join(_)
        | MailboxCommand::Devices
        | MailboxCommand::Revoke { .. }
        | MailboxCommand::Tokens
        | MailboxCommand::RevokeToken { .. }
        | MailboxCommand::Archive
        | MailboxCommand::Ratchets => {}
    }
    let home = match identity::home_dir() {
        Ok(home) => home,
        Err(error) => return failure(error, "no-home"),
    };
    let client = match loquax::home::client::load_client(&home) {
        Ok(client) => client,
        Err(error) => return failure(error, "no-home"),
    };
    let on_home_node = |store: &dyn Fn(&mut loquax::store::Store) -> ExitCode| {
        if client.is_some() {
            return failure(
                "this device is a client of a Loquax Mailbox; run this on the mailbox",
                "client",
            );
        }
        let identity = match identity::load_or_create(&home.join("identity")) {
            Ok(identity) => identity,
            Err(error) => return failure(error, "no-identity"),
        };
        let address = match loquax::engine::delivery_address(&identity) {
            Ok(address) => address,
            Err(error) => return failure(error, "no-identity"),
        };
        match loquax::store::Store::open_in(&home, address) {
            Ok(mut opened) => store(&mut opened),
            Err(error) => failure(error, "no-store"),
        }
    };
    let now = leviculum_std::local_time::unix_now_secs();
    match command {
        MailboxCommand::Admit {
            name,
            server,
            code_only,
        } => on_home_node(&|store| {
            let name = match &name {
                Some(name) => name.trim().to_string(),
                None => match store.devices() {
                    Ok(devices) => format!("device-{}", devices.len() + 1),
                    Err(error) => return failure(error, "no-store"),
                },
            };
            if name.is_empty() {
                return usage("--name may not be empty");
            }
            let config = match loquax::config::load(&home) {
                Ok(config) => config,
                Err(error) => return usage(error.to_string()),
            };
            // Without --server, the fixed address `mailbox setup` kept
            // (`home_address`, 1266) is the one the codes carry.
            let server = server
                .as_deref()
                .or(config.home_address.as_deref())
                .map(str::trim);
            if let Some(server) = server {
                if let Err(error) = loquax::engine::parse_server(server) {
                    return usage(format!("--server: {error}"));
                }
            }
            let public_key = match identity::load_or_create(&home.join("identity")) {
                Ok(identity) => identity.public_key_bytes(),
                Err(error) => return failure(error, "no-identity"),
            };
            // The rendezvous list the phone tries in order (1196): this
            // mailbox's own phone port first when --server names it, then
            // the list the mailbox dials itself, on the phones' ports.
            let list = loquax::home::admit::computer_list(&config, server);
            // The phone's "Link a device" opens its admissions the same way
            // (1264).
            let admission = match loquax::home::admit::open(store, public_key, &name, list, now) {
                Ok(admission) => admission,
                Err(error) => return failure(error, "no-store"),
            };
            let code = &admission.code;
            let text = admission.text();
            let qr = loquax::home::qr::lines(&text);
            if code_only {
                println!("{text}\n");
                match qr {
                    Ok(lines) => println!("{}", lines.join("\n")),
                    Err(error) => eprintln!("loquax: no QR code: {error}"),
                }
                return ExitCode::SUCCESS;
            }
            println!("Admission code for {name:?}, good for one device, once:\n\n{text}\n");
            match qr {
                Ok(lines) => println!("{}\n", lines.join("\n")),
                Err(error) => eprintln!("loquax: no QR code: {error}"),
            }
            match server.is_some() || !admission.list.is_empty() {
                true => println!(
                    "On a phone, in Loquax: \"On a computer at home\", then \"Scan the code\"; \
                     or paste it into the join screen."
                ),
                false => println!(
                    "On a phone: paste it, with this node's address; --server puts that in the \
                     code."
                ),
            }
            println!("On a desktop: loquax mailbox join {}", code.encode());
            ExitCode::SUCCESS
        }),
        MailboxCommand::Archive => on_home_node(&|store| match store.archive_index() {
            Ok(entries) => {
                for (id, kind, at) in entries {
                    println!(
                        "{} {} {at}",
                        address::to_hex(&id),
                        loquax::home::kind_word(kind)
                    );
                }
                ExitCode::SUCCESS
            }
            Err(error) => failure(error, "no-store"),
        }),
        MailboxCommand::Ratchets => on_home_node(&|_| {
            let identity = match identity::load_or_create(&home.join("identity")) {
                Ok(identity) => identity,
                Err(error) => return failure(error, "no-identity"),
            };
            match loquax::import::ratchets::ring_hash(&home, &identity) {
                Ok(Some((count, hash))) => {
                    println!("ratchets={count} ring={}", address::to_hex(&hash));
                    ExitCode::SUCCESS
                }
                Ok(None) => {
                    println!("ratchets=0");
                    ExitCode::SUCCESS
                }
                Err(error) => failure(error, "failed"),
            }
        }),
        MailboxCommand::Devices => on_home_node(&|store| match store.devices() {
            Ok(devices) => {
                for device in devices {
                    let at = loquax::when::WallTime {
                        unix_secs: device.admitted_at,
                        offset_secs: leviculum_std::local_time::utc_offset_secs(device.admitted_at),
                    }
                    .civil();
                    println!(
                        "{}  {}  admitted {:04}-{:02}-{:02} {:02}:{:02}",
                        address::to_hex(&device.device),
                        device.name,
                        at.year,
                        at.month,
                        at.day,
                        at.hour,
                        at.minute
                    );
                }
                ExitCode::SUCCESS
            }
            Err(error) => failure(error, "no-store"),
        }),
        MailboxCommand::Revoke { device } => on_home_node(&|store| match store.revoke(&device) {
            Ok(loquax::store::Revoked::One(gone)) => {
                println!("revoked {}  {}", address::to_hex(&gone.device), gone.name);
                ExitCode::SUCCESS
            }
            Ok(loquax::store::Revoked::None) => {
                failure(format!("no admitted device is called {device:?}"), "failed")
            }
            Ok(loquax::store::Revoked::Ambiguous(matched)) => {
                let names: Vec<String> = matched
                    .iter()
                    .map(|d| format!("{} {}", address::to_hex(&d.device), d.name))
                    .collect();
                usage(format!(
                    "{device:?} names {} devices ({}); give the hash",
                    matched.len(),
                    names.join(", ")
                ))
            }
            Err(error) => failure(error, "no-store"),
        }),
        MailboxCommand::Tokens => {
            let ttl = match loquax::config::load(&home) {
                Ok(config) => loquax::home::token_ttl_secs(&config),
                Err(error) => return usage(error),
            };
            on_home_node(&|store| match store.open_tokens(now, ttl) {
                Ok(tokens) => {
                    for token in tokens {
                        let expires = token.created_at.saturating_add(ttl);
                        let at = loquax::when::WallTime {
                            unix_secs: expires,
                            offset_secs: leviculum_std::local_time::utc_offset_secs(expires),
                        }
                        .civil();
                        println!(
                            "{}  {}  expires {:04}-{:02}-{:02} {:02}:{:02}",
                            address::to_hex(&token.token[..4]),
                            token.name,
                            at.year,
                            at.month,
                            at.day,
                            at.hour,
                            at.minute
                        );
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => failure(error, "no-store"),
            })
        }
        MailboxCommand::RevokeToken { prefix } => {
            on_home_node(&|store| match store.revoke_token(&prefix) {
                Ok(loquax::store::TokenRevoked::One(gone)) => {
                    println!(
                        "withdrew {}  {}",
                        address::to_hex(&gone.token[..4]),
                        gone.name
                    );
                    ExitCode::SUCCESS
                }
                Ok(loquax::store::TokenRevoked::None) => failure(
                    format!("no open admission code starts with {prefix:?}"),
                    "failed",
                ),
                Ok(loquax::store::TokenRevoked::Ambiguous(matched)) => {
                    let names: Vec<String> = matched
                        .iter()
                        .map(|t| format!("{} {}", address::to_hex(&t.token[..4]), t.name))
                        .collect();
                    usage(format!(
                        "{prefix:?} starts {} codes ({}); give more of it",
                        matched.len(),
                        names.join(", ")
                    ))
                }
                Ok(loquax::store::TokenRevoked::TooShort) => {
                    usage(format!("{prefix:?}: give at least four hex characters"))
                }
                Err(error) => failure(error, "no-store"),
            })
        }
        MailboxCommand::Join(args) => run_join(home, client, args).await,
        // Dispatched above, before the state directory is made.
        MailboxCommand::Run(_)
        | MailboxCommand::Setup(_)
        | MailboxCommand::Enable
        | MailboxCommand::Disable => ExitCode::SUCCESS,
    }
}

/// The packaged system service that runs `loquax mailbox run`
/// (`packaging/loquax/loquax-mailbox.service`).
#[cfg(feature = "tui")]
const MAILBOX_UNIT: &str = "loquax-mailbox.service";

/// `loquax mailbox enable` and `disable`: systemctl does the work and says
/// what went wrong, so its exit is ours; one that cannot start at all, on a
/// host without systemd, is an operational failure.
#[cfg(feature = "tui")]
fn systemctl(args: &[&str]) -> ExitCode {
    match std::process::Command::new("systemctl").args(args).status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => failure(
            format!(
                "systemctl {} failed ({status}); the service comes with the \
                 loquax package, and enabling it needs root",
                args.join(" ")
            ),
            "failed",
        ),
        Err(error) => failure(format!("systemctl: {error}"), "failed"),
    }
}

/// Ask one numbered question on stdin: the number typed, or `default` on
/// an empty line or the end of input. `None` when the answer is no choice.
#[cfg(feature = "tui")]
fn choose(choices: usize, default: usize) -> Option<usize> {
    let typed = prompt(&format!("     Choose 1 or {choices} [{default}]: "));
    match typed.as_str() {
        "" => Some(default),
        text => text
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=choices).contains(n)),
    }
}

/// Print `question` without a newline and read one line; empty at the end
/// of input, so a pipe that runs dry takes the defaults.
#[cfg(feature = "tui")]
fn prompt(question: &str) -> String {
    use std::io::Write as _;
    print!("{question}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) | Err(_) => {
            println!();
            String::new()
        }
        Ok(_) => line.trim().to_string(),
    }
}

/// Whether this process runs as root: `/proc/self` belongs to the
/// effective user.
#[cfg(feature = "tui")]
fn is_root() -> bool {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata("/proc/self").is_ok_and(|meta| meta.uid() == 0)
}

/// `loquax mailbox setup` (1266): the questions first, so the run that
/// needs root asks nothing; then the steps, printed in their order.
#[cfg(feature = "tui")]
async fn run_setup(args: SetupArgs) -> ExitCode {
    use loquax::home::setup::{self, Answers, Reach, Store};

    println!("{}", setup::TITLE);
    let account_dir = match identity::home_path() {
        Ok(dir) => dir,
        Err(error) => return failure(error, "no-home"),
    };
    let unit = setup::unit_installed();
    let store = match args.store.as_deref().map(Store::parse) {
        Some(Ok(store)) => store,
        Some(Err(error)) => return usage(format!("--store: {error}")),
        None if args.yes => Store::default_here(),
        None => {
            println!("{}", setup::store_question(&account_dir, unit));
            let default = match Store::default_here() {
                Store::System => 1,
                Store::Account => 2,
            };
            match choose(2, default) {
                Some(1) => Store::System,
                Some(_) => Store::Account,
                None => return usage("choose 1 or 2"),
            }
        }
    };
    if store == Store::System && !unit {
        return failure(
            "the system mailbox comes with the loquax package, which is not installed \
             here; install it, or keep the mailbox in your account with --store account",
            "failed",
        );
    }
    let dir = match store {
        Store::System => setup::system_dir(),
        Store::Account => account_dir,
    };
    // What is set already is the default; the system mailbox's directory
    // is not readable before sudo, which keeps the shipped default.
    let held = loquax::config::load(&dir)
        .map(|config| Reach::of(&config))
        .unwrap_or(Reach::Rendezvous);
    let address = match args.address.as_deref().map(setup::check_address) {
        Some(Ok(address)) => Some(address),
        Some(Err(error)) => return usage(format!("--address: {error}")),
        None => None,
    };
    let reach = match (args.reach.as_deref().map(str::trim), address) {
        (Some("rendezvous"), Some(_)) => {
            return usage("--address goes with --reach fixed, not rendezvous")
        }
        (Some("rendezvous"), None) => Reach::Rendezvous,
        (Some("fixed") | None, Some(address)) => Reach::Fixed(address),
        (Some("fixed"), None) => match (&held, args.yes) {
            (Reach::Fixed(address), _) => Reach::Fixed(address.clone()),
            (_, true) => return usage("--reach fixed needs --address host:port"),
            (_, false) => match ask_address() {
                Ok(reach) => reach,
                Err(code) => return code,
            },
        },
        (Some(other), _) => return usage(format!("--reach: {other:?} is not rendezvous or fixed")),
        (None, None) if args.yes => held,
        (None, None) => {
            let offered = match &held {
                Reach::Fixed(address) => Some(address.clone()),
                Reach::Rendezvous => setup::default_fixed(),
            };
            println!("{}", setup::reach_question(offered.as_deref()));
            let default = match held {
                Reach::Rendezvous => 1,
                Reach::Fixed(_) => 2,
            };
            match choose(2, default) {
                Some(1) => Reach::Rendezvous,
                Some(_) => match ask_address() {
                    Ok(reach) => reach,
                    Err(code) => return code,
                },
                None => return usage("choose 1 or 2"),
            }
        }
    };
    if args.phone_port.is_some() && reach == Reach::Rendezvous {
        return usage("--phone-port goes with a fixed address, not the rendezvous");
    }
    let answers = Answers {
        store,
        reach,
        name: args.name.map(|name| name.trim().to_string()),
        phone_port: args.phone_port.unwrap_or(loquax::home::phone_server::PORT),
    };
    println!();
    match store {
        Store::System if !is_root() => {
            println!("{}\n", setup::sudo_why());
            let exe = match std::env::current_exe() {
                Ok(exe) => exe,
                Err(error) => return failure(format!("this program's path: {error}"), "failed"),
            };
            let argv = setup::sudo_argv(&exe, &answers);
            match std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .status()
            {
                Ok(status) => match status.code() {
                    Some(0) => ExitCode::SUCCESS,
                    Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(EXIT_FAILURE)),
                    None => ExitCode::from(EXIT_FAILURE),
                },
                Err(error) => failure(format!("sudo: {error}"), "failed"),
            }
        }
        Store::System => setup_system(&answers, &dir),
        Store::Account => {
            let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
            Box::pin(setup_account(&answers, &dir, &config_dir)).await
        }
    }
}

/// The fixed address, asked with the LAN address offered.
#[cfg(feature = "tui")]
fn ask_address() -> Result<loquax::home::setup::Reach, ExitCode> {
    let offered = loquax::home::setup::default_fixed();
    let typed = prompt(&match &offered {
        Some(address) => format!("     The address, host:port [{address}]: "),
        None => "     The address, host:port: ".to_string(),
    });
    let text = match (typed.is_empty(), offered) {
        (true, Some(address)) => address,
        (true, None) => return Err(usage("a fixed address needs host:port")),
        (false, _) => typed,
    };
    loquax::home::setup::check_address(&text)
        .map(loquax::home::setup::Reach::Fixed)
        .map_err(|error| usage(format!("the address: {error}")))
}

/// How long a code stays good on the mailbox in `dir`, in hours.
#[cfg(feature = "tui")]
fn token_hours(dir: &Path) -> u64 {
    loquax::config::load(dir)
        .ok()
        .and_then(|config| config.home_token_hours)
        .unwrap_or(loquax::home::DEFAULT_TOKEN_HOURS)
}

/// The steps for a mailbox in this account: no service, the settings and
/// the code written as this user.
#[cfg(feature = "tui")]
async fn setup_account(
    answers: &loquax::home::setup::Answers,
    dir: &Path,
    config_dir: &Path,
) -> ExitCode {
    use loquax::home::setup::{self, Reach};

    match loquax::home::client::load_client(dir) {
        Ok(None) => {}
        Ok(Some(_)) => {
            return failure(
                "this account is a client of a Loquax Mailbox, so it cannot be one; \
                 set the mailbox up where the identity is",
                "client",
            )
        }
        Err(error) => return failure(error, "no-home"),
    }
    if dir.join("identity").exists() {
        let state = identity::load_or_create(&dir.join("identity"))
            .ok()
            .and_then(|id| loquax::engine::delivery_address(&id).ok());
        if let Some(address) = state {
            let devices = loquax::store::Store::open_in(dir, address)
                .ok()
                .and_then(|store| store.devices().ok())
                .map(|devices| devices.len());
            println!("{}\n", setup::already(&address::to_hex(&address), devices));
        }
    }
    println!("{}", setup::store_done(answers.store, dir));
    let changed = match setup::save_reach(dir, &answers.reach) {
        Ok(changed) => changed,
        Err(error) => return failure(format!("{}: {error}", dir.display()), "no-home"),
    };
    println!("{}", setup::service_done(None, changed));
    let port = match &answers.reach {
        Reach::Fixed(_) => Some(loquax::home::phone_server::add(
            config_dir,
            answers.phone_port,
        )),
        Reach::Rendezvous => None,
    };
    println!(
        "{}",
        setup::reach_done(
            answers.store,
            &answers.reach,
            changed,
            port.as_ref(),
            answers.phone_port
        )
    );
    println!("{}", setup::code_head(token_hours(dir)));
    let code = run_mailbox(MailboxCommand::Admit {
        name: answers.name.clone(),
        server: None,
        code_only: true,
    })
    .await;
    if code != ExitCode::SUCCESS {
        return code;
    }
    println!("{}", setup::PHONE_LINE);
    ExitCode::SUCCESS
}

/// The steps for the system mailbox, as root: the settings written and
/// handed to the service's account, the port for phones, the code issued
/// as that account, then the service enabled and (re)started. The code
/// comes before the service so the identity is minted once, by the code,
/// and never by two processes at the same time.
#[cfg(feature = "tui")]
fn setup_system(answers: &loquax::home::setup::Answers, dir: &Path) -> ExitCode {
    use loquax::home::setup::{self, Reach, Service};
    use std::os::unix::fs::MetadataExt as _;

    let owner = match std::fs::metadata(dir) {
        Ok(meta) => (meta.uid(), meta.gid()),
        Err(error) => {
            return failure(
                format!(
                    "{}: {error}; the loquax package makes it, so reinstalling it does",
                    dir.display()
                ),
                "failed",
            )
        }
    };
    let identity_file = dir.join("identity");
    if identity_file.exists() {
        let address = identity::load_or_create(&identity_file)
            .ok()
            .and_then(|id| loquax::engine::delivery_address(&id).ok());
        if let Some(address) = address {
            println!("{}\n", setup::already(&address::to_hex(&address), None));
        }
    }
    let changed = match setup::save_reach(dir, &answers.reach) {
        Ok(changed) => changed,
        Err(error) => return failure(format!("{}: {error}", dir.display()), "failed"),
    };
    let config_file = dir.join(loquax::config::CONFIG_FILE);
    if config_file.exists() {
        if let Err(error) = std::os::unix::fs::chown(&config_file, Some(owner.0), Some(owner.1)) {
            return failure(format!("{}: {error}", config_file.display()), "failed");
        }
    }
    let port = match &answers.reach {
        Reach::Fixed(_) => {
            let added = loquax::home::phone_server::add(
                Path::new(setup::SYSTEM_RETICULUM),
                answers.phone_port,
            );
            if matches!(added, loquax::home::phone_server::Added::Written(_)) {
                let _ = std::process::Command::new("systemctl")
                    .args(["try-restart", "lnsd.service"])
                    .status();
            }
            Some(added)
        }
        Reach::Rendezvous => None,
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => return failure(format!("this program's path: {error}"), "failed"),
    };
    let argv = setup::admit_argv(&exe, dir, answers.name.as_deref());
    let admitted = match std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .output()
    {
        Ok(output) if output.status.success() => output.stdout,
        Ok(output) => {
            return failure(
                format!(
                    "the code, as {}: {}",
                    setup::SYSTEM_USER,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                "failed",
            )
        }
        Err(error) => return failure(format!("runuser: {error}"), "failed"),
    };
    let quiet = |args: &[&str]| {
        std::process::Command::new("systemctl")
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    let enabled = quiet(&["is-enabled", "--quiet", setup::UNIT]);
    let active = quiet(&["is-active", "--quiet", setup::UNIT]);
    if !enabled {
        let code = systemctl(&["enable", setup::UNIT]);
        if code != ExitCode::SUCCESS {
            return code;
        }
    }
    let service = match (active, changed, enabled) {
        (false, _, _) => match systemctl(&["start", setup::UNIT]) {
            ExitCode::SUCCESS => Service::Started,
            code => return code,
        },
        (true, true, _) => match systemctl(&["restart", setup::UNIT]) {
            ExitCode::SUCCESS => Service::Restarted,
            code => return code,
        },
        (true, false, false) => Service::Started,
        (true, false, true) => Service::AlreadyOn,
    };
    println!("{}", setup::store_done(answers.store, dir));
    println!("{}", setup::service_done(Some(service), changed));
    println!(
        "{}",
        setup::reach_done(
            answers.store,
            &answers.reach,
            changed,
            port.as_ref(),
            answers.phone_port
        )
    );
    println!("{}", setup::code_head(token_hours(dir)));
    print!("{}", String::from_utf8_lossy(&admitted));
    println!("{}", setup::PHONE_LINE);
    ExitCode::SUCCESS
}

/// `loquax mailbox join <code>`.
#[cfg(feature = "tui")]
async fn run_join(home: PathBuf, client: Option<[u8; 64]>, args: JoinArgs) -> ExitCode {
    let code = match loquax::home::code::HomeCode::parse(&args.code) {
        Ok(code) => code,
        Err(error) => return usage(error),
    };
    if client.is_some_and(|key| key != code.public_key) {
        return failure(
            "this device is already the client of another Loquax Mailbox",
            "client",
        );
    }
    let address = match loquax::home::lxmf_address(&code.public_key) {
        Ok(address) => address,
        Err(error) => return usage(error),
    };
    let device =
        match identity::load_or_create(&home.join(loquax::home::client::DEVICE_IDENTITY_FILE)) {
            Ok(identity) => identity,
            Err(error) => return failure(error, "no-identity"),
        };
    let display_name = match display_name::from_process(None) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };
    let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
    let instance = args.instance.unwrap_or_else(|| instance_name(&config_dir));
    let storage_dir = match storage_dir(&home) {
        Ok(dir) => dir,
        Err(error) => return failure(error, "no-daemon"),
    };
    let attached = match loquax::engine::attach_as(
        AttachConfig {
            instance,
            config_dir: Some(config_dir.clone()),
            storage_dir,
            identity: device.clone(),
            display_name: Vec::new(),
            notices: Terminal::new(home.clone()),
        },
        loquax::engine::Role::Client { address },
    )
    .await
    {
        Ok(attached) => attached,
        Err(error) => return no_daemon(&error),
    };
    let node = attached.node();
    let joined = loquax::home::client::join(
        &node,
        &code.public_key,
        &device,
        &code.token,
        &display_name.name,
        Duration::from_secs(args.timeout),
    )
    .await;
    drop(node);
    attached.stop().await;
    match joined {
        Ok(()) => match loquax::home::client::save_client(&home, &code.public_key) {
            Ok(()) => {
                println!(
                    "joined: this device reads the mail of {}",
                    address::to_hex(&address)
                );
                ExitCode::SUCCESS
            }
            Err(error) => failure(error, "no-home"),
        },
        Err(error) => failure(error, "failed"),
    }
}

/// `loquax fetch` on a client of a home node: pull what the home node holds
/// after this device's cursor and print what is new.
async fn run_client_fetch(home: PathBuf, public_key: [u8; 64], args: FetchArgs) -> ExitCode {
    let address = match loquax::home::lxmf_address(&public_key) {
        Ok(address) => address,
        Err(error) => return failure(error, "no-identity"),
    };
    let device =
        match identity::load_or_create(&home.join(loquax::home::client::DEVICE_IDENTITY_FILE)) {
            Ok(identity) => identity,
            Err(error) => return failure(error, "no-identity"),
        };
    let mut seen = match SeenStore::load(&home) {
        Ok(seen) => seen,
        Err(error) => return failure(format!("seen store: {error}"), "no-home"),
    };
    let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
    let instance = args.instance.unwrap_or_else(|| instance_name(&config_dir));
    let storage_dir = match storage_dir(&home) {
        Ok(dir) => dir,
        Err(error) => return failure(error, "no-daemon"),
    };
    let mut attached = match loquax::engine::attach_as(
        AttachConfig {
            instance,
            config_dir: Some(config_dir.clone()),
            storage_dir,
            identity: device.clone(),
            display_name: Vec::new(),
            notices: Terminal::new(home.clone()),
        },
        loquax::engine::Role::Client { address },
    )
    .await
    {
        Ok(attached) => attached,
        Err(error) => return no_daemon(&error),
    };
    let node = attached.node();
    let mut effects = Terminal::new(home.clone());
    let outcome = loquax::home::client::fetch(
        &mut attached.outbox,
        &node,
        &home,
        &public_key,
        &device,
        Duration::from_secs(args.timeout),
        &mut effects,
        &mut seen,
    )
    .await;
    drop(node);
    attached.stop().await;
    match outcome {
        Ok(new) => {
            events::done(None, if new > 0 { "fetched" } else { "empty" }, 0);
            ExitCode::SUCCESS
        }
        Err(error) => failure(error, "failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1598: the lookup table. The running daemon is asked for by
    /// `--instance`, the moved home's config, the user's config, then as
    /// `default`; a daemon is started only in the start directory, under
    /// that directory's name (1570).
    #[cfg(any(feature = "tui", feature = "gui"))]
    #[test]
    fn the_daemon_lookup_asks_each_config_before_it_starts_one() {
        use std::path::PathBuf;
        let root = tempfile::tempdir().expect("root");
        let moved = root.path().join("lnmsg").join(DAEMON_DIR);
        let users = root.path().join("dot-reticulum");
        let flag = root.path().join("flag");
        let configure = |dir: &PathBuf, name: &str| {
            std::fs::create_dir_all(dir).expect("dir");
            std::fs::write(
                dir.join("config"),
                format!("[reticulum]\n  share_instance = Yes\n  instance_name = {name}\n"),
            )
            .expect("config");
        };
        let look = |dir: &PathBuf, instance: &str, source| Look {
            dir: dir.clone(),
            instance: instance.to_string(),
            source,
        };
        let plan = |config: Option<&PathBuf>, instance: Option<&str>, with_home: bool| {
            lookup_in(
                config.cloned(),
                instance.map(str::to_string),
                with_home.then(|| moved.clone()),
                users.clone(),
            )
        };

        // No config anywhere: `default`, found or started where Loquax
        // lives.
        let got = plan(None, None, true);
        assert_eq!(got.looks, vec![look(&users, "default", NameSource::None)]);
        assert_eq!(got.start, look(&moved, "default", NameSource::None));
        let got = plan(None, None, false);
        assert_eq!(got.start, look(&users, "default", NameSource::None));

        // The user's daemon names its instance (the conformance cell):
        // asked for by that name, then as `default`; a start still in the
        // moved home under its own name.
        configure(&users, "node-x");
        let got = plan(None, None, true);
        assert_eq!(
            got.looks,
            vec![
                look(&users, "node-x", NameSource::DefaultDir),
                look(&users, "default", NameSource::None),
            ]
        );
        assert_eq!(got.start, look(&moved, "default", NameSource::None));
        let got = plan(None, None, false);
        assert_eq!(got.looks[0], look(&users, "node-x", NameSource::DefaultDir));
        assert_eq!(got.start, look(&users, "node-x", NameSource::DefaultDir));

        // A config in the moved home comes first.
        configure(&moved, "home-y");
        let got = plan(None, None, true);
        assert_eq!(
            got.looks,
            vec![
                look(&moved, "home-y", NameSource::Home),
                look(&users, "node-x", NameSource::DefaultDir),
                look(&users, "default", NameSource::None),
            ]
        );
        assert_eq!(got.start, look(&moved, "home-y", NameSource::Home));

        // `--instance` and `--config` are asked for alone.
        let got = plan(None, Some("z"), true);
        assert_eq!(got.looks, vec![look(&moved, "z", NameSource::Flag)]);
        assert_eq!(got.start, look(&moved, "z", NameSource::Flag));
        configure(&flag, "flag-w");
        let got = plan(Some(&flag), None, true);
        assert_eq!(got.looks, vec![look(&flag, "flag-w", NameSource::Flag)]);
        assert_eq!(got.start, look(&flag, "flag-w", NameSource::Flag));
    }

    /// 1342: plain `loquax` with a flag only the terminal view takes asks
    /// for it, so a key script never opens a window on a desktop; the
    /// options both frontends take leave the choice to Loquax.
    #[cfg(feature = "tui")]
    #[test]
    fn a_terminal_flag_asks_for_the_terminal_and_a_shared_one_does_not() {
        use clap::CommandFactory as _;
        let ask = |line: &[&str]| {
            let command = Args::command();
            let matches = command
                .clone()
                .try_get_matches_from(line)
                .expect("the line parses");
            ask_of(&command, &matches)
        };
        assert_eq!(ask(&["loquax"]), Ask::Default);
        for shared in [
            &["loquax", "--pn", "00112233445566778899aabbccddeeff"][..],
            &["loquax", "--instance", "x"],
            &["loquax", "--config", "/etc/x"],
            &["loquax", "--timeout", "9"],
            &["loquax", "--sync-interval", "off"],
            &["loquax", "--verbose"],
        ] {
            assert_eq!(ask(shared), Ask::Default, "{shared:?}");
        }
        for terminal in [
            &["loquax", "--ascii"][..],
            &["loquax", "--script", "keys"],
            &["loquax", "--to", "00112233445566778899aabbccddeeff"],
            &["loquax", "--no-mouse"],
            &["loquax", "--graphics", "braille"],
        ] {
            assert_eq!(ask(terminal), Ask::Tui, "{terminal:?}");
        }
    }

    /// 1279, walk finding 23: a script still reads the address alone; at a
    /// terminal the contact code follows, the one that adds us.
    #[test]
    fn address_prints_the_code_under_the_hex_at_a_terminal_only() {
        let private: [u8; 64] = core::array::from_fn(|i| i as u8 + 1);
        let key = leviculum_core::Identity::from_private_key_bytes(&private)
            .expect("a key")
            .public_key_bytes();
        let hex = address::to_hex(&loquax::home::lxmf_address(&key).expect("an address"));
        // The same address the engine registers, which it printed before.
        let identity = leviculum_core::Identity::from_private_key_bytes(&private).expect("a key");
        let registered = loquax::engine::delivery_address(&identity).expect("an address");
        assert_eq!(hex, address::to_hex(&registered));
        assert_eq!(
            address_lines(&key, false).expect("lines"),
            std::slice::from_ref(&hex)
        );
        let code = loquax::lxma::encode(&key).expect("a code");
        assert!(code.starts_with(&format!("lxma://{hex}:")), "{code}");
        assert_eq!(address_lines(&key, true).expect("lines"), [hex, code]);
    }

    /// `--position` (1209): degrees to a report's Telemeter, which reads
    /// back as a position report; anything else is refused before a send.
    #[test]
    fn a_position_argument_is_a_reports_telemeter() {
        let fields = position_fields("52.516275,-13.3777").expect("a place");
        let read = loquax::telemetry::read([0; 16], &fields, None);
        let fix = loquax::telemetry::shown_fix(&read).expect("a fix");
        assert_eq!(
            (fix.location.latitude_e6, fix.location.longitude_e6),
            (52_516_275, -13_377_700)
        );
        for bad in ["52.5", "91,0", "0,181", "a,b", "NaN,0"] {
            assert!(position_fields(bad).is_err(), "{bad}");
        }
    }
}

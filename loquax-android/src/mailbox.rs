//! The mailbox on the phone (1152; `docs/drafts/loquax-onboarding-2026-10.md`,
//! section 3): `loquax --daemon`'s home node, the same
//! [`loquax::home::daemon::run`], inside the app's process. It has a node of
//! its own that dials the entry servers, since a phone has no lnsd, and
//! serves the app's screens on a loopback port. The screens stay a client
//! of it exactly as of a mailbox on a computer: they join with a code, pull,
//! send and watch over `loquax.home`, so the local and the external case are
//! one code path. Only the server they dial differs.
//!
//! Its files live in a directory of their own under the app's, [`DIR`]: the
//! mailbox identity, its store and its node's storage. The app's own
//! directory keeps the client's (the device identity, the client store),
//! as on a phone that is the client of a computer; both stores are keyed by
//! the same LXMF address and must not be the same file.

use std::path::{Path, PathBuf};
use std::time::Duration;

use leviculum_core::Identity;
use loquax::home::code::HomeCode;
use loquax::home::daemon::{DaemonOptions, DaemonUpstream};
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// The mailbox's directory under the app's.
pub const DIR: &str = "mailbox";
/// The file under the app's directory that says the mailbox is on this
/// phone. Its presence is the choice, so a start the system makes after it
/// ended the process (the sticky service) finds it.
pub const HERE_FILE: &str = "mailbox-here";
/// The file under [`DIR`] that names the entry servers, `host:port` one per
/// line, tried one at a time in this order with failover; the shipped
/// rendezvous list when it is absent or holds none.
pub const SERVERS_FILE: &str = "servers";
/// The file under [`DIR`] with the name recipients see.
pub const NAME_FILE: &str = "name";
/// The shipped entry servers, as the mailbox dials them: the rendezvous
/// list (`loquax::home::rendezvous::DEFAULT`) on its ordinary port, as a
/// peer (1196). The phones' default is the same node's access point port,
/// `leviculum.network:4343`, with `:4242` as its last fallback.
pub const DEFAULT_SERVERS: &[&str] = &["leviculum.network:4242"];
/// The name the screens' device is admitted under.
const SCREENS: &str = "this phone";
/// How long a stop waits for the home node to close its node.
const STOP_WAIT: Duration = Duration::from_secs(5);

/// Whether the mailbox is on this phone.
pub fn is_here(app: &Path) -> bool {
    app.join(HERE_FILE).exists()
}

/// Make this phone the mailbox: the choice, kept, and the identity, made
/// once. The identity's public key back.
pub fn choose_here(app: &Path) -> Result<[u8; 64], String> {
    let dir = app.join(DIR);
    std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let identity = identity(app)?;
    std::fs::write(app.join(HERE_FILE), b"")
        .map_err(|error| format!("{}: {error}", app.join(HERE_FILE).display()))?;
    Ok(identity.public_key_bytes())
}

/// The mailbox identity, made on the first call.
pub fn identity(app: &Path) -> Result<Identity, String> {
    loquax::identity::load_or_create(&app.join(DIR).join("identity"))
        .map_err(|error| error.to_string())
}

/// The entry servers: the file's lines that are not empty or a comment,
/// the shipped list when there are none.
pub fn servers(app: &Path) -> Vec<String> {
    let listed: Vec<String> = std::fs::read_to_string(app.join(DIR).join(SERVERS_FILE))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect();
    if listed.is_empty() {
        DEFAULT_SERVERS.iter().map(|s| (*s).to_string()).collect()
    } else {
        listed
    }
}

/// The name recipients see; empty when none was given.
pub fn name(app: &Path) -> String {
    std::fs::read_to_string(app.join(DIR).join(NAME_FILE))
        .map(|name| name.trim().to_string())
        .unwrap_or_default()
}

/// Keep the name recipients see. It goes out with the next start of the
/// mailbox, which [`Here::restart`] is.
pub fn set_name(app: &Path, name: &str) -> Result<(), String> {
    let path = app.join(DIR).join(NAME_FILE);
    std::fs::write(&path, name.trim()).map_err(|error| format!("{}: {error}", path.display()))
}

/// The mailbox's store, beside the running home node's (SQLite takes both).
fn store(app: &Path) -> Result<loquax::store::Store, String> {
    let address = loquax::engine::delivery_address(&identity(app)?)?;
    loquax::store::Store::open_in(&app.join(DIR), address).map_err(|e| e.to_string())
}

/// An admission for the screens: a token issued in the mailbox's store, as
/// `loquax mailbox admit` issues one ([`loquax::home::admit::open`]), and
/// the code that carries it.
pub fn admission(app: &Path) -> Result<HomeCode, String> {
    let public_key = identity(app)?.public_key_bytes();
    let now = leviculum_std::local_time::unix_now_secs();
    loquax::home::admit::open(&mut store(app)?, public_key, SCREENS, Vec::new(), now)
        .map(|admission| admission.code)
        .map_err(|e| e.to_string())
}

/// The rendezvous list a code of this phone's mailbox carries (1264): what
/// the mailbox dials itself ([`servers`]), as another device dials it. The
/// shipped entry servers are the shipped rendezvous list, the access point
/// port first (`loquax::home::rendezvous::DEFAULT`); a list of the
/// servers file goes as it stands, since one server for both is what a
/// file names (a test's, a power user's).
pub fn code_list(app: &Path) -> Vec<String> {
    let dialled = servers(app);
    let shipped: Vec<String> = DEFAULT_SERVERS.iter().map(|s| (*s).to_string()).collect();
    match dialled == shipped {
        true => loquax::home::rendezvous::default_list(),
        false => dialled,
    }
}

/// How long this mailbox's codes are good for, in seconds: its config's
/// `home_token_hours`, a day by default, as on a computer.
pub fn token_ttl_secs(app: &Path) -> Result<i64, String> {
    let config = loquax::config::load(&app.join(DIR)).map_err(|error| error.to_string())?;
    Ok(loquax::home::token_ttl_secs(&config))
}

/// A device being linked (1264): its code's token, the devices the mailbox
/// had admitted when the code was made, and when it ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Linking {
    pub token: [u8; 16],
    pub known: Vec<([u8; 16], i64)>,
    pub expires_at: i64,
}

/// Where a link stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Linked {
    /// The code is open; no device redeemed it yet.
    Waiting,
    /// A device redeemed it, under this name.
    Linked(String),
    /// The code ran out or was withdrawn, and no device came with it.
    Over,
}

/// "Link a device" (1264, finding 7): a code for another device of the
/// same `lqxh1` form `loquax mailbox admit` prints, issued self-named so
/// the device is listed under the name its join brings, the rendezvous
/// list of [`code_list`] behind it; and what [`linked`] needs to see the
/// device come.
pub fn link_code(app: &Path) -> Result<(loquax::home::admit::Admission, Linking), String> {
    let public_key = identity(app)?.public_key_bytes();
    let ttl = token_ttl_secs(app)?;
    let now = leviculum_std::local_time::unix_now_secs();
    let mut store = store(app)?;
    let known = store
        .devices()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|device| (device.device, device.admitted_at))
        .collect();
    let admission = loquax::home::admit::open(
        &mut store,
        public_key,
        loquax::home::admit::SELF_NAMED,
        code_list(app),
        now,
    )
    .map_err(|e| e.to_string())?;
    let linking = Linking {
        token: admission.code.token,
        known,
        expires_at: admission.expires_at(ttl),
    };
    Ok((admission, linking))
}

/// The names of the devices the mailbox on this phone admitted, the
/// earliest first (1443, the profile's devices).
pub fn devices(app: &Path) -> Result<Vec<String>, String> {
    let store = store(app)?;
    let devices = store.devices().map_err(|e| e.to_string())?;
    Ok(devices.into_iter().map(|device| device.name).collect())
}

/// Whether the device of `linking` came: its token still open, a device
/// admitted since that was not (a device admitted again counts, under its
/// new name), or neither.
pub fn linked(app: &Path, linking: &Linking) -> Result<Linked, String> {
    let ttl = token_ttl_secs(app)?;
    let now = leviculum_std::local_time::unix_now_secs();
    let mut store = store(app)?;
    let devices = store.devices().map_err(|e| e.to_string())?;
    if let Some(device) = devices
        .iter()
        .find(|device| !linking.known.contains(&(device.device, device.admitted_at)))
    {
        return Ok(Linked::Linked(device.name.clone()));
    }
    let open = store.open_tokens(now, ttl).map_err(|e| e.to_string())?;
    Ok(match open.iter().any(|open| open.token == linking.token) {
        true => Linked::Waiting,
        false => Linked::Over,
    })
}

/// The scratch directory under the app's a move in works in (1281): the
/// node that reaches the old phone keeps its storage there, and it goes
/// when the move ends either way.
pub const MOVING_DIR: &str = "moving";
/// How long a restore looks for its address live elsewhere (1281).
pub const PROBE_BUDGET: Duration = Duration::from_secs(15);

/// Where a move to a new phone stands, on the old one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Moving {
    /// The code is open; the new phone did not take everything yet.
    Waiting,
    /// The new phone took everything, `name` the name it joined under;
    /// this phone's mailbox has stopped.
    Moved(String),
    /// The code ran out, or a newer one replaced it, and nobody moved.
    Over,
}

/// "Move to a new phone" (1281, walk finding 24): a join code of the same
/// form as [`link_code`]'s, and a move opened for its token, so the device
/// that joins with it may take the identity, the archive and the rest
/// (`loquax::home::handover`).
pub fn move_code(app: &Path) -> Result<(loquax::home::admit::Admission, Linking), String> {
    let (admission, linking) = link_code(app)?;
    loquax::home::handover::open(&app.join(DIR), &linking.token, linking.expires_at)?;
    Ok((admission, linking))
}

/// Whether the new phone of `linking` took the mailbox.
pub fn moving(app: &Path, linking: &Linking) -> Result<Moving, String> {
    let dir = app.join(DIR);
    if loquax::home::handover::gone_at(&dir).is_some() {
        let name = match linked(app, linking)? {
            Linked::Linked(name) => name,
            Linked::Waiting | Linked::Over => String::new(),
        };
        return Ok(Moving::Moved(name));
    }
    let now = leviculum_std::local_time::unix_now_secs();
    Ok(match loquax::home::handover::token(&dir, now) {
        Some(token) if token == linking.token => Moving::Waiting,
        _ => Moving::Over,
    })
}

/// Whether the mailbox on this phone moved to another one and was not
/// taken back: it does not start, and this phone sends nothing.
pub fn has_moved(app: &Path) -> bool {
    loquax::home::handover::gone_at(&app.join(DIR)).is_some()
}

/// "Use this phone again": the owner takes the moved mailbox back here.
pub fn take_back(app: &Path) -> Result<(), String> {
    loquax::home::handover::take_back(&app.join(DIR))
}

/// Why a move in did not work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveFailed {
    /// The text is no join code.
    Code(String),
    /// This phone holds an address or reads a mailbox already.
    HasAddress,
    /// The old phone was not reached, or refused, or the move broke off;
    /// the old phone still has everything.
    Move(loquax::home::client::HomeError),
    /// Something on this phone failed.
    Here(String),
}

/// Move in from the old phone (1281): join its mailbox with the move code,
/// take the identity, its ratchets, the contacts' keys, the name and the
/// whole archive, keep them as this phone's mailbox, and tell the old
/// phone, which stops. This phone becomes the mailbox only once the old one
/// said it took the word; before that, a failure leaves nothing here.
pub fn move_in(app: &Path, text: &str, name: &str, runtime: &Handle) -> Result<(), MoveFailed> {
    let client = loquax::home::client::load_client(app).map_err(MoveFailed::Here)?;
    if is_here(app) || app.join(DIR).join("identity").exists() || client.is_some() {
        return Err(MoveFailed::HasAddress);
    }
    let (code, list) = HomeCode::parse_with_rendezvous(text.trim())
        .map_err(|error| MoveFailed::Code(error.to_string()))?;
    if list.is_empty() {
        return Err(MoveFailed::Code("the code names no server".to_string()));
    }
    let device =
        loquax::identity::load_or_create(&app.join(loquax::home::client::DEVICE_IDENTITY_FILE))
            .map_err(|error| MoveFailed::Here(error.to_string()))?;
    let scratch = app.join(MOVING_DIR);
    let moved = runtime.block_on(take_in(app, &scratch, &code, list, &device, name));
    let _ = std::fs::remove_dir_all(&scratch);
    if moved.is_err() {
        let _ = std::fs::remove_dir_all(app.join(DIR));
    }
    moved
}

/// A leaf node of this phone's own to `list`, for the mailbox of
/// `public_key`, its storage under `scratch`.
async fn leaf(
    app: &Path,
    scratch: &Path,
    public_key: &[u8; 64],
    list: Vec<String>,
    device: &Identity,
) -> Result<loquax::engine::Attached, String> {
    let storage = scratch.join("storage");
    std::fs::create_dir_all(&storage).map_err(|error| format!("{}: {error}", storage.display()))?;
    let config = loquax::config::load(app).map_err(|error| error.to_string())?;
    let address = loquax::home::lxmf_address(public_key)?;
    loquax::engine::attach_leaf(
        loquax::engine::LeafConfig {
            servers: list,
            home_destination: Some(loquax::home::destination_hash(public_key)?),
            timing: loquax::home::rendezvous::Timing::default(),
            storage_dir: storage,
            identity: device.clone(),
            display_name: Vec::new(),
            notices: loquax::effects::Terminal::new(scratch.to_path_buf()),
            tcp: config.tcp,
            network: Default::default(),
        },
        loquax::engine::Role::Client { address },
    )
    .await
    .map_err(|error| error.to_string())
}

async fn take_in(
    app: &Path,
    scratch: &Path,
    code: &HomeCode,
    list: Vec<String>,
    device: &Identity,
    name: &str,
) -> Result<(), MoveFailed> {
    let attached = leaf(app, scratch, &code.public_key, list, device)
        .await
        .map_err(MoveFailed::Here)?;
    let node = attached.node();
    let moved = take_over(app, &node, code, device, name).await;
    drop(node);
    attached.stop().await;
    moved
}

/// How often a `/moved` the old phone answers with "more came" is tried
/// again after taking the rest.
const BEHIND_TRIES: usize = 5;

async fn take_over(
    app: &Path,
    node: &leviculum_std::ReticulumNode,
    code: &HomeCode,
    device: &Identity,
    name: &str,
) -> Result<(), MoveFailed> {
    use loquax::home::client::{self, HomeError};
    use loquax::home::wire::Status;
    let budget = crate::session::BUDGET;
    client::join(node, &code.public_key, device, &code.token, name, budget)
        .await
        .map_err(MoveFailed::Move)?;
    let taken = client::take_move(node, &code.public_key, device, &code.token, 0, budget)
        .await
        .map_err(MoveFailed::Move)?;
    let identity = Identity::from_private_key_bytes(&taken.private_key)
        .map_err(|_| MoveFailed::Move(HomeError::Malformed))?;
    if identity.public_key_bytes() != code.public_key {
        return Err(MoveFailed::Move(HomeError::Malformed));
    }
    let dir = app.join(DIR);
    std::fs::create_dir_all(&dir).map_err(|error| MoveFailed::Here(error.to_string()))?;
    loquax::identity::store(&dir.join("identity"), &identity)
        .map_err(|error| MoveFailed::Here(error.to_string()))?;
    let mut store = store(app).map_err(MoveFailed::Here)?;
    let cap = loquax::config::load(&dir)
        .map_err(|error| MoveFailed::Here(error.to_string()))?
        .archive_cap_mb
        .unwrap_or(loquax::home::DEFAULT_ARCHIVE_CAP_MB)
        .saturating_mul(1 << 20);
    let mut cursor = 0;
    // The old home, by its `loquax.home` destination.
    let peer_home = loquax::home::destination_hash(&code.public_key).unwrap_or([0; 16]);
    let ratchets = match taken.ratchets.is_empty() {
        true => "differ",
        false => "same",
    };
    let mut keep = |entries: &[loquax::home::wire::Moving], cursor: &mut u64| {
        *cursor = entries.last().map_or(*cursor, |entry| entry.seq);
        // The old home's ring is written below as this one's (1305).
        loquax::events::home_sync(&peer_home, entries.len(), 0, ratchets);
        store
            .archive_moved_in(entries, cap)
            .map(|_| ())
            .map_err(|error| MoveFailed::Here(error.to_string()))
    };
    keep(&taken.entries, &mut cursor)?;
    let storage = dir.join("storage");
    std::fs::create_dir_all(&storage).map_err(|error| MoveFailed::Here(error.to_string()))?;
    if !taken.ratchets.is_empty() {
        let address = loquax::engine::delivery_address(&identity).map_err(MoveFailed::Here)?;
        let path = leviculum_std::dest_ratchet_keys_path(&storage, &address);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| MoveFailed::Here(error.to_string()))?;
        }
        leviculum_std::atomic_write_secret(&path, &taken.ratchets)
            .map_err(|error| MoveFailed::Here(error.to_string()))?;
    }
    if !taken.known.is_empty() {
        std::fs::write(storage.join("known_destinations"), &taken.known)
            .map_err(|error| MoveFailed::Here(error.to_string()))?;
    }
    if !taken.name.is_empty() {
        set_name(app, &taken.name).map_err(MoveFailed::Here)?;
    }
    // Announce often for a day: the peers that heard the old phone learn
    // the new path sooner (`loquax::home::daemon::announce_period`).
    loquax::identity::mark_moved(&dir).map_err(MoveFailed::Here)?;
    let mut tries = 0;
    loop {
        match client::confirm_move(node, &code.public_key, device, &code.token, cursor, budget)
            .await
        {
            Ok(()) => break,
            Err(HomeError::Refused(Status::Behind)) if tries < BEHIND_TRIES => {
                tries += 1;
                let more =
                    client::take_move(node, &code.public_key, device, &code.token, cursor, budget)
                        .await
                        .map_err(MoveFailed::Move)?;
                keep(&more.entries, &mut cursor)?;
            }
            Err(error) => return Err(MoveFailed::Move(error)),
        }
    }
    std::fs::write(app.join(HERE_FILE), b"")
        .map_err(|error| MoveFailed::Here(format!("{}: {error}", app.join(HERE_FILE).display())))
}

/// Whether the mailbox of `public_key` answers right now (1281), looked
/// for over this phone's code list ([`code_list`]) within `budget`: the
/// restore's guard against a second live copy.
pub fn answers_elsewhere(
    app: &Path,
    public_key: &[u8; 64],
    budget: Duration,
    runtime: &Handle,
) -> Result<bool, String> {
    let device =
        loquax::identity::load_or_create(&app.join(loquax::home::client::DEVICE_IDENTITY_FILE))
            .map_err(|error| error.to_string())?;
    let scratch = app.join(MOVING_DIR);
    let answered = runtime.block_on(async {
        let attached = leaf(app, &scratch, public_key, code_list(app), &device).await?;
        let node = attached.node();
        let answered = loquax::home::client::answers(&node, public_key, &device, budget).await;
        drop(node);
        attached.stop().await;
        Ok::<bool, String>(answered)
    });
    let _ = std::fs::remove_dir_all(&scratch);
    answered
}

/// The public key a backup line holds, read with `passphrase`.
pub fn backup_key(line: &str, passphrase: &str) -> Result<[u8; 64], String> {
    let private =
        loquax::home::backup::open(line, passphrase).map_err(|error| error.to_string())?;
    Identity::from_private_key_bytes(&private)
        .map(|identity| identity.public_key_bytes())
        .map_err(|_| "the backup holds no key".to_string())
}

/// The address's backup line, sealed with `passphrase`
/// ([`loquax::home::backup`]): the file's content and the paper code's.
pub fn backup(app: &Path, passphrase: &str) -> Result<String, String> {
    let private = identity(app)?
        .private_key_bytes()
        .map_err(|error| format!("the identity cannot be read: {error:?}"))?;
    loquax::home::backup::seal(&private, passphrase, &mut rand_core::OsRng)
        .map_err(|error| error.to_string())
}

/// Restore the address from a backup line before the first start of the
/// mailbox on this phone. A phone that holds another address already, or
/// reads a mailbox elsewhere, keeps it: a restore never overwrites one.
pub fn restore(app: &Path, line: &str, passphrase: &str) -> Result<(), String> {
    let private =
        loquax::home::backup::open(line, passphrase).map_err(|error| error.to_string())?;
    let restored =
        Identity::from_private_key_bytes(&private).map_err(|_| "the backup holds no key")?;
    if loquax::home::client::load_client(app)?.is_some() && !is_here(app) {
        return Err("This phone already reads a mailbox on a computer.".to_string());
    }
    let path = app.join(DIR).join("identity");
    if path.exists() {
        let held = identity(app)?;
        return if held.public_key_bytes() == restored.public_key_bytes() {
            Ok(())
        } else {
            Err("This phone already holds another address.".to_string())
        };
    }
    loquax::identity::store(&path, &restored).map_err(|error| error.to_string())
}

/// The running mailbox on this phone.
pub struct Here {
    app: PathBuf,
    port: u16,
    /// The platform's word on the network (1298), kept for a restart.
    network: loquax::home::rendezvous::Network,
    /// Where its node is lent at every start: the Browse tab's fetch (1337
    /// pass two) and the Rooms' hub link (1384), which reach the network
    /// through it.
    lends: loquax::home::daemon::Lends,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), String>>>,
}

impl Here {
    /// Start the home node on `runtime`, its screens' server on a free
    /// loopback port, its node lent on `lends` once it runs.
    pub fn start(
        app: &Path,
        runtime: &Handle,
        network: loquax::home::rendezvous::Network,
        lends: loquax::home::daemon::Lends,
    ) -> Result<Self, String> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("no port for the screens: {error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("no port for the screens: {error}"))?
            .port();
        let mut here = Self {
            app: app.to_path_buf(),
            port,
            network,
            lends,
            stop: None,
            task: None,
        };
        here.spawn(listener, runtime)?;
        Ok(here)
    }

    /// Where the screens dial it.
    pub fn server(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// Whether the home node ended by itself.
    pub fn is_over(&self) -> bool {
        self.task.as_ref().is_none_or(JoinHandle::is_finished)
    }

    fn spawn(&mut self, listener: std::net::TcpListener, runtime: &Handle) -> Result<(), String> {
        let dir = self.app.join(DIR);
        let storage_dir = dir.join("storage");
        std::fs::create_dir_all(&storage_dir)
            .map_err(|error| format!("{}: {error}", storage_dir.display()))?;
        let config = loquax::config::load(&dir).map_err(|error| error.to_string())?;
        let (preferred_pn, pn_source) = match config.propagation_node {
            Some(node) => (Some(node), "config"),
            None => (None, "mailbox"),
        };
        let name = name(&self.app);
        let display_name = if name.is_empty() {
            loquax::display_name::FALLBACK.to_string()
        } else {
            name
        };
        let (stop, stopped) = oneshot::channel();
        let options = DaemonOptions {
            home: dir,
            upstream: DaemonUpstream::Mesh {
                servers: servers(&self.app),
                screens: listener,
                // The phone's own mailbox dials from the phone's battery:
                // the long idle, as the screens' leaf has (1346).
                tcp: config.tcp.for_device(),
                timing: loquax::home::rendezvous::Timing::default(),
            },
            storage_dir,
            identity: identity(&self.app)?,
            display_name: display_name.into_bytes(),
            archive_cap_bytes: config
                .archive_cap_mb
                .unwrap_or(loquax::home::DEFAULT_ARCHIVE_CAP_MB)
                .saturating_mul(1 << 20),
            token_ttl_secs: loquax::home::token_ttl_secs(&config),
            policy: loquax::home::policy(&config),
            preferred_pn,
            pn_source,
            sync_bound_ms: config
                .sync_interval
                .unwrap_or(Some(loquax::sync::DEFAULT_BOUND_MS)),
            fetch_budget: Duration::from_secs(120),
            wake: true,
            stop: Some(stopped),
            network: self.network.clone(),
            lends: self.lends.clone(),
        };
        self.stop = Some(stop);
        self.task = Some(runtime.spawn(loquax::home::daemon::run(options)));
        Ok(())
    }

    /// Stop the home node and wait, up to a bound, until its node closed.
    pub async fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            if let Ok(Ok(Err(error))) = tokio::time::timeout(STOP_WAIT, task).await {
                tracing::warn!("mailbox ended: {error}");
            }
        }
    }

    /// Start it again on the same port, so the screens' TCP interface finds
    /// it when it reconnects: after a change of its name.
    pub async fn restart(&mut self, runtime: &Handle) -> Result<(), String> {
        self.stop().await;
        let listener = std::net::TcpListener::bind(("127.0.0.1", self.port))
            .map_err(|error| format!("the screens' port {}: {error}", self.port))?;
        self.spawn(listener, runtime)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_servers_are_the_rendezvous_list_on_its_peer_port() {
        let shipped: Vec<String> = DEFAULT_SERVERS.iter().map(|s| (*s).to_string()).collect();
        assert_eq!(
            shipped,
            loquax::home::rendezvous::peer_list(&loquax::home::rendezvous::default_list())
        );
    }

    #[test]
    fn the_servers_file_replaces_the_shipped_list_and_an_empty_one_does_not() {
        let app = tempfile::tempdir().expect("app dir");
        assert_eq!(servers(app.path()), ["leviculum.network:4242"]);
        std::fs::create_dir_all(app.path().join(DIR)).expect("dir");
        std::fs::write(app.path().join(DIR).join(SERVERS_FILE), "# none\n\n").expect("write");
        assert_eq!(servers(app.path()), ["leviculum.network:4242"]);
        std::fs::write(
            app.path().join(DIR).join(SERVERS_FILE),
            "rns.example.org:4242\n 10.0.0.2:4965 \n",
        )
        .expect("write");
        assert_eq!(
            servers(app.path()),
            ["rns.example.org:4242", "10.0.0.2:4965"]
        );
    }

    #[test]
    fn a_backup_restores_the_same_address_and_a_wrong_passphrase_restores_none() {
        let old = tempfile::tempdir().expect("old phone");
        let key = choose_here(old.path()).expect("chosen");
        let line = backup(old.path(), "Kastanienallee 7").expect("backup");

        let new = tempfile::tempdir().expect("new phone");
        let refused = restore(new.path(), &line, "Kastanienallee 8");
        assert!(refused.unwrap_err().contains("passphrase"));
        assert!(!new.path().join(DIR).join("identity").exists());
        restore(new.path(), &line, "Kastanienallee 7").expect("restored");
        assert_eq!(choose_here(new.path()).expect("chosen"), key);

        // Again with the same line is no change; another address is no
        // overwrite.
        restore(new.path(), &line, "Kastanienallee 7").expect("the same again");
        let other = tempfile::tempdir().expect("other phone");
        choose_here(other.path()).expect("chosen");
        let other_line = backup(other.path(), "Lindenstraße 3").expect("backup");
        let error = restore(new.path(), &other_line, "Lindenstraße 3").unwrap_err();
        assert!(error.contains("another address"), "{error}");
        assert_eq!(identity(new.path()).expect("kept").public_key_bytes(), key);
    }

    #[test]
    fn choosing_the_phone_keeps_the_choice_and_one_identity() {
        let app = tempfile::tempdir().expect("app dir");
        assert!(!is_here(app.path()));
        let first = choose_here(app.path()).expect("chosen");
        assert!(is_here(app.path()));
        assert_eq!(choose_here(app.path()).expect("again"), first);
        let code = admission(app.path()).expect("admission");
        assert_eq!(code.public_key, first);
    }

    /// A phone's link code (1264): the shipped entry servers go into it as
    /// the shipped rendezvous list, a servers file's list as it stands; the
    /// device redeeming it is seen under the name it brought, and a code
    /// withdrawn with no device is over.
    #[test]
    fn a_link_code_carries_the_list_and_sees_its_device_come() {
        let app = tempfile::tempdir().expect("app dir");
        let key = choose_here(app.path()).expect("chosen");
        assert_eq!(
            code_list(app.path()),
            loquax::home::rendezvous::default_list()
        );
        std::fs::write(
            app.path().join(DIR).join(SERVERS_FILE),
            "# a test's\n10.0.0.2:4242\n",
        )
        .expect("servers");
        assert_eq!(code_list(app.path()), ["10.0.0.2:4242"]);
        admission(app.path()).expect("the screens' own");
        let (code, linking) = link_code(app.path()).expect("a link code");
        let (parsed, list) = HomeCode::parse_with_rendezvous(&code.text()).expect("it reads back");
        assert_eq!(parsed.public_key, key);
        assert_eq!(list, ["10.0.0.2:4242"]);
        assert_eq!(
            linking.expires_at,
            code.issued_at + 3600 * loquax::home::DEFAULT_TOKEN_HOURS as i64
        );
        assert_eq!(linked(app.path(), &linking), Ok(Linked::Waiting));
        let mut store = store(app.path()).expect("store");
        let now = leviculum_std::local_time::unix_now_secs();
        store
            .redeem_token_as(&code.code.token, &[7; 16], "Pixel 8", now, 3600)
            .expect("redeemed");
        assert_eq!(
            linked(app.path(), &linking),
            Ok(Linked::Linked("Pixel 8".to_string()))
        );
        let (other, linking) = link_code(app.path()).expect("another");
        let prefix = loquax::address::to_hex(&other.code.token);
        store.revoke_token(&prefix[..8]).expect("withdrawn");
        assert_eq!(linked(app.path(), &linking), Ok(Linked::Over));
    }
}

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

/// An admission for the screens: a token issued in the mailbox's store, as
/// `loquax mailbox admit` issues one, and the code that carries it.
pub fn admission(app: &Path) -> Result<HomeCode, String> {
    let identity = identity(app)?;
    let address = loquax::engine::delivery_address(&identity)?;
    let mut store =
        loquax::store::Store::open_in(&app.join(DIR), address).map_err(|e| e.to_string())?;
    let mut token = [0u8; 16];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut token);
    store
        .issue_token(&token, SCREENS, leviculum_std::local_time::unix_now_secs())
        .map_err(|e| e.to_string())?;
    Ok(HomeCode {
        public_key: identity.public_key_bytes(),
        token,
    })
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
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), String>>>,
}

impl Here {
    /// Start the home node on `runtime`, its screens' server on a free
    /// loopback port.
    pub fn start(app: &Path, runtime: &Handle) -> Result<Self, String> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("no port for the screens: {error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("no port for the screens: {error}"))?
            .port();
        let mut here = Self {
            app: app.to_path_buf(),
            port,
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
                tcp: config.tcp,
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
}

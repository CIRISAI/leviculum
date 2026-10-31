//! Bringing a user's identity, contacts and history over from the messenger
//! they used before (`docs/src/concepts/loquax-import.md`; the survey is
//! `docs/drafts/loquax-import-2026-10.md`).
//!
//! Slice 1 reads raw 64-byte identity files of every app that writes one
//! (NomadNet, MeshChat and MeshChatX, Sideband, Ratspeak's unsealed file),
//! a pasted Base32 key, and NomadNet's whole tree: contacts, blocks, the
//! propagation node and the history. The loquax export writes NomadNet's
//! tree (`crate::export`), so the same reader takes it back.
//!
//! Ratspeak's history comes from a copy of its `ratspeak.db` the user names
//! ([`ratspeak`], 1150, 1158): two schemas, lossy, every row marked as imported
//! with no signature. On a Loquax Mailbox the imported rows go into the
//! archive too ([`archive`]), so its clients take them on their next pull.
//!
//! # The boundary
//!
//! The scan looks only at the apps' documented default locations, one level
//! deep, and reads no more than the line it shows needs: an identity file
//! for the address, directory listings for the counts. A path the user
//! names is read as whatever it turns out to be. Nothing here logs key
//! material or message content; the log (`import.log` beside the store)
//! names paths, addresses and counts.
//!
//! # The three questions
//!
//! Contacts (with names and the propagation node), default yes; history,
//! default yes; the identity, default no, adopted only on a typed `yes`
//! after the warning about running both apps on one identity. Lew,
//! 2026-10-29: the old identity is set aside and kept, and its history stays
//! in the store under its address; `retain_synced_on_node` is not offered.
//!
//! History is written under the address it belongs to (rows are scoped by
//! identity, `crate::store`): it is the live history once that identity is
//! loquax's, and waits in the store until then.

pub mod archive;
pub mod key;
mod mpack;
pub mod nomadnet;
pub mod ratchets;
pub mod ratspeak;

use std::collections::BTreeSet;
use std::fmt;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use leviculum_std::Identity;

use crate::address::to_hex;
use crate::names::{load_contacts, save_contact, Contact};
use crate::store::{ImportCounts, Store, STORE_FILE};

/// The import's log under the loquax home.
pub const LOG_FILE: &str = "import.log";

/// Why a source could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportError {
    pub path: PathBuf,
    pub detail: String,
}

impl ImportError {
    pub fn new(path: &Path, detail: impl fmt::Display) -> Self {
        Self {
            path: path.to_path_buf(),
            detail: detail.to_string(),
        }
    }
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.detail)
    }
}

impl std::error::Error for ImportError {}

/// What a found source is.
pub enum Source {
    /// A NomadNet tree, or a loquax export.
    NomadNet(nomadnet::Tree),
    /// An identity and nothing else: a raw file or a pasted key.
    Identity([u8; key::KEY_LEN]),
    /// Ratspeak's passcode vault: found and named, not read in this slice.
    Protected,
    /// A copy of Ratspeak's message database, schema 33 or 45.
    Ratspeak(ratspeak::Db),
}

/// One source found, as the screen lists it.
pub struct Found {
    /// The app it belongs to.
    pub app: &'static str,
    pub path: PathBuf,
    pub source: Source,
    /// The address its identity answers on, when it has a readable one.
    pub address: Option<[u8; 16]>,
    pub messages: usize,
    pub contacts: usize,
}

impl Found {
    /// The one line the list shows for it.
    pub fn line(&self) -> String {
        let what = match (&self.source, self.address) {
            (Source::Protected, _) => "identity protected by a passcode, not read".to_string(),
            (Source::Ratspeak(_), _) => format!("history of {} identities", self.contacts),
            (_, Some(address)) => format!("address <{}>", to_hex(&address)),
            (_, None) => "no identity".to_string(),
        };
        let counts = match &self.source {
            Source::NomadNet(_) => {
                format!("  {} messages, {} contacts", self.messages, self.contacts)
            }
            Source::Ratspeak(_) => format!("  {} messages", self.messages),
            _ => String::new(),
        };
        format!("{:<9} {}  {what}{counts}", self.app, self.path.display())
    }

    /// Whether there is anything this slice can bring over.
    pub fn importable(&self) -> bool {
        !matches!(self.source, Source::Protected)
    }

    /// The file the source app keeps its delivery destination's own
    /// ratchets in, when there is one: NomadNet's router writes
    /// `<storage>/lxmf/ratchets/<address>.ratchets` (`LXMRouter.py:130-131`
    /// and `:357`), Ratspeak its ring beside the identity
    /// (`identities/<hash>/ratchets/ring`). Taken over with the identity
    /// ([`ratchets`]).
    pub fn ratchets_file(&self) -> Option<PathBuf> {
        let path = match (&self.source, self.app) {
            (Source::NomadNet(tree), _) => tree
                .root()
                .join("storage/lxmf/ratchets")
                .join(format!("{}.ratchets", to_hex(&self.address?))),
            (Source::Identity(_), "Ratspeak") => self.path.parent()?.join("ratchets/ring"),
            _ => return None,
        };
        path.is_file().then_some(path)
    }

    /// The identity's 64 bytes, when it has one.
    pub fn key(&self) -> Result<Option<[u8; key::KEY_LEN]>, ImportError> {
        match &self.source {
            Source::NomadNet(tree) => tree.identity(),
            Source::Identity(key) => Ok(Some(*key)),
            Source::Protected | Source::Ratspeak(_) => Ok(None),
        }
    }

    /// The tag its imported rows carry (`messages.origin`).
    fn origin(&self) -> &'static str {
        match self.app {
            "Loquax" => "loquax",
            "Ratspeak" => "ratspeak",
            _ => "nomadnet",
        }
    }
}

/// Where the apps keep their identity, under the user's home, and NomadNet's
/// trees (`docs/drafts/loquax-import-2026-10.md`, "What is scanned").
fn locations(user_home: &Path, system: bool) -> Vec<(&'static str, PathBuf, Shape)> {
    let mut out = Vec::new();
    if system {
        out.push(("NomadNet", PathBuf::from("/etc/nomadnetwork"), Shape::Tree));
    }
    out.push((
        "NomadNet",
        user_home.join(".config/nomadnetwork"),
        Shape::Tree,
    ));
    out.push(("NomadNet", user_home.join(".nomadnetwork"), Shape::Tree));
    out.push((
        "Sideband",
        user_home.join(".config/sideband/app_storage/primary_identity"),
        Shape::Raw,
    ));
    out.push((
        "MeshChat",
        user_home.join(".reticulum-meshchat/identity"),
        Shape::Raw,
    ));
    out.push((
        "MeshChatX",
        user_home.join(".reticulum-meshchatx/identities"),
        Shape::Profiles("identity"),
    ));
    out.push((
        "Ratspeak",
        user_home.join(".local/share/org.ratspeak.desktop/.ratspeak/identities"),
        Shape::Profiles("identity"),
    ));
    out.push(("file", user_home.join("Downloads"), Shape::Downloads));
    out
}

/// How a location holds what it holds.
#[derive(Clone, Copy)]
enum Shape {
    /// A NomadNet tree.
    Tree,
    /// One raw identity file.
    Raw,
    /// One directory per profile, each with this raw file.
    Profiles(&'static str),
    /// Identity files the apps export, by their names.
    Downloads,
}

/// The names the apps give an exported raw identity
/// (`docs/drafts/loquax-import-2026-10.md`, "What is scanned").
fn exported_identity(name: &str) -> bool {
    name.ends_with(".rnsidentity")
        || name.ends_with("-reticulum-identity.identity")
        || name == "reticulum.raw-private-key"
        || name == "identity.bin"
}

/// Every source at the documented locations under `user_home` (and
/// `/etc/nomadnetwork` with `system`), in the order of the list.
pub fn scan(user_home: &Path, system: bool) -> Vec<Found> {
    let mut out = Vec::new();
    for (app, path, shape) in locations(user_home, system) {
        match shape {
            Shape::Tree => {
                if let Some(tree) = nomadnet::Tree::at(&path) {
                    out.push(tree_found(app, tree));
                }
            }
            Shape::Raw => out.extend(raw_found(app, &path)),
            Shape::Profiles(file) => {
                let Ok(entries) = std::fs::read_dir(&path) else {
                    continue;
                };
                let mut dirs: Vec<PathBuf> = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect();
                dirs.sort();
                for dir in dirs {
                    match raw_found(app, &dir.join(file)) {
                        Some(found) => out.push(found),
                        None if dir.join("identity.enc").is_file() => out.push(Found {
                            app,
                            path: dir.join("identity.enc"),
                            source: Source::Protected,
                            address: None,
                            messages: 0,
                            contacts: 0,
                        }),
                        None => {}
                    }
                }
            }
            Shape::Downloads => {
                let Ok(entries) = std::fs::read_dir(&path) else {
                    continue;
                };
                let mut files: Vec<PathBuf> = entries
                    .flatten()
                    .filter(|e| e.file_name().to_str().is_some_and(exported_identity))
                    .map(|e| e.path())
                    .collect();
                files.sort();
                out.extend(files.iter().filter_map(|f| raw_found(app, f)));
            }
        }
    }
    out
}

fn tree_found(app: &'static str, tree: nomadnet::Tree) -> Found {
    let address = tree
        .identity()
        .ok()
        .flatten()
        .and_then(|k| key::address_of(&k).ok());
    let messages = tree
        .conversations()
        .map(|c| c.iter().map(|c| c.files.len()).sum())
        .unwrap_or(0);
    let contacts = tree.contact_count();
    Found {
        app,
        path: tree.root().to_path_buf(),
        source: Source::NomadNet(tree),
        address,
        messages,
        contacts,
    }
}

/// A raw identity file, when `path` is one: exactly 64 bytes, read only
/// when the size says so.
fn raw_found(app: &'static str, path: &Path) -> Option<Found> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() != key::KEY_LEN as u64 {
        return None;
    }
    let key = key::from_raw(&std::fs::read(path).ok()?)?;
    Some(Found {
        app,
        path: path.to_path_buf(),
        address: key::address_of(&key).ok(),
        source: Source::Identity(key),
        messages: 0,
        contacts: 0,
    })
}

/// A path the user named, read as whatever it is: a NomadNet tree or a
/// loquax export (a directory with `storage/`), a raw identity file, or a
/// file holding a Base32 key.
pub fn sniff(path: &Path) -> Result<Found, ImportError> {
    if path.is_dir() {
        let tree = nomadnet::Tree::at(path)
            .ok_or_else(|| ImportError::new(path, "not a NomadNet tree or a Loquax export"))?;
        let app = match path.join("storage").join(crate::export::MARKER).is_file() {
            true => "Loquax",
            false => "NomadNet",
        };
        return Ok(tree_found(app, tree));
    }
    let meta = std::fs::metadata(path).map_err(|e| ImportError::new(path, e))?;
    if ratspeak::looks_like(path) {
        return ratspeak_found(ratspeak::Db::open(path)?);
    }
    if meta.len() == key::KEY_LEN as u64 {
        return raw_found("file", path)
            .ok_or_else(|| ImportError::new(path, "64 bytes, but not an identity"));
    }
    // A Base32 key is 104 characters with padding; a file much larger than
    // that is something else and is not read.
    if meta.len() <= 256 {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if let Some(key) = key::from_base32(&text) {
            return Ok(pasted("file", path, key));
        }
    }
    Err(ImportError::new(
        path,
        "not an identity file, a NomadNet tree, a Loquax export or a Ratspeak database",
    ))
}

/// A Ratspeak database as the list shows it: its messages, and how many
/// identities hold them (in `contacts`, which a database source has no other
/// use for).
fn ratspeak_found(db: ratspeak::Db) -> Result<Found, ImportError> {
    let profiles = db.profiles()?;
    Ok(Found {
        app: "Ratspeak",
        path: db.path().to_path_buf(),
        address: None,
        messages: profiles.iter().map(|p| p.messages).sum(),
        contacts: profiles.len(),
        source: Source::Ratspeak(db),
    })
}

/// A key the user pasted, or a file with one.
pub fn pasted(app: &'static str, path: &Path, key: [u8; key::KEY_LEN]) -> Found {
    Found {
        app,
        path: path.to_path_buf(),
        address: key::address_of(&key).ok(),
        source: Source::Identity(key),
        messages: 0,
        contacts: 0,
    }
}

/// Whether loquax starts for the first time in `home`: no identity, no
/// history, no config. Only then does the scan run by itself.
pub fn first_start(home: &Path) -> bool {
    ["identity", STORE_FILE, crate::config::CONFIG_FILE]
        .iter()
        .all(|name| std::fs::symlink_metadata(home.join(name)).is_err())
}

/// What the user chose for one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choices {
    pub contacts: bool,
    pub history: bool,
    pub identity: bool,
}

/// One conversation's counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationCounts {
    pub peer: [u8; 16],
    pub new: usize,
    pub held: usize,
    pub unreadable: usize,
}

/// What importing (or, in a dry run, what would be imported from) one
/// source came to.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Contacts the file did not say yet, and those it already said.
    pub contacts_new: usize,
    pub contacts_held: usize,
    pub conversations: Vec<ConversationCounts>,
    /// Attachments the source holds that are not imported (no store yet).
    pub attachments_skipped: usize,
    /// The address the history was written under.
    pub history_address: Option<[u8; 16]>,
    /// The propagation node taken over, when loquax had none.
    pub propagation_node: Option<[u8; 16]>,
    /// The source's own display name: offered, not taken.
    pub display_name: Option<String>,
    pub adopted: Option<key::Adopted>,
    /// Files that are not readable messages, with the reason.
    pub refused: Vec<(PathBuf, String)>,
    /// Messages the user had deleted in the source app, left deleted.
    pub deleted_skipped: usize,
    /// On a Loquax Mailbox: the archive entries appended (or, in a dry run,
    /// the imported messages that would be offered to the archive).
    pub archived: Option<usize>,
    /// On a Loquax Mailbox: the petname entries appended for the imported
    /// contacts' names (or, in a dry run, those that would be).
    pub names_archived: Option<usize>,
    /// On a Loquax Mailbox: the peer state entries appended for the imported
    /// blocks (or, in a dry run, those that would be).
    pub blocks_archived: Option<usize>,
    /// What the source says about itself: the identities a database holds
    /// and which one is imported, and how its times were read.
    pub notes: Vec<String>,
    /// The source app's ratchets taken over with its identity, as the
    /// line says it ([`ratchets::line`]).
    pub ratchets: Option<String>,
}

impl Report {
    /// The lines that tell the user what happened, or would.
    pub fn lines(&self, dry_run: bool) -> Vec<String> {
        let mut out = self.notes.clone();
        let would = |done: &str, maybe: &str| match dry_run {
            true => maybe.to_string(),
            false => done.to_string(),
        };
        if self.contacts_new + self.contacts_held > 0 {
            out.push(format!(
                "contacts: {} {}, {} already in the contacts file",
                self.contacts_new,
                would("added", "to add"),
                self.contacts_held
            ));
        }
        for c in &self.conversations {
            out.push(format!(
                "  <{}>  new {}  held {}  unreadable {}",
                to_hex(&c.peer),
                c.new,
                c.held,
                c.unreadable
            ));
        }
        if let Some(address) = self.history_address {
            let new: usize = self.conversations.iter().map(|c| c.new).sum();
            out.push(format!(
                "history: {new} messages {} under the address <{}>",
                would("written", "to write"),
                to_hex(&address)
            ));
        }
        if self.deleted_skipped > 0 {
            out.push(format!(
                "deleted: {} messages deleted there stay deleted",
                self.deleted_skipped
            ));
        }
        if let Some(archived) = self.archived {
            out.push(match dry_run {
                true => format!(
                    "mailbox archive: {archived} imported messages offered, its clients take \
                     them on their next sync"
                ),
                false => format!(
                    "mailbox archive: {archived} entries appended, its clients take them on \
                     their next sync"
                ),
            });
        }
        if let Some(names) = self.names_archived {
            out.push(format!(
                "mailbox archive: {names} contact names {}, its clients take them on their \
                 next sync",
                would("appended", "to append")
            ));
        }
        if let Some(blocks) = self.blocks_archived {
            out.push(format!(
                "mailbox archive: {blocks} blocks {}, its clients take them on their next \
                 sync",
                would("appended", "to append")
            ));
        }
        if self.attachments_skipped > 0 {
            out.push(format!(
                "attachments: {} skipped, Loquax has no attachment store yet",
                self.attachments_skipped
            ));
        }
        for (path, reason) in &self.refused {
            out.push(format!("unreadable: {}: {reason}", path.display()));
        }
        if let Some(line) = &self.ratchets {
            out.push(line.clone());
        }
        if let Some(pn) = self.propagation_node {
            out.push(format!(
                "propagation node: <{}> {}",
                to_hex(&pn),
                would("set, Loquax had none", "to set, Loquax has none")
            ));
        }
        if let Some(name) = &self.display_name {
            out.push(format!(
                "display name there: {name} (Loquax takes its own from --from or LNMSG_DISPLAY_NAME)"
            ));
        }
        match &self.adopted {
            Some(key::Adopted::Already) => {
                out.push("identity: Loquax already uses this identity".to_string());
            }
            Some(key::Adopted::Switched { address, set_aside }) => {
                out.push(format!(
                    "identity: Loquax now answers on <{}>",
                    to_hex(address)
                ));
                if let Some((path, old)) = set_aside {
                    out.push(format!(
                        "  the previous identity <{}> is set aside at {}; its history stays in \
                         the store, and moving the file back to `identity` switches back",
                        to_hex(old),
                        path.display()
                    ));
                }
            }
            None => {}
        }
        out
    }
}

/// Import from `found` into the loquax home `home` as `choices` says, or,
/// with `dry_run`, count what would be written and write nothing.
pub fn import(
    found: &Found,
    home: &Path,
    choices: Choices,
    dry_run: bool,
) -> Result<Report, ImportError> {
    let mut report = Report::default();
    let key = found.key()?;
    if let Source::NomadNet(tree) = &found.source {
        if choices.contacts {
            contacts(tree, home, dry_run, &mut report)?;
        }
        if choices.history {
            let key = key.ok_or_else(|| {
                ImportError::new(
                    found.path.as_path(),
                    "no identity, so no telling which messages were sent and which received",
                )
            })?;
            history(found, tree, home, &key, dry_run, &mut report)?;
        }
    }
    if let Source::Ratspeak(db) = &found.source {
        ratspeak_import(found, db, home, choices, dry_run, &mut report)?;
    }
    if choices.identity {
        let key = key.ok_or_else(|| ImportError::new(&found.path, "no identity to adopt"))?;
        if crate::home::client::load_client(home)
            .map_err(|e| ImportError::new(home, e))?
            .is_some()
        {
            return Err(ImportError::new(
                home,
                "this Loquax is a client of a home node and answers on its address; \
                 an identity is adopted on the home node",
            ));
        }
        if !dry_run {
            let adopted = key::adopt(home, &key).map_err(|e| ImportError::new(home, e))?;
            report.adopted = Some(adopted);
            if let Some(file) = found.ratchets_file() {
                match take_ratchets(home, &file, false) {
                    Ok(line) => report.ratchets = Some(line),
                    Err(error) => report.refused.push((file, error)),
                }
            }
        }
    }
    if !dry_run {
        log_report(home, found, &report);
    }
    Ok(report)
}

/// Take the ratchet file `file` over for the identity loquax answers on in
/// `home`, under the delivery lock ([`ratchets::take_over`]), and say so in
/// one line.
pub fn take_ratchets(home: &Path, file: &Path, dry_run: bool) -> Result<String, String> {
    let path = home.join("identity");
    if std::fs::symlink_metadata(&path).is_err() {
        return Err(format!(
            "{}: no identity here yet; the ratchets belong to one, import it first",
            path.display()
        ));
    }
    let identity = crate::identity::load_or_create(&path).map_err(|e| e.to_string())?;
    let _lock = match dry_run {
        true => None,
        false => Some(crate::home::lock::DeliveryLock::acquire(
            home,
            "the ratchet import",
        )?),
    };
    let (format, taken) = ratchets::import_file(home, &identity, file, dry_run)?;
    Ok(ratchets::line(file, format, &taken, dry_run))
}

fn contacts(
    tree: &nomadnet::Tree,
    home: &Path,
    dry_run: bool,
    report: &mut Report,
) -> Result<(), ImportError> {
    merge_contacts(home, tree.contacts()?, dry_run, report)?;
    let settings = tree.settings();
    take_settings(
        home,
        settings.display_name,
        settings.propagation_node,
        dry_run,
        report,
    )
}

/// Each contact the source names, merged with ours: the reader's own word
/// wins.
fn merge_contacts(
    home: &Path,
    contacts: Vec<Contact>,
    dry_run: bool,
    report: &mut Report,
) -> Result<(), ImportError> {
    let have = load_contacts(home).map_err(|e| ImportError::new(home, e))?;
    let mut names = Vec::new();
    let mut blocks = Vec::new();
    for theirs in contacts {
        let ours = have.of(&theirs.peer);
        if theirs.blocked {
            blocks.push(theirs.peer);
        }
        // The reader's own word wins: a petname loquax has stays.
        let merged = Contact {
            peer: theirs.peer,
            petname: ours.petname.clone().or(theirs.petname),
            blocked: ours.blocked || theirs.blocked,
        };
        if let Some(name) = &merged.petname {
            names.push((merged.peer, name.clone()));
        }
        if merged == ours {
            report.contacts_held += 1;
            continue;
        }
        report.contacts_new += 1;
        if !dry_run {
            leviculum_std::create_secret_dir_all(home).map_err(|e| ImportError::new(home, e))?;
            save_contact(home, &merged).map_err(|e| ImportError::new(home, e))?;
        }
    }
    archive_contacts(home, &names, &blocks, dry_run, report)
}

/// On a Loquax Mailbox: each imported contact's name as a petname entry,
/// as a client's `/petname` is kept (1159), and each imported block as a
/// peer state entry, as a client's block is kept (1162), so every device of
/// the home takes them on its next pull. A peer the archive holds an entry
/// of that kind for keeps it. In a dry run, the entries that would be
/// appended are counted.
fn archive_contacts(
    home: &Path,
    names: &[([u8; 16], String)],
    blocks: &[[u8; 16]],
    dry_run: bool,
    report: &mut Report,
) -> Result<(), ImportError> {
    let Some(address) = current_address(home) else {
        return Ok(());
    };
    let path = home.join(STORE_FILE);
    let store_error = |e: crate::store::StoreError| ImportError::new(&path, e);
    if dry_run {
        let Some(store) = Store::open_read_only(&path, address).map_err(store_error)? else {
            return Ok(());
        };
        if !store.serves_as_home_node().map_err(store_error)? {
            return Ok(());
        }
        let mut would = 0;
        for (peer, _) in names {
            if !store.archive_holds_petname(peer).map_err(store_error)? {
                would += 1;
            }
        }
        report.names_archived = Some(would);
        let mut would = 0;
        for peer in blocks {
            if !store.archive_holds_peer_state(peer).map_err(store_error)? {
                would += 1;
            }
        }
        report.blocks_archived = Some(would);
        return Ok(());
    }
    if std::fs::symlink_metadata(&path).is_err() {
        return Ok(());
    }
    let mut store = Store::open_in(home, address).map_err(store_error)?;
    if !store.serves_as_home_node().map_err(store_error)? {
        return Ok(());
    }
    let config = crate::config::load(home).map_err(|e| ImportError::new(home, e))?;
    let cap = config
        .archive_cap_mb
        .unwrap_or(crate::home::DEFAULT_ARCHIVE_CAP_MB)
        .saturating_mul(1 << 20);
    let now = leviculum_std::local_time::unix_now_secs();
    let names = names.iter().map(|(peer, name)| (peer, name.as_str()));
    let appended =
        crate::home::server::archive_petnames(&mut store, names, now, cap).map_err(store_error)?;
    report.names_archived = Some(appended);
    let appended =
        crate::home::server::archive_blocks(&mut store, blocks, now, cap).map_err(store_error)?;
    report.blocks_archived = Some(appended);
    Ok(())
}

/// The source's own name, offered, and its propagation node, taken only when
/// loquax has none.
fn take_settings(
    home: &Path,
    display_name: Option<String>,
    propagation_node: Option<[u8; 16]>,
    dry_run: bool,
    report: &mut Report,
) -> Result<(), ImportError> {
    report.display_name = display_name;
    if let Some(pn) = propagation_node {
        let config = crate::config::load(home).map_err(|e| ImportError::new(home, e))?;
        if config.propagation_node.is_none() {
            report.propagation_node = Some(pn);
            if !dry_run {
                crate::config::save(home, "propagation_node", &to_hex(&pn))
                    .map_err(|e| ImportError::new(home, e))?;
            }
        }
    }
    Ok(())
}

fn history(
    found: &Found,
    tree: &nomadnet::Tree,
    home: &Path,
    key: &[u8; key::KEY_LEN],
    dry_run: bool,
    report: &mut Report,
) -> Result<(), ImportError> {
    let own = Identity::from_private_key_bytes(key)
        .map_err(|e| ImportError::new(&found.path, format!("not an identity: {e:?}")))?;
    let address = key::address_of(key).map_err(|e| ImportError::new(&found.path, e))?;
    report.history_address = Some(address);
    let store_error = |e: crate::store::StoreError| ImportError::new(&home.join(STORE_FILE), e);
    let mut store = match dry_run {
        true => None,
        false => {
            leviculum_std::create_secret_dir_all(home).map_err(|e| ImportError::new(home, e))?;
            Some(Store::open_in(home, address).map_err(store_error)?)
        }
    };
    let peek = match dry_run {
        true => Store::open_read_only(&home.join(STORE_FILE), address).map_err(store_error)?,
        false => None,
    };
    let mut read_all = Vec::new();
    for conversation in tree.conversations()? {
        let read = tree.read(&conversation, Some(&own), address);
        report.attachments_skipped += read.attachments;
        let counts = write_conversation(
            &mut store,
            &peek,
            conversation.peer,
            found.origin(),
            &read.rows,
        )
        .map_err(store_error)?;
        report.conversations.push(ConversationCounts {
            peer: conversation.peer,
            new: counts.new,
            held: counts.held,
            unreadable: read.unreadable.len(),
        });
        report.refused.extend(read.unreadable);
        read_all.push((conversation.peer, read.rows));
    }
    // On a Loquax Mailbox the rows go into the archive too, as a Ratspeak
    // history's do (1162).
    if on_mailbox(&store, &peek).map_err(store_error)? {
        let conversations: Vec<_> = read_all
            .iter()
            .map(|(peer, rows)| (peer, rows.as_slice()))
            .collect();
        archive_history(home, found.origin(), &conversations, store.as_mut(), report)?;
    }
    Ok(())
}

/// Whether the store an import writes, or in a dry run the one it counts
/// against, is a Loquax Mailbox's.
fn on_mailbox(
    store: &Option<Store>,
    peek: &Option<Store>,
) -> Result<bool, crate::store::StoreError> {
    match (store, peek) {
        (Some(store), _) | (None, Some(store)) => store.serves_as_home_node(),
        (None, None) => Ok(false),
    }
}

/// One conversation's rows into `store`, or, in a dry run (`store` is
/// `None`), counted against what `peek` holds.
fn write_conversation(
    store: &mut Option<Store>,
    peek: &Option<Store>,
    peer: [u8; 16],
    origin: &str,
    rows: &[crate::store::ImportRow],
) -> Result<ImportCounts, crate::store::StoreError> {
    if let Some(store) = store {
        return store.import_conversation(peer, origin, rows);
    }
    let mut counts = ImportCounts::default();
    let mut seen = BTreeSet::new();
    for row in rows {
        let held = !seen.insert((row.id, row.outbound))
            || match peek {
                Some(store) => store.holds(&row.id, row.outbound)?,
                None => false,
            };
        match held {
            true => counts.held += 1,
            false => counts.new += 1,
        }
    }
    Ok(counts)
}

/// The rule the Ratspeak import reads times by, said in its report.
pub const RATSPEAK_TIME_RULE: &str = "times: Ratspeak stores unix seconds, which are UTC; \
     taken as they are, no zone applied";

/// Ratspeak's database: the identity whose address is Loquax's, or the only
/// one it holds, with its contacts, blocks, settings and history; on a
/// Loquax Mailbox the history goes into the archive too.
fn ratspeak_import(
    found: &Found,
    db: &ratspeak::Db,
    home: &Path,
    choices: Choices,
    dry_run: bool,
    report: &mut Report,
) -> Result<(), ImportError> {
    let profiles = db.profiles()?;
    let current = current_address(home);
    let chosen = match profiles
        .iter()
        .find(|p| p.address.is_some() && p.address == current)
    {
        Some(profile) => Some(profile),
        None if profiles.len() == 1 && profiles[0].address.is_some() => profiles.first(),
        None => None,
    };
    report.notes.push(format!(
        "Ratspeak schema {}, {} identities:",
        db.schema(),
        profiles.len()
    ));
    for profile in &profiles {
        let line = ratspeak::profile_line(profile, Some(profile) == chosen);
        report.notes.push(line);
    }
    let Some(profile) = chosen else {
        let reason = match current {
            Some(address) => format!(
                "none of its identities is Loquax's <{}>; adopt the one to import first \
                 (Ratspeak exports it as Base32: loquax import --key), then run again",
                to_hex(&address)
            ),
            None => "it holds several identities and Loquax has none to match; adopt the one \
                     to import first (Ratspeak exports it as Base32: loquax import --key), then \
                     run again"
                .to_string(),
        };
        if dry_run {
            report.refused.push((found.path.clone(), reason));
            return Ok(());
        }
        return Err(ImportError::new(&found.path, reason));
    };
    // `chosen` only ever names a profile with an address.
    let Some(address) = profile.address else {
        return Err(ImportError::new(
            &found.path,
            "the identity names no address",
        ));
    };
    if choices.contacts {
        merge_contacts(home, db.contacts(profile)?, dry_run, report)?;
        take_settings(
            home,
            profile.display_name.clone(),
            profile.propagation_node,
            dry_run,
            report,
        )?;
    }
    if !choices.history {
        return Ok(());
    }
    report.notes.push(RATSPEAK_TIME_RULE.to_string());
    report.history_address = Some(address);
    let read = db.history(profile, address)?;
    report.deleted_skipped = read.deleted;
    if read.replies_linked + read.replies_unlinked > 0 {
        report.notes.push(format!(
            "replies: {} linked to their message, {} whose message is not imported come as \
             text alone",
            read.replies_linked, read.replies_unlinked
        ));
    }
    if read.reactions + read.reactions_skipped > 0 {
        report.notes.push(format!(
            "reactions: {} imported, {} skipped: their message is not imported, or the \
             sender is neither side of it",
            read.reactions, read.reactions_skipped
        ));
    }
    report
        .refused
        .extend(read.unreadable.iter().map(|(id, reason)| {
            (
                PathBuf::from(format!("{} message {id:?}", found.path.display())),
                reason.clone(),
            )
        }));
    let store_error = |e: crate::store::StoreError| ImportError::new(&home.join(STORE_FILE), e);
    let mut store = match dry_run {
        true => None,
        false => {
            leviculum_std::create_secret_dir_all(home).map_err(|e| ImportError::new(home, e))?;
            Some(Store::open_in(home, address).map_err(store_error)?)
        }
    };
    let peek = match dry_run {
        true => Store::open_read_only(&home.join(STORE_FILE), address).map_err(store_error)?,
        false => None,
    };
    for conversation in &read.conversations {
        report.attachments_skipped += conversation.attachments;
        let counts = write_conversation(
            &mut store,
            &peek,
            conversation.peer,
            found.origin(),
            &conversation.rows,
        )
        .map_err(store_error)?;
        report.conversations.push(ConversationCounts {
            peer: conversation.peer,
            new: counts.new,
            held: counts.held,
            unreadable: 0,
        });
    }
    if on_mailbox(&store, &peek).map_err(store_error)? {
        let conversations: Vec<_> = read
            .conversations
            .iter()
            .map(|c| (&c.peer, c.rows.as_slice()))
            .collect();
        archive_history(home, found.origin(), &conversations, store.as_mut(), report)?;
    }
    Ok(())
}

/// On a Loquax Mailbox: every imported message of `conversations` as an
/// archive entry ([`archive::entry`]), all conversations in one time order,
/// so the clients take them on their next pull. A message the archive holds
/// already is not appended again. An import that would push the archive
/// past its cap writes no entry and says so: the cap evicts the oldest
/// entries, which may be ones a client has not pulled yet.
fn archive_history(
    home: &Path,
    origin: &str,
    conversations: &[(&[u8; 16], &[crate::store::ImportRow])],
    store: Option<&mut Store>,
    report: &mut Report,
) -> Result<(), ImportError> {
    let mut rows: Vec<(&[u8; 16], &crate::store::ImportRow)> = conversations
        .iter()
        .flat_map(|(peer, rows)| rows.iter().map(move |row| (*peer, row)))
        .collect();
    let when = |row: &crate::store::ImportRow| {
        row.timestamp
            .or(row.received_at.map(|secs| secs as f64))
            .unwrap_or(0.0)
    };
    rows.sort_by(|(_, a), (_, b)| when(a).total_cmp(&when(b)).then(a.id.cmp(&b.id)));
    let Some(store) = store else {
        report.archived = Some(rows.len());
        return Ok(());
    };
    let entries: Vec<([u8; 32], i64, Vec<u8>)> = rows
        .iter()
        .map(|(peer, row)| {
            let at = row.received_at.unwrap_or(0);
            (row.id, at, archive::entry(origin, peer, row))
        })
        .collect();
    let config = crate::config::load(home).map_err(|e| ImportError::new(home, e))?;
    let cap = config
        .archive_cap_mb
        .unwrap_or(crate::home::DEFAULT_ARCHIVE_CAP_MB)
        .saturating_mul(1 << 20);
    let store_error = |e: crate::store::StoreError| ImportError::new(&home.join(STORE_FILE), e);
    let held = store.archive_bytes().map_err(store_error)?;
    let adding: u64 = entries.iter().map(|(_, _, bytes)| bytes.len() as u64).sum();
    if held.saturating_add(adding) > cap {
        report.refused.push((
            home.join(STORE_FILE),
            format!(
                "the mailbox archive holds {held} bytes of its cap of {cap}; the {adding} \
                 bytes of imported history would evict entries, so none was appended: raise \
                 archive_cap_mb and run the import again"
            ),
        ));
        return Ok(());
    }
    report.archived = Some(store.archive_imported(&entries, cap).map_err(store_error)?);
    Ok(())
}

/// `IMPORT` lines in `home/import.log` and the tracing log: one for the
/// source, one a conversation, one a refused file. Never a key, never a
/// message's content.
fn log_report(home: &Path, found: &Found, report: &Report) {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let source = found.origin();
    let mut lines = vec![format!(
        "IMPORT source={source} path={} address={} contacts_new={} contacts_held={} \
         attachments_skipped={} deleted_skipped={} archived={} adopted={} t={t}",
        found.path.display(),
        found
            .address
            .or(report.history_address)
            .map_or("none".to_string(), |a| to_hex(&a)),
        report.contacts_new,
        report.contacts_held,
        report.attachments_skipped,
        report.deleted_skipped,
        report
            .archived
            .map_or("none".to_string(), |n| n.to_string()),
        matches!(report.adopted, Some(key::Adopted::Switched { .. })),
    )];
    for c in &report.conversations {
        lines.push(format!(
            "IMPORT source={source} conv={} new={} held={} unreadable={} t={t}",
            to_hex(&c.peer),
            c.new,
            c.held,
            c.unreadable
        ));
    }
    for (path, reason) in &report.refused {
        lines.push(format!(
            "IMPORT source={source} refused={} reason={:?} t={t}",
            path.display(),
            reason
        ));
    }
    for line in &lines {
        tracing::info!("{line}");
    }
    let path = home.join(LOG_FILE);
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| lines.iter().try_for_each(|line| writeln!(file, "{line}")));
    if let Err(error) = written {
        tracing::warn!("{}: {error}", path.display());
    }
}

/// The warning before an identity is adopted (`loquax-import.md`, "One
/// identity in two apps").
pub fn identity_warning(found: &Found, current: Option<[u8; 16]>) -> String {
    let address = found
        .address
        .map_or("?".to_string(), |a| format!("<{}>", to_hex(&a)));
    let mut text = format!(
        "Adopting this identity makes Loquax answer on {address}, the address {} uses.\n\
         Do not run {} and Loquax on this identity at the same time:\n\
         - both announce it, so peers' paths flap between the two and a direct message\n  \
           reaches only one of them;\n\
         - both collect from the same propagation node, and the first to sync takes the mail;\n\
         - both may prove the same message, so its sender sees the wrong device.\n",
        found.app, found.app
    );
    if let Some(current) = current {
        text.push_str(&format!(
            "Loquax's current identity <{}> is set aside, not deleted; its history stays.\n",
            to_hex(&current)
        ));
    }
    text
}

/// The questions on a terminal or a pipe, for each source in turn: lines in
/// from `input`, text out to `output`. An empty answer, or the end of the
/// input, takes the default; the identity needs the word `yes`. One identity
/// at most is adopted in a run. Returns the reports, one per source asked.
pub fn ask_and_import<R: BufRead, W: Write>(
    found: &[Found],
    home: &Path,
    input: &mut R,
    output: &mut W,
) -> std::io::Result<Vec<Result<Report, ImportError>>> {
    let mut reports = Vec::new();
    let mut adopted = false;
    for source in found.iter().filter(|f| f.importable()) {
        writeln!(output, "\n{}", source.line())?;
        let tree = matches!(source.source, Source::NomadNet(_) | Source::Ratspeak(_));
        let contacts = tree && yes_no(input, output, "Import contacts and names? [Y/n] ")?;
        let history = tree && yes_no(input, output, "Import the history? [Y/n] ")?;
        let current = current_address(home);
        let offer = !adopted && source.address.is_some() && source.address != current;
        let identity = offer && {
            write!(output, "\n{}", identity_warning(source, current))?;
            write!(
                output,
                "Type yes to adopt this identity, anything else keeps Loquax's: "
            )?;
            output.flush()?;
            read_line(input)?.trim() == "yes"
        };
        if !(contacts || history || identity) {
            writeln!(output, "nothing imported from {}", source.path.display())?;
            continue;
        }
        let choices = Choices {
            contacts,
            history,
            identity,
        };
        let report = import(source, home, choices, false);
        match &report {
            Ok(report) => {
                adopted |= matches!(report.adopted, Some(key::Adopted::Switched { .. }));
                for line in report.lines(false) {
                    writeln!(output, "{line}")?;
                }
            }
            Err(error) => writeln!(output, "not imported: {error}")?,
        }
        reports.push(report);
    }
    Ok(reports)
}

/// The address of loquax's identity in `home`, when it has a readable one.
pub fn current_address(home: &Path) -> Option<[u8; 16]> {
    let bytes = std::fs::read(home.join("identity")).ok()?;
    let identity = leviculum_core::identity_store::decode_identity(&bytes)?;
    crate::engine::delivery_address(&identity).ok()
}

fn yes_no<R: BufRead, W: Write>(input: &mut R, output: &mut W, ask: &str) -> std::io::Result<bool> {
    write!(output, "{ask}")?;
    output.flush()?;
    let answer = read_line(input)?;
    Ok(!matches!(answer.trim(), "n" | "N" | "no" | "No" | "NO"))
}

fn read_line<R: BufRead>(input: &mut R) -> std::io::Result<String> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(line)
}

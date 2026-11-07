//! The import and the export (`loquax::import`, `loquax::export`), on
//! fixtures written by the tests from the formats
//! (`docs/drafts/loquax-import-2026-10.md`): a NomadNet tree, raw identity
//! files, a Base32 key. No messenger's real data directory is read: every
//! path here is under a temporary directory, and the scan is pointed at a
//! fake home with the system location off.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use leviculum_lxmf::message::{DeliveryMethod, Message as Lxm};
use leviculum_lxmf::msgpack;
use leviculum_std::Identity;
use leviculum_ui::delivery::DeliveryState;
use loquax::engine::delivery_address;
use loquax::import::{self, key, Choices, Source};
use loquax::store::{Store, STORE_FILE};
use loquax::ui::history::StoreWrite;
use loquax::ui::{LedgerEntry, Message, Signature};

fn identity() -> Identity {
    leviculum_std::generate_identity()
}

fn raw(identity: &Identity) -> [u8; 64] {
    identity.private_key_bytes().expect("key")
}

fn address(identity: &Identity) -> [u8; 16] {
    delivery_address(identity).expect("address")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const ALL: Choices = Choices {
    contacts: true,
    history: true,
    identity: false,
};

/// LXMF's `packed_container()`, as NomadNet writes each message.
fn container(state: i64, packed: &[u8], method: i64) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::map(&mut out, 5);
    msgpack::string(&mut out, "state");
    msgpack::int(&mut out, state);
    msgpack::string(&mut out, "lxmf_bytes");
    msgpack::bin(&mut out, packed);
    msgpack::string(&mut out, "transport_encrypted");
    msgpack::bool(&mut out, false);
    msgpack::string(&mut out, "transport_encryption");
    msgpack::nil(&mut out);
    msgpack::string(&mut out, "method");
    msgpack::int(&mut out, method);
    out
}

fn message(from: &Identity, to: [u8; 16], ts: f64, body: &str) -> Lxm {
    Lxm::create(
        to,
        address(from),
        from,
        ts,
        Vec::new(),
        body.as_bytes().to_vec(),
        Vec::new(),
        DeliveryMethod::Direct,
    )
    .expect("message")
}

/// A NomadNet tree for `own`, talking to `peer`.
struct NomadNet {
    root: PathBuf,
    own: Identity,
    peer: Identity,
    /// The ids, in time order: sent, received intact, received stripped.
    ids: Vec<[u8; 32]>,
}

fn nomadnet(base: &Path) -> NomadNet {
    let root = base.join(".nomadnetwork");
    let storage = root.join("storage");
    let own = identity();
    let peer = identity();
    let (me, them) = (address(&own), address(&peer));
    let conv = storage.join("conversations").join(hex(&them));
    std::fs::create_dir_all(&conv).expect("mkdir");
    std::fs::write(storage.join("identity"), raw(&own)).expect("identity");

    let sent = message(&own, them, 1_000.0, "hello from nomadnet");
    std::fs::write(
        conv.join(hex(&sent.message_id)),
        container(0x08, &sent.pack(), 2),
    )
    .expect("sent");
    let received = message(&peer, me, 2_000.0, "an answer");
    std::fs::write(
        conv.join(hex(&received.message_id)),
        container(0x08, &received.pack(), 2),
    )
    .expect("received");
    // A message with a file attachment, as NomadNet keeps it: the field
    // moved out to `attachments/<id>/`, the bytes rewritten without it, the
    // original id kept as the name, and its own verdict in `.index`.
    let mut file = Vec::new();
    msgpack::array(&mut file, 1);
    msgpack::array(&mut file, 2);
    msgpack::string(&mut file, "notes.txt");
    msgpack::bin(&mut file, b"attached");
    let mut attached = Lxm::create(
        me,
        them,
        &peer,
        3_000.0,
        Vec::new(),
        b"see the file".to_vec(),
        vec![(5, file)],
        DeliveryMethod::Direct,
    )
    .expect("attached");
    let id = attached.message_id;
    attached.fields.clear();
    std::fs::write(conv.join(hex(&id)), container(0x08, &attached.pack(), 2)).expect("stripped");
    let att = storage.join("attachments").join(hex(&id));
    std::fs::create_dir_all(&att).expect("attachments");
    std::fs::write(att.join("file_0"), b"attached").expect("file");
    let mut manifest = Vec::new();
    msgpack::map(&mut manifest, 1);
    msgpack::string(&mut manifest, "files");
    msgpack::array(&mut manifest, 1);
    msgpack::map(&mut manifest, 1);
    msgpack::string(&mut manifest, "name");
    msgpack::string(&mut manifest, "notes.txt");
    std::fs::write(att.join("manifest"), manifest).expect("manifest");
    let mut index = Vec::new();
    msgpack::map(&mut index, 1);
    msgpack::string(&mut index, &hex(&id));
    msgpack::map(&mut index, 2);
    msgpack::string(&mut index, "signature_validated");
    msgpack::bool(&mut index, true);
    msgpack::string(&mut index, "sort_timestamp");
    msgpack::f64(&mut index, 3_005.5);
    std::fs::write(conv.join(".index"), index).expect("index");
    // A file that is not a message, and a temporary one NomadNet left.
    std::fs::write(conv.join("ab".repeat(32)), b"garbage").expect("garbage");
    std::fs::write(conv.join(format!("{}.tmp.1.x", "cd".repeat(32))), b"tmp").expect("tmp");

    // The directory: the peer named, a node entry, a short old entry.
    let blocked = [0xbb; 16];
    let other = [0xcc; 16];
    let mut directory = Vec::new();
    msgpack::map(&mut directory, 2);
    msgpack::string(&mut directory, "entry_list");
    msgpack::array(&mut directory, 3);
    msgpack::array(&mut directory, 8);
    msgpack::bin(&mut directory, &them);
    msgpack::string(&mut directory, "Bob");
    msgpack::int(&mut directory, 0xff);
    msgpack::bool(&mut directory, false);
    msgpack::nil(&mut directory);
    msgpack::bool(&mut directory, false);
    msgpack::nil(&mut directory);
    msgpack::string(&mut directory, "");
    msgpack::array(&mut directory, 4);
    msgpack::bin(&mut directory, &[0xdd; 16]);
    msgpack::string(&mut directory, "a node");
    msgpack::int(&mut directory, 2);
    msgpack::bool(&mut directory, true);
    msgpack::array(&mut directory, 3);
    msgpack::bin(&mut directory, &other);
    msgpack::string(&mut directory, "Carol");
    msgpack::int(&mut directory, 2);
    msgpack::string(&mut directory, "announce_stream");
    msgpack::array(&mut directory, 0);
    std::fs::write(storage.join("directory"), directory).expect("directory");
    std::fs::write(
        root.join("ignored"),
        format!("{}\nnot hex\n", hex(&blocked)),
    )
    .expect("ignored");
    let mut settings = Vec::new();
    msgpack::map(&mut settings, 2);
    msgpack::string(&mut settings, "display_name");
    msgpack::string(&mut settings, "Nomad Me");
    msgpack::string(&mut settings, "propagation_node");
    msgpack::bin(&mut settings, &[0xee; 16]);
    std::fs::write(storage.join("peersettings"), settings).expect("peersettings");
    NomadNet {
        root,
        own,
        peer,
        ids: vec![sent.message_id, received.message_id, id],
    }
}

fn found(tree: &NomadNet) -> import::Found {
    import::sniff(&tree.root).expect("a NomadNet tree")
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            match path.is_dir() {
                true => out.extend(files_under(&path)),
                false => out.push(path),
            }
        }
    }
    out
}

fn conversation(home: &Path, me: [u8; 16], peer: [u8; 16]) -> Vec<Message> {
    let store = Store::open_in(home, me).expect("store");
    store
        .restore()
        .expect("restore")
        .into_iter()
        .find(|c| c.peer == peer)
        .map(|c| c.messages)
        .unwrap_or_default()
}

#[test]
fn a_dry_run_counts_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("dir");
    let tree = nomadnet(dir.path());
    let home = dir.path().join("loquax");
    let report = import::import(&found(&tree), &home, ALL, true).expect("dry run");
    assert!(!home.exists(), "a dry run made {:?}", files_under(&home));
    assert_eq!(report.conversations.len(), 1);
    let c = &report.conversations[0];
    assert_eq!((c.new, c.held, c.unreadable), (3, 0, 1));
    assert_eq!(report.contacts_new, 3, "Bob, Carol, the blocked peer");
    assert_eq!(report.attachments_skipped, 1);
    assert_eq!(report.propagation_node, Some([0xee; 16]));
    assert!(report
        .lines(true)
        .iter()
        .any(|l| l.contains("3 messages to write")));
}

#[test]
fn contacts_names_blocks_and_the_propagation_node_come_over_and_ours_win() {
    let dir = tempfile::tempdir().expect("dir");
    let tree = nomadnet(dir.path());
    let home = dir.path().join("loquax");
    std::fs::create_dir_all(&home).expect("home");
    let them = address(&tree.peer);
    std::fs::write(
        home.join("contacts"),
        format!("# mine\nname {} Robert\n", hex(&them)),
    )
    .expect("contacts");
    let choices = Choices {
        history: false,
        ..ALL
    };
    let report = import::import(&found(&tree), &home, choices, false).expect("import");
    assert_eq!((report.contacts_new, report.contacts_held), (2, 1));
    let contacts = loquax::names::load_contacts(&home).expect("contacts");
    assert_eq!(
        contacts.petnames.get(&them).map(String::as_str),
        Some("Robert")
    );
    assert_eq!(
        contacts.petnames.get(&[0xcc; 16]).map(String::as_str),
        Some("Carol")
    );
    assert!(!contacts.petnames.contains_key(&[0xdd; 16]), "a node entry");
    assert!(contacts.blocked.contains(&[0xbb; 16]));
    let text = std::fs::read_to_string(home.join("contacts")).expect("text");
    assert!(
        text.starts_with("# mine\n"),
        "the reader's lines stay: {text}"
    );
    let config = loquax::config::load(&home).expect("config");
    assert_eq!(config.propagation_node, Some([0xee; 16]));
    assert!(!home.join(STORE_FILE).exists(), "no history was asked for");
    assert!(!home.join("identity").exists(), "nor the identity");
}

#[test]
fn history_verifies_what_it_can_and_keeps_the_source_verdict_as_the_sources() {
    let dir = tempfile::tempdir().expect("dir");
    let tree = nomadnet(dir.path());
    let home = dir.path().join("loquax");
    let report = import::import(&found(&tree), &home, ALL, false).expect("import");
    let (me, them) = (address(&tree.own), address(&tree.peer));
    assert_eq!(report.history_address, Some(me));
    assert_eq!(report.refused.len(), 1, "{:?}", report.refused);
    let messages = conversation(&home, me, them);
    let bodies: Vec<&str> = messages.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies, ["hello from nomadnet", "an answer", "see the file"]);
    let ids: Vec<[u8; 32]> = messages.iter().filter_map(|m| m.id).collect();
    assert_eq!(ids, tree.ids);

    // Ours, signed with the tree's identity: checked, and it holds.
    assert!(messages[0].outbound);
    assert_eq!(messages[0].signature, Signature::Verified);
    assert_eq!(messages[0].delivery, Some(DeliveryState::Arrived));
    // Intact from a sender whose key loquax does not have: unverified, its
    // bytes kept for the check when the key comes.
    assert_eq!(messages[1].signature, Signature::Unverified);
    let store = Store::open_in(&home, me).expect("store");
    let held = store.unverified().expect("held");
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].0, tree.ids[1]);
    assert_eq!(held[0].1, them);
    let kept = Lxm::unpack(&held[0].2, None, Some(&tree.peer), DeliveryMethod::Direct)
        .expect("kept bytes");
    assert_eq!(
        kept.verification,
        leviculum_lxmf::Verification::Valid,
        "the kept bytes are the signed ones"
    );
    // Stripped of its attachment: not ours to call invalid; NomadNet's word
    // is shown as NomadNet's.
    assert_eq!(messages[2].signature, Signature::Unverified);
    let ledger: Vec<&str> = messages[2].ledger.iter().map(|e| e.what.as_str()).collect();
    assert_eq!(
        ledger,
        [
            "imported from NomadNet",
            "NomadNet said: signature verified"
        ]
    );
    assert_eq!(messages[2].at.map(|at| at.unix_secs), Some(3_005));
    let log = std::fs::read_to_string(home.join(import::LOG_FILE)).expect("log");
    assert!(log.contains(&format!("conv={} new=3 held=0 unreadable=1", hex(&them))));
    assert!(
        !log.contains("hello from nomadnet"),
        "no content in the log"
    );
    assert!(!log.contains(&hex(&raw(&tree.own))), "no key in the log");
}

#[test]
fn a_second_run_is_idempotent() {
    let dir = tempfile::tempdir().expect("dir");
    let tree = nomadnet(dir.path());
    let home = dir.path().join("loquax");
    import::import(&found(&tree), &home, ALL, false).expect("first");
    let again = import::import(&found(&tree), &home, ALL, false).expect("second");
    let c = &again.conversations[0];
    assert_eq!((c.new, c.held), (0, 3));
    assert_eq!(again.contacts_new, 0);
    let me = address(&tree.own);
    let store = Store::open_in(&home, me).expect("store");
    assert_eq!(store.message_count().expect("count"), 3);
    let dry = import::import(&found(&tree), &home, ALL, true).expect("dry");
    assert_eq!(
        (dry.conversations[0].new, dry.conversations[0].held),
        (0, 3)
    );
}

fn own_message(key: u64, body: &str, id: [u8; 32]) -> Message {
    Message {
        outbound: true,
        body: body.to_string(),
        id: Some(id),
        delivery: Some(DeliveryState::Delivered),
        signature: Signature::Verified,
        leg: None,
        unproven: false,
        at: None,
        size: body.len(),
        ledger: vec![LedgerEntry {
            at: None,
            what: "queued".to_string(),
        }],
        last_leg: None,
        activity: key,
        fields: Vec::new(),
    }
}

/// Imported history is older than what loquax wrote, so it sits below it,
/// and a later import of still older messages goes first without moving
/// loquax's own rows.
#[test]
fn imported_history_sorts_below_loquaxs_own_and_a_later_import_stays_in_time_order() {
    let dir = tempfile::tempdir().expect("dir");
    let tree = nomadnet(dir.path());
    let home = dir.path().join("loquax");
    let (me, them) = (address(&tree.own), address(&tree.peer));
    std::fs::create_dir_all(&home).expect("home");
    let mut store = Store::open_in(&home, me).expect("store");
    let key = store.last_key().expect("key") + 1;
    store
        .write(&[StoreWrite::Message {
            peer: them,
            message: own_message(key, "written in loquax", [0x42; 32]),
        }])
        .expect("own");
    drop(store);
    import::import(&found(&tree), &home, ALL, false).expect("import");
    // An older message turns up in a second import.
    let older = message(&tree.own, them, 500.0, "the oldest");
    let conv = tree.root.join("storage/conversations").join(hex(&them));
    std::fs::write(
        conv.join(hex(&older.message_id)),
        container(0x08, &older.pack(), 2),
    )
    .expect("older");
    let second = import::import(&found(&tree), &home, ALL, false).expect("second");
    assert_eq!(
        (second.conversations[0].new, second.conversations[0].held),
        (1, 3)
    );
    let messages = conversation(&home, me, them);
    let bodies: Vec<&str> = messages.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(
        bodies,
        [
            "the oldest",
            "hello from nomadnet",
            "an answer",
            "see the file",
            "written in loquax"
        ]
    );
    assert_eq!(messages[4].activity, key, "loquax's own row keeps its key");
    let store = Store::open_in(&home, me).expect("store");
    assert_eq!(store.last_key().expect("key"), key);
    // The text index follows the rewritten block.
    let mut loaded = std::collections::BTreeMap::new();
    loaded.insert(them, u64::MAX);
    let hits = store.search("oldest", &loaded).expect("search");
    assert_eq!(hits.len(), 1);
}

#[test]
fn the_scan_looks_only_at_the_documented_places() {
    let dir = tempfile::tempdir().expect("dir");
    let user = dir.path();
    let tree = nomadnet(user);
    let write = |path: &Path, bytes: &[u8]| {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, bytes).expect("write");
    };
    let sideband = identity();
    write(
        &user.join(".config/sideband/app_storage/primary_identity"),
        &raw(&sideband),
    );
    let meshchatx = identity();
    write(
        &user.join(".reticulum-meshchatx/identities/aaaa/identity"),
        &raw(&meshchatx),
    );
    write(
        &user.join(".local/share/org.ratspeak.desktop/.ratspeak/identities/bbbb/identity.enc"),
        b"{\"version\": 1}",
    );
    let exported = identity();
    write(&user.join("Downloads/mine.rnsidentity"), &raw(&exported));
    // Not looked at: another name in Downloads, a deeper directory.
    write(&user.join("Downloads/other.bin"), &raw(&identity()));
    write(
        &user.join(".reticulum-meshchatx/identities/aaaa/deeper/identity"),
        &raw(&identity()),
    );
    let found = import::scan(user, false);
    let apps: Vec<&str> = found.iter().map(|f| f.app).collect();
    assert_eq!(
        apps,
        ["NomadNet", "Sideband", "MeshChatX", "Ratspeak", "file"]
    );
    assert_eq!(found[0].address, Some(address(&tree.own)));
    assert_eq!((found[0].messages, found[0].contacts), (4, 3));
    assert_eq!(found[1].address, Some(address(&sideband)));
    assert_eq!(found[2].address, Some(address(&meshchatx)));
    assert!(matches!(found[3].source, Source::Protected));
    assert!(!found[3].importable());
    assert!(found[3].line().contains("protected by a passcode"));
    assert_eq!(found[4].address, Some(address(&exported)));
}

#[test]
fn every_adoption_path_takes_the_address_and_sets_the_old_one_aside() {
    let dir = tempfile::tempdir().expect("dir");
    let identity_only = Choices {
        contacts: false,
        history: false,
        identity: true,
    };
    // A raw file, a Base32 file, a pasted key, a NomadNet tree.
    let raw_id = identity();
    let raw_path = dir.path().join("x.rnsidentity");
    std::fs::write(&raw_path, raw(&raw_id)).expect("raw");
    let b32_id = identity();
    let b32_path = dir.path().join("key.txt");
    std::fs::write(&b32_path, key::to_base32(&raw(&b32_id))).expect("b32");
    let pasted_id = identity();
    let tree = nomadnet(dir.path());
    let sources = [
        (import::sniff(&raw_path).expect("raw"), address(&raw_id)),
        (import::sniff(&b32_path).expect("b32"), address(&b32_id)),
        (
            import::pasted("pasted", Path::new("(stdin)"), raw(&pasted_id)),
            address(&pasted_id),
        ),
        (found(&tree), address(&tree.own)),
    ];
    for (n, (source, want)) in sources.iter().enumerate() {
        let home = dir.path().join(format!("home{n}"));
        let before =
            loquax::identity::load_or_create(&home.join("identity")).expect("loquax's own");
        let old = address(&before);
        let report = import::import(source, &home, identity_only, false).expect("adopt");
        assert_eq!(import::current_address(&home), Some(*want), "source {n}");
        let aside = home.join(format!("identity.{}.bak", hex(&old)));
        assert!(aside.is_file(), "source {n}: the old identity is kept");
        assert!(report.lines(false).iter().any(|l| l.contains("set aside")));
    }
}

/// The old identity's history stays in the store under its address, and
/// moving the file back brings both back.
#[test]
fn the_set_aside_identity_keeps_its_history() {
    let dir = tempfile::tempdir().expect("dir");
    let home = dir.path().join("loquax");
    let mine = loquax::identity::load_or_create(&home.join("identity")).expect("own");
    let old = address(&mine);
    let mut store = Store::open_in(&home, old).expect("store");
    let key = store.last_key().expect("key") + 1;
    store
        .write(&[StoreWrite::Message {
            peer: [0x55; 16],
            message: own_message(key, "before the switch", [0x43; 32]),
        }])
        .expect("write");
    drop(store);
    let new = identity();
    let source = import::pasted("pasted", Path::new("(stdin)"), raw(&new));
    let identity_only = Choices {
        contacts: false,
        history: false,
        identity: true,
    };
    import::import(&source, &home, identity_only, false).expect("adopt");
    assert!(conversation(&home, address(&new), [0x55; 16]).is_empty());
    std::fs::rename(
        home.join(format!("identity.{}.bak", hex(&old))),
        home.join("identity"),
    )
    .expect("switch back");
    assert_eq!(import::current_address(&home), Some(old));
    let back = conversation(&home, old, [0x55; 16]);
    assert_eq!(back[0].body, "before the switch");
}

#[test]
fn the_questions_take_defaults_and_adopt_only_on_yes() {
    let dir = tempfile::tempdir().expect("dir");
    let tree = nomadnet(dir.path());
    let sources = [found(&tree)];

    let home = dir.path().join("defaults");
    let mut out = Vec::new();
    let reports =
        import::ask_and_import(&sources, &home, &mut &b"\n\ny\n"[..], &mut out).expect("asked");
    let text = String::from_utf8(out).expect("utf-8");
    assert!(text.contains("Do not run NomadNet and Loquax"), "{text}");
    assert_eq!(reports.len(), 1);
    assert!(reports[0].as_ref().expect("ok").adopted.is_none());
    assert_eq!(import::current_address(&home), None, "y is not yes");
    assert_eq!(
        Store::open_in(&home, address(&tree.own))
            .expect("store")
            .message_count()
            .expect("count"),
        3,
        "history is yes by default"
    );

    let home = dir.path().join("yes");
    let mut out = Vec::new();
    import::ask_and_import(&sources, &home, &mut &b"n\nn\nyes\n"[..], &mut out).expect("asked");
    assert_eq!(import::current_address(&home), Some(address(&tree.own)));
    assert!(!home.join(STORE_FILE).exists(), "history was declined");
    assert!(!home.join("contacts").exists(), "contacts were declined");

    let home = dir.path().join("eof");
    let mut out = Vec::new();
    import::ask_and_import(&sources, &home, &mut &b""[..], &mut out).expect("asked");
    assert_eq!(import::current_address(&home), None);
}

/// Export, import into an empty loquax, and the same messages come back in
/// the same verification state: ours verified by the signature the export
/// made, a held message held again with its bytes, and a message loquax
/// verified on arrival, whose bytes the store does not keep, with
/// loquax's verdict shown as the export's word.
#[test]
fn the_export_round_trips_through_the_import() {
    let dir = tempfile::tempdir().expect("dir");
    let a = dir.path().join("a");
    let own = loquax::identity::load_or_create(&a.join("identity")).expect("own");
    let me = address(&own);
    let peer = identity();
    let them = address(&peer);
    std::fs::write(
        a.join("contacts"),
        format!("name {} Bob\nblock {}\n", hex(&them), hex(&[0xbb; 16])),
    )
    .expect("contacts");
    let mut store = Store::open_in(&a, me).expect("store");
    let base = store.last_key().expect("key");
    let held = message(&peer, me, 4_000.0, "held for its key");
    let received = |key: u64, body: &str, id: [u8; 32], signature| Message {
        outbound: false,
        delivery: None,
        signature,
        ledger: Vec::new(),
        ..own_message(key, body, id)
    };
    store
        .write(&[
            StoreWrite::Message {
                peer: them,
                message: own_message(base + 1, "sent from loquax", [0x11; 32]),
            },
            StoreWrite::Message {
                peer: them,
                message: received(
                    base + 2,
                    "verified on arrival",
                    [0x22; 32],
                    Signature::Verified,
                ),
            },
            StoreWrite::Message {
                peer: them,
                message: received(
                    base + 3,
                    "held for its key",
                    held.message_id,
                    Signature::Unverified,
                ),
            },
        ])
        .expect("write");
    store
        .keep_unverified(&held.message_id, &them, &held.pack())
        .expect("keep");
    drop(store);
    // The delivery destination's ratchets, as the node keeps them (1185).
    let ratchet = [0x5au8; 32];
    loquax::import::ratchets::take_over(&a, &own, &[ratchet], false).expect("ratchets");

    let out = dir.path().join("export");
    let done = loquax::export::export(&a, &out).expect("export");
    assert!(
        out.join(format!("storage/lxmf/ratchets/{}.ratchets", hex(&me)))
            .is_file(),
        "the ratchets go where NomadNet's router loads them"
    );
    assert_eq!(
        (done.messages, done.conversations, done.contacts),
        (3, 1, 1)
    );
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(out.join("storage/identity"))
            .expect("identity")
            .permissions()
            .mode()
    };
    assert_eq!(mode & 0o777, 0o600);
    assert!(
        loquax::export::export(&a, &out).is_err(),
        "an export never writes over one"
    );

    let b = dir.path().join("b");
    let source = import::sniff(&out).expect("the export reads as a tree");
    assert_eq!(source.app, "Loquax");
    assert_eq!(source.address, Some(me));
    let choices = Choices {
        identity: true,
        ..ALL
    };
    let report = import::import(&source, &b, choices, false).expect("import");
    assert_eq!(import::current_address(&b), Some(me));
    // The ratchets came back with the identity.
    assert!(
        report
            .ratchets
            .as_deref()
            .is_some_and(|l| l.starts_with("ratchets: 1 ")),
        "{:?}",
        report.ratchets
    );
    let kept_ratchets = std::fs::read(leviculum_std::dest_ratchet_keys_path(
        &b.join("storage"),
        &me,
    ))
    .expect("the ratchets in the new home");
    let mut destination = leviculum_lxmf::node::LxmfNode::delivery_destination(
        loquax::identity::load_or_create(&b.join("identity")).expect("identity"),
    )
    .expect("destination");
    assert_eq!(destination.load_ratchets_signed(&kept_ratchets), Ok(1));
    assert_eq!(
        destination.current_ratchet_public(),
        Some(leviculum_core::Destination::ratchet_public_key(&ratchet))
    );
    let back = conversation(&b, me, them);
    let summary: Vec<(bool, &str, Option<[u8; 32]>, Signature)> = back
        .iter()
        .map(|m| (m.outbound, m.body.as_str(), m.id, m.signature))
        .collect();
    assert_eq!(
        summary,
        [
            (
                true,
                "sent from loquax",
                Some([0x11; 32]),
                Signature::Verified
            ),
            (
                false,
                "verified on arrival",
                Some([0x22; 32]),
                Signature::Unverified
            ),
            (
                false,
                "held for its key",
                Some(held.message_id),
                Signature::Unverified
            ),
        ]
    );
    let said: Vec<&str> = back[1].ledger.iter().map(|e| e.what.as_str()).collect();
    assert_eq!(
        said,
        ["imported from Loquax", "Loquax said: signature verified"]
    );
    let store = Store::open_in(&b, me).expect("store");
    let kept = store.unverified().expect("held");
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].2, held.pack(), "the held bytes travel whole");
    let contacts = loquax::names::load_contacts(&b).expect("contacts");
    assert_eq!(
        contacts.petnames.get(&them).map(String::as_str),
        Some("Bob")
    );
    assert!(contacts.blocked.contains(&[0xbb; 16]));
    // A second round adds nothing.
    let again = import::import(&source, &b, ALL, false).expect("again");
    assert_eq!(again.conversations[0].new, 0);
}

fn loquax(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_loquax"));
    command
        .env("LNMSG_HOME", home)
        .env("HOME", home.join("user"))
        .env_remove("LEVICULUM_EVENT_LOG");
    command
}

/// Run `command` with `input` as its answers on stdin, to its end.
fn answered(mut command: Command, input: &[u8]) -> std::process::Output {
    use std::io::Write;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input)
        .expect("answers");
    child.wait_with_output().expect("run")
}

#[test]
fn the_command_line_dry_run_lists_and_the_import_asks() {
    let dir = tempfile::tempdir().expect("dir");
    let tree = nomadnet(dir.path());
    let home = dir.path().join("loquax");
    let output = loquax(&home)
        .args(["import", "--dry-run"])
        .arg(&tree.root)
        .output()
        .expect("run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("new 3  held 0  unreadable 1"), "{stdout}");
    assert!(stdout.contains("identity: offered"), "{stdout}");
    assert!(!home.exists(), "a dry run writes nothing");

    let mut command = loquax(&home);
    command.arg("import").arg(&tree.root);
    let output = answered(command, b"\n\nyes\n");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Loquax now answers on"), "{stdout}");
    let printed = loquax(&home).arg("address").output().expect("address");
    assert_eq!(
        String::from_utf8_lossy(&printed.stdout).trim(),
        hex(&address(&tree.own))
    );

    let key = identity();
    let mut command = loquax(&home);
    command.args(["import", "--key"]);
    let typed = format!("{}\nyes\n", key::to_base32(&raw(&key)));
    let output = answered(command, typed.as_bytes());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(
        !stdout.contains(&key::to_base32(&raw(&key))),
        "the key is not echoed"
    );
    assert_eq!(import::current_address(&home), Some(address(&key)));
}

// Ratspeak (1150): a copy of `ratspeak.db` at schema 45, written by the test
// from the schema (`loquax::import::ratspeak`'s table), never a real one.

/// 2026-10-25 00:30 UTC, 02:30 in Berlin's summer time; an hour later it is
/// 01:30 UTC and 02:30 again, in winter time: the same wall clock twice.
const BEFORE_DST_END: f64 = 1_792_888_200.0;
const AFTER_DST_END: f64 = BEFORE_DST_END + 3_600.0;

struct Ratspeak {
    db: PathBuf,
    own: Identity,
    peer: Identity,
    other: Identity,
    /// Sent, received, received with a file: the peer's conversation.
    ids: Vec<[u8; 32]>,
    deleted: [u8; 32],
    to_carol: [u8; 32],
    others_message: [u8; 32],
}

fn ratspeak_db(base: &Path, version: i64) -> Ratspeak {
    let db = base.join("ratspeak-copy.db");
    let conn = rusqlite::Connection::open(&db).expect("db");
    conn.execute_batch(
        "CREATE TABLE schema_version (version INTEGER NOT NULL);
         CREATE TABLE identities (hash TEXT PRIMARY KEY, lxmf_hash TEXT, nickname TEXT DEFAULT '',
             display_name TEXT DEFAULT '', status TEXT NOT NULL DEFAULT '', created_at REAL NOT NULL,
             last_used REAL, is_active INTEGER DEFAULT 0, propagation_node TEXT DEFAULT '');
         CREATE TABLE contacts (dest_hash TEXT NOT NULL, identity_id TEXT DEFAULT '',
             display_name TEXT, identity_pubkey TEXT, first_seen REAL, last_seen REAL,
             trust TEXT DEFAULT 'pending', notes TEXT DEFAULT '', UNIQUE(dest_hash, identity_id));
         CREATE TABLE messages (id TEXT NOT NULL, source TEXT NOT NULL, destination TEXT NOT NULL,
             content TEXT DEFAULT '', title TEXT DEFAULT '', timestamp REAL NOT NULL,
             state TEXT DEFAULT 'unknown', direction TEXT DEFAULT 'outbound', rtt_ms REAL,
             hops INTEGER, path TEXT, identity_id TEXT NOT NULL DEFAULT '',
             attachment_name TEXT DEFAULT '', attachment_stored_name TEXT DEFAULT '',
             image_name TEXT DEFAULT '', image_stored_name TEXT DEFAULT '',
             reply_to_id TEXT DEFAULT '', reply_to_preview TEXT DEFAULT '',
             delivery_method TEXT, audio_mode INTEGER,
             audio_stored_name TEXT NOT NULL DEFAULT '', PRIMARY KEY (id, identity_id));
         CREATE TABLE blocked_contacts (dest_hash TEXT NOT NULL, identity_id TEXT NOT NULL DEFAULT '',
             display_name TEXT DEFAULT '', blocked_at REAL, PRIMARY KEY (dest_hash, identity_id));
         CREATE TABLE lxmf_user_deleted (identity_id TEXT NOT NULL, message_id BLOB NOT NULL,
             deleted_at INTEGER NOT NULL, PRIMARY KEY(identity_id, message_id));",
    )
    .expect("schema");
    conn.execute("INSERT INTO schema_version VALUES (?1)", [version])
        .expect("version");
    let (own, peer, other) = (identity(), identity(), identity());
    let (me, them, theirs) = (address(&own), address(&peer), address(&other));
    let (own_id, other_id) = (hex(own.hash()), hex(other.hash()));
    for (hash, lxmf, name, pn) in [
        (&own_id, me, "Rat Me", hex(&[0xee; 16])),
        (&other_id, theirs, "Second", String::new()),
    ] {
        conn.execute(
            "INSERT INTO identities (hash, lxmf_hash, display_name, created_at, propagation_node) \
             VALUES (?1, ?2, ?3, 0.0, ?4)",
            rusqlite::params![hash, hex(&lxmf), name, pn],
        )
        .expect("identity");
    }
    for (peer, owner, name) in [
        (them, &own_id, Some("Bob")),
        ([0xcc; 16], &own_id, Some("Carol")),
        ([0xdd; 16], &other_id, Some("Zed")),
    ] {
        conn.execute(
            "INSERT INTO contacts (dest_hash, identity_id, display_name, trust) \
             VALUES (?1, ?2, ?3, 'trusted')",
            rusqlite::params![hex(&peer), owner, name],
        )
        .expect("contact");
    }
    conn.execute(
        "INSERT INTO blocked_contacts (dest_hash, identity_id, blocked_at) VALUES (?1, ?2, 1.0)",
        rusqlite::params![hex(&[0xbb; 16]), own_id],
    )
    .expect("blocked");
    let insert = |id: &str,
                  source: [u8; 16],
                  destination: [u8; 16],
                  content: &str,
                  ts: f64,
                  state: &str,
                  direction: &str,
                  owner: &str,
                  attachment: &str| {
        conn.execute(
            "INSERT INTO messages (id, source, destination, content, timestamp, state, \
             direction, identity_id, attachment_name) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                id,
                hex(&source),
                hex(&destination),
                content,
                ts,
                state,
                direction,
                owner,
                attachment
            ],
        )
        .expect("message");
    };
    let ids = vec![[0x11; 32], [0x22; 32], [0x33; 32]];
    let (deleted, to_carol, others_message) = ([0x44; 32], [0x55; 32], [0x66; 32]);
    let t = BEFORE_DST_END;
    insert(
        &hex(&ids[0]),
        me,
        them,
        "sent before",
        t,
        "delivered",
        "outbound",
        &own_id,
        "",
    );
    insert(
        &hex(&ids[1]),
        them,
        me,
        "received after",
        AFTER_DST_END,
        "read",
        "inbound",
        &own_id,
        "",
    );
    insert(
        &hex(&ids[2]),
        them,
        me,
        "with a file",
        AFTER_DST_END + 60.0,
        "received",
        "inbound",
        &own_id,
        "notes.txt",
    );
    insert(
        &hex(&deleted),
        them,
        me,
        "deleted there",
        AFTER_DST_END + 120.0,
        "received",
        "inbound",
        &own_id,
        "",
    );
    insert(
        &hex(&to_carol),
        me,
        [0xcc; 16],
        "still sending",
        t + 30.0,
        "sending",
        "outbound",
        &own_id,
        "",
    );
    insert(
        "local-1",
        me,
        them,
        "not an LXMF id",
        t + 40.0,
        "sending",
        "outbound",
        &own_id,
        "",
    );
    insert(
        &hex(&others_message),
        [0xdd; 16],
        theirs,
        "for the second",
        t,
        "received",
        "inbound",
        &other_id,
        "",
    );
    conn.execute(
        "INSERT INTO lxmf_user_deleted (identity_id, message_id, deleted_at) VALUES (?1, ?2, 9)",
        rusqlite::params![own_id, &deleted[..]],
    )
    .expect("deleted");
    Ratspeak {
        db,
        own,
        peer,
        other,
        ids,
        deleted,
        to_carol,
        others_message,
    }
}

/// A loquax home that already uses `identity`, as after `import --key`.
fn home_with(base: &Path, identity: &Identity) -> PathBuf {
    let home = base.join("loquax");
    std::fs::create_dir_all(&home).expect("home");
    let found = import::pasted("pasted", Path::new("(stdin)"), raw(identity));
    let choices = Choices {
        contacts: false,
        history: false,
        identity: true,
    };
    import::import(&found, &home, choices, false).expect("adopt");
    home
}

#[test]
fn ratspeak_a_dry_run_lists_its_identities_and_counts_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("dir");
    let rat = ratspeak_db(dir.path(), 45);
    let home = home_with(dir.path(), &rat.own);
    let before = files_under(&home);
    let found = import::sniff(&rat.db).expect("a Ratspeak db");
    assert!(matches!(found.source, Source::Ratspeak(_)));
    assert_eq!((found.messages, found.contacts), (7, 2), "{}", found.line());
    let report = import::import(&found, &home, ALL, true).expect("dry run");
    assert_eq!(files_under(&home), before, "a dry run writes nothing");
    let lines = report.lines(true).join("\n");
    assert!(
        lines.contains("Ratspeak schema 45, 2 identities"),
        "{lines}"
    );
    let own = format!(
        "<{}> \"Rat Me\"  6 messages  imported",
        hex(&address(&rat.own))
    );
    let other = format!(
        "<{}> \"Second\"  1 messages  not imported",
        hex(&address(&rat.other))
    );
    assert!(lines.contains(&own), "{lines}");
    assert!(lines.contains(&other), "{lines}");
    assert!(lines.contains(import::RATSPEAK_TIME_RULE), "{lines}");
    let counts: Vec<([u8; 16], usize)> = report
        .conversations
        .iter()
        .map(|c| (c.peer, c.new))
        .collect();
    assert_eq!(counts, [(address(&rat.peer), 3), ([0xcc; 16], 1)]);
    assert_eq!(report.deleted_skipped, 1);
    assert_eq!(report.attachments_skipped, 1);
    assert_eq!(report.refused.len(), 1, "the row with no LXMF id");
    assert_eq!(
        report.contacts_new, 3,
        "Bob, Carol, the blocked peer; not Zed"
    );
    assert_eq!(report.propagation_node, Some([0xee; 16]));
    assert_eq!(report.archived, None, "not a mailbox");
}

#[test]
fn ratspeak_history_comes_over_unsigned_in_utc_and_deleted_stays_deleted() {
    let dir = tempfile::tempdir().expect("dir");
    let rat = ratspeak_db(dir.path(), 45);
    let home = home_with(dir.path(), &rat.own);
    let found = import::sniff(&rat.db).expect("db");
    let report = import::import(&found, &home, ALL, false).expect("import");
    let (me, them) = (address(&rat.own), address(&rat.peer));
    assert_eq!(report.history_address, Some(me));
    let messages = conversation(&home, me, them);
    let bodies: Vec<&str> = messages.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies, ["sent before", "received after", "with a file"]);
    let ids: Vec<[u8; 32]> = messages.iter().filter_map(|m| m.id).collect();
    assert_eq!(ids, rat.ids);
    assert!(!ids.contains(&rat.deleted));
    // The same wall clock in Berlin twice, an hour apart in UTC: the values
    // are taken as the unix seconds they are, whatever zone this runs in.
    let secs: Vec<i64> = messages
        .iter()
        .filter_map(|m| m.at.map(|at| at.unix_secs))
        .collect();
    assert_eq!(
        secs,
        [
            BEFORE_DST_END as i64,
            AFTER_DST_END as i64,
            AFTER_DST_END as i64 + 60
        ]
    );
    assert!(messages[0].outbound && !messages[1].outbound);
    assert_eq!(messages[0].delivery, Some(DeliveryState::Arrived));
    for message in &messages {
        assert_eq!(message.signature, Signature::Unverified);
        let ledger: Vec<&str> = message.ledger.iter().map(|e| e.what.as_str()).collect();
        assert_eq!(
            ledger,
            [
                "imported from Ratspeak",
                "imported, no signature: Ratspeak keeps none"
            ]
        );
    }
    let carol = conversation(&home, me, [0xcc; 16]);
    assert_eq!(carol.len(), 1);
    assert_eq!(carol[0].id, Some(rat.to_carol));
    assert_eq!(
        carol[0].delivery,
        Some(DeliveryState::Failed),
        "Ratspeak was still sending it; Loquax never will"
    );
    // The second identity's history is not this one's.
    assert!(conversation(&home, me, [0xdd; 16]).is_empty());
    let contacts = loquax::names::load_contacts(&home).expect("contacts");
    assert_eq!(
        contacts.petnames.get(&them).map(String::as_str),
        Some("Bob")
    );
    assert_eq!(
        contacts.petnames.get(&[0xcc; 16]).map(String::as_str),
        Some("Carol")
    );
    assert!(!contacts.petnames.contains_key(&[0xdd; 16]));
    assert!(contacts.blocked.contains(&[0xbb; 16]));
    let log = std::fs::read_to_string(home.join(import::LOG_FILE)).expect("log");
    assert!(log.contains("IMPORT source=ratspeak"), "{log}");
    assert!(log.contains("deleted_skipped=1"), "{log}");
    assert!(!log.contains("sent before"), "no content in the log: {log}");
}

#[test]
fn ratspeak_a_second_run_is_idempotent() {
    let dir = tempfile::tempdir().expect("dir");
    let rat = ratspeak_db(dir.path(), 45);
    let home = home_with(dir.path(), &rat.own);
    let found = import::sniff(&rat.db).expect("db");
    import::import(&found, &home, ALL, false).expect("first");
    let store = Store::open_in(&home, address(&rat.own)).expect("store");
    let count = store.message_count().expect("count");
    drop(store);
    let again = import::import(&found, &home, ALL, false).expect("again");
    let news: Vec<(usize, usize)> = again
        .conversations
        .iter()
        .map(|c| (c.new, c.held))
        .collect();
    assert_eq!(news, [(0, 3), (0, 1)]);
    assert_eq!((again.contacts_new, again.contacts_held), (0, 3));
    let store = Store::open_in(&home, address(&rat.own)).expect("store");
    assert_eq!(store.message_count().expect("count"), count);
}

#[test]
fn ratspeak_another_schema_is_refused_with_both_numbers() {
    let dir = tempfile::tempdir().expect("dir");
    let rat = ratspeak_db(dir.path(), 46);
    let error = match import::sniff(&rat.db) {
        Err(error) => error,
        Ok(_) => panic!("schema 46 was read"),
    };
    assert!(
        error
            .detail
            .contains("written for Ratspeak schemas 33 and 45, this db is 46"),
        "{error}"
    );
}

#[test]
fn ratspeak_imports_the_identity_loquax_uses_and_refuses_to_guess() {
    let dir = tempfile::tempdir().expect("dir");
    let rat = ratspeak_db(dir.path(), 45);
    let found = import::sniff(&rat.db).expect("db");
    // Loquax answers on an address none of the db's identities has.
    let stranger = identity();
    let home = home_with(dir.path(), &stranger);
    let error = match import::import(&found, &home, ALL, false) {
        Err(error) => error,
        Ok(_) => panic!("imported without a matching identity"),
    };
    assert!(
        error.detail.contains("adopt the one to import first"),
        "{error}"
    );
    let dry = import::import(&found, &home, ALL, true).expect("a dry run still lists");
    assert_eq!(dry.refused.len(), 1);
    assert!(dry.conversations.is_empty());
    // Loquax answers on the second identity: that one's history comes.
    let second = dir.path().join("second");
    std::fs::create_dir_all(&second).expect("dir");
    let home = home_with(&second, &rat.other);
    let report = import::import(&found, &home, ALL, false).expect("import");
    assert_eq!(report.history_address, Some(address(&rat.other)));
    let messages = conversation(&home, address(&rat.other), [0xdd; 16]);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id, Some(rat.others_message));
    let contacts = loquax::names::load_contacts(&home).expect("contacts");
    assert_eq!(
        contacts.petnames.get(&[0xdd; 16]).map(String::as_str),
        Some("Zed")
    );
}

#[test]
fn ratspeak_on_a_mailbox_the_history_goes_into_the_archive_for_the_clients() {
    let dir = tempfile::tempdir().expect("dir");
    let rat = ratspeak_db(dir.path(), 45);
    let home = home_with(dir.path(), &rat.own);
    let me = address(&rat.own);
    {
        // A home node: one device admitted.
        let mut store = Store::open_in(&home, me).expect("store");
        store.issue_token(&[1; 16], "phone", 10).expect("token");
        store
            .redeem_token(&[1; 16], &[2; 16], 11, 3_600)
            .expect("admit");
        assert!(store.serves_as_home_node().expect("home node"));
    }
    let found = import::sniff(&rat.db).expect("db");
    let dry = import::import(&found, &home, ALL, true).expect("dry run");
    assert_eq!(dry.archived, Some(4));
    let report = import::import(&found, &home, ALL, false).expect("import");
    assert_eq!(report.archived, Some(4));
    let store = Store::open_in(&home, me).expect("store");
    let mut page = store.archive_since(0, usize::MAX).expect("archive");
    // The contacts' names travel beside the history as petnames (1159),
    // their blocks as peer states (1162).
    page.entries
        .retain(|e| e.kind != loquax::home::KIND_PETNAME && e.kind != loquax::home::KIND_PEERSTATE);
    assert!(page
        .entries
        .iter()
        .all(|e| e.kind == loquax::home::KIND_IMPORTED));
    let read: Vec<([u8; 16], loquax::store::ImportRow)> = page
        .entries
        .iter()
        .map(|e| {
            let (origin, peer, row) = import::archive::read_entry(&e.packed).expect("entry");
            assert_eq!(origin, "ratspeak");
            (peer, row)
        })
        .collect();
    let order: Vec<[u8; 32]> = read.iter().map(|(_, row)| row.id).collect();
    assert_eq!(
        order,
        [rat.ids[0], rat.to_carol, rat.ids[1], rat.ids[2]],
        "one time order over all conversations"
    );
    drop(store);
    // A second import appends nothing.
    let again = import::import(&found, &home, ALL, false).expect("again");
    assert_eq!(again.archived, Some(0));

    // A client of this home node writes the pulled entries into its store.
    let client = dir.path().join("client");
    std::fs::create_dir_all(&client).expect("client");
    let entries: Vec<Vec<u8>> = page.entries.iter().map(|e| e.packed.clone()).collect();
    let public = rat.own.public_key_bytes();
    let new = loquax::home::runner::keep_imported(&client, &public, &entries).expect("keep");
    assert_eq!(new, 4);
    let messages = conversation(&client, me, address(&rat.peer));
    let bodies: Vec<&str> = messages.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies, ["sent before", "received after", "with a file"]);
    let ledger: Vec<&str> = messages[0].ledger.iter().map(|e| e.what.as_str()).collect();
    assert_eq!(
        ledger,
        [
            "imported from Ratspeak",
            "imported, no signature: Ratspeak keeps none"
        ]
    );
    assert_eq!(
        loquax::home::runner::keep_imported(&client, &public, &entries).expect("again"),
        0
    );
}

/// A home node, as after a device joined: one device admitted.
fn make_mailbox(home: &Path, me: [u8; 16]) {
    let mut store = Store::open_in(home, me).expect("store");
    store.issue_token(&[1; 16], "phone", 10).expect("token");
    store
        .redeem_token(&[1; 16], &[2; 16], 11, 3_600)
        .expect("admit");
    assert!(store.serves_as_home_node().expect("home node"));
}

/// The petname entries the archive holds, oldest first.
fn archived_petnames(home: &Path, me: [u8; 16]) -> Vec<([u8; 16], Option<String>)> {
    let store = Store::open_in(home, me).expect("store");
    store
        .archive_since(0, usize::MAX)
        .expect("archive")
        .entries
        .iter()
        .filter(|e| e.kind == loquax::home::KIND_PETNAME)
        .map(|e| loquax::home::wire::read_petname_entry(&e.packed).expect("a petname"))
        .collect()
}

/// 1159: Lew's migration. The mailbox wrote the imported names into its
/// contacts file alone, so no device ever saw them. Each imported name is a
/// petname entry now, as a `/petname` is kept; a name a device gave that
/// peer before stays; a second import appends nothing.
#[test]
fn ratspeak_on_a_mailbox_each_imported_name_is_a_petname_for_every_device() {
    let dir = tempfile::tempdir().expect("dir");
    let rat = ratspeak_db(dir.path(), 45);
    let home = home_with(dir.path(), &rat.own);
    let me = address(&rat.own);
    let bob = address(&rat.peer);
    make_mailbox(&home, me);
    {
        let mut store = Store::open_in(&home, me).expect("store");
        let said = loquax::home::wire::petname_entry(&[0xcc; 16], Some("Caz"));
        store
            .archive_petname([0xcc; 16], &said, 5, u64::MAX)
            .expect("a device named Carol");
    }
    let found = import::sniff(&rat.db).expect("db");
    let dry = import::import(&found, &home, ALL, true).expect("dry run");
    assert_eq!(dry.names_archived, Some(1), "Bob; Carol has hers");
    let report = import::import(&found, &home, ALL, false).expect("import");
    assert_eq!(report.names_archived, Some(1));
    let lines = report.lines(false).join("\n");
    assert!(
        lines.contains("mailbox archive: 1 contact names appended"),
        "{lines}"
    );
    assert_eq!(
        archived_petnames(&home, me),
        [
            ([0xcc; 16], Some("Caz".to_string())),
            (bob, Some("Bob".to_string()))
        ]
    );
    let again = import::import(&found, &home, ALL, false).expect("again");
    assert_eq!(again.names_archived, Some(0));
    assert_eq!(archived_petnames(&home, me).len(), 2, "one entry a name");
}

/// 1159: the NomadNet import merges its contacts the same way, so on a
/// mailbox its names travel too: one petname entry for each name the
/// contacts file holds for an imported peer.
#[test]
fn nomadnet_on_a_mailbox_each_imported_name_is_a_petname_too() {
    let dir = tempfile::tempdir().expect("dir");
    let tree = nomadnet(dir.path());
    let home = home_with(dir.path(), &tree.own);
    let me = address(&tree.own);
    make_mailbox(&home, me);
    let choices = Choices {
        history: false,
        ..ALL
    };
    let report = import::import(&found(&tree), &home, choices, false).expect("import");
    let contacts = loquax::names::load_contacts(&home).expect("contacts");
    assert!(!contacts.petnames.is_empty(), "the fixture names peers");
    assert_eq!(report.names_archived, Some(contacts.petnames.len()));
    let mut archived = archived_petnames(&home, me);
    archived.sort();
    let expected: Vec<([u8; 16], Option<String>)> = contacts
        .petnames
        .into_iter()
        .map(|(peer, name)| (peer, Some(name)))
        .collect();
    assert_eq!(archived, expected);
}

/// 1162: a NomadNet history on a mailbox goes into the archive as a
/// Ratspeak one does, each row with our verdict, NomadNet's and the held
/// bytes, so a client stores the row the mailbox stores: the sent message
/// verified, the intact received one held for its sender's key, the
/// stripped one with NomadNet's word. A second import appends nothing.
#[test]
fn nomadnet_on_a_mailbox_the_history_goes_into_the_archive_with_its_signatures() {
    let dir = tempfile::tempdir().expect("dir");
    let tree = nomadnet(dir.path());
    let home = home_with(dir.path(), &tree.own);
    let (me, them) = (address(&tree.own), address(&tree.peer));
    make_mailbox(&home, me);
    let dry = import::import(&found(&tree), &home, ALL, true).expect("dry run");
    assert_eq!(dry.archived, Some(3));
    let report = import::import(&found(&tree), &home, ALL, false).expect("import");
    assert_eq!(report.archived, Some(3));
    let store = Store::open_in(&home, me).expect("store");
    let entries: Vec<Vec<u8>> = store
        .archive_since(0, usize::MAX)
        .expect("archive")
        .entries
        .into_iter()
        .filter(|e| e.kind == loquax::home::KIND_IMPORTED)
        .map(|e| e.packed)
        .collect();
    let order: Vec<[u8; 32]> = entries
        .iter()
        .map(|e| import::archive::read_entry(e).expect("entry").2.id)
        .collect();
    assert_eq!(order, tree.ids, "in time order");
    drop(store);
    let again = import::import(&found(&tree), &home, ALL, false).expect("again");
    assert_eq!(again.archived, Some(0));

    let client = dir.path().join("client");
    std::fs::create_dir_all(&client).expect("client");
    let public = tree.own.public_key_bytes();
    let new = loquax::home::runner::keep_imported(&client, &public, &entries).expect("keep");
    assert_eq!(new, 3);
    let shown = |home: &Path| -> Vec<(String, Signature, Vec<String>)> {
        conversation(home, me, them)
            .into_iter()
            .map(|m| {
                let ledger = m.ledger.into_iter().map(|e| e.what).collect();
                (m.body, m.signature, ledger)
            })
            .collect()
    };
    assert_eq!(shown(&client), shown(&home), "the row the mailbox stores");
    assert_eq!(
        shown(&client).iter().map(|m| m.1).collect::<Vec<_>>(),
        [
            Signature::Verified,
            Signature::Unverified,
            Signature::Unverified
        ]
    );
    let held = Store::open_in(&client, me)
        .expect("client store")
        .unverified()
        .expect("held");
    assert_eq!(held.len(), 1);
    assert_eq!((held[0].0, held[0].1), (tree.ids[1], them));
    let kept = Lxm::unpack(&held[0].2, None, Some(&tree.peer), DeliveryMethod::Direct)
        .expect("kept bytes");
    assert_eq!(kept.verification, leviculum_lxmf::Verification::Valid);
}

/// 1162: an imported block on a mailbox is a peer state entry, the one a
/// device's block sends, so every device blocks the peer too; a state a
/// device set for that peer wins, and a second import appends nothing.
#[test]
fn nomadnet_on_a_mailbox_each_imported_block_is_a_peer_state_for_every_device() {
    let dir = tempfile::tempdir().expect("dir");
    let tree = nomadnet(dir.path());
    let home = home_with(dir.path(), &tree.own);
    let me = address(&tree.own);
    make_mailbox(&home, me);
    let blocked = [0xbb; 16];
    let choices = Choices {
        history: false,
        ..ALL
    };
    let dry = import::import(&found(&tree), &home, choices, true).expect("dry run");
    assert_eq!(dry.blocks_archived, Some(1));
    let report = import::import(&found(&tree), &home, choices, false).expect("import");
    assert_eq!(report.blocks_archived, Some(1));
    let lines = report.lines(false).join("\n");
    assert!(
        lines.contains("mailbox archive: 1 blocks appended"),
        "{lines}"
    );
    let states = |home: &Path| -> Vec<([u8; 16], loquax::home::wire::PeerState)> {
        Store::open_in(home, me)
            .expect("store")
            .archive_since(0, usize::MAX)
            .expect("archive")
            .entries
            .iter()
            .filter(|e| e.kind == loquax::home::KIND_PEERSTATE)
            .map(|e| loquax::home::wire::read_peer_state_entry(&e.packed).expect("a state"))
            .collect()
    };
    let state = loquax::home::wire::PeerState {
        blocked: true,
        ..loquax::home::wire::PeerState::default()
    };
    assert_eq!(states(&home), [(blocked, state)]);
    let again = import::import(&found(&tree), &home, choices, false).expect("again");
    assert_eq!(again.blocks_archived, Some(0));
    assert_eq!(states(&home).len(), 1, "one entry a block");

    // A device's word on a peer wins over the import's block.
    let other = tempfile::tempdir().expect("dir");
    let tree = nomadnet(other.path());
    let home = home_with(other.path(), &tree.own);
    let me = address(&tree.own);
    make_mailbox(&home, me);
    {
        let mut store = Store::open_in(&home, me).expect("store");
        let said = loquax::home::wire::peer_state_entry(
            &blocked,
            &loquax::home::wire::PeerState::default(),
        );
        store
            .archive_peer_state(blocked, &said, 5, u64::MAX)
            .expect("a device unblocked it");
    }
    let report = import::import(&found(&tree), &home, choices, false).expect("import");
    assert_eq!(report.blocks_archived, Some(0));
}

/// Not a mailbox: the names stay in the contacts file, and no store is made
/// for them.
#[test]
fn ratspeak_off_a_mailbox_no_name_is_archived() {
    let dir = tempfile::tempdir().expect("dir");
    let rat = ratspeak_db(dir.path(), 45);
    let home = home_with(dir.path(), &rat.own);
    let found = import::sniff(&rat.db).expect("db");
    let report = import::import(&found, &home, ALL, false).expect("import");
    assert_eq!(report.names_archived, None);
}

#[test]
fn ratspeak_on_the_command_line_in_another_zone_the_times_are_the_same() {
    let dir = tempfile::tempdir().expect("dir");
    let rat = ratspeak_db(dir.path(), 45);
    let home = home_with(dir.path(), &rat.own);
    let output = loquax(&home)
        .env("TZ", "Europe/Berlin")
        .args(["import", "--dry-run"])
        .arg(&rat.db)
        .output()
        .expect("run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("Ratspeak schema 45, 2 identities"),
        "{stdout}"
    );
    assert!(
        stdout.contains("deleted: 1 messages deleted there stay deleted"),
        "{stdout}"
    );
    let mut command = loquax(&home);
    command
        .env("TZ", "Europe/Berlin")
        .arg("import")
        .arg(&rat.db);
    let output = answered(command, b"\n\n");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("history: 4 messages written"), "{stdout}");
    let messages = conversation(&home, address(&rat.own), address(&rat.peer));
    let secs: Vec<i64> = messages
        .iter()
        .filter_map(|m| m.at.map(|at| at.unix_secs))
        .collect();
    assert_eq!(secs[..2], [BEFORE_DST_END as i64, AFTER_DST_END as i64]);
}

// Ratspeak schema 33 (1158): the layout of Lew's own database, read by the
// reviewer as schema only, written here from Ratspeak's 39a6323 schema with
// made-up rows. No voice notes, no deletion record; replies and reactions.

struct Ratspeak33 {
    db: PathBuf,
    own: Identity,
    peer: Identity,
    /// Sent, received, the reply to the sent one, the reply to a stranger.
    ids: Vec<[u8; 32]>,
}

fn ratspeak33_db(base: &Path) -> Ratspeak33 {
    let db = base.join("ratspeak-33.db");
    let conn = rusqlite::Connection::open(&db).expect("db");
    conn.execute_batch(
        "CREATE TABLE schema_version (version INTEGER NOT NULL);
         CREATE TABLE identities (hash TEXT PRIMARY KEY, lxmf_hash TEXT, nickname TEXT DEFAULT '',
             display_name TEXT DEFAULT '', status TEXT NOT NULL DEFAULT '', created_at REAL NOT NULL,
             last_used REAL, is_active INTEGER DEFAULT 0, propagation_node TEXT DEFAULT '',
             propagation_enabled INTEGER DEFAULT 0, propagation_mode TEXT NOT NULL DEFAULT 'auto',
             propagation_auto_favor_static INTEGER NOT NULL DEFAULT 1);
         CREATE TABLE contacts (dest_hash TEXT NOT NULL, identity_id TEXT DEFAULT '',
             display_name TEXT, identity_pubkey TEXT, first_seen REAL, last_seen REAL,
             trust TEXT DEFAULT 'pending', notes TEXT DEFAULT '', UNIQUE(dest_hash, identity_id));
         CREATE TABLE messages (id TEXT NOT NULL, source TEXT NOT NULL, destination TEXT NOT NULL,
             content TEXT DEFAULT '', title TEXT DEFAULT '', timestamp REAL NOT NULL,
             state TEXT DEFAULT 'unknown', direction TEXT DEFAULT 'outbound', rtt_ms REAL,
             hops INTEGER, path TEXT, identity_id TEXT NOT NULL DEFAULT '',
             attachment_name TEXT DEFAULT '', attachment_stored_name TEXT DEFAULT '',
             image_name TEXT DEFAULT '', image_stored_name TEXT DEFAULT '',
             reply_to_id TEXT DEFAULT '', reply_to_preview TEXT DEFAULT '',
             game_id TEXT DEFAULT '', game_action TEXT DEFAULT '', game_move_san TEXT DEFAULT '',
             delivery_method TEXT, PRIMARY KEY (id, identity_id));
         CREATE TABLE hidden_conversations (dest_hash TEXT NOT NULL,
             identity_id TEXT NOT NULL DEFAULT '', hidden_at REAL,
             PRIMARY KEY (dest_hash, identity_id));
         CREATE TABLE blocked_contacts (dest_hash TEXT NOT NULL, identity_id TEXT NOT NULL DEFAULT '',
             display_name TEXT DEFAULT '', blocked_at REAL, PRIMARY KEY (dest_hash, identity_id));
         CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);
         CREATE TABLE reactions (id INTEGER PRIMARY KEY AUTOINCREMENT, message_id TEXT NOT NULL,
             sender TEXT NOT NULL, emoji TEXT NOT NULL, timestamp REAL NOT NULL,
             identity_id TEXT DEFAULT '', UNIQUE(message_id, sender, emoji, identity_id));
         INSERT INTO schema_version VALUES (33);",
    )
    .expect("schema");
    let (own, peer, other) = (identity(), identity(), identity());
    let (me, them, theirs) = (address(&own), address(&peer), address(&other));
    let (own_id, other_id) = (hex(own.hash()), hex(other.hash()));
    for (hash, lxmf, name, active) in [(&own_id, me, "Rat Me", 1), (&other_id, theirs, "Old", 0)] {
        conn.execute(
            "INSERT INTO identities (hash, lxmf_hash, display_name, created_at, is_active) \
             VALUES (?1, ?2, ?3, 0.0, ?4)",
            rusqlite::params![hash, hex(&lxmf), name, active],
        )
        .expect("identity");
    }
    conn.execute(
        "INSERT INTO contacts (dest_hash, identity_id, display_name) VALUES (?1, ?2, 'Bob')",
        rusqlite::params![hex(&them), own_id],
    )
    .expect("contact");
    let ids = vec![[0x11; 32], [0x22; 32], [0x33; 32], [0x44; 32]];
    let t = BEFORE_DST_END;
    // id, from, to, content, time, state, direction, reply to, preview,
    // delivery method, owner.
    let insert = |id: [u8; 32],
                  from: [u8; 16],
                  to: [u8; 16],
                  content: &str,
                  ts: f64,
                  (state, direction): (&str, &str),
                  (reply_to, preview): (String, &str),
                  method: Option<&str>,
                  owner: &str| {
        conn.execute(
            "INSERT INTO messages (id, source, destination, content, timestamp, state, \
             direction, identity_id, reply_to_id, reply_to_preview, delivery_method) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                hex(&id),
                hex(&from),
                hex(&to),
                content,
                ts,
                state,
                direction,
                owner,
                reply_to,
                preview,
                method
            ],
        )
        .expect("message");
    };
    let none = || (String::new(), "");
    let sent = ("delivered", "outbound");
    insert(
        ids[0],
        me,
        them,
        "ping",
        t,
        sent,
        none(),
        Some("direct"),
        &own_id,
    );
    let read = ("read", "inbound");
    insert(
        ids[1],
        them,
        me,
        "pong",
        t + 10.5,
        read,
        none(),
        None,
        &own_id,
    );
    let received = ("received", "inbound");
    let to_ping = (hex(&ids[0]), "ping");
    insert(
        ids[2],
        them,
        me,
        "re: ping",
        t + 20.0,
        received,
        to_ping,
        None,
        &own_id,
    );
    let propagated = ("propagated", "outbound");
    let to_lost = (hex(&[0x99; 32]), "lost");
    insert(
        ids[3],
        me,
        them,
        "re: lost",
        t + 30.0,
        propagated,
        to_lost,
        Some("propagated"),
        &own_id,
    );
    insert(
        [0x66; 32],
        [0xdd; 16],
        theirs,
        "old",
        t,
        received,
        none(),
        None,
        &other_id,
    );
    for (target, sender, emoji, ts) in [
        (hex(&ids[0]), hex(&them), "\u{1f44d}", t + 5.0),
        (hex(&ids[1]), hex(&me), "\u{2764}\u{fe0f}", t + 11.0),
        (hex(&[0x99; 32]), hex(&them), "\u{1f602}", t + 12.0),
    ] {
        conn.execute(
            "INSERT INTO reactions (message_id, sender, emoji, timestamp, identity_id) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![target, sender, emoji, ts, own_id],
        )
        .expect("reaction");
    }
    Ratspeak33 { db, own, peer, ids }
}

#[test]
fn ratspeak_schema_33_is_read_with_its_replies_and_reactions() {
    let dir = tempfile::tempdir().expect("dir");
    let rat = ratspeak33_db(dir.path());
    let home = home_with(dir.path(), &rat.own);
    let found = import::sniff(&rat.db).expect("a schema 33 db");
    assert!(matches!(found.source, Source::Ratspeak(_)));
    assert_eq!((found.messages, found.contacts), (5, 2), "{}", found.line());
    let dry = import::import(&found, &home, ALL, true).expect("dry run");
    let lines = dry.lines(true).join("\n");
    assert!(
        lines.contains("Ratspeak schema 33, 2 identities"),
        "{lines}"
    );
    assert!(
        lines.contains("replies: 1 linked to their message, 1 whose message is not imported"),
        "{lines}"
    );
    assert!(
        lines.contains("reactions: 2 imported, 1 skipped"),
        "{lines}"
    );
    let report = import::import(&found, &home, ALL, false).expect("import");
    let (me, them) = (address(&rat.own), address(&rat.peer));
    assert_eq!(report.history_address, Some(me));
    assert!(!report.conversations.iter().any(|c| c.peer == [0xdd; 16]));
    let messages = conversation(&home, me, them);
    let bodies: Vec<&str> = messages
        .iter()
        .filter(|m| !m.is_reaction())
        .map(|m| m.body.as_str())
        .collect();
    assert_eq!(bodies, ["ping", "pong", "re: ping", "re: lost"]);
    let by_id = |id: [u8; 32]| {
        messages
            .iter()
            .find(|m| m.id == Some(id))
            .expect("imported")
    };
    assert_eq!(by_id(rat.ids[0]).delivery, Some(DeliveryState::Arrived));
    assert_eq!(by_id(rat.ids[3]).delivery, Some(DeliveryState::InMailbox));
    // Half a second of the received time is Ratspeak's, kept as whole seconds.
    assert_eq!(
        by_id(rat.ids[1]).at.map(|at| at.unix_secs),
        Some(BEFORE_DST_END as i64 + 10)
    );
    let reply = by_id(rat.ids[2]).reply().expect("a reply");
    assert_eq!(reply.to, rat.ids[0]);
    assert_eq!(reply.quote.as_deref(), Some("ping"));
    assert!(
        by_id(rat.ids[3]).reply().is_none(),
        "its target is not here"
    );
    let reactions: Vec<([u8; 32], String, bool)> = messages
        .iter()
        .filter_map(|m| m.reaction().map(|(to, content)| (to, content, m.outbound)))
        .collect();
    assert_eq!(
        reactions,
        [
            (rat.ids[0], "\u{1f44d}".to_string(), false),
            (rat.ids[1], "\u{2764}\u{fe0f}".to_string(), true)
        ]
    );
    // Time order: each reaction after what it reacts to.
    let order: Vec<Option<[u8; 32]>> = messages.iter().map(|m| m.id).collect();
    let at = |id: [u8; 32]| order.iter().position(|o| *o == Some(id));
    let thumbs = import::ratspeak::reaction_id(
        &hex(rat.own.hash()),
        &hex(&rat.ids[0]),
        &hex(&them),
        "\u{1f44d}",
    );
    assert!(at(rat.ids[0]) < at(thumbs) && at(thumbs) < at(rat.ids[1]));
    // A second run writes nothing new.
    let again = import::import(&found, &home, ALL, false).expect("again");
    let news: Vec<(usize, usize)> = again
        .conversations
        .iter()
        .map(|c| (c.new, c.held))
        .collect();
    assert_eq!(news, [(0, 6)]);
}

#[test]
fn ratspeak_schema_33_on_a_mailbox_the_clients_get_the_replies_and_reactions() {
    let dir = tempfile::tempdir().expect("dir");
    let rat = ratspeak33_db(dir.path());
    let home = home_with(dir.path(), &rat.own);
    let me = address(&rat.own);
    {
        let mut store = Store::open_in(&home, me).expect("store");
        store.issue_token(&[1; 16], "phone", 10).expect("token");
        store
            .redeem_token(&[1; 16], &[2; 16], 11, 3_600)
            .expect("admit");
    }
    let found = import::sniff(&rat.db).expect("db");
    let report = import::import(&found, &home, ALL, false).expect("import");
    assert_eq!(report.archived, Some(6));
    let store = Store::open_in(&home, me).expect("store");
    let entries: Vec<Vec<u8>> = store
        .archive_since(0, usize::MAX)
        .expect("archive")
        .entries
        .into_iter()
        .map(|e| e.packed)
        .collect();
    drop(store);
    let client = dir.path().join("client");
    std::fs::create_dir_all(&client).expect("client");
    let public = rat.own.public_key_bytes();
    let new = loquax::home::runner::keep_imported(&client, &public, &entries).expect("keep");
    assert_eq!(new, 6);
    let messages = conversation(&client, me, address(&rat.peer));
    let reply = messages
        .iter()
        .find(|m| m.id == Some(rat.ids[2]))
        .and_then(Message::reply)
        .expect("the reply came with its link");
    assert_eq!(reply.to, rat.ids[0]);
    assert_eq!(messages.iter().filter(|m| m.is_reaction()).count(), 2);
    assert!(conversation(&client, me, [0xdd; 16]).is_empty());
}

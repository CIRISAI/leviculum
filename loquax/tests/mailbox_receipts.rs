//! Read receipts through the mailbox (1234, slice five of
//! `docs/src/concepts/loquax-read-receipts.md`): the devices of a home
//! node hold no identity, so a device says what it saw with a seen
//! `/mark` and the mailbox signs the receipt. Driven as the mailbox
//! answers a request ([`loquax::home::server::answer`]) over its store,
//! with real identities and signed LXMF messages, and the devices' side as
//! the screen's model takes what a pull hands it.

use leviculum_lxmf::lev::{Lev, Receipt};
use leviculum_lxmf::message::{DeliveryMethod, Message as Lxm, Verification};
use leviculum_std::Identity;
use leviculum_ui::delivery::DeliveryState;
use leviculum_ui::input::{InputBuffer, KeyEvent};
use loquax::engine::delivery_address;
use loquax::home::server::{answer, Home};
use loquax::home::wire::{self, Mark, MarkFlag, Status};
use loquax::home::{KIND_MARK, KIND_RECEIVED, KIND_SENT, MARK_PATH, SEND_PATH};
use loquax::outbox::OutboxEvent;
use loquax::store::{Outgoing, Store};
use loquax::ui::{update, UiEvent, UiModel};
use loquax::when::WallTime;

const LAPTOP: [u8; 16] = [0x21; 16];
const PHONE: [u8; 16] = [0x22; 16];
const STRANGER: [u8; 16] = [0x23; 16];
const RT: u32 = 1_800_000_000;

/// A compose field nobody types into.
#[derive(Debug, Default, Clone)]
struct Line(String);

impl InputBuffer for Line {
    fn value(&self) -> &str {
        &self.0
    }
    fn cursor(&self) -> usize {
        self.0.chars().count()
    }
    fn edit(&mut self, _key: &KeyEvent) -> bool {
        false
    }
    fn take(&mut self) -> String {
        std::mem::take(&mut self.0)
    }
    fn newline(&mut self) {
        self.0.push('\n');
    }
    fn set_cursor(&mut self, _at: usize) {}
    fn set_value(&mut self, text: &str) {
        self.0 = text.to_string();
    }
}

/// A mailbox: its identity, its address, its store with the laptop and
/// the phone admitted.
struct Mailbox {
    _dir: tempfile::TempDir,
    identity: Identity,
    address: [u8; 16],
    store: Store,
}

impl Mailbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("dir");
        let identity = leviculum_std::generate_identity();
        let address = delivery_address(&identity).expect("address");
        let mut store = Store::open_in(dir.path(), address).expect("store");
        for (n, device) in [LAPTOP, PHONE].iter().enumerate() {
            let token = [n as u8 + 1; 16];
            store.issue_token(&token, "device", 10).expect("token");
            store
                .redeem_token(&token, device, 11, 3_600)
                .expect("admit");
        }
        Self {
            _dir: dir,
            identity,
            address,
            store,
        }
    }

    fn ask(&mut self, path: &str, data: &[u8], device: [u8; 16]) -> (Status, Vec<Outgoing>) {
        let home = Home {
            identity: &self.identity,
            address: self.address,
            token_ttl_secs: 3_600,
            cap_bytes: u64::MAX,
            handover: None,
            groups: None,
            shown: Default::default(),
        };
        let answer = answer(
            &mut self.store,
            &home,
            path,
            data,
            Some(device),
            f64::from(RT) + 500.0,
        );
        (
            wire::read_status(&answer.bytes).expect("status"),
            answer.accepted,
        )
    }

    fn seen(&mut self, device: [u8; 16], at: u32, ids: &[[u8; 32]]) -> (Status, Vec<Outgoing>) {
        let mark = Mark {
            flag: MarkFlag::Seen { at },
            ids: ids.to_vec(),
        };
        self.ask(MARK_PATH, &wire::mark_request(&mark), device)
    }

    /// A message `from` sent to this mailbox, archived as the daemon keeps
    /// what comes: its id.
    fn receive(&mut self, from: &Identity, body: &str, lev: Option<Lev>) -> [u8; 32] {
        let fields = lev.map(|lev| lev.fields().to_vec()).unwrap_or_default();
        let message = Lxm::create(
            self.address,
            delivery_address(from).expect("address"),
            from,
            f64::from(RT),
            Vec::new(),
            body.as_bytes().to_vec(),
            fields,
            DeliveryMethod::Direct,
        )
        .expect("message");
        self.store
            .archive_append(
                KIND_RECEIVED,
                &message.message_id,
                &message.pack(),
                i64::from(RT),
                u64::MAX,
            )
            .expect("archive");
        message.message_id
    }

    /// The archive's entries of `kind`, oldest first.
    fn entries(&self, kind: u8) -> Vec<Vec<u8>> {
        self.store
            .archive_since(0, usize::MAX)
            .expect("archive")
            .entries
            .into_iter()
            .filter(|e| e.kind == kind)
            .map(|e| e.packed)
            .collect()
    }

    /// The seen marks the archive holds: their time and ids.
    fn seen_marks(&self) -> Vec<(u32, Vec<[u8; 32]>)> {
        self.entries(KIND_MARK)
            .iter()
            .filter_map(|packed| match wire::read_mark(packed).expect("a mark") {
                Mark {
                    flag: MarkFlag::Seen { at },
                    ids,
                } => Some((at, ids)),
                _ => None,
            })
            .collect()
    }
}

/// A receipt the mailbox put in its outbox, opened with the mailbox's
/// public key: its destination and the receipt, checked to be a receipt
/// alone signed by the mailbox.
fn opened(mailbox: &Mailbox, outgoing: &Outgoing) -> ([u8; 16], Receipt) {
    let message = Lxm::unpack(
        &outgoing.packed,
        None,
        Some(&mailbox.identity),
        DeliveryMethod::Direct,
    )
    .expect("unpack");
    assert_eq!(message.verification, Verification::Valid);
    assert_eq!(message.source_hash, mailbox.address);
    assert_eq!(message.destination_hash, outgoing.destination);
    assert_eq!(message.message_id, outgoing.id);
    assert!(message.content.is_empty() && message.title.is_empty());
    let lev = Lev::from_fields(&message.fields).expect("the lev namespace");
    assert!(lev.supports_receipts(), "the mark rides with it");
    (message.destination_hash, lev.receipt.expect("a receipt"))
}

/// A device seeing two messages of a sender whose messages carry the mark,
/// and one of a stock client, hands the mailbox one seen mark: one receipt
/// goes, signed by the mailbox, to the marked sender alone, with both ids
/// and the reader's clock. The archive keeps the seen mark for every
/// device and the receipt as a sent entry, and the outbox delivers it.
#[test]
fn a_seen_mark_makes_one_receipt_signed_by_the_mailbox() {
    let mut mailbox = Mailbox::new();
    let alice = leviculum_std::generate_identity();
    let stock = leviculum_std::generate_identity();
    let one = mailbox.receive(&alice, "one", Some(Lev::mark()));
    let two = mailbox.receive(&alice, "two", Some(Lev::mark()));
    let plain = mailbox.receive(&stock, "no mark", None);

    let (status, accepted) = mailbox.seen(LAPTOP, RT, &[one, two, plain]);
    assert_eq!(status, Status::Ok);
    assert_eq!(accepted.len(), 1, "one receipt, none to the stock client");
    let (to, receipt) = opened(&mailbox, &accepted[0]);
    assert_eq!(to, delivery_address(&alice).expect("address"));
    assert_eq!(receipt.read_at, RT);
    assert_eq!(receipt.ids, [one, two]);
    assert_eq!(mailbox.seen_marks(), [(RT, vec![one, two, plain])]);
    let sent = mailbox.entries(KIND_SENT);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0], accepted[0].packed);
    let open = mailbox.store.outbox_open().expect("outbox");
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, accepted[0].id);

    // A device not admitted is refused, and nothing goes.
    let three = mailbox.receive(&alice, "three", Some(Lev::mark()));
    let (status, accepted) = mailbox.seen(STRANGER, RT, &[three]);
    assert_eq!(status, Status::NotAdmitted);
    assert!(accepted.is_empty());
    assert_eq!(mailbox.seen_marks().len(), 1);
}

/// Two devices on one mailbox: the first seen wins the read time. The
/// phone showing what the laptop saw sends nothing new and appends
/// nothing; a mark with one new id among the old makes a receipt of that
/// one alone, at the phone's clock.
#[test]
fn the_first_device_to_see_a_message_wins_and_a_second_sends_nothing_new() {
    let mut mailbox = Mailbox::new();
    let alice = leviculum_std::generate_identity();
    let one = mailbox.receive(&alice, "one", Some(Lev::mark()));
    let (_, accepted) = mailbox.seen(LAPTOP, RT, &[one]);
    assert_eq!(accepted.len(), 1);

    let (status, accepted) = mailbox.seen(PHONE, RT + 100, &[one]);
    assert_eq!(status, Status::Ok);
    assert!(accepted.is_empty(), "seen on the laptop already");
    assert_eq!(mailbox.seen_marks(), [(RT, vec![one])]);
    assert_eq!(mailbox.entries(KIND_SENT).len(), 1);

    let two = mailbox.receive(&alice, "two", Some(Lev::mark()));
    let (_, accepted) = mailbox.seen(PHONE, RT + 200, &[one, two]);
    assert_eq!(accepted.len(), 1);
    let (_, receipt) = opened(&mailbox, &accepted[0]);
    assert_eq!(receipt.ids, [two]);
    assert_eq!(receipt.read_at, RT + 200);
    assert_eq!(
        mailbox.seen_marks(),
        [(RT, vec![one]), (RT + 200, vec![two])]
    );
}

/// A device's screen model, a client of a home node, at the clock `at`.
fn device(at: i64) -> UiModel<Line> {
    let mut model = UiModel::<Line>::new(&[]);
    model.home_client = true;
    update(
        &mut model,
        UiEvent::Clock(WallTime {
            unix_secs: at,
            offset_secs: 0,
        }),
    );
    model
}

/// What a pull hands a device's engine and the engine hands the screen,
/// for the archive's received and sent entries: each as the event the
/// engine makes of it.
fn pulled(mailbox: &Mailbox, model: &mut UiModel<Line>) {
    for entry in mailbox
        .store
        .archive_since(0, usize::MAX)
        .expect("archive")
        .entries
    {
        let Ok(message) = Lxm::unpack(&entry.packed, None, None, DeliveryMethod::Direct) else {
            continue;
        };
        let body = message.content.clone();
        let event = match entry.kind {
            KIND_RECEIVED => OutboxEvent::Received {
                sent_at: 0,
                message_id: message.message_id,
                source: message.source_hash,
                title: message.title,
                body,
                fields: message.fields,
                verified: true,
            },
            KIND_SENT => OutboxEvent::HomeSent {
                message_id: message.message_id,
                destination: message.destination_hash,
                sent_at: message.timestamp as i64,
                body,
                fields: message.fields,
            },
            _ => continue,
        };
        update(model, UiEvent::Engine(event));
    }
}

/// A receipt arriving at the mailbox for a message one device sent
/// through it is archived as every message that comes, and both devices
/// show the message read at the reader's time after their pull; the
/// receipt the mailbox signed for what the reader saw comes to them as a
/// sent entry and is no bubble.
#[test]
fn a_receipt_arriving_at_the_mailbox_reads_the_message_on_every_device() {
    let mut mailbox = Mailbox::new();
    let alice = leviculum_std::generate_identity();
    let alice_address = delivery_address(&alice).expect("address");
    let submission = wire::Submission {
        client_id: [9; 16],
        destination: alice_address,
        title: Vec::new(),
        content: b"hello".to_vec(),
        fields: Lev::mark().fields().to_vec(),
    };
    let (status, accepted) = mailbox.ask(SEND_PATH, &wire::send_request(&submission), LAPTOP);
    assert_eq!(status, Status::Ok);
    let ours = accepted[0].id;
    // Alice's own message, which the laptop sees: the mailbox signs its
    // receipt.
    let theirs = mailbox.receive(&alice, "hi back", Some(Lev::mark()));
    let (_, accepted) = mailbox.seen(LAPTOP, RT + 10, &[theirs]);
    assert_eq!(accepted.len(), 1);
    // Alice reads ours (the mailbox signed it at its clock, RT + 500),
    // and her receipt comes alone.
    mailbox.receive(
        &alice,
        "",
        Some(Lev::mark_with(Receipt {
            read_at: RT + 530,
            ids: vec![ours],
        })),
    );

    for at in [i64::from(RT) + 560, i64::from(RT) + 1_100] {
        let mut model = device(at);
        pulled(&mailbox, &mut model);
        assert_eq!(model.conversations.len(), 1);
        let conversation = &model.conversations[0];
        assert_eq!(conversation.peer, alice_address);
        let bodies: Vec<&str> = conversation
            .messages
            .iter()
            .map(|m| m.body.as_str())
            .collect();
        assert_eq!(bodies, ["hello", "hi back"], "the receipts are no bubbles");
        let ours = conversation
            .messages
            .iter()
            .find(|m| m.id == Some(ours))
            .expect("ours");
        assert_eq!(ours.delivery, Some(DeliveryState::Read { at: RT + 530 }));
        assert!(conversation.flags.receipts);
    }
}

/// A seen mark from a client is the only kind that makes a receipt: a read
/// mark is kept as it always was and sends nothing.
#[test]
fn a_read_mark_sends_no_receipt() {
    let mut mailbox = Mailbox::new();
    let alice = leviculum_std::generate_identity();
    let one = mailbox.receive(&alice, "one", Some(Lev::mark()));
    let mark = Mark {
        flag: MarkFlag::Read,
        ids: vec![one],
    };
    let (status, accepted) = mailbox.ask(MARK_PATH, &wire::mark_request(&mark), LAPTOP);
    assert_eq!(status, Status::Ok);
    assert!(accepted.is_empty());
    assert!(mailbox.seen_marks().is_empty());
    assert_eq!(mailbox.entries(KIND_MARK).len(), 1);
}

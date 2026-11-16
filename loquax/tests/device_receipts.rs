//! The ticks tell the truth (1462, `docs/src/concepts/loquax-read-receipts.md`,
//! section 3): one tick is the proof of the recipient's identity, which its
//! mailbox signs as soon as it archived the message; two ticks are the
//! recipient's device saying it has it, `lev` `0x07`, sent by the device
//! itself or, on a client of a mailbox, signed by the mailbox on the
//! device's arrived mark.
//!
//! Each cell drives the writer's screen model, the recipient's screen model
//! and, where the recipient has one, its mailbox's answers
//! ([`loquax::home::server::answer`]) over its store, with real identities
//! and signed LXMF messages carried between them as the engines would.
//! Happy and unhappy paths: a mailbox-backed recipient whose device pulls,
//! a writer that does not ask, a recipient without a mailbox, a receipt
//! that is lost, a stock recipient.

use leviculum_lxmf::lev::{Lev, PropagationReceipt, CAP_PROPAGATION_RECEIPTS};
use leviculum_lxmf::message::{DeliveryMethod, Message as Lxm, Verification};
use leviculum_lxmf::router::MessageState;
use leviculum_std::Identity;
use leviculum_ui::delivery::DeliveryState;
use leviculum_ui::input::{InputBuffer, KeyEvent};
use loquax::engine::delivery_address;
use loquax::home::server::{answer, Home};
use loquax::home::wire::{self, Mark, MarkFlag, Status};
use loquax::home::{KIND_MARK, KIND_RECEIVED, MARK_PATH};
use loquax::outbox::OutboxEvent;
use loquax::store::Store;
use loquax::ui::chat::Mark as Ticks;
use loquax::ui::receipts::BATCH_MS;
use loquax::ui::strings::Lang;
use loquax::ui::{update, UiEffect, UiEvent, UiModel};
use loquax::when::WallTime;

/// The writer's clock at the send, unix seconds.
const RT: i64 = 1_800_000_000;
const PHONE: [u8; 16] = [0x22; 16];
const WEEK_MS: u64 = 7 * 24 * 3_600 * 1_000;

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

/// A screen model at the clock `at`, a client of a mailbox or its own node.
fn screen(at: i64, home_client: bool) -> UiModel<Line> {
    let mut model = UiModel::<Line>::new(&[]);
    model.home_client = home_client;
    update(
        &mut model,
        UiEvent::Clock(WallTime {
            unix_secs: at,
            offset_secs: 0,
        }),
    );
    model
}

/// One side of a conversation: an identity, its address, its screen.
struct Side {
    identity: Identity,
    address: [u8; 16],
    model: UiModel<Line>,
}

impl Side {
    fn new(home_client: bool) -> Self {
        let identity = leviculum_std::generate_identity();
        let address = delivery_address(&identity).expect("address");
        Self {
            identity,
            address,
            model: screen(RT, home_client),
        }
    }

    /// A phone with the app in the back (1506): its list is a screen of
    /// its own and no chat is open, so what arrives stays unread and a
    /// notification is what shows it.
    fn phone_in_the_back(home_client: bool) -> Self {
        let mut side = Self::new(home_client);
        side.model.list_apart = true;
        update(&mut side.model, UiEvent::Close);
        update(&mut side.model, UiEvent::Front(false));
        side
    }

    /// Write `body` to `to` on the screen and sign what the screen hands
    /// the engine as the engine would, `fields` passed through `edit` (a
    /// writer that does not ask strips bit 2 there); the router queues it
    /// and the runner is done with it. The signed message.
    fn write(&mut self, to: [u8; 16], body: &str, edit: impl Fn(&mut Lev)) -> Lxm {
        let effects = update(
            &mut self.model,
            UiEvent::Write {
                peer: to,
                body: body.to_string(),
            },
        );
        let fields = effects
            .iter()
            .find_map(|effect| match effect {
                UiEffect::Send {
                    to: peer,
                    body: sent,
                    fields,
                } if *peer == to && sent == body => Some(fields.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the screen sends it: {effects:?}"));
        let mut lev = Lev::from_fields(&fields).expect("our sends carry the mark");
        edit(&mut lev);
        let message = Lxm::create(
            to,
            self.address,
            &self.identity,
            RT as f64,
            Vec::new(),
            body.as_bytes().to_vec(),
            lev.fields().to_vec(),
            DeliveryMethod::Direct,
        )
        .expect("message");
        update(
            &mut self.model,
            UiEvent::Engine(OutboxEvent::Queued {
                message_id: message.message_id,
            }),
        );
        update(&mut self.model, UiEvent::SendDone(Ok(())));
        message
    }

    /// A reply typed into the notification of `to`'s messages (1506),
    /// `key` the newest message it showed: the reply as the engine signs
    /// it, when the screen sends one.
    fn notice_reply(&mut self, to: [u8; 16], key: u64, body: &str) -> Option<Lxm> {
        let effects = update(
            &mut self.model,
            UiEvent::NoticeReply {
                peer: to,
                key,
                body: body.to_string(),
            },
        );
        let fields = effects.iter().find_map(|effect| match effect {
            UiEffect::Send {
                to: peer,
                body: sent,
                fields,
            } if *peer == to && sent == body => Some(fields.clone()),
            _ => None,
        })?;
        let message = Lxm::create(
            to,
            self.address,
            &self.identity,
            RT as f64 + 30.0,
            Vec::new(),
            body.as_bytes().to_vec(),
            fields,
            DeliveryMethod::Direct,
        )
        .expect("message");
        update(
            &mut self.model,
            UiEvent::Engine(OutboxEvent::Queued {
                message_id: message.message_id,
            }),
        );
        update(&mut self.model, UiEvent::SendDone(Ok(())));
        Some(message)
    }

    /// The key a notification of `peer`'s chat is posted with now.
    fn notice_key(&self, peer: [u8; 16]) -> u64 {
        self.model.notice_key(&peer).expect("a message to notify")
    }

    /// The conversation with `peer`'s unread count.
    fn unread(&self, peer: [u8; 16]) -> usize {
        self.model
            .conversations
            .iter()
            .find(|c| c.peer == peer)
            .map_or(0, |c| c.unread)
    }

    /// The proof of `message` came back: the recipient's identity, its
    /// mailbox or its device, signed it.
    fn proven(&mut self, message: &Lxm) {
        update(
            &mut self.model,
            UiEvent::Engine(OutboxEvent::State {
                message_id: message.message_id,
                state: MessageState::Delivered,
            }),
        );
    }

    /// The engine hands the screen `message` as it received it, its
    /// signature checked against `from`.
    fn takes(&mut self, message: &Lxm, from: &Identity) -> Vec<UiEffect> {
        let packed = message.pack();
        let opened =
            Lxm::unpack(&packed, None, Some(from), DeliveryMethod::Direct).expect("unpack");
        assert_eq!(opened.verification, Verification::Valid);
        update(
            &mut self.model,
            UiEvent::Engine(OutboxEvent::Received {
                sent_at: opened.timestamp as i64,
                message_id: opened.message_id,
                source: opened.source_hash,
                title: opened.title,
                body: opened.content,
                fields: opened.fields,
                verified: true,
            }),
        )
    }

    /// The state and the mark of our message `id`.
    fn ticks(&self, id: &[u8; 32]) -> (Option<DeliveryState>, Ticks) {
        let message = self
            .model
            .conversations
            .iter()
            .flat_map(|c| &c.messages)
            .find(|m| m.outbound && m.id == Some(*id))
            .expect("our message");
        (message.delivery, Ticks::of(message.delivery))
    }

    /// The Info sheet's labels for our message `id` to `peer`, in English.
    fn info_labels(&self, peer: [u8; 16], id: &[u8; 32]) -> Vec<String> {
        let key = self
            .model
            .conversations
            .iter()
            .find(|c| c.peer == peer)
            .and_then(|c| c.messages.iter().find(|m| m.id == Some(*id)))
            .expect("our message")
            .activity;
        loquax::ui::info::message_info(&self.model, peer, key)
            .expect("its info")
            .rows
            .iter()
            .map(|row| row.resolve(Lang::En).0)
            .collect()
    }
}

/// The device receipts (`0x07`) among a screen's effects, signed by `by` as
/// its engine would sign every send: each as the message the writer gets.
fn device_receipts(effects: &[UiEffect], by: &Side) -> Vec<Lxm> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            UiEffect::Send { to, body, fields }
                if body.is_empty()
                    && Lev::from_fields(fields)
                        .is_some_and(|l| l.propagation_receipt.is_some()) =>
            {
                Some(
                    Lxm::create(
                        *to,
                        by.address,
                        &by.identity,
                        RT as f64 + 10.0,
                        Vec::new(),
                        Vec::new(),
                        fields.clone(),
                        DeliveryMethod::Direct,
                    )
                    .expect("the receipt"),
                )
            }
            _ => None,
        })
        .collect()
}

/// A recipient's mailbox: the recipient's identity (it signs as the
/// recipient), its store with the phone admitted.
struct Mailbox {
    _dir: tempfile::TempDir,
    identity: Identity,
    address: [u8; 16],
    store: Store,
}

impl Mailbox {
    fn of(identity: Identity) -> Self {
        let dir = tempfile::tempdir().expect("dir");
        let address = delivery_address(&identity).expect("address");
        let mut store = Store::open_in(dir.path(), address).expect("store");
        store.issue_token(&[1; 16], "phone", 10).expect("token");
        store
            .redeem_token(&[1; 16], &PHONE, 11, 3_600)
            .expect("admit");
        Self {
            _dir: dir,
            identity,
            address,
            store,
        }
    }

    /// The message came to the mailbox and is archived, as the daemon keeps
    /// what comes before it proves it.
    fn archive(&mut self, message: &Lxm) {
        self.store
            .archive_append(
                KIND_RECEIVED,
                &message.message_id,
                &message.pack(),
                RT,
                u64::MAX,
            )
            .expect("archive");
    }

    /// The device's marks handed in: the receipts the mailbox signed.
    fn marks(&mut self, marks: &[Mark]) -> Vec<Lxm> {
        let home = Home {
            identity: &self.identity,
            address: self.address,
            token_ttl_secs: 3_600,
            cap_bytes: u64::MAX,
            handover: None,
            groups: None,
            shown: Default::default(),
        };
        let mut signed = Vec::new();
        for mark in marks {
            let answered = answer(
                &mut self.store,
                &home,
                MARK_PATH,
                &wire::mark_request(mark),
                Some(PHONE),
                RT as f64 + 600.0,
            );
            assert_eq!(wire::read_status(&answered.bytes), Ok(Status::Ok));
            for outgoing in answered.accepted {
                signed.push(
                    Lxm::unpack(&outgoing.packed, None, None, DeliveryMethod::Direct)
                        .expect("the receipt"),
                );
            }
        }
        signed
    }

    /// The arrived marks the archive keeps.
    fn arrived_marks(&self) -> usize {
        self.store
            .archive_since(0, usize::MAX)
            .expect("archive")
            .entries
            .into_iter()
            .filter(|e| e.kind == KIND_MARK)
            .filter(|e| {
                matches!(
                    wire::read_mark(&e.packed),
                    Ok(Mark {
                        flag: MarkFlag::Arrived { .. },
                        ..
                    })
                )
            })
            .count()
    }
}

/// The arrived marks a screen made.
fn arrived_marks(model: &mut UiModel<Line>) -> Vec<Mark> {
    model
        .take_marks()
        .into_iter()
        .filter(|mark| matches!(mark.flag, MarkFlag::Arrived { .. }))
        .collect()
}

/// Cell 1, happy: a recipient with a mailbox. The mailbox archives the
/// message and proves it at once: one tick, "Arrived", and it stays one
/// while no device has pulled it, however long. The recipient's phone
/// pulls it, says so with an arrived mark, the mailbox signs the device
/// receipt with the recipient's identity, and the writer shows two ticks
/// with both times in the Info sheet. The phone's second mark of the same
/// id signs nothing.
#[test]
fn a_mailbox_backed_recipient_shows_two_ticks_only_after_its_device_pulls() {
    let mut alice = Side::new(false);
    let bob = leviculum_std::generate_identity();
    let mut mailbox = Mailbox::of(bob);
    let message = alice.write(mailbox.address, "bist du da?", |_| {});
    mailbox.archive(&message);
    alice.proven(&message);
    assert_eq!(
        alice.ticks(&message.message_id),
        (Some(DeliveryState::Arrived), Ticks::Arrived)
    );
    update(&mut alice.model, UiEvent::Elapsed(WEEK_MS));
    assert_eq!(
        alice.ticks(&message.message_id).1,
        Ticks::Arrived,
        "no device yet"
    );
    assert!(
        mailbox.store.outbox_open().expect("outbox").is_empty(),
        "archiving signs no device receipt"
    );

    // The phone pulls.
    let mut phone = screen(RT + 300, true);
    let archived =
        Lxm::unpack(&message.pack(), None, None, DeliveryMethod::Direct).expect("unpack");
    update(
        &mut phone,
        UiEvent::Engine(OutboxEvent::Received {
            sent_at: archived.timestamp as i64,
            message_id: archived.message_id,
            source: archived.source_hash,
            title: archived.title,
            body: archived.content,
            fields: archived.fields,
            verified: true,
        }),
    );
    update(&mut phone, UiEvent::Elapsed(BATCH_MS - 1));
    assert!(
        arrived_marks(&mut phone).is_empty(),
        "inside the batch window"
    );
    let effects = update(&mut phone, UiEvent::Elapsed(BATCH_MS));
    assert!(
        !effects.iter().any(|e| matches!(e, UiEffect::Send { .. })),
        "a client signs nothing itself: {effects:?}"
    );
    let marks = arrived_marks(&mut phone);
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0].ids, [message.message_id]);
    assert_eq!(
        marks[0].flag,
        MarkFlag::Arrived {
            at: (RT + 300) as u32
        }
    );
    let receipts = mailbox.marks(&marks);
    assert_eq!(receipts.len(), 1);
    assert_eq!(
        receipts[0].source_hash, mailbox.address,
        "the recipient's identity"
    );
    assert_eq!(receipts[0].destination_hash, alice.address);
    assert_eq!(mailbox.arrived_marks(), 1);

    alice.takes(&receipts[0], &mailbox.identity);
    assert_eq!(
        alice.ticks(&message.message_id),
        (Some(DeliveryState::Delivered), Ticks::Delivered)
    );
    let labels = alice.info_labels(mailbox.address, &message.message_id);
    for label in ["Sent", "Arrived", "On the device"] {
        assert!(labels.iter().any(|l| l == label), "{label}: {labels:?}");
    }
    // Another device's mark of the same id: nothing new is signed.
    assert!(mailbox.marks(&marks).is_empty());
}

/// Cell 2, unhappy: a recipient with a mailbox and a writer that does not
/// ask (no bit 2 in its mark). The phone pulls and owes nothing, so it
/// makes no arrived mark; a mark handed in anyway makes the mailbox sign
/// nothing. The writer stays at one tick, never two.
#[test]
fn a_mailbox_backed_recipient_without_the_capability_stays_at_one_tick() {
    let mut alice = Side::new(false);
    let mut mailbox = Mailbox::of(leviculum_std::generate_identity());
    let message = alice.write(mailbox.address, "ohne bit 2", |lev| {
        let caps = lev.capabilities.unwrap_or(0);
        lev.capabilities = Some(caps & !CAP_PROPAGATION_RECEIPTS);
    });
    mailbox.archive(&message);
    alice.proven(&message);
    let mut phone = screen(RT + 300, true);
    let archived =
        Lxm::unpack(&message.pack(), None, None, DeliveryMethod::Direct).expect("unpack");
    update(
        &mut phone,
        UiEvent::Engine(OutboxEvent::Received {
            sent_at: archived.timestamp as i64,
            message_id: archived.message_id,
            source: archived.source_hash,
            title: archived.title,
            body: archived.content,
            fields: archived.fields,
            verified: true,
        }),
    );
    update(&mut phone, UiEvent::Elapsed(BATCH_MS * 10));
    assert!(arrived_marks(&mut phone).is_empty(), "nothing owed");
    let forced = Mark {
        flag: MarkFlag::Arrived {
            at: (RT + 300) as u32,
        },
        ids: vec![message.message_id],
    };
    assert!(
        mailbox.marks(&[forced]).is_empty(),
        "the mailbox signs none"
    );
    update(&mut alice.model, UiEvent::Elapsed(WEEK_MS));
    assert_eq!(
        alice.ticks(&message.message_id),
        (Some(DeliveryState::Arrived), Ticks::Arrived)
    );
}

/// Cell 3, happy: a recipient without a mailbox. Its device proves the
/// message (one tick: the writer cannot know there is no mailbox) and says
/// it has it with its own `0x07` after the batch window; two ticks.
#[test]
fn a_recipient_without_a_mailbox_shows_two_ticks_on_its_devices_receipt() {
    let mut alice = Side::new(false);
    let mut bob = Side::new(false);
    let message = alice.write(bob.address, "direkt", |_| {});
    bob.takes(&message, &alice.identity);
    alice.proven(&message);
    assert_eq!(alice.ticks(&message.message_id).1, Ticks::Arrived);
    assert!(device_receipts(
        &update(&mut bob.model, UiEvent::Elapsed(BATCH_MS - 1)),
        &bob
    )
    .is_empty());
    let receipts = device_receipts(&update(&mut bob.model, UiEvent::Elapsed(BATCH_MS)), &bob);
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].destination_hash, alice.address);
    let lev = Lev::from_fields(&receipts[0].fields).expect("lev");
    assert_eq!(
        lev.propagation_receipt.map(|r| (r.received_at, r.ids)),
        Some((RT as u32, vec![message.message_id]))
    );
    alice.takes(&receipts[0], &bob.identity);
    assert_eq!(
        alice.ticks(&message.message_id),
        (Some(DeliveryState::Delivered), Ticks::Delivered)
    );
    // A late proof takes nothing back.
    alice.proven(&message);
    assert_eq!(alice.ticks(&message.message_id).1, Ticks::Delivered);
    // Bob owes nothing more: the batch went.
    assert!(device_receipts(
        &update(&mut bob.model, UiEvent::Elapsed(BATCH_MS * 3)),
        &bob
    )
    .is_empty());
}

/// Cell 3b, happy (1486): the answer carries the receipt. Bob reads Alice's
/// message and answers inside the batch window, so the device receipt he
/// owes her rides on his answer. Her message goes to two ticks on it, and
/// his answer is a message like any other: her device owes him its own
/// receipt, and his answer goes to two ticks too.
#[test]
fn an_answer_that_carries_a_device_receipt_is_acknowledged_itself() {
    let mut alice = Side::new(false);
    let mut bob = Side::new(false);
    let message = alice.write(bob.address, "frage", |_| {});
    bob.takes(&message, &alice.identity);
    alice.proven(&message);
    update(&mut bob.model, UiEvent::Elapsed(BATCH_MS - 1));
    let answer = bob.write(alice.address, "antwort", |_| {});
    let rider = Lev::from_fields(&answer.fields).expect("lev");
    assert_eq!(
        rider.propagation_receipt.map(|r| r.ids),
        Some(vec![message.message_id]),
        "the receipt rides on the answer"
    );
    assert!(
        device_receipts(
            &update(&mut bob.model, UiEvent::Elapsed(BATCH_MS * 3)),
            &bob
        )
        .is_empty(),
        "nothing owed alone once it rode"
    );
    alice.takes(&answer, &bob.identity);
    assert_eq!(
        alice.ticks(&message.message_id),
        (Some(DeliveryState::Delivered), Ticks::Delivered)
    );
    bob.proven(&answer);
    assert_eq!(bob.ticks(&answer.message_id).1, Ticks::Arrived);
    let receipts = device_receipts(
        &update(&mut alice.model, UiEvent::Elapsed(BATCH_MS)),
        &alice,
    );
    assert_eq!(
        receipts.len(),
        1,
        "Alice's device owes Bob his answer's receipt"
    );
    let lev = Lev::from_fields(&receipts[0].fields).expect("lev");
    assert_eq!(
        lev.propagation_receipt.map(|r| r.ids),
        Some(vec![answer.message_id])
    );
    bob.takes(&receipts[0], &alice.identity);
    assert_eq!(
        bob.ticks(&answer.message_id),
        (Some(DeliveryState::Delivered), Ticks::Delivered)
    );
}

/// Cell 3c (1557), at the mailbox: the same rule on a client of a mailbox.
/// Bob answers Alice with his device receipt riding on the answer, and
/// sends a bare receipt that asks (bit 2) as well. Alice's mailbox
/// archives both and her phone pulls both: the phone marks the answer and
/// not the bare receipt, and the mailbox signs the answer's receipt and
/// none for the bare one, even when a mark names it.
#[test]
fn at_the_mailbox_an_answer_with_a_receipt_is_acknowledged_and_a_bare_one_not() {
    let alice = leviculum_std::generate_identity();
    let mut mailbox = Mailbox::of(alice);
    let bob = Side::new(false);
    let asked = [0x5a; 32];
    let signed = |body: &str, at: f64| {
        let lev = Lev {
            capabilities: Some(CAP_PROPAGATION_RECEIPTS),
            propagation_receipt: Some(PropagationReceipt {
                received_at: RT as u32,
                ids: vec![asked],
            }),
            writer: Some(bob.address),
            ..Lev::default()
        };
        Lxm::create(
            mailbox.address,
            bob.address,
            &bob.identity,
            at,
            Vec::new(),
            body.as_bytes().to_vec(),
            lev.fields().to_vec(),
            DeliveryMethod::Direct,
        )
        .expect("message")
    };
    let answer = signed("antwort", RT as f64);
    let bare = signed("", RT as f64 + 1.0);
    mailbox.archive(&answer);
    mailbox.archive(&bare);

    let mut phone = screen(RT + 300, true);
    for message in [&answer, &bare] {
        let archived =
            Lxm::unpack(&message.pack(), None, None, DeliveryMethod::Direct).expect("unpack");
        update(
            &mut phone,
            UiEvent::Engine(OutboxEvent::Received {
                sent_at: archived.timestamp as i64,
                message_id: archived.message_id,
                source: archived.source_hash,
                title: archived.title,
                body: archived.content,
                fields: archived.fields,
                verified: true,
            }),
        );
    }
    update(&mut phone, UiEvent::Elapsed(BATCH_MS));
    let marks = arrived_marks(&mut phone);
    let marked: Vec<[u8; 32]> = marks.iter().flat_map(|m| m.ids.clone()).collect();
    assert_eq!(
        marked,
        [answer.message_id],
        "the phone marks the answer, not the bare receipt"
    );
    let forced = Mark {
        flag: MarkFlag::Arrived {
            at: (RT + 300) as u32,
        },
        ids: vec![answer.message_id, bare.message_id],
    };
    let receipts = mailbox.marks(&[forced]);
    assert_eq!(
        receipts.len(),
        1,
        "the mailbox signs the answer's receipt only"
    );
    assert_eq!(receipts[0].destination_hash, bob.address);
    let lev = Lev::from_fields(&receipts[0].fields).expect("lev");
    assert_eq!(
        lev.propagation_receipt.map(|r| r.ids),
        Some(vec![answer.message_id])
    );
}

/// Cell 4, unhappy: the device's receipt is lost on the way. The writer
/// stays at one tick: no timer turns a proof into two, a week later
/// neither.
#[test]
fn a_lost_device_receipt_stays_one_tick_and_never_flips_by_time() {
    let mut alice = Side::new(false);
    let mut bob = Side::new(false);
    let message = alice.write(bob.address, "geht verloren", |_| {});
    bob.takes(&message, &alice.identity);
    alice.proven(&message);
    let lost = device_receipts(&update(&mut bob.model, UiEvent::Elapsed(BATCH_MS)), &bob);
    assert_eq!(lost.len(), 1, "it was sent, and is lost");
    for ms in [BATCH_MS, 60_000, 3_600_000, WEEK_MS] {
        update(&mut alice.model, UiEvent::Elapsed(ms));
        update(
            &mut alice.model,
            UiEvent::Clock(WallTime {
                unix_secs: RT + (ms / 1_000) as i64,
                offset_secs: 0,
            }),
        );
        assert_eq!(
            alice.ticks(&message.message_id),
            (Some(DeliveryState::Arrived), Ticks::Arrived),
            "{ms} ms on"
        );
    }
    let labels = alice.info_labels(bob.address, &message.message_id);
    assert!(labels.iter().any(|l| l == "Arrived"), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "On the device"), "{labels:?}");
}

/// Cell 5, unhappy: a stock recipient. It proves, as LXMF does, and
/// writes back without any `lev` mark; it never sends a device receipt.
/// One tick on its proof, never two, and its answer changes none.
#[test]
fn a_stock_recipient_shows_one_tick_on_its_proof_and_never_two() {
    let mut alice = Side::new(false);
    let stock = leviculum_std::generate_identity();
    let stock_address = delivery_address(&stock).expect("address");
    let message = alice.write(stock_address, "an einen Stock-Client", |_| {});
    alice.proven(&message);
    let reply = Lxm::create(
        alice.address,
        stock_address,
        &stock,
        RT as f64 + 30.0,
        Vec::new(),
        b"got it".to_vec(),
        Vec::new(),
        DeliveryMethod::Direct,
    )
    .expect("reply");
    let mut writer = Side {
        identity: alice.identity,
        address: alice.address,
        model: alice.model,
    };
    writer.takes(&reply, &stock);
    let effects = update(&mut writer.model, UiEvent::Elapsed(WEEK_MS));
    assert!(
        device_receipts(&effects, &writer).is_empty(),
        "a stock sender is owed none either"
    );
    assert_eq!(
        writer.ticks(&message.message_id),
        (Some(DeliveryState::Arrived), Ticks::Arrived)
    );
}

// ---------------------------------------------------------------------
// 1466: the group's ticks tell the truth too. A member's mailbox proves
// the group's copy with the member's identity as soon as it archived it,
// so every copy proven is one tick, "Arrived"; two ticks, "On the
// devices", come only once every member's device said it has its copy
// (`0x07`, which the group's home books and counts in the writer's
// `0x03`), and the Info names each member's rung and since when.
// ---------------------------------------------------------------------

use leviculum_lxmf::lev::{CAP_GROUPS, CAP_READ_RECEIPTS};
use loquax::home::group::{self, Hosted, Outgoing};
use loquax::home::outgoing::Change;
use loquax::home::KIND_GROUP;
use loquax::store::groups::{MEMBER_DEVICES, MEMBER_GROUPS, MEMBER_JOINED, MEMBER_RECEIPTS};
use loquax::ui::groups::{GroupInfo, GroupMember, Standing};

/// A group's home: the admin's store with the group, its key, and what it
/// sent.
struct GroupHome {
    _dir: tempfile::TempDir,
    store: Store,
    hosted: Hosted,
}

impl GroupHome {
    /// "Familie", hosted by `admin`'s home, with `members` named.
    fn new(admin: [u8; 16], members: &[([u8; 16], &str)]) -> Self {
        let dir = tempfile::tempdir().expect("dir");
        let mut store = Store::open_in(dir.path(), admin).expect("store");
        let named: Vec<([u8; 16], String)> =
            members.iter().map(|(a, n)| (*a, n.to_string())).collect();
        let hosted = group::create(
            dir.path(),
            &mut store,
            "Familie",
            (admin, "Alice"),
            &named,
            RT,
        )
        .expect("the group");
        Self {
            _dir: dir,
            store,
            hosted,
        }
    }

    fn address(&self) -> [u8; 16] {
        self.hosted.group.address
    }

    /// A message to the group came: archived, as the daemon keeps it before
    /// it proves it, and taken in. What the home sends for it.
    fn takes(&mut self, message: &Lxm) -> Vec<Outgoing> {
        let opened =
            Lxm::unpack(&message.pack(), None, None, DeliveryMethod::Direct).expect("unpack");
        self.store
            .archive_append(KIND_GROUP, &opened.message_id, &opened.pack(), RT, u64::MAX)
            .expect("archive");
        group::accept(&mut self.store, &self.hosted, &opened, RT as f64 + 1.0)
            .expect("accept")
            .outgoing()
    }

    /// A member's identity proved `copy`, its mailbox or its device: the
    /// statuses that made due, as the daemon sends them.
    fn proven(&mut self, copy: &Outgoing, at: i64) -> Vec<Outgoing> {
        let (_, fresh) = group::book(&mut self.store, &copy.id, Change::Delivered, at)
            .expect("book")
            .expect("a copy");
        fresh
            .iter()
            .filter_map(|original| {
                group::status_after_proof(&self.store, &self.hosted, original, at as f64)
                    .expect("status")
            })
            .collect()
    }

    /// The member's flags as the home keeps them.
    fn flags(&self, member: &[u8; 16]) -> i64 {
        self.store
            .group_members(&self.address())
            .expect("members")
            .iter()
            .find(|m| m.address == *member)
            .map_or(0, |m| m.flags)
    }
}

/// What the home sends `to`, as `to` receives it.
fn opened_for(out: &[Outgoing], to: [u8; 16]) -> Lxm {
    let one = out
        .iter()
        .find(|o| o.destination == to)
        .unwrap_or_else(|| panic!("nothing for {to:02x?}: {out:?}"));
    Lxm::unpack(&one.packed, None, None, DeliveryMethod::Direct).expect("unpack")
}

/// A member's Join, as its client sends it: an empty message to the group
/// with its capabilities.
fn join(identity: &Identity, group: [u8; 16], caps: u64) -> Lxm {
    let address = delivery_address(identity).expect("address");
    Lxm::create(
        group,
        address,
        identity,
        RT as f64 - 60.0,
        Vec::new(),
        Vec::new(),
        Lev {
            capabilities: Some(caps),
            ..Lev::default()
        }
        .fields()
        .to_vec(),
        DeliveryMethod::Direct,
    )
    .expect("join")
}

/// The writer's device knows "Familie" and its members, as the admin's
/// devices learn it from the membership.
fn knows_the_group(model: &mut UiModel<Line>, group: [u8; 16], members: &[([u8; 16], &str)]) {
    model.groups.insert(
        group,
        GroupInfo {
            name: "Familie".into(),
            ours: true,
            standing: Standing::Joined,
            epoch: 1,
            members: members
                .iter()
                .map(|(address, name)| GroupMember {
                    address: *address,
                    name: name.to_string(),
                    admin: false,
                    joined: true,
                    left: false,
                })
                .collect(),
            ..GroupInfo::new(group)
        },
    );
}

impl Side {
    /// The Info sheet's rows for our message `id` to `peer`, in English.
    fn info(&self, peer: [u8; 16], id: &[u8; 32]) -> Vec<(String, String)> {
        let key = self
            .model
            .conversations
            .iter()
            .find(|c| c.peer == peer)
            .and_then(|c| c.messages.iter().find(|m| m.id == Some(*id)))
            .expect("our message")
            .activity;
        loquax::ui::info::message_info(&self.model, peer, key)
            .expect("its info")
            .rows
            .iter()
            .map(|row| row.resolve(Lang::En))
            .collect()
    }

    /// The words the chat's scene gives our bubble `id`'s mark, in English.
    fn mark_words(&self, peer: [u8; 16], id: &[u8; 32]) -> String {
        let c = self
            .model
            .conversations
            .iter()
            .position(|c| c.peer == peer)
            .expect("the chat");
        let key = self.model.conversations[c]
            .messages
            .iter()
            .find(|m| m.id == Some(*id))
            .expect("our message")
            .activity;
        let chat = loquax::ui::scene::chat(&self.model, c);
        chat.groups
            .iter()
            .flat_map(|g| &g.bubbles)
            .find(|b| b.bubble.key == key)
            .and_then(|b| b.mark.as_ref())
            .expect("our mark")
            .words
            .resolve(Lang::En)
    }
}

/// The Info's row of `name`.
fn rung_of<'a>(info: &'a [(String, String)], name: &str) -> &'a str {
    info.iter()
        .find(|(label, _)| label == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or_else(|| panic!("no row for {name}: {info:?}"))
}

/// A recipient's phone, a client of its mailbox, pulls `copy`: the arrived
/// marks it hands its mailbox once the batch window passed.
fn phone_pulls(copy: &Lxm) -> Vec<Mark> {
    let mut phone = screen(RT + 300, true);
    update(
        &mut phone,
        UiEvent::Engine(OutboxEvent::Received {
            sent_at: copy.timestamp as i64,
            message_id: copy.message_id,
            source: copy.source_hash,
            title: copy.title.clone(),
            body: copy.content.clone(),
            fields: copy.fields.clone(),
            verified: true,
        }),
    );
    update(&mut phone, UiEvent::Elapsed(BATCH_MS));
    arrived_marks(&mut phone)
}

/// Group cell 1, happy: Anna is a client of her mailbox and says device
/// receipts, Ben runs his own node and says them too. Every copy proven is
/// one tick, "Arrived", and the Info names both at arrived; Ben's device
/// receipt alone makes no status (Anna's copy is only in her mailbox) and
/// the writer stays at one, a week later too. Anna's phone pulls, her mailbox signs
/// the `0x07` to the group, the home counts it, and the writer shows two
/// ticks, "On the devices", both members on their device with the times.
/// Controls: a device receipt alone (no capability mark) leaves Ben's
/// read-receipt flag alone; a second mark of Anna's signs nothing.
#[test]
fn a_group_shows_two_ticks_only_once_every_members_device_has_it() {
    let mut alice = Side::new(false);
    let anna = leviculum_std::generate_identity();
    let mut annas_mailbox = Mailbox::of(anna);
    let mut ben = Side::new(false);
    let members = [(annas_mailbox.address, "Anna"), (ben.address, "Ben")];
    let mut home = GroupHome::new(alice.address, &members);
    let familie = home.address();
    knows_the_group(&mut alice.model, familie, &members);
    let all = CAP_READ_RECEIPTS | CAP_GROUPS | CAP_PROPAGATION_RECEIPTS;
    home.takes(&join(&annas_mailbox.identity, familie, all));
    home.takes(&join(&ben.identity, familie, all));
    for member in [annas_mailbox.address, ben.address] {
        let flags = home.flags(&member);
        assert_eq!(
            flags & (MEMBER_GROUPS | MEMBER_JOINED | MEMBER_RECEIPTS | MEMBER_DEVICES),
            MEMBER_GROUPS | MEMBER_JOINED | MEMBER_RECEIPTS | MEMBER_DEVICES,
            "the Join's mark"
        );
    }

    let message = alice.write(familie, "Brot ist da", |_| {});
    let copies = home.takes(&message);
    assert_eq!(copies.len(), 2, "{copies:?}");
    alice.proven(&message);
    assert_eq!(
        alice.ticks(&message.message_id),
        (Some(DeliveryState::InMailbox), Ticks::Mailbox),
        "the group has it"
    );
    let annas_copy = opened_for(&copies, annas_mailbox.address);
    let bens_copy = opened_for(&copies, ben.address);
    let copy_lev = Lev::from_fields(&annas_copy.fields).expect("Anna understands groups");
    assert!(
        copy_lev.supports_propagation_receipts(),
        "the writer's bit 2 rides on the copy"
    );

    // Anna's mailbox archives her copy and proves it; Ben's device takes
    // his and proves it.
    annas_mailbox.archive(&annas_copy);
    let effects = ben.takes(&bens_copy, &home.hosted.identity);
    assert!(
        device_receipts(&effects, &ben).is_empty(),
        "the batch waits"
    );
    let mut statuses = Vec::new();
    for copy in &copies {
        statuses.extend(home.proven(copy, RT + 5));
    }
    let status = opened_for(&statuses, alice.address);
    alice.takes(&status, &home.hosted.identity);
    assert_eq!(
        alice.ticks(&message.message_id),
        (Some(DeliveryState::Arrived), Ticks::Arrived),
        "every copy proven is one tick"
    );
    assert_eq!(alice.mark_words(familie, &message.message_id), "Arrived");

    // Ben's device says it has his copy.
    let receipts = device_receipts(&update(&mut ben.model, UiEvent::Elapsed(BATCH_MS)), &ben);
    assert_eq!(receipts.len(), 1, "Ben's device answers the group");
    assert_eq!(receipts[0].destination_hash, familie);
    let out = home.takes(&receipts[0]);
    assert_ne!(
        home.flags(&ben.address) & MEMBER_RECEIPTS,
        0,
        "control: a receipt alone says nothing of read receipts"
    );
    assert!(
        out.iter().all(|o| o.destination != alice.address),
        "Anna's device has not said it: no status yet {out:?}"
    );
    update(&mut alice.model, UiEvent::Elapsed(WEEK_MS));
    assert_eq!(
        alice.ticks(&message.message_id),
        (Some(DeliveryState::Arrived), Ticks::Arrived),
        "Anna's copy is only in her mailbox"
    );
    let info = alice.info(familie, &message.message_id);
    assert_eq!(rung_of(&info, "Status"), "Arrived", "{info:?}");
    for name in ["Anna", "Ben"] {
        assert!(rung_of(&info, name).starts_with("Arrived · "), "{info:?}");
    }

    // Anna's phone pulls; her mailbox signs the device receipt to the group.
    let marks = phone_pulls(&annas_copy);
    assert_eq!(marks.len(), 1);
    let signed = annas_mailbox.marks(&marks);
    assert_eq!(signed.len(), 1);
    assert_eq!(signed[0].destination_hash, familie);
    assert_eq!(signed[0].source_hash, annas_mailbox.address);
    let out = home.takes(&signed[0]);
    let status = opened_for(&out, alice.address);
    alice.takes(&status, &home.hosted.identity);
    assert_eq!(
        alice.ticks(&message.message_id),
        (Some(DeliveryState::Delivered), Ticks::Delivered),
        "every member's device"
    );
    assert_eq!(
        alice.mark_words(familie, &message.message_id),
        "On the devices"
    );
    let info = alice.info(familie, &message.message_id);
    assert_eq!(rung_of(&info, "Status"), "On the devices", "{info:?}");
    for name in ["Anna", "Ben"] {
        assert!(
            rung_of(&info, name).starts_with("On the device · "),
            "{name}: {info:?}"
        );
    }
    assert!(annas_mailbox.marks(&marks).is_empty(), "control: once");
}

/// Group cell 2, unhappy: Anna (mailbox, says device receipts), Ben (a
/// Loquax whose mark lacks bit 2, an older one that never says it) and
/// Carl, a stock client (no `lev` on his copy, so nothing for him to
/// answer). Every copy proven is one tick; Anna's device makes the home
/// tell the writer, who stays at one tick, a week later too, and the Info
/// says Ben and Carl hold it at arrived and Anna on her device.
#[test]
fn a_group_with_a_member_that_never_says_its_device_stays_at_one_tick() {
    let mut alice = Side::new(false);
    let mut annas_mailbox = Mailbox::of(leviculum_std::generate_identity());
    let ben = leviculum_std::generate_identity();
    let ben_address = delivery_address(&ben).expect("address");
    let carl = leviculum_std::generate_identity();
    let carl_address = delivery_address(&carl).expect("address");
    let members = [
        (annas_mailbox.address, "Anna"),
        (ben_address, "Ben"),
        (carl_address, "Carl"),
    ];
    let mut home = GroupHome::new(alice.address, &members);
    let familie = home.address();
    knows_the_group(&mut alice.model, familie, &members);
    home.takes(&join(
        &annas_mailbox.identity,
        familie,
        CAP_READ_RECEIPTS | CAP_GROUPS | CAP_PROPAGATION_RECEIPTS,
    ));
    home.takes(&join(&ben, familie, CAP_READ_RECEIPTS | CAP_GROUPS));
    assert_eq!(home.flags(&ben_address) & MEMBER_DEVICES, 0, "no bit 2");
    assert_eq!(home.flags(&carl_address), 0, "a stock member never wrote");

    let message = alice.write(familie, "Wer kommt mit?", |_| {});
    let copies = home.takes(&message);
    assert_eq!(copies.len(), 3);
    alice.proven(&message);
    let carls_copy = opened_for(&copies, carl_address);
    assert_eq!(
        Lev::from_fields(&carls_copy.fields),
        None,
        "a stock member's copy has no lev, nothing to answer"
    );
    let annas_copy = opened_for(&copies, annas_mailbox.address);
    annas_mailbox.archive(&annas_copy);
    let mut statuses = Vec::new();
    for (n, copy) in copies.iter().enumerate() {
        statuses.extend(home.proven(copy, RT + 10 + n as i64));
    }
    let status = opened_for(&statuses, alice.address);
    alice.takes(&status, &home.hosted.identity);
    assert_eq!(alice.ticks(&message.message_id).1, Ticks::Arrived);

    let signed = annas_mailbox.marks(&phone_pulls(&annas_copy));
    let out = home.takes(&signed[0]);
    let status = opened_for(&out, alice.address);
    let said = Lev::from_fields(&status.fields)
        .and_then(|l| l.status)
        .expect("0x03");
    assert_eq!((said.on_device, said.members), (Some(1), 3));
    alice.takes(&status, &home.hosted.identity);
    for ms in [BATCH_MS, 3_600_000, WEEK_MS] {
        update(&mut alice.model, UiEvent::Elapsed(ms));
        assert_eq!(
            alice.ticks(&message.message_id),
            (Some(DeliveryState::Arrived), Ticks::Arrived),
            "{ms} ms on"
        );
    }
    let info = alice.info(familie, &message.message_id);
    assert_eq!(rung_of(&info, "Status"), "Arrived", "{info:?}");
    assert!(
        rung_of(&info, "Anna").starts_with("On the device · "),
        "{info:?}"
    );
    for name in ["Ben", "Carl"] {
        assert!(
            rung_of(&info, name).starts_with("Arrived · "),
            "{name} holds it at one: {info:?}"
        );
    }
}

/// Group cell 3, unhappy: Ben's device receipt is lost on the way. Anna's
/// comes, the writer hears it (Anna on her device, Ben at arrived) and
/// stays at one tick: nothing flips it by time.
#[test]
fn a_groups_lost_device_receipt_stays_at_one_tick() {
    let mut alice = Side::new(false);
    let mut annas_mailbox = Mailbox::of(leviculum_std::generate_identity());
    let mut ben = Side::new(false);
    let members = [(annas_mailbox.address, "Anna"), (ben.address, "Ben")];
    let mut home = GroupHome::new(alice.address, &members);
    let familie = home.address();
    knows_the_group(&mut alice.model, familie, &members);
    let all = CAP_READ_RECEIPTS | CAP_GROUPS | CAP_PROPAGATION_RECEIPTS;
    home.takes(&join(&annas_mailbox.identity, familie, all));
    home.takes(&join(&ben.identity, familie, all));
    let message = alice.write(familie, "Verloren?", |_| {});
    let copies = home.takes(&message);
    alice.proven(&message);
    let annas_copy = opened_for(&copies, annas_mailbox.address);
    annas_mailbox.archive(&annas_copy);
    ben.takes(&opened_for(&copies, ben.address), &home.hosted.identity);
    let mut statuses = Vec::new();
    for copy in &copies {
        statuses.extend(home.proven(copy, RT + 10));
    }
    alice.takes(&opened_for(&statuses, alice.address), &home.hosted.identity);
    let lost = device_receipts(&update(&mut ben.model, UiEvent::Elapsed(BATCH_MS)), &ben);
    assert_eq!(lost.len(), 1, "sent, and lost");
    let signed = annas_mailbox.marks(&phone_pulls(&annas_copy));
    let out = home.takes(&signed[0]);
    assert!(
        out.iter().all(|o| o.destination != alice.address),
        "not every device said it: no status yet {out:?}"
    );
    for ms in [BATCH_MS, 3_600_000, WEEK_MS] {
        update(&mut alice.model, UiEvent::Elapsed(ms));
        assert_eq!(
            alice.ticks(&message.message_id),
            (Some(DeliveryState::Arrived), Ticks::Arrived),
            "{ms} ms on"
        );
    }
    let info = alice.info(familie, &message.message_id);
    assert!(rung_of(&info, "Ben").starts_with("Arrived · "), "{info:?}");
}

/// The read receipt (`0x01`) a message carries, its reader's time and ids.
fn read_receipt(message: &Lxm) -> Option<(u32, Vec<[u8; 32]>)> {
    Lev::from_fields(&message.fields)
        .and_then(|lev| lev.receipt)
        .map(|r| (r.read_at, r.ids))
}

/// Cell N1, happy (1506): Bob answers Alice from the notification and never
/// opens the chat. Whoever answers has read what the notification showed:
/// the read receipt rides on the answer, and Alice's message turns read at
/// Bob's clock. Bob's unread goes with it. Control: a plain write without
/// a viewport reads nothing, so it is the notification's reply that reads.
#[test]
fn a_reply_from_the_notification_reads_the_message_for_its_writer() {
    let mut alice = Side::new(false);
    let mut bob = Side::phone_in_the_back(false);
    let message = alice.write(bob.address, "kommst du?", |_| {});
    bob.takes(&message, &alice.identity);
    alice.proven(&message);
    assert_eq!(bob.unread(alice.address), 1);
    let key = bob.notice_key(alice.address);
    let answer = bob
        .notice_reply(alice.address, key, "bin gleich da")
        .expect("the reply goes");
    assert_eq!(
        read_receipt(&answer),
        Some((RT as u32, vec![message.message_id])),
        "the read receipt rides on the reply"
    );
    assert_eq!(bob.unread(alice.address), 0, "the reply read the chat");
    // Nothing more is owed alone: it rode.
    let later = update(&mut bob.model, UiEvent::Elapsed(BATCH_MS * 3));
    assert!(
        !later
            .iter()
            .any(|e| matches!(e, UiEffect::Send { body, fields, .. }
            if body.is_empty() && Lev::from_fields(fields).is_some_and(|l| l.receipt.is_some()))),
        "no second receipt: {later:?}"
    );
    alice.takes(&answer, &bob.identity);
    assert_eq!(
        alice.ticks(&message.message_id),
        (Some(DeliveryState::Read { at: RT as u32 }), Ticks::Read),
        "Alice sees her message read"
    );

    // Control: the same answer written plainly, no viewport, reads nothing.
    let mut alice = Side::new(false);
    let mut bob = Side::phone_in_the_back(false);
    let message = alice.write(bob.address, "kommst du?", |_| {});
    bob.takes(&message, &alice.identity);
    let answer = bob.write(alice.address, "bin gleich da", |_| {});
    assert_eq!(read_receipt(&answer), None, "control: a write is no read");
    alice.takes(&answer, &bob.identity);
    assert_ne!(alice.ticks(&message.message_id).1, Ticks::Read);
}

/// Cell N2, unhappy (1506): a message that came after the notification was
/// posted was not shown by it. The reply reads up to what it showed: the
/// receipt names the first message only, and the second stays unread and
/// unseen, so its own receipt waits for a viewport.
#[test]
fn a_reply_from_the_notification_leaves_a_later_message_unread() {
    let mut alice = Side::new(false);
    let mut bob = Side::phone_in_the_back(false);
    let first = alice.write(bob.address, "eins", |_| {});
    bob.takes(&first, &alice.identity);
    let key = bob.notice_key(alice.address);
    let second = alice.write(bob.address, "zwei", |_| {});
    bob.takes(&second, &alice.identity);
    assert_eq!(bob.unread(alice.address), 2);
    let answer = bob
        .notice_reply(alice.address, key, "ok")
        .expect("the reply goes");
    assert_eq!(
        read_receipt(&answer).map(|(_, ids)| ids),
        Some(vec![first.message_id]),
        "only what the notification showed"
    );
    assert_eq!(bob.unread(alice.address), 1, "the later one stays unread");
    alice.takes(&answer, &bob.identity);
    assert_eq!(alice.ticks(&first.message_id).1, Ticks::Read);
    assert_ne!(alice.ticks(&second.message_id).1, Ticks::Read);
}

/// Cell N3 (1506): the reply from the notification on a client of a
/// mailbox, and with the switch off. On the client the device holds no
/// identity: nothing rides on the reply, the seen mark goes to the mailbox
/// (which signs the receipt, section 8). With read receipts off, the reply
/// goes and no receipt and no seen mark does.
#[test]
fn a_reply_from_the_notification_on_a_mailbox_client_hands_a_seen_mark() {
    let mut alice = Side::new(false);
    let mut bob = Side::phone_in_the_back(true);
    let message = alice.write(bob.address, "hallo", |_| {});
    bob.takes(&message, &alice.identity);
    let key = bob.notice_key(alice.address);
    let answer = bob
        .notice_reply(alice.address, key, "hi")
        .expect("the reply goes");
    assert_eq!(
        read_receipt(&answer),
        None,
        "nothing rides on a client's reply"
    );
    let seen: Vec<Mark> = bob
        .model
        .take_marks()
        .into_iter()
        .filter(|m| matches!(m.flag, MarkFlag::Seen { .. }))
        .collect();
    assert_eq!(seen.len(), 1, "one seen mark: {seen:?}");
    assert_eq!(seen[0].ids, vec![message.message_id]);
    assert_eq!(seen[0].flag, MarkFlag::Seen { at: RT as u32 });

    let mut alice = Side::new(false);
    let mut bob = Side::phone_in_the_back(false);
    update(&mut bob.model, UiEvent::ReadReceipts(false));
    let message = alice.write(bob.address, "hallo", |_| {});
    bob.takes(&message, &alice.identity);
    let key = bob.notice_key(alice.address);
    let answer = bob
        .notice_reply(alice.address, key, "hi")
        .expect("the reply goes with the switch off too");
    assert_eq!(read_receipt(&answer), None, "the switch is off");
    assert_eq!(bob.unread(alice.address), 0, "read all the same");
}

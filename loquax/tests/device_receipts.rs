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

use leviculum_lxmf::lev::{Lev, CAP_PROPAGATION_RECEIPTS};
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

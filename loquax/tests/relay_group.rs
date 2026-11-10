//! A relayed group (1482): a bot such as SebastianObi's LXMF-Tools
//! `lxmf_distribution_group_extended` on its default configuration takes a
//! member's plain 1:1 message, puts `Name\n<hash>\n` in front of its text,
//! keeps the writer's timestamp and sends one 1:1 copy to every other
//! member, the writer's LXMF fields passed through unchanged (`fields =
//! Yes`); a message with fields and no text goes out too.
//!
//! The relay here does exactly that over real identities and signed LXMF
//! messages, between Loquax screen models as the engines would carry them.
//! The cells: two or three Loquax members through the relay see names and
//! texts, no receipt leaves to the relay, no empty post comes back (happy);
//! the relay changes its prefix and the posts still show, only the
//! writer's name is no longer taken from them (unhappy).

use leviculum_lxmf::lev::Lev;
use leviculum_lxmf::message::{DeliveryMethod, Message as Lxm, Verification};
use leviculum_std::Identity;
use leviculum_ui::input::{InputBuffer, KeyEvent};
use loquax::engine::delivery_address;
use loquax::outbox::OutboxEvent;
use loquax::ui::receipts::{BATCH_MS, SEEN_MS};
use loquax::ui::strings::Lang;
use loquax::ui::{update, UiEffect, UiEvent, UiModel};
use loquax::when::WallTime;

/// The writers' clock at their sends, unix seconds.
const RT: i64 = 1_800_000_000;

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

/// One Loquax member: an identity, its address, its screen with its engine
/// ready (so the screen knows its own address, as a running one does).
struct Member {
    name: &'static str,
    identity: Identity,
    address: [u8; 16],
    model: UiModel<Line>,
    /// Every effect the screen made, in order.
    effects: Vec<UiEffect>,
}

impl Member {
    fn new(name: &'static str) -> Self {
        let identity = leviculum_std::generate_identity();
        let address = delivery_address(&identity).expect("address");
        let mut model = UiModel::<Line>::new(&[]);
        update(
            &mut model,
            UiEvent::Clock(WallTime {
                unix_secs: RT,
                offset_secs: 0,
            }),
        );
        update(&mut model, UiEvent::Engine(OutboxEvent::Ready { address }));
        Self {
            name,
            identity,
            address,
            model,
            effects: Vec::new(),
        }
    }

    fn step(&mut self, event: UiEvent) {
        let effects = update(&mut self.model, event);
        self.effects.extend(effects);
    }

    /// Write `body` to the relay on the screen and sign what the screen
    /// hands the engine, as the engine would.
    fn write(&mut self, relay: [u8; 16], body: &str) -> Lxm {
        let effects = update(
            &mut self.model,
            UiEvent::Write {
                peer: relay,
                body: body.to_string(),
            },
        );
        let fields = effects
            .iter()
            .find_map(|effect| match effect {
                UiEffect::Send {
                    to,
                    body: sent,
                    fields,
                } if *to == relay && sent == body => Some(fields.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the screen sends it: {effects:?}"));
        update(&mut self.model, UiEvent::SendDone(Ok(())));
        Lxm::create(
            relay,
            self.address,
            &self.identity,
            RT as f64,
            Vec::new(),
            body.as_bytes().to_vec(),
            fields,
            DeliveryMethod::Direct,
        )
        .expect("message")
    }

    /// The engine hands the screen `message` as it received it, its
    /// signature checked against `from`.
    fn takes(&mut self, message: &Lxm, from: &Identity) {
        let opened =
            Lxm::unpack(&message.pack(), None, Some(from), DeliveryMethod::Direct).expect("unpack");
        assert_eq!(opened.verification, Verification::Valid);
        self.step(UiEvent::Engine(OutboxEvent::Received {
            sent_at: opened.timestamp as i64,
            message_id: opened.message_id,
            source: opened.source_hash,
            title: opened.title,
            body: opened.content,
            fields: opened.fields,
            verified: true,
        }));
    }

    /// The reader opens the relay's chat, in front, and every bubble stays
    /// on screen long enough to be seen; then the batch window passes.
    fn reads(&mut self, relay: [u8; 16], from_ms: u64) {
        self.step(UiEvent::Open { peer: relay });
        self.step(UiEvent::Front(true));
        let keys = self
            .model
            .conversations
            .iter()
            .find(|c| c.peer == relay)
            .map(|c| c.messages.iter().map(|m| m.activity).collect())
            .unwrap_or_default();
        self.step(UiEvent::Viewport { peer: relay, keys });
        self.step(UiEvent::Elapsed(from_ms + SEEN_MS));
        self.step(UiEvent::Elapsed(from_ms + SEEN_MS + BATCH_MS));
    }

    /// Every send to `to` that carries a receipt of either kind, signed as
    /// the engine would sign it.
    fn receipts_to(&self, to: [u8; 16]) -> Vec<Lxm> {
        self.effects
            .iter()
            .filter_map(|effect| match effect {
                UiEffect::Send {
                    to: peer,
                    body,
                    fields,
                } if *peer == to
                    && Lev::from_fields(fields).is_some_and(|lev| {
                        lev.receipt.is_some() || lev.propagation_receipt.is_some()
                    }) =>
                {
                    Some(
                        Lxm::create(
                            to,
                            self.address,
                            &self.identity,
                            RT as f64 + 30.0,
                            Vec::new(),
                            body.as_bytes().to_vec(),
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

    /// The bodies of the bubbles the relay's chat draws.
    fn bubbles(&self, relay: [u8; 16]) -> Vec<loquax::ui::chat::Bubble> {
        let c = self
            .model
            .conversations
            .iter()
            .position(|c| c.peer == relay)
            .expect("the relay's chat");
        loquax::ui::chat::bubbles(&self.model, c)
    }
}

/// The bot: its identity and address, and its prefix
/// (`send_prefix = !source_name!!n!<!source_address!>!n!` by default).
struct Relay {
    identity: Identity,
    address: [u8; 16],
    prefix: fn(&str, &[u8; 16]) -> String,
}

fn default_prefix(name: &str, address: &[u8; 16]) -> String {
    format!("{name}\n<{}>\n", loquax::address::to_hex(address))
}

impl Relay {
    fn new(prefix: fn(&str, &[u8; 16]) -> String) -> Self {
        let identity = leviculum_std::generate_identity();
        let address = delivery_address(&identity).expect("address");
        Self {
            identity,
            address,
            prefix,
        }
    }

    /// One copy of `original` from `writer` to `to`: the prefix in front of
    /// its text, its fields as they came, its writer's timestamp. A message
    /// with neither text nor fields is not relayed (`:1073-1076`).
    fn copy(&self, original: &Lxm, writer: &str, to: [u8; 16]) -> Option<Lxm> {
        if original.content.is_empty() && original.fields.is_empty() {
            return None;
        }
        let mut content = (self.prefix)(writer, &original.source_hash).into_bytes();
        content.extend_from_slice(&original.content);
        Some(
            Lxm::create(
                to,
                self.address,
                &self.identity,
                original.timestamp,
                original.title.clone(),
                content,
                original.fields.clone(),
                DeliveryMethod::Direct,
            )
            .expect("the copy"),
        )
    }
}

/// `writer` sent `original` to the relay: every other member takes the
/// relay's copy.
fn relay_to_all(relay: &Relay, original: &Lxm, writer: &str, members: &mut [&mut Member]) {
    for member in members.iter_mut() {
        if member.address == original.source_hash {
            continue;
        }
        if let Some(copy) = relay.copy(original, writer, member.address) {
            member.takes(&copy, &relay.identity);
        }
    }
}

/// What a post says beyond a relay's default prefix: a bubble that is
/// nothing but `Name\n<hash>\n` is an empty post, however many bytes.
fn words(body: &str) -> &str {
    let mut lines = body.splitn(3, '\n');
    match (lines.next(), lines.next(), lines.next()) {
        (Some(name), Some(hash), Some(rest))
            if !name.is_empty()
                && hash.len() == 34
                && hash.starts_with('<')
                && hash.ends_with('>') =>
        {
            rest
        }
        _ => body,
    }
}

/// The mvr and the happy cell. Anna and Ben write through the relay; Ben
/// and Carl take Anna's copy, Anna and Carl take Ben's, and everyone reads.
/// No receipt of either kind goes to the relay: the mark on the copies is
/// Anna's or Ben's, passed through, never the relay's own word. Whatever
/// the relay still hands back (here: nothing) makes no empty post.
#[test]
fn a_relay_gets_no_receipts_and_hands_back_no_empty_posts() {
    let relay = Relay::new(default_prefix);
    let mut anna = Member::new("Anna");
    let mut ben = Member::new("Ben");
    let mut carl = Member::new("Carl");

    let hallo = anna.write(relay.address, "Hallo zusammen");
    relay_to_all(&relay, &hallo, anna.name, &mut [&mut ben, &mut carl]);
    let moin = ben.write(relay.address, "Moin Anna");
    relay_to_all(&relay, &moin, ben.name, &mut [&mut anna, &mut carl]);

    for member in [&mut anna, &mut ben, &mut carl] {
        member.step(UiEvent::Elapsed(BATCH_MS));
        member.reads(relay.address, BATCH_MS);
    }

    // What the relay would hand everyone back: each receipt as a post.
    let mut handed_back = 0;
    for (name, receipts) in [
        (anna.name, anna.receipts_to(relay.address)),
        (ben.name, ben.receipts_to(relay.address)),
        (carl.name, carl.receipts_to(relay.address)),
    ] {
        for receipt in receipts {
            handed_back += 1;
            relay_to_all(
                &relay,
                &receipt,
                name,
                &mut [&mut anna, &mut ben, &mut carl],
            );
        }
    }
    assert_eq!(handed_back, 0, "no receipt goes to the relay");

    // Carl sees two posts, each under its writer, without the prefix; the
    // bar says the chat is a group via a relay; the Info names the writer
    // with the 4-hex tail.
    let bubbles = carl.bubbles(relay.address);
    let shown: Vec<(Option<String>, &str)> = bubbles
        .iter()
        .map(|b| (b.writer.as_ref().map(|w| w.1.clone()), b.body.as_str()))
        .collect();
    assert_eq!(
        shown,
        [
            (Some("Anna".to_string()), "Hallo zusammen"),
            (Some("Ben".to_string()), "Moin Anna")
        ]
    );
    assert_eq!(bubbles[0].writer.as_ref().map(|w| w.0), Some(anna.address));
    let c = carl
        .model
        .conversations
        .iter()
        .position(|c| c.peer == relay.address)
        .expect("chat");
    let chat = loquax::ui::scene::chat(&carl.model, c);
    let title = chat.bar.title.resolve(Lang::De);
    assert!(title.ends_with(" · Gruppe über Relais"), "{title}");
    let from = loquax::ui::info::message_info(&carl.model, relay.address, bubbles[0].key)
        .expect("info")
        .rows[0]
        .value
        .resolve(Lang::En);
    let tail = &loquax::address::to_hex(&anna.address)[..4];
    assert!(from.starts_with(&format!("Anna ·{tail}, via ")), "{from}");
    let row = carl
        .model
        .conversations
        .iter()
        .find(|c| c.peer == relay.address)
        .and_then(|c| c.messages.last())
        .map(|m| loquax::ui::chat::preview(m, Lang::En));
    assert_eq!(row.as_deref(), Some("Ben: Moin Anna"));

    // Control: the same members 1:1 still say they have it.
    let direct = anna.write(carl.address, "direkt");
    carl.takes(&direct, &anna.identity);
    carl.step(UiEvent::Elapsed(10 * BATCH_MS));
    assert_eq!(
        carl.receipts_to(anna.address).len(),
        1,
        "a writer's own mark is still honoured"
    );

    for member in [&anna, &ben, &carl] {
        let bubbles = member.bubbles(relay.address);
        let theirs: Vec<&str> = bubbles
            .iter()
            .filter(|b| !b.ours)
            .map(|b| b.body.as_str())
            .collect();
        assert!(
            theirs.iter().all(|body| !words(body).trim().is_empty()),
            "{}: no empty post: {theirs:?}",
            member.name
        );
    }
}

/// A writer before 1482 put no writer in its mark. Through the relay the
/// mark is still not taken as the relay's: the copy names another writer
/// in its text. And what the relay makes of such a writer's receipts, its
/// name over no words, is no post.
#[test]
fn a_mark_from_before_the_writer_key_is_not_the_relays_either() {
    let relay = Relay::new(default_prefix);
    let mut anna = Member::new("Anna");
    let mut carl = Member::new("Carl");
    let old_lev = |fields: &[leviculum_lxmf::Field]| {
        let mut lev = Lev::from_fields(fields).expect("lev");
        lev.writer = None;
        lev
    };
    let mut post = anna.write(relay.address, "von gestern");
    post = Lxm::create(
        relay.address,
        anna.address,
        &anna.identity,
        RT as f64,
        Vec::new(),
        post.content.clone(),
        old_lev(&post.fields).fields().to_vec(),
        DeliveryMethod::Direct,
    )
    .expect("an old writer's post");
    let again = Lxm::create(
        relay.address,
        anna.address,
        &anna.identity,
        RT as f64 + 1.0,
        Vec::new(),
        b"und heute".to_vec(),
        post.fields.clone(),
        DeliveryMethod::Direct,
    )
    .expect("a second");
    relay_to_all(&relay, &post, anna.name, &mut [&mut carl]);
    relay_to_all(&relay, &again, anna.name, &mut [&mut carl]);
    carl.step(UiEvent::Elapsed(BATCH_MS));
    carl.reads(relay.address, BATCH_MS);
    assert!(
        carl.receipts_to(relay.address).is_empty(),
        "{:?}",
        carl.effects
    );

    // An old Ben's receipt to the relay, handed to Carl.
    let ben = leviculum_std::generate_identity();
    let ben_address = delivery_address(&ben).expect("address");
    let receipt = Lev {
        propagation_receipt: Some(leviculum_lxmf::lev::PropagationReceipt {
            received_at: RT as u32,
            ids: vec![post.message_id],
        }),
        capabilities: Some(7),
        ..Lev::default()
    };
    let old_receipt = Lxm::create(
        relay.address,
        ben_address,
        &ben,
        RT as f64 + 20.0,
        Vec::new(),
        Vec::new(),
        receipt.fields().to_vec(),
        DeliveryMethod::Direct,
    )
    .expect("an old receipt");
    let before = carl.bubbles(relay.address).len();
    let copy = relay
        .copy(&old_receipt, "Ben", carl.address)
        .expect("relayed");
    carl.takes(&copy, &relay.identity);
    assert_eq!(carl.bubbles(relay.address).len(), before, "no empty post");
    let kept = carl
        .model
        .conversations
        .iter()
        .find(|c| c.peer == relay.address)
        .map_or(0, |c| c.messages.len());
    assert_eq!(kept, 2, "nothing kept of it");
}

/// The unhappy cell: the relay's operator changed the prefix to
/// `[Name] `. The posts still show, whole, and no writer is named, since
/// nothing in them names one any more; the bar is a plain chat's. No
/// receipt goes to the relay all the same: the marks name their writers.
#[test]
fn a_changed_prefix_still_shows_the_posts_and_names_nobody() {
    fn bracket(name: &str, _: &[u8; 16]) -> String {
        format!("[{name}] ")
    }
    let relay = Relay::new(bracket);
    let mut anna = Member::new("Anna");
    let mut ben = Member::new("Ben");
    let mut carl = Member::new("Carl");
    let hallo = anna.write(relay.address, "Hallo zusammen");
    relay_to_all(&relay, &hallo, anna.name, &mut [&mut ben, &mut carl]);
    let moin = ben.write(relay.address, "Moin Anna");
    relay_to_all(&relay, &moin, ben.name, &mut [&mut anna, &mut carl]);
    for member in [&mut anna, &mut ben, &mut carl] {
        member.step(UiEvent::Elapsed(BATCH_MS));
        member.reads(relay.address, BATCH_MS);
    }
    for member in [&anna, &ben, &carl] {
        assert!(
            member.receipts_to(relay.address).is_empty(),
            "{}: no receipt to the relay",
            member.name
        );
    }
    let bubbles = carl.bubbles(relay.address);
    let shown: Vec<(bool, &str)> = bubbles
        .iter()
        .map(|b| (b.writer.is_some(), b.body.as_str()))
        .collect();
    assert_eq!(
        shown,
        [(false, "[Anna] Hallo zusammen"), (false, "[Ben] Moin Anna")]
    );
    let c = carl
        .model
        .conversations
        .iter()
        .position(|c| c.peer == relay.address)
        .expect("chat");
    let title = loquax::ui::scene::chat(&carl.model, c)
        .bar
        .title
        .resolve(Lang::En);
    assert!(!title.contains("relay"), "{title}");
}

/// A reaction and a reply through the relay point at the replier's own
/// copy, an id no other member holds. The reaction is a bubble that says
/// so under its writer, never a chip on nothing; the reply quotes what its
/// `0x31` carries under its writer's name, or says the message is not here.
#[test]
fn a_target_that_is_not_here_is_said_not_dropped() {
    use leviculum_lxmf::constants::{FIELD_REPLY_QUOTE, FIELD_REPLY_TO};
    use leviculum_lxmf::reaction::Reaction;
    let relay = Relay::new(default_prefix);
    let mut anna = Member::new("Anna");
    let mut carl = Member::new("Carl");
    let hallo = anna.write(relay.address, "Hallo zusammen");
    relay_to_all(&relay, &hallo, anna.name, &mut [&mut carl]);
    let elsewhere = [0x77; 32];
    let stock = |body: &[u8], fields: Vec<leviculum_lxmf::Field>, at: f64| {
        let ben = leviculum_std::generate_identity();
        let ben_address = delivery_address(&ben).expect("address");
        let message = Lxm::create(
            relay.address,
            ben_address,
            &ben,
            at,
            Vec::new(),
            body.to_vec(),
            fields,
            DeliveryMethod::Direct,
        )
        .expect("a stock member's message");
        relay.copy(&message, "Ben", carl.address).expect("relayed")
    };
    let reaction = Reaction {
        to: elsewhere,
        content: "👍".to_string(),
    };
    let mut quoted = Vec::new();
    leviculum_lxmf::msgpack::bin(&mut quoted, &elsewhere);
    let mut quote_text = Vec::new();
    leviculum_lxmf::msgpack::bin(
        &mut quote_text,
        format!(
            "Anna\n<{}>\nHallo zusammen",
            loquax::address::to_hex(&anna.address)
        )
        .as_bytes(),
    );
    let copies = [
        stock(
            b"",
            vec![(
                leviculum_lxmf::constants::FIELD_REACTION,
                reaction.encode_field_value(),
            )],
            RT as f64 + 5.0,
        ),
        stock(
            b"Moin!",
            vec![
                (FIELD_REPLY_TO, quoted.clone()),
                (FIELD_REPLY_QUOTE, quote_text),
            ],
            RT as f64 + 6.0,
        ),
        stock(
            b"Ohne Zitat",
            vec![(FIELD_REPLY_TO, quoted)],
            RT as f64 + 7.0,
        ),
    ];
    for copy in &copies {
        carl.takes(copy, &relay.identity);
    }
    let bubbles = carl.bubbles(relay.address);
    let shown: Vec<(Option<String>, &str, Option<&str>)> = bubbles
        .iter()
        .map(|b| {
            (
                b.writer.as_ref().map(|w| w.1.clone()),
                b.body.as_str(),
                b.quote.as_ref().map(|q| q.text.as_str()),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            (Some("Anna".into()), "Hallo zusammen", None),
            (
                Some("Ben".into()),
                "reacted 👍 to a message that is not here",
                None
            ),
            (Some("Ben".into()), "Moin!", Some("Anna: Hallo zusammen")),
            (
                Some("Ben".into()),
                "Ohne Zitat",
                Some("A message that is not here")
            ),
        ]
    );
    assert!(
        bubbles.iter().all(|b| b.chips.is_empty()),
        "no chip on nothing"
    );
}

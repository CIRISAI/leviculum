//! The profile on the phone (1443): the activity is the renderer of the
//! core's profile scene, the one the window and the terminal draw. Java's
//! `Core.profileShown` decodes to the avatar's `Intent::Profile` and Back,
//! as the window's click and the terminal's `P` do; the answer's `words`
//! are that scene, resolved, and pinned here as Java decodes them, in
//! German: `golden/profile-words.json`; `LOQUAX_WRITE_GOLDEN=1` writes it
//! again.

use std::path::PathBuf;

use leviculum_ui::strings::Lang;
use loquax::ui::intent::Intent;
use loquax::ui::profile::{Device, Mailbox, ProfileFacts, Rung};
use loquax::ui::scene::LayoutClass;
use loquax::ui::{update, UiEvent, UiModel};
use loquax_android::schema::{self, ProfileWords, Request};
use loquax_android::session::{self, Decoded};

mod support {
    pub mod java;
}
use support::java::{java_map, Java};

/// A phone's model on a fixed identity, so the address and the code are
/// the golden's: a client of a mailbox on a server, reached over the
/// relay and connected, this phone its one device here.
fn phone() -> (tempfile::TempDir, UiModel<session::Line>) {
    let home = tempfile::tempdir().expect("app dir");
    let private: [u8; 64] = core::array::from_fn(|i| i as u8 + 3);
    let identity = leviculum_core::Identity::from_private_key_bytes(&private).expect("an identity");
    let me = loquax::engine::delivery_address(&identity).expect("address");
    let store = loquax::store::Store::open_in(home.path(), me).expect("the store");
    let mut model = session::known(home.path(), &store).expect("a model");
    model.lang = Lang::De;
    model.address = Some(me);
    let code = loquax::lxma::encode(&identity.public_key_bytes()).expect("a code");
    let qr = loquax::home::qr::verbatim_modules(&code).expect("a QR");
    model.own_code = Some(loquax::ui::OwnCode::of(code, Some(qr)));
    update(
        &mut model,
        UiEvent::ProfileFacts(Box::new(ProfileFacts {
            name: String::new(),
            mailbox: Mailbox::Away {
                name: Some("Lews Rechner".to_string()),
                rung: Some(Rung::Relayed),
                word: Some(loquax::home::board::Word::Connected),
            },
            devices: vec![Device {
                name: String::new(),
                this: true,
            }],
            ..Default::default()
        })),
    );
    (home, model)
}

/// What Java's `Core.profileShown(open)` sends, decoded as the session
/// decodes it.
fn shown(open: bool) -> Intent {
    let request: Request = schema::decode(&java_map(&[
        ("t", Java::Str("profile")),
        ("open", Java::Bool(open)),
    ]))
    .expect("a request");
    match session::decode(request).expect("decodes") {
        Decoded::Intent(intent) => intent,
        Decoded::Platform(_) => panic!("the profile is the core's intent"),
    }
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

fn check(name: &str, words: &ProfileWords) {
    let mut got = serde_json::to_string_pretty(words).expect("json");
    got.push('\n');
    let path = golden(name);
    if std::env::var_os("LOQUAX_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, &got).expect("write the golden");
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        got == want,
        "{name} moved; the answer now:\n{got}\n(LOQUAX_WRITE_GOLDEN=1 writes it)"
    );
}

/// The avatar's tap on the phone is the intent the window's click and the
/// terminal's `P` raise, and leaving the activity is Back.
#[test]
fn the_activity_comes_with_the_avatars_intent_and_leaves_with_back() {
    assert_eq!(shown(true), Intent::Profile);
    assert_eq!(shown(false), Intent::Back);
}

/// The words Java draws are the core's scene, resolved: the same scene
/// the window and the terminal draw once the profile is open, and as
/// Java decodes them the golden.
#[test]
fn the_profile_words_are_the_cores_scene() {
    let (_home, mut model) = phone();
    update(&mut model, UiEvent::Profile { open: true });
    let scene = model
        .scene(LayoutClass::Compact)
        .profile
        .expect("the profile is open");
    let words: ProfileWords =
        schema::decode(&schema::encode(&session::profile_words(&model, Lang::De)))
            .expect("Java's decode");
    assert_eq!(words.title, scene.title.resolve(Lang::De));
    assert_eq!(words.mailbox, scene.mailbox.resolve(Lang::De));
    assert_eq!(words.fours, scene.fours);
    assert_eq!(words.title, "Dein Profil");
    assert_eq!(words.state, "Verbunden über einen Vermittlungsserver");
    assert_eq!(words.devices, ["Dieses Gerät"]);
    check("profile-words.json", &words);
}

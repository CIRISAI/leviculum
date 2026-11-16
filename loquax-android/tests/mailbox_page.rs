//! The mailbox page on the phone (1642): the settings page is the renderer
//! of the core's mailbox page, the one the window and the terminal draw.
//! Java's `Core.mailboxPage` sends `{"t": "mailboxpage"}`; the answer is
//! that scene resolved, pinned here as Java decodes it, in German, for the
//! shared fixture (`loquax/tests/support/mailbox_page.rs`) reachable and
//! unreachable: `golden/mailbox-page-reachable.json` and
//! `golden/mailbox-page-unreachable.json`; `LOQUAX_WRITE_GOLDEN=1` writes
//! them again.

use std::path::PathBuf;

use leviculum_ui::strings::Lang;
use loquax::ui::UiModel;
use loquax_android::schema::{self, MailboxPage, Query};
use loquax_android::session;

mod support {
    pub mod java;
}
use support::java::{java_map, Java};

mod fixture {
    include!("../../loquax/tests/support/mailbox_page.rs");
}

/// A phone's model with the fixture told, in German.
fn phone(reachable: bool) -> (tempfile::TempDir, UiModel<session::Line>) {
    let home = tempfile::tempdir().expect("app dir");
    let store =
        loquax::store::Store::open_in(home.path(), fixture::MAILBOX_ADDRESS).expect("the store");
    let mut model = session::known(home.path(), &store).expect("a model");
    model.lang = Lang::De;
    fixture::mailbox_told(&mut model, reachable);
    (home, model)
}

fn check(name: &str, page: &MailboxPage) {
    let mut got = serde_json::to_string_pretty(page).expect("json");
    got.push('\n');
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("LOQUAX_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, &got).expect("write the golden");
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        got == want,
        "{name} moved; the answer now:\n{got}\n(LOQUAX_WRITE_GOLDEN=1 writes it)"
    );
}

/// What Java decodes of the page of `model`.
fn as_java_reads(model: &UiModel<session::Line>) -> MailboxPage {
    schema::decode(&schema::encode(&session::mailbox_page_words(
        model,
        Lang::De,
    )))
    .expect("Java's decode")
}

/// Java's bytes are the query, and the words Java draws are the core's
/// page, resolved, line for line: the mailbox named, its address in full
/// with the copy, the way and the devices.
#[test]
fn the_mailbox_page_is_the_cores_scene_for_java() {
    let request: Query =
        schema::decode(&java_map(&[("t", Java::Str("mailboxpage"))])).expect("Java's bytes");
    assert_eq!(request, Query::MailboxPage);
    let (_home, model) = phone(true);
    let page = as_java_reads(&model);
    let scene = loquax::ui::mailbox_page::page(&model);
    assert_eq!(page.groups.len(), scene.groups.len());
    for (drawn, core) in page.groups.iter().zip(&scene.groups) {
        assert_eq!(drawn.title, core.title.resolve(Lang::De));
        let labels: Vec<String> = core
            .lines
            .iter()
            .map(|l| l.label.resolve(Lang::De))
            .collect();
        assert_eq!(
            drawn
                .lines
                .iter()
                .map(|l| l.label.clone())
                .collect::<Vec<_>>(),
            labels
        );
    }
    let address = loquax::address::to_hex(&fixture::MAILBOX_ADDRESS);
    let copy = page.groups[0].lines.iter().find_map(|l| l.copy.clone());
    assert_eq!(copy.as_deref(), Some(address.as_str()));
    assert_eq!(page.copy_label, "Kopieren");
    check("mailbox-page-reachable.json", &page);
}

/// The unreachable older mailbox, as Java reads it.
#[test]
fn the_unreachable_older_mailbox_page_for_java() {
    let (_home, model) = phone(false);
    let page = as_java_reads(&model);
    let said: Vec<String> = page
        .groups
        .iter()
        .flat_map(|g| g.lines.iter())
        .map(|l| l.label.clone())
        .collect();
    assert!(
        said.iter()
            .any(|l| l == "Dieses Gerät nicht verbunden seit 2 Std."),
        "{said:#?}"
    );
    check("mailbox-page-unreachable.json", &page);
}

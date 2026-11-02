//! The app's words that walk findings 27 to 30 (1283) changed, read from
//! `android/res/values*/strings.xml` as the APK carries them: the count of
//! new messages is a plural in English and German, not "message(s)"; the
//! battery question says "Allow"; a failed check for messages says it tries
//! again, not "Pull failed" with the library's words; the chat menu's
//! Refresh is gone; the contact screen speaks of the name they chose and of
//! no hash; Advanced says "Relay" for the propagation node.

use std::path::PathBuf;

/// One resource file's text, `values` or `values-de`.
fn resources(dir: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("android/res")
        .join(dir)
        .join("strings.xml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The text of `<string name="name">`, if the file has one.
fn string(xml: &str, name: &str) -> Option<String> {
    let open = format!("<string name=\"{name}\">");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find("</string>")?;
    Some(xml[start..start + end].to_string())
}

/// The `(quantity, text)` items of `<plurals name="name">`, in file order.
fn plurals(xml: &str, name: &str) -> Vec<(String, String)> {
    let open = format!("<plurals name=\"{name}\">");
    let Some(start) = xml.find(&open).map(|at| at + open.len()) else {
        return Vec::new();
    };
    let Some(end) = xml[start..].find("</plurals>") else {
        return Vec::new();
    };
    let mut items = Vec::new();
    let mut rest = &xml[start..start + end];
    while let Some(at) = rest.find("<item quantity=\"") {
        rest = &rest[at + "<item quantity=\"".len()..];
        let Some(quote) = rest.find("\">") else { break };
        let quantity = rest[..quote].to_string();
        rest = &rest[quote + 2..];
        let Some(close) = rest.find("</item>") else {
            break;
        };
        items.push((quantity, rest[..close].to_string()));
        rest = &rest[close..];
    }
    items
}

const LANGUAGES: [&str; 2] = ["values", "values-de"];

#[test]
fn the_count_of_new_messages_is_a_plural_in_both_languages() {
    for dir in LANGUAGES {
        let xml = resources(dir);
        let items = plurals(&xml, "pulled");
        let quantities: Vec<&str> = items.iter().map(|(q, _)| q.as_str()).collect();
        assert_eq!(quantities, ["one", "other"], "{dir}: pulled's quantities");
        for (quantity, text) in &items {
            assert!(
                text.contains("%1$d"),
                "{dir}: pulled/{quantity} {text:?} has no count"
            );
        }
        assert_ne!(items[0].1, items[1].1, "{dir}: one and other say the same");
        assert!(
            string(&xml, "pulled").is_none(),
            "{dir}: a plain pulled string beside the plural"
        );
        assert!(!xml.contains("(s)"), "{dir}: a word with (s) is left");
    }
    let en = plurals(&resources("values"), "pulled");
    assert_eq!(en[0].1, "%1$d new message.");
    assert_eq!(en[1].1, "%1$d new messages.");
    let de = plurals(&resources("values-de"), "pulled");
    assert_eq!(de[0].1, "%1$d neue Nachricht.");
    assert_eq!(de[1].1, "%1$d neue Nachrichten.");
}

#[test]
fn a_failed_check_says_it_tries_again_without_the_librarys_words() {
    let en = resources("values");
    assert_eq!(
        string(&en, "pull_failed").as_deref(),
        Some("Checking for messages failed, trying again")
    );
    for dir in LANGUAGES {
        let xml = resources(dir);
        let failed = string(&xml, "pull_failed").unwrap_or_default();
        assert!(!failed.is_empty(), "{dir}: no pull_failed");
        // Ui.eventLine passes no argument: a placeholder would show raw.
        assert!(
            !failed.contains('%'),
            "{dir}: pull_failed {failed:?} takes an argument"
        );
        assert!(!failed.contains("Pull"), "{dir}: pull_failed {failed:?}");
        assert!(
            string(&xml, "refresh").is_none(),
            "{dir}: Refresh is still a string"
        );
    }
}

#[test]
fn the_battery_question_answers_allow_or_not_now() {
    let en = resources("values");
    assert_eq!(string(&en, "battery_yes").as_deref(), Some("Allow"));
    assert_eq!(string(&en, "battery_no").as_deref(), Some("Not now"));
    let de = resources("values-de");
    assert_eq!(string(&de, "battery_yes").as_deref(), Some("Erlauben"));
    assert_eq!(string(&de, "battery_no").as_deref(), Some("Nicht jetzt"));
}

#[test]
fn the_contact_screen_speaks_of_the_name_they_chose_and_of_no_hash() {
    let en = resources("values");
    assert_eq!(
        string(&en, "nickname_none").as_deref(),
        Some("None; the name they chose")
    );
    for dir in LANGUAGES {
        let xml = resources(dir);
        for name in ["nickname_none", "identity_why", "path_home"] {
            let text = string(&xml, name).unwrap_or_default();
            assert!(!text.is_empty(), "{dir}: no {name}");
            for word in ["hash", "Hash", "announce", "meldet", "routes", "routet"] {
                assert!(!text.contains(word), "{dir}: {name} {text:?} says {word:?}");
            }
        }
    }
}

#[test]
fn advanced_says_relay_for_the_propagation_node() {
    for (dir, relay) in [("values", "Relay"), ("values-de", "Relais")] {
        let xml = resources(dir);
        assert_eq!(
            string(&xml, "propagation_node").as_deref(),
            Some(relay),
            "{dir}"
        );
        assert!(
            !xml.contains("Propagation Node"),
            "{dir}: Propagation Node is left"
        );
        let summary = string(&xml, "settings_advanced_summary").unwrap_or_default();
        assert!(
            summary.starts_with(relay),
            "{dir}: Advanced's summary {summary:?}"
        );
    }
}

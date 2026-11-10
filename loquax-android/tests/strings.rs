//! The app's words that walk findings 27 to 30 (1283) changed, read from
//! `android/res/values*/strings.xml` as the APK carries them: the count of
//! new messages is a plural in English and German, not "message(s)"; the
//! battery question says "Allow"; a failed check for messages says it tries
//! again, not "Pull failed" with the library's words; the chat menu's
//! Refresh is gone; the contact screen speaks of the name they chose and of
//! no hash; Advanced says "Relay" for the propagation node.
//!
//! Every translatable string has its German (1345): the session channel's
//! name showed English on a German phone because `values-de` lacked it.

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
        // The relay's words and the settings' sections are the core's
        // catalogue since 1450, which `strings_scene.xml` carries.
        let scene = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("android/res")
                .join(dir)
                .join("strings_scene.xml"),
        )
        .expect("strings_scene.xml");
        let xml = resources(dir) + &scene;
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

/// 1377, the second walk: the German app calls the device a Handy on
/// every screen (the core's catalogue did, `strings.xml` said Telefon);
/// the join screen's address field names no daemon; a move says what the
/// old phone does afterwards in plain words.
#[test]
fn the_german_app_says_handy_and_the_join_screen_names_no_daemon() {
    let de = resources("values-de");
    let scene = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("android/res/values-de/strings_scene.xml"),
    )
    .expect("strings_scene.xml");
    for (file, xml) in [("strings.xml", &de), ("strings_scene.xml", &scene)] {
        for (name, _, text) in entries(xml) {
            let said = text
                .split(|c: char| !c.is_alphanumeric())
                .any(|word| word == "Telefon" || word == "Telefons");
            assert!(!said, "{file}: {name} {text:?} says Telefon");
        }
    }
    for dir in LANGUAGES {
        let xml = resources(dir);
        let label = string(&xml, "server_label").unwrap_or_default();
        assert!(!label.is_empty(), "{dir}: no server_label");
        for word in ["lnsd", "TCP", "host:port", "Host:Port"] {
            assert!(
                !label.contains(word),
                "{dir}: server_label {label:?} says {word:?}"
            );
        }
        for name in ["move_text", "move_moved"] {
            let text = string(&xml, name).unwrap_or_default();
            for word in [
                "stops then",
                "has stopped",
                "hört dann auf",
                "hat aufgehört",
            ] {
                assert!(!text.contains(word), "{dir}: {name} {text:?} says {word:?}");
            }
        }
    }
}

/// Each `<string>` and `<plurals>` of one resource file by name: its
/// attributes after the name, and its text.
fn entries(xml: &str) -> Vec<(String, String, String)> {
    let mut found = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find('<') {
        rest = &rest[at + 1..];
        let kind = if rest.starts_with("string name=\"") {
            "string"
        } else if rest.starts_with("plurals name=\"") {
            "plurals"
        } else {
            continue;
        };
        let after = &rest[kind.len() + " name=\"".len()..];
        let Some(quote) = after.find('"') else { break };
        let name = after[..quote].to_string();
        let Some(gt) = after.find('>') else { break };
        let attributes = after[quote + 1..gt].to_string();
        let close = format!("</{kind}>");
        let Some(end) = after[gt..].find(&close) else {
            break;
        };
        found.push((name, attributes, after[gt + 1..gt + end].to_string()));
        rest = &after[gt + end..];
    }
    found
}

/// The format arguments of a text, `%1$d` and the like, sorted.
fn arguments(text: &str) -> Vec<String> {
    let mut found: Vec<String> = text
        .match_indices('%')
        .filter_map(|(at, _)| {
            let tail = &text[at + 1..];
            let digits = tail.chars().take_while(char::is_ascii_digit).count();
            let rest = &tail[digits..];
            (digits > 0 && rest.starts_with('$'))
                .then(|| rest[1..].chars().next())
                .flatten()
                .map(|kind| format!("%{}${kind}", &tail[..digits]))
        })
        .collect();
    found.sort();
    found.dedup();
    found
}

#[test]
fn every_translatable_string_has_its_german_with_the_same_arguments() {
    for file in ["strings.xml", "strings_scene.xml"] {
        let read = |dir: &str| {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("android/res")
                .join(dir)
                .join(file);
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        };
        let en = entries(&read("values"));
        let de = entries(&read("values-de"));
        assert!(en.len() > 50, "{file}: only {} entries read", en.len());
        let german = |name: &str| de.iter().find(|(n, _, _)| n == name);
        let mut missing = Vec::new();
        for (name, attributes, text) in &en {
            let fixed = attributes.contains("translatable=\"false\"");
            match (fixed, german(name)) {
                (true, Some(_)) => {
                    panic!("{file}: {name} is the same everywhere, yet has a German")
                }
                (true, None) => {}
                (false, None) => missing.push(name.clone()),
                (false, Some((_, _, words))) => assert_eq!(
                    arguments(words),
                    arguments(text),
                    "{file}: {name}'s arguments differ in German"
                ),
            }
        }
        assert!(missing.is_empty(), "{file}: no German for {missing:?}");
        for (name, _, _) in &de {
            assert!(
                en.iter().any(|(n, _, _)| n == name),
                "{file}: {name} is German only"
            );
        }
    }
    let de = resources("values-de");
    assert_eq!(
        string(&de, "channel_session").as_deref(),
        Some("Verbindung zu deinem Postfach")
    );
    assert_eq!(
        string(&de, "connection_notice").as_deref(),
        Some("Benachrichtigung für die Verbindung")
    );
    let en = resources("values");
    assert_eq!(
        string(&en, "connection_notice").as_deref(),
        Some("Connection notice")
    );
}

#[test]
fn the_parity_check_names_a_missing_german_and_a_changed_argument() {
    // Positive control for the check above: a German file without one
    // string and with another's argument changed.
    let en = entries(
        r#"<resources><string name="a">One %1$d</string><string name="b">Two</string>
        <string name="c" translatable="false">C</string>
        <plurals name="p"><item quantity="one">%1$d x</item></plurals></resources>"#,
    );
    assert_eq!(en.len(), 4);
    assert!(en[2].1.contains("translatable=\"false\""));
    assert_eq!(arguments(&en[0].2), ["%1$d"]);
    assert_eq!(arguments(&en[3].2), ["%1$d"]);
    let de = entries(r#"<resources><string name="a">Eins %1$s</string></resources>"#);
    assert_ne!(arguments(&de[0].2), arguments(&en[0].2));
    assert!(!de.iter().any(|(n, _, _)| n == "b"));
    assert_eq!(arguments("100% sure, %2$s and %1$d"), ["%1$d", "%2$s"]);
}

/// The body of the Java method that starts with `head` in `source`: from
/// its line to the next method's comment at the same indent.
fn method<'a>(source: &'a str, head: &str) -> &'a str {
    let start = source
        .find(head)
        .unwrap_or_else(|| panic!("no {head:?} in the source"));
    let rest = &source[start..];
    let end = rest.find("\n    /**").unwrap_or(rest.len());
    &rest[..end]
}

/// 1448, walk finding 15: the first start has one primary button,
/// "Continue" whatever is chosen, and its three special ways as links; the
/// computer's command is a block of its own, never inside a sentence, and
/// the sentence after it says where the code appears ("Er zeigt einen
/// Code" had no clear "er"). Control: the method before 1448, four
/// buttons and the renamed one, fails the same checks.
#[test]
fn the_first_start_has_one_primary_button_and_the_command_stands_alone() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("android/src/network/leviculum/loquax/WelcomeActivity.java");
    let source = std::fs::read_to_string(&path).expect("WelcomeActivity.java");
    let problems = |choice: &str| {
        let mut problems = Vec::new();
        let primaries = choice.matches("primary(").count();
        if primaries != 1 {
            problems.push(format!("{primaries} primary buttons"));
        }
        if choice.contains("button(") {
            problems.push("a plain button beside the primary one".to_string());
        }
        if choice.matches("link(").count() != 3 {
            problems.push("not three links".to_string());
        }
        if choice.contains("setText(") || choice.contains("use_this_phone") {
            problems.push("the primary button is renamed".to_string());
        }
        problems
    };
    let choice = method(&source, "    private void choice(LinearLayout body) {");
    assert_eq!(problems(choice), Vec::<String>::new());
    let before = r#"
        final Button next = button(getString(here ? R.string.use_this_phone : R.string.next));
        next.setText(here ? R.string.use_this_phone : R.string.next);
        Button join = button(getString(R.string.join_existing));
        Button move = button(getString(R.string.move_from_old));
        Button have = button(getString(R.string.have_address));"#;
    assert_eq!(
        problems(before).len(),
        4,
        "the control: {:?}",
        problems(before)
    );

    let command = "loquax mailbox setup";
    for dir in LANGUAGES {
        let xml = resources(dir);
        for (name, attributes, text) in entries(&xml) {
            if text.contains(command) {
                assert!(
                    name == "computer_command" && attributes.contains("translatable=\"false\""),
                    "{dir}: {name} names the command inside a sentence: {text:?}"
                );
            }
        }
        let code = string(&xml, "computer_code").unwrap_or_default();
        assert!(
            code.contains("appears there") || code.contains("Dort erscheint"),
            "{dir}: computer_code {code:?}"
        );
    }
    let commands: Vec<String> = entries(&resources("values"))
        .into_iter()
        .filter(|(name, _, _)| name == "computer_command")
        .map(|(_, _, text)| text)
        .collect();
    assert_eq!(commands, [command]);
}

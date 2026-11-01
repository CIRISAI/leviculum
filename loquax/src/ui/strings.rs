//! The words of the list and the chat, in English and German, keyed (1217,
//! decision 2 of `attachments/1217-loquax-one-ui-contract.md`: one
//! catalogue in Rust, every target translated).
//!
//! A [`StrId`]'s key is the Android resource name it had, so the phone's
//! `R.string.pin` still finds it: `just loquax-strings` writes
//! `loquax-android/android/res/values{,-de}/strings_scene.xml` from
//! [`CATALOGUE`], and the tests below refuse a generated file that differs
//! from the catalogue and an Android `strings.xml` that still holds one of
//! its keys (aapt would refuse the duplicate as well, but only on a phone
//! build). The rest of Android's strings move in later slices.
//!
//! The texts keep Android's positional format (`%1$s`, `%1$d`), which
//! [`format()`] fills, so one text serves both the phone's `getString` and
//! the core.
//!
//! The machinery is `leviculum_ui::strings` (1240, slice 0 of
//! `docs/src/concepts/lnomad-everywhere.md`): [`Lang`], the
//! `leviculum_ui::catalogue!` macro, [`Text`] and [`Arg`] over a key type,
//! [`format()`] and [`android_xml`]'s writer. What is Loquax's is the
//! catalogue below, its [`StrId`], and the names this module gives the
//! shared types over it.

use alloc::string::String;
// The tests below name `to_string` as they did when the machinery was here.
#[cfg(test)]
use alloc::string::ToString;

pub use leviculum_ui::strings::{android_escape, Lang, JOIN};

/// A value for a text's `%n$s` or `%n$d`, of Loquax's catalogue.
pub type Arg = leviculum_ui::strings::Arg<StrId>;

/// What a scene node says, in Loquax's catalogue.
pub type Text = leviculum_ui::strings::Text<StrId>;

leviculum_ui::catalogue! {
    enum StrId;
    const CATALOGUE;
    // The tabs and the top bar.
    TabChats = "tab_chats", "Chats", "Chats";
    TabChatsUnread = "tab_chats_unread", "Chats, %1$d with unread messages", "Chats, %1$d mit ungelesenen Nachrichten";
    Map = "map", "Map", "Karte";
    Settings = "settings", "Settings", "Einstellungen";
    MailboxConnected = "mailbox_connected", "You. Your mailbox is connected.", "Du. Dein Postfach ist verbunden.";
    MailboxNotConnected = "mailbox_not_connected", "You. Your mailbox is not connected.", "Du. Dein Postfach ist nicht verbunden.";
    Search = "search", "Search", "Suchen";
    CloseSearch = "close_search", "Close search", "Suche schließen";
    // The list and a row's menu.
    NoConversations = "no_conversations", "No conversations yet. The pencil starts one.", "Noch keine Unterhaltungen. Der Stift beginnt eine.";
    Pinned = "pinned", "Pinned", "Fixiert";
    Archived = "archived", "Archived", "Archiviert";
    Pin = "pin", "Pin", "Fixieren";
    Unpin = "unpin", "Unpin", "Lösen";
    Mute = "mute", "Mute", "Stumm schalten";
    Unmute = "unmute", "Unmute", "Ton an";
    Archive = "archive", "Archive", "Archivieren";
    Unarchive = "unarchive", "Unarchive", "Aus dem Archiv holen";
    MarkRead = "mark_read", "Mark as read", "Gelesen";
    MarkUnread = "mark_unread", "Mark as unread", "Ungelesen";
    Delete = "delete", "Delete", "Löschen";
    // Times and the liveness words (1209).
    Today = "today", "Today", "Heute";
    Yesterday = "yesterday", "Yesterday", "Gestern";
    JustNow = "just_now", "Now", "Jetzt";
    MinutesAgo = "minutes_ago", "%1$d min", "%1$d Min.";
    HoursAgo = "hours_ago", "%1$d h", "%1$d Std.";
    DaysAgo = "days_ago", "%1$d d", "%1$d T.";
    ListHours = "list_hours", "%1$dh", "%1$dh";
    Position = "position", "Position", "Position";
    Live = "live", "live", "live";
    LiveEvery = "live_every", "every %1$s", "alle %1$s";
    LiveAgo = "live_ago", "%1$s ago", "vor %1$s";
    LiveNow = "live_now", "now", "gerade eben";
    LastPosition = "last_position", "last position %1$s", "letzte Position %1$s";
    PositionOf = "position_of", "Position of %1$s, %2$s", "Position von %1$s, %2$s";
    PositionMarker = "position_marker", "⌖", "⌖";
    NoPosition = "no_position", "No position from this friend yet", "Noch keine Position von diesem Freund";
    // A row's last line while a message is typed there and not sent (1219).
    DraftRow = "draft_row", "Draft: %1$s", "Entwurf: %1$s";
    Reacted = "reacted", "reacted %1$s", "hat mit %1$s reagiert";
    YouReacted = "you_reacted", "You reacted %1$s", "Du hast mit %1$s reagiert";
    RunUpdates = "run_updates", "%1$d updates since %2$s", "%1$d Meldungen seit %2$s";
    QuoteYou = "quote_you", "You", "Du";
    // The chat: compose, the marks, a bubble's menu.
    ComposeHint = "compose_hint", "Message", "Nachricht";
    Send = "send", "Send", "Senden";
    StateQueued = "state_queued", "queued", "wartet";
    StateSending = "state_sending", "sending", "wird gesendet";
    StateSent = "state_sent", "sent", "gesendet";
    StateDelivered = "state_delivered", "delivered ✓", "zugestellt ✓";
    StateFailed = "state_failed", "failed ✗", "fehlgeschlagen ✗";
    ActionReply = "action_reply", "Reply", "Antworten";
    ActionCopy = "action_copy", "Copy", "Kopieren";
    ActionForward = "action_forward", "Forward", "Weiterleiten";
    ActionInfo = "action_info", "Info", "Info";
    ActionDelete = "action_delete", "Delete", "Löschen";
    ActionSelect = "action_select", "Select", "Auswählen";
    // The verbs no Android string named yet: each intent's label
    // (`super::intent::Intent::label`), new keys under `scene_`.
    Back = "scene_back", "Back", "Zurück";
    Open = "scene_open", "Open", "Öffnen";
    Close = "scene_close", "Close", "Schließen";
    Dismiss = "scene_dismiss", "Dismiss", "Verwerfen";
    Menu = "scene_menu", "More", "Mehr";
    NextPane = "scene_next_pane", "Next pane", "Nächster Bereich";
    PreviousPane = "scene_previous_pane", "Previous pane", "Voriger Bereich";
    FocusList = "scene_focus_list", "Go to conversations", "Zu den Unterhaltungen";
    FocusMessages = "scene_focus_messages", "Go to messages", "Zu den Nachrichten";
    FocusCompose = "scene_focus_compose", "Write", "Schreiben";
    Palette = "scene_palette", "Commands", "Befehle";
    Help = "scene_help", "Help", "Hilfe";
    Quit = "scene_quit", "Quit", "Beenden";
    QuitHint = "scene_quit_hint", "How to quit", "Wie man beendet";
    Redraw = "scene_redraw", "Redraw", "Neu zeichnen";
    Edit = "scene_edit", "Edit text", "Text bearbeiten";
    Newline = "scene_newline", "New line", "Neue Zeile";
    ClearDraft = "scene_clear_draft", "Clear draft", "Entwurf leeren";
    Draft = "scene_draft", "Draft", "Entwurf";
    Insert = "scene_insert", "Insert", "Einfügen";
    Emoji = "scene_emoji", "Emoji", "Emoji";
    React = "scene_react", "React", "Reagieren";
    CancelReply = "scene_cancel_reply", "Cancel reply", "Antwort abbrechen";
    Scroll = "scene_scroll", "Scroll", "Blättern";
    ScrollMessages = "scene_scroll_messages", "Scroll messages", "Nachrichten blättern";
    NextConversation = "scene_next_conversation", "Next conversation", "Nächste Unterhaltung";
    PreviousConversation = "scene_previous_conversation", "Previous conversation", "Vorige Unterhaltung";
    NextUnread = "scene_next_unread", "Next unread", "Nächste ungelesene";
    PreviousUnread = "scene_previous_unread", "Previous unread", "Vorige ungelesene";
    OpenResult = "scene_open_result", "Open result", "Treffer öffnen";
    NextMatch = "scene_next_match", "Next match", "Nächster Treffer";
    PreviousMatch = "scene_previous_match", "Previous match", "Voriger Treffer";
    OpenMessage = "scene_open_message", "Open message", "Nachricht öffnen";
    PeerOnMap = "scene_peer_on_map", "Show on map", "Auf der Karte zeigen";
    Share = "scene_share", "Share position", "Position teilen";
    Rename = "scene_rename", "Rename", "Umbenennen";
    Verify = "scene_verify", "Verified in person", "Persönlich bestätigt";
    Block = "scene_block", "Block", "Blockieren";
    Blocked = "scene_blocked", "Blocked contacts", "Blockierte Kontakte";
    Forget = "scene_forget", "Delete conversation", "Unterhaltung löschen";
    NewConversation = "scene_new_conversation", "New conversation", "Neue Unterhaltung";
    AddContact = "scene_add_contact", "Add contact", "Kontakt hinzufügen";
    Name = "scene_name", "Your name", "Dein Name";
    Pull = "scene_pull", "Fetch messages", "Nachrichten abholen";
    Front = "scene_front", "App in front", "App im Vordergrund";
    Theme = "scene_theme", "Light or dark", "Hell oder dunkel";
    SendOnEnter = "scene_send_on_enter", "Enter sends", "Enter sendet";
    Mouse = "scene_mouse", "Mouse", "Maus";
    Import = "scene_import", "Import", "Importieren";
    ZoomIn = "scene_zoom_in", "Zoom in", "Vergrößern";
    ZoomOut = "scene_zoom_out", "Zoom out", "Verkleinern";
    Pan = "scene_pan", "Move the map", "Karte verschieben";
    Fit = "scene_fit", "Show every pin", "Alle Stecknadeln zeigen";
    StepPin = "scene_step_pin", "Next pin", "Nächste Stecknadel";
    SelectPin = "scene_select_pin", "Select pin", "Stecknadel wählen";
    MapDrawing = "scene_map_drawing", "Map drawing", "Kartendarstellung";
    GlyphTest = "scene_glyph_test", "Glyph test", "Zeichentest";
    LoadMapArea = "scene_load_map_area", "Load this map area", "Diesen Kartenausschnitt laden";
    // The day line: the weekday, the day and the month, the year when it
    // is not this one.
    Date = "scene_date", "%1$s %2$d %3$s", "%1$s, %2$d. %3$s";
    DateYear = "scene_date_year", "%1$s %2$d %3$s %4$d", "%1$s, %2$d. %3$s %4$d";
    Monday = "scene_monday", "Monday", "Montag";
    Tuesday = "scene_tuesday", "Tuesday", "Dienstag";
    Wednesday = "scene_wednesday", "Wednesday", "Mittwoch";
    Thursday = "scene_thursday", "Thursday", "Donnerstag";
    Friday = "scene_friday", "Friday", "Freitag";
    Saturday = "scene_saturday", "Saturday", "Samstag";
    Sunday = "scene_sunday", "Sunday", "Sonntag";
    January = "scene_january", "January", "Januar";
    February = "scene_february", "February", "Februar";
    March = "scene_march", "March", "März";
    April = "scene_april", "April", "April";
    May = "scene_may", "May", "Mai";
    June = "scene_june", "June", "Juni";
    July = "scene_july", "July", "Juli";
    August = "scene_august", "August", "August";
    September = "scene_september", "September", "September";
    October = "scene_october", "October", "Oktober";
    November = "scene_november", "November", "November";
    December = "scene_december", "December", "Dezember";
}

/// [`leviculum_ui::strings::format`] over Loquax's catalogue.
pub fn format(text: &str, args: &[Arg], lang: Lang) -> String {
    leviculum_ui::strings::format(text, args, lang)
}

/// The phone's `strings_scene.xml` for `lang`: every catalogue string, in
/// the table's order.
pub fn android_xml(lang: Lang) -> String {
    leviculum_ui::strings::android_xml(
        lang,
        CATALOGUE,
        "loquax/src/ui/strings.rs",
        "just loquax-strings",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    #[test]
    fn every_string_has_both_languages_and_a_unique_key() {
        let mut keys: Vec<&str> = CATALOGUE.iter().map(|(_, key, _, _)| *key).collect();
        for (id, key, en, de) in CATALOGUE {
            assert_eq!(id.key(), *key);
            assert!(!en.trim().is_empty(), "{key}: no English");
            assert!(!de.trim().is_empty(), "{key}: no German");
            assert!(
                key.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "{key}: not a resource name"
            );
            // Both languages fill the same places.
            fn places(text: &str) -> Vec<&str> {
                let mut found: Vec<&str> =
                    text.match_indices('%').map(|(i, _)| &text[i..]).collect();
                found.iter_mut().for_each(|s| *s = &s[..s.len().min(4)]);
                found.sort_unstable();
                found
            }
            assert_eq!(places(en), places(de), "{key}");
        }
        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), count, "a key twice");
    }

    #[test]
    fn the_format_fills_android_places() {
        let args = [Arg::Int(3), Arg::Str("12:00".to_string())];
        assert_eq!(
            format(StrId::RunUpdates.text(Lang::En), &args, Lang::En),
            "3 updates since 12:00"
        );
        assert_eq!(
            format(StrId::RunUpdates.text(Lang::De), &args, Lang::De),
            "3 Meldungen seit 12:00"
        );
        assert_eq!(
            format("100%% %1$s %3$s", &args[..1], Lang::En),
            "100% 3 %3$s"
        );
        assert_eq!(format("%", &[], Lang::En), "%");
    }

    #[test]
    fn a_text_resolves_in_either_language() {
        let text = Text::Key(StrId::LastPosition, vec![Arg::Str("3 h".to_string())]);
        assert_eq!(text.resolve(Lang::En), "last position 3 h");
        assert_eq!(text.resolve(Lang::De), "letzte Position 3 h");
        assert_eq!(Text::Raw("Anna".to_string()).resolve(Lang::De), "Anna");
    }

    #[test]
    fn a_locale_names_its_language() {
        for tag in ["de", "DE", "de-AT", "de_DE"] {
            assert_eq!(Lang::of_tag(tag), Lang::De, "{tag}");
        }
        for tag in ["", "en", "en-US", "fr", "dee", "d"] {
            assert_eq!(Lang::of_tag(tag), Lang::En, "{tag}");
        }
    }

    #[test]
    fn android_escapes_what_aapt_reads() {
        assert_eq!(
            android_escape("it's <b> & \"x\""),
            "it\\'s &lt;b&gt; &amp; \\\"x\\\""
        );
        assert_eq!(android_escape("@home ?"), "\\@home ?");
    }

    fn res(dir: &str, file: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../loquax-android/android/res")
            .join(dir)
            .join(file)
    }

    /// The checked-in `strings_scene.xml` is the catalogue's: `just
    /// loquax-strings` writes it, this refuses one written by hand or left
    /// behind. With `LOQUAX_WRITE_STRINGS=1` the test writes them instead.
    #[test]
    #[cfg(feature = "std")]
    fn the_phone_reads_the_catalogue() {
        for lang in Lang::ALL {
            let path = res(lang.values_dir(), "strings_scene.xml");
            let want = android_xml(lang);
            if std::env::var_os("LOQUAX_WRITE_STRINGS").is_some() {
                std::fs::write(&path, &want).expect("write strings_scene.xml");
            }
            let got = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(
                got == want,
                "{} differs from the catalogue: run `just loquax-strings`",
                path.display()
            );
        }
    }

    /// No Android `strings.xml` keeps a key the catalogue has: the phone
    /// gets each from one place.
    #[test]
    #[cfg(feature = "std")]
    fn android_holds_no_string_of_the_catalogue() {
        for lang in Lang::ALL {
            let path = res(lang.values_dir(), "strings.xml");
            let xml = std::fs::read_to_string(&path).expect("strings.xml");
            let twice: Vec<&str> = CATALOGUE
                .iter()
                .map(|(_, key, _, _)| *key)
                .filter(|key| xml.contains(&std::format!("<string name=\"{key}\"")))
                .collect();
            assert!(twice.is_empty(), "{} also holds {twice:?}", path.display());
        }
    }
}

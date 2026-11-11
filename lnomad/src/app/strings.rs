//! The browser's words, keyed, in English and German (1255, slice 1 of
//! `docs/src/concepts/lnomad-everywhere.md`, the "Strings" row of its
//! section 3).
//!
//! The machinery is `leviculum_ui::strings`, the catalogue Loquax uses
//! too: [`Lang`], the `leviculum_ui::catalogue!` macro, [`Text`] and
//! [`Arg`] over a key type, `format`. What is lnomad's is the catalogue
//! below and its [`StrId`]. Every key starts with `browser_`, so the day
//! the browser is a tab inside the Loquax APK its keys sit beside
//! Loquax's in one `values/` directory without a collision (the test
//! below holds that).
//!
//! The English texts are the literals the terminal drew before the
//! catalogue, character for character: the `TestBackend` frames of
//! `tui.rs` pin them, so a word changed here fails a frame there.
//!
//! The texts keep Android's positional format (`%1$s`, `%1$d`).

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

pub use leviculum_ui::strings::Lang;

/// A value for a text's `%n$s` or `%n$d`, of lnomad's catalogue.
pub type Arg = leviculum_ui::strings::Arg<StrId>;

/// What a scene node or a toast says, in lnomad's catalogue.
pub type Text = leviculum_ui::strings::Text<StrId>;

leviculum_ui::catalogue! {
    enum StrId;
    const CATALOGUE;
    // The top bar.
    AddressHint = "browser_address_hint", "press : to enter a URL", "Mit : eine URL eingeben";
    Address = "browser_address", "Address", "Adresse";
    Hops = "browser_hops", "%1$d hops", "%1$d Sprünge";
    HopsUnknown = "browser_hops_unknown", "unknown", "unbekannt";
    HopsNone = "browser_hops_none", "? hops", "? Sprünge";
    Cached = "browser_cached", "served from the page cache", "aus dem Seitenspeicher";
    Identity = "browser_identity", "identifying as %1$s", "identifiziert als %1$s";
    Page = "browser_page", "Page", "Seite";
    Link = "browser_link", "Link %1$d", "Link %1$d";
    Field = "browser_field", "Field %1$d", "Feld %1$d";
    // The status chip over the page's last row.
    Loading = "browser_loading", "Loading %1$s", "Lade %1$s";
    CachedRefreshing = "browser_cached_refreshing", "from the page cache · updating", "aus dem Seitenspeicher · wird aktualisiert";
    Downloading = "browser_downloading", "Downloading %1$s", "Lade %1$s herunter";
    LinkTarget = "browser_link_target", "→ %1$s", "→ %1$s";
    // The intents, as a menu or a screen reader names them.
    Back = "browser_back", "back", "zurück";
    Forward = "browser_forward", "forward", "vor";
    Reload = "browser_reload", "reload", "neu laden";
    Go = "browser_go", "go", "los";
    Cancel = "browser_cancel", "cancel", "abbrechen";
    Bookmark = "browser_bookmark", "bookmark", "Lesezeichen";
    Unbookmark = "browser_unbookmark", "remove bookmark", "Lesezeichen entfernen";
    Identify = "browser_identify", "identify", "identifizieren";
    Anonymous = "browser_anonymous", "anonymous", "anonym";
    Open = "browser_open", "open", "öffnen";
    Remove = "browser_remove", "remove", "entfernen";
    Undo = "browser_undo", "undo", "rückgängig";
    HintMode = "browser_hint", "hint", "Hinweis";
    Search = "browser_search", "search", "suchen";
    FindName = "browser_find_name", "find a site by name", "Seite nach Namen suchen";
    Places = "browser_places", "places", "Orte";
    Mark = "browser_mark", "mark", "merken";
    Copy = "browser_copy", "copy", "kopieren";
    Help = "browser_help", "help", "Hilfe";
    Quit = "browser_quit", "quit", "beenden";
    Done = "browser_done", "done", "fertig";
    NextField = "browser_next_field", "next field", "nächstes Feld";
    PrevField = "browser_prev_field", "previous field", "voriges Feld";
    Toggle = "browser_toggle", "toggle", "umschalten";
    Submit = "browser_submit", "submit", "absenden";
    Edit = "browser_edit", "edit", "bearbeiten";
    Next = "browser_next", "next", "weiter";
    Prev = "browser_prev", "prev", "zurück";
    Scroll = "browser_scroll", "scroll", "blättern";
    Dismiss = "browser_dismiss", "close", "schließen";
    Yes = "browser_yes", "yes", "ja";
    HintSentence = "browser_hint_sentence", "type a hint label or link text", "Hinweis oder Linktext tippen";
    // The places panel.
    PlacesTitle = "browser_places_title", "places", "Orte";
    Bookmarks = "browser_bookmarks", "Bookmarks", "Lesezeichen";
    NoBookmarks = "browser_no_bookmarks", "(none)", "(keine)";
    Nodes = "browser_nodes", "Pages heard", "Gehörte Seiten";
    NoNodesYet = "browser_no_nodes_yet", "(listening for announces…)", "(lausche auf Ankündigungen…)";
    PlaceBookmark = "browser_place_bookmark", "%1$s  ·  %2$s", "%1$s  ·  %2$s";
    PlaceNode = "browser_place_node", "%1$s  ·  %2$s  ·  %3$s", "%1$s  ·  %2$s  ·  %3$s";
    // A heard node's row (1632, walk 4): its name, when it was heard and
    // how far it is, the words Explore's page list uses; no hash.
    PlaceNodeSeen = "browser_place_node_seen", "%1$s  ·  %2$s", "%1$s  ·  %2$s";
    PlaceUnnamed = "browser_place_unnamed", "Unnamed page", "Unbenannte Seite";
    PlaceUnnamedTail = "browser_place_unnamed_tail", "Unnamed page ·%1$s", "Unbenannte Seite ·%1$s";
    PlaceSeenNow = "browser_place_seen_now", "seen just now", "gesehen gerade eben";
    PlaceSeenMinutes = "browser_place_seen_minutes", "seen %1$d min ago", "gesehen vor %1$d Min.";
    PlaceSeenHours = "browser_place_seen_hours", "seen %1$d h ago", "gesehen vor %1$d Std.";
    PlaceSeenDays = "browser_place_seen_days", "seen %1$d days ago", "gesehen vor %1$d Tagen";
    PlaceHop = "browser_place_hop", "directly reachable", "direkt erreichbar";
    PlaceHops = "browser_place_hops", "reachable over %1$d stations", "über %1$d Stationen erreichbar";
    // The name search's result list (1227).
    NamesTitle = "browser_names_title", "sites matching \"%1$s\"", "Seiten zu \"%1$s\"";
    NamesHint = "browser_names_hint", "Enter open · Esc close · a name is a hint, the hash is the address", "Enter öffnet · Esc schließt · ein Name ist ein Hinweis, der Hash ist die Adresse";
    // Toasts.
    Downloaded = "browser_downloaded", "downloaded %1$s (%2$d bytes) to %3$s", "%1$s (%2$d Bytes) nach %3$s heruntergeladen";
    ImageNotArrived = "browser_image_not_arrived", "that image has not arrived yet", "das Bild ist noch nicht da";
    CouldNotSave = "browser_could_not_save", "could not save %1$s: %2$s", "%1$s nicht gespeichert: %2$s";
    NoDownloadDir = "browser_no_download_dir", "no download directory (neither XDG_DOWNLOAD_DIR nor HOME set)", "kein Download-Verzeichnis (weder XDG_DOWNLOAD_DIR noch HOME gesetzt)";
    AnchorNotFound = "browser_anchor_not_found", "anchor #%1$s not found", "Anker #%1$s nicht gefunden";
    NururlIgnored = "browser_nururl_ignored", "nururl: ignored %1$s", "nururl: %1$s ignoriert";
    BadUrl = "browser_bad_url", "bad URL", "falsche URL";
    BadBookmark = "browser_bad_bookmark", "bad bookmark", "falsches Lesezeichen";
    BadLink = "browser_bad_link", "bad link", "falscher Link";
    TryLocal = "browser_try_local", "%1$s — try :%2$s (a local URL keeps the \":\")", "%1$s — versuch :%2$s (eine lokale URL behält den \":\")";
    BadUrlWhy = "browser_bad_url_why", "%1$s: %2$s — %3$s", "%1$s: %2$s — %3$s";
    Cancelled = "browser_cancelled", "cancelled", "abgebrochen";
    NoMatches = "browser_no_matches", "no matches for \"%1$s\"", "keine Treffer für \"%1$s\"";
    NoSiteNamed = "browser_no_site_named", "no site named \"%1$s\" seen yet (names come from announces and pins)", "noch keine Seite namens \"%1$s\" gesehen (Namen kommen aus Ankündigungen und Pins)";
    RemovedUndo = "browser_removed_undo", "removed %1$s — u to undo", "%1$s entfernt — u macht es rückgängig";
    NothingToUndo = "browser_nothing_to_undo", "nothing to undo", "nichts rückgängig zu machen";
    AlreadyBookmarked = "browser_already_bookmarked", "already bookmarked: %1$s", "schon gemerkt: %1$s";
    Restored = "browser_restored", "restored %1$s", "%1$s wiederhergestellt";
    NothingToBookmark = "browser_nothing_to_bookmark", "nothing to bookmark", "nichts zu merken";
    RemovedBookmark = "browser_removed_bookmark", "removed bookmark %1$s", "Lesezeichen %1$s entfernt";
    Bookmarked = "browser_bookmarked", "bookmarked %1$s", "%1$s gemerkt";
    NothingToCopy = "browser_nothing_to_copy", "nothing to copy", "nichts zu kopieren";
    Copied = "browser_copied", "copied %1$s", "%1$s kopiert";
    IdentifyingTo = "browser_identifying_to", "Identifying to %1$s as %2$s  (i to stop)", "Identifiziert bei %1$s als %2$s  (i beendet)";
    IdentifyExplained = "browser_identify_explained", "The page learns your address while this is on", "Die Seite erfährt deine Adresse, solange das an ist";
    AnonymousTo = "browser_anonymous_to", "Now anonymous to %1$s", "Jetzt anonym bei %1$s";
    WontOpen = "browser_wont_open", "won't open %1$s: link", "öffne keinen %1$s:-Link";
    LxmfCopied = "browser_lxmf_copied", "LXMF address copied: %1$s", "LXMF-Adresse kopiert: %1$s";
    SavedImage = "browser_saved_image", "saved %1$s (%2$d bytes)", "%1$s gespeichert (%2$d Bytes)";
    OpenedImage = "browser_opened_image", "opened %1$s externally", "%1$s extern geöffnet";
    FailedToOpenImage = "browser_failed_to_open_image", "failed to open %1$s", "%1$s ließ sich nicht öffnen";
    IdentifyNotSaved = "browser_identify_not_saved", "could not save identify decision: %1$s", "Identifizier-Entscheidung nicht gespeichert: %1$s";
    OpenedExternally = "browser_opened_externally", "opened externally: %1$s", "extern geöffnet: %1$s";
    FailedToOpen = "browser_failed_to_open", "failed to open: %1$s", "ließ sich nicht öffnen: %1$s";
    // A page that did not load: the fetch's errors.
    FetchConnect = "browser_fetch_connect", "could not connect to shared instance: %1$s", "keine Verbindung zur gemeinsamen Instanz: %1$s";
    FetchNoPath = "browser_fetch_no_path", "no path to destination", "kein Pfad zum Ziel";
    FetchLinkFailed = "browser_fetch_link_failed", "link failed", "Link fehlgeschlagen";
    FetchTimeout = "browser_fetch_timeout", "request timed out", "Anfrage ohne Antwort";
    FetchNotFound = "browser_fetch_not_found", "path not found on destination", "Pfad am Ziel nicht gefunden";
    FetchSave = "browser_fetch_save", "could not save download: %1$s", "Download nicht gespeichert: %1$s";
    FetchDecode = "browser_fetch_decode", "malformed response payload", "fehlerhafte Antwort";
    FetchNode = "browser_fetch_node", "node error: %1$s", "Knotenfehler: %1$s";
    // The name search's rows (1227).
    NameRow = "browser_name_row", "%1$s  ·  %2$s  ·  %3$s%4$s", "%1$s  ·  %2$s  ·  %3$s%4$s";
    OneHop = "browser_one_hop", "1 hop", "1 Sprung";
    JustNow = "browser_just_now", "just now", "gerade eben";
    MinutesAgo = "browser_minutes_ago", "%1$d min ago", "vor %1$d min";
    HoursAgo = "browser_hours_ago", "%1$d h ago", "vor %1$d h";
    DaysAgo = "browser_days_ago", "%1$d d ago", "vor %1$d d";
    MarkPinned = "browser_mark_pinned", "  pinned", "  gepinnt";
    MarkPinnedAs = "browser_mark_pinned_as", "  pinned as %1$s", "  gepinnt als %1$s";
    MarkConflict = "browser_mark_conflict", "  ! name pinned to %1$s", "  ! Name gepinnt auf %1$s";
    NameConflict = "browser_name_conflict", "\"%1$s\" is pinned to %2$s; this is ·%3$s, another key. Open and pin it too? y/n", "\"%1$s\" ist auf %2$s gepinnt; das hier ist ·%3$s, ein anderer Schlüssel. Öffnen und auch pinnen? y/n";
    // The status line under an image that is not a picture.
    ImageQueued = "browser_image_queued", "[%1$s: queued]", "[%1$s: wartet]";
    ImageLoading = "browser_image_loading", "[%1$s: loading...]", "[%1$s: lädt...]";
    ImageNotFetched = "browser_image_not_fetched", "[%1$s: not fetched]", "[%1$s: nicht geholt]";
    // The start page, micron: the tagline and what the reader can do.
    StartTagline = "browser_start_tagline", "Terminal browser for NomadNet pages.", "Terminal-Browser für NomadNet-Seiten.";
    StartNoPage = "browser_start_no_page", "No page is open. Node discovery runs in the background, so nodes appear in the places panel as their announces arrive.", "Keine Seite offen. Die Knotensuche läuft im Hintergrund, Knoten erscheinen in den Orten, sobald ihre Ankündigungen eintreffen.";
    Url = "browser_url", "URL", "URL";
    // The start page of a browser a messenger hosts (Loquax's Browse tab).
    StartHostedTitle = "browser_start_hosted_title", "NomadNet pages", "NomadNet-Seiten";
    StartHosted = "browser_start_hosted", "Tap a node to open its pages, or type an address above.", "Tippe auf einen Knoten, um seine Seiten zu öffnen, oder gib oben eine Adresse ein.";
    // The help overlay: its title, the groups, and the words of each row.
    HelpTitle = "browser_help_title", " help — Esc, ? or click to close ", " Hilfe — Esc, ? oder Klick schließt ";
    HelpMove = "browser_help_move", "Move", "Bewegen";
    HelpFollow = "browser_help_follow", "Follow", "Folgen";
    HelpPage = "browser_help_page", "Page", "Seite";
    HelpImages = "browser_help_images", "Images", "Bilder";
    HelpFields = "browser_help_fields", "Fields", "Felder";
    HelpApp = "browser_help_app", "App", "Programm";
    HelpPlacesGroup = "browser_help_places_group", "Places", "Orte";
    HelpScrollLine = "browser_help_scroll_line", "scroll a line", "eine Zeile blättern";
    HelpScrollPage = "browser_help_scroll_page", "page down / up", "Seite vor / zurück";
    HelpScrollHalf = "browser_help_scroll_half", "half a page", "halbe Seite";
    HelpScrollEnds = "browser_help_scroll_ends", "top / bottom", "Anfang / Ende";
    HelpWheel = "browser_help_wheel", "wheel", "Mausrad";
    HelpFocus = "browser_help_focus", "focus a link or field", "Link oder Feld fokussieren";
    HelpFollowLink = "browser_help_follow_link", "follow a link or footer button, or click it", "Link oder Knopf der Fußleiste folgen, oder anklicken";
    HelpHint = "browser_help_hint", "hint — type a label or the link text", "Hinweis — Kürzel oder Linktext tippen";
    HelpHistory = "browser_help_history", "back / forward, or the mouse side buttons", "zurück / vor, oder die Maus-Seitentasten";
    HelpHoverKeys = "browser_help_hover_keys", "hover a link", "Zeiger auf Link";
    HelpHover = "browser_help_hover", "show its URL", "zeigt seine URL";
    HelpUrl = "browser_help_url", "enter a URL, or click the URL", "URL eingeben, oder Klick auf die URL";
    HelpGo = "browser_help_go", "go to the URL, or search the name", "zur URL gehen oder den Namen suchen";
    HelpReload = "browser_help_reload", "reload (always refetches)", "neu laden (holt immer neu)";
    HelpCancel = "browser_help_cancel", "cancel a load", "Laden abbrechen";
    HelpRedraw = "browser_help_redraw", "lay the page out again", "Seite neu umbrechen";
    HelpMark = "browser_help_mark", "bookmark this page, or click star", "diese Seite merken, oder Klick auf Stern";
    HelpCopy = "browser_help_copy", "copy the link's / page's URL", "URL des Links / der Seite kopieren";
    HelpPlaces = "browser_help_places", "places panel, or click a place; click an entry to open it", "Orte, oder Klick auf einen Ort; Klick auf einen Eintrag öffnet ihn";
    HelpOpenPlace = "browser_help_open_place", "open the selected place or name", "gewählten Ort oder Namen öffnen";
    HelpForget = "browser_help_forget", "remove the selected bookmark", "gewähltes Lesezeichen entfernen";
    HelpUndo = "browser_help_undo", "bring the removed bookmark back", "entferntes Lesezeichen zurückholen";
    HelpAnswer = "browser_help_answer", "open and pin a name pinned to another key; any other key: no", "Namen mit anderem Schlüssel öffnen und pinnen; jede andere Taste: nein";
    HelpIdentify = "browser_help_identify", "identify / go anonymous to this node", "bei diesem Knoten identifizieren / anonym werden";
    HelpIdentifyNote = "browser_help_identify_note", "default anonymous; identifying lets this node attribute your posts", "anonym ist Vorgabe; identifiziert ordnet dieser Knoten dir deine Beiträge zu";
    HelpSearch = "browser_help_search", "search / next / prev", "suchen / nächster / voriger";
    HelpSearchGo = "browser_help_search_go", "search the page for what is typed", "Seite nach dem Getippten durchsuchen";
    HelpSaveImage = "browser_help_save_image", "Enter saves the focused picture (no second transfer)", "Enter speichert das fokussierte Bild (ohne zweite Übertragung)";
    HelpOpenImage = "browser_help_open_image", "open the focused picture in the system viewer", "fokussiertes Bild im Bildbetrachter des Systems öffnen";
    HelpImagesOff = "browser_help_images_off", "start with --images off to leave them as plain links", "mit --images off starten lässt sie einfache Links";
    HelpField = "browser_help_field", "toggle a checkbox or radio; typing edits a text field", "Kästchen oder Option umschalten; Tippen bearbeitet ein Textfeld";
    HelpTheme = "browser_help_theme", "light / dark theme", "helles / dunkles Thema";
    HelpClose = "browser_help_close", "close this help, or click", "diese Hilfe schließen, oder Klick";
    HelpDismiss = "browser_help_dismiss", "close the panel, cancel the entry, leave the field", "Panel schließen, Eingabe abbrechen, Feld verlassen";
}

/// The language a locale names, read through `var` (`std::env::var` in the
/// shell): the first of `LC_ALL`, `LC_MESSAGES` and `LANG` that is set and
/// not empty, as the Loquax window reads it. German for German, English for
/// anything else or nothing.
pub fn locale_lang(var: impl Fn(&str) -> Option<String>) -> Lang {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|name| var(name))
        .find(|value| !value.is_empty())
        .map_or(Lang::En, |tag| Lang::of_tag(&tag))
}

/// `id` with one string argument.
pub fn with(id: StrId, arg: impl Into<String>) -> Text {
    Text::Key(id, vec![Arg::Str(arg.into())])
}

/// `id` with these arguments.
pub fn with_args(id: StrId, args: Vec<Arg>) -> Text {
    Text::Key(id, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Every key is unique, prefixed `browser_` so it cannot meet one of
    /// Loquax's in the APK's `values/`, and an Android resource name.
    #[test]
    fn every_key_is_the_browsers_own() {
        let mut seen = BTreeSet::new();
        for (_, key, en, de) in CATALOGUE {
            assert!(seen.insert(*key), "{key} twice");
            assert!(key.starts_with("browser_"), "{key}");
            assert!(
                key.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "{key}"
            );
            assert!(!en.is_empty() && !de.is_empty(), "{key}");
        }
        for key in loquax_keys() {
            assert!(!seen.contains(key.as_str()), "{key} is Loquax's too");
        }
    }

    /// Loquax's keys, read from its catalogue's source: lnomad does not
    /// depend on the messenger, and must not for this.
    fn loquax_keys() -> Vec<String> {
        let src = include_str!("../../../loquax/src/ui/strings.rs");
        let keys: Vec<String> = src
            .lines()
            .filter_map(|line| {
                let (_, rest) = line.split_once(" = \"")?;
                let (key, _) = rest.split_once('"')?;
                Some(key.to_string())
            })
            .collect();
        // A positive control on the reader: Loquax's catalogue has its tabs.
        assert!(keys.iter().any(|k| k == "tab_chats"), "read no Loquax key");
        keys
    }

    /// The same places in both languages: a German text never drops or
    /// adds an argument the English one fills.
    #[test]
    fn both_languages_fill_the_same_places() {
        fn places(text: &str) -> BTreeSet<String> {
            text.match_indices('%')
                .filter_map(|(at, _)| {
                    let tail = &text[at + 1..];
                    let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
                    let spec = tail.get(digits..digits + 2)?;
                    (digits > 0 && spec.starts_with('$')).then(|| tail[..digits + 2].to_string())
                })
                .collect()
        }
        // A positive control on the reader: the download toast fills three.
        assert_eq!(places(StrId::Downloaded.text(Lang::En)).len(), 3);
        for (_, key, en, de) in CATALOGUE {
            assert_eq!(places(en), places(de), "{key}");
        }
    }

    /// The start page in English is the micron the terminal drew before the
    /// catalogue, byte for byte; in German no English sentence is left.
    #[test]
    fn the_start_page_speaks_both_languages() {
        let before = "\
>lnomad

Terminal browser for NomadNet pages.

No page is open. Node discovery runs in the background, so nodes appear in the \
places panel as their announces arrive.

`!d`! places   `!:`! URL   `!?`! help   `!q`! quit";
        let start_page = super::super::event::start_page;
        assert_eq!(start_page(Lang::En, false), before);
        let de = start_page(Lang::De, false);
        assert!(de.contains("Keine Seite offen"), "{de}");
        assert!(!de.contains("No page") && !de.contains("quit"), "{de}");
    }

    /// A hosted browser's start page (Loquax's Browse tab) is one sentence
    /// and names no key, in both languages; the terminal's names its keys
    /// (the control).
    #[test]
    fn a_hosted_start_page_names_no_key() {
        let start_page = super::super::event::start_page;
        for lang in [Lang::En, Lang::De] {
            let page = start_page(lang, true);
            assert!(!page.contains("`!"), "no key in {page:?}");
            assert!(!page.contains("Terminal"), "{page:?}");
            assert!(start_page(lang, false).contains("`!d`!"), "the control");
        }
        assert!(start_page(Lang::En, true).contains("Tap a node"));
        assert!(start_page(Lang::De, true).contains("Tippe auf einen Knoten"));
    }

    /// Every shared scroll row the help lists has the catalogue's words: in
    /// German none keeps its English description.
    #[test]
    fn every_scroll_row_has_its_words() {
        use super::super::chrome::scroll_desc;
        use leviculum_ui::keymap::SCROLL_ROWS;
        assert!(!SCROLL_ROWS.is_empty());
        for row in SCROLL_ROWS {
            assert_eq!(scroll_desc(row.desc, Lang::En), row.desc);
            assert_ne!(scroll_desc(row.desc, Lang::De), row.desc, "{}", row.desc);
        }
        // A positive control on the lookup: an unknown row stays English.
        assert_eq!(scroll_desc("fly away", Lang::De), "fly away");
    }

    /// The help in German: every group title and description is the
    /// German text, and the layout still has as many rows as in English.
    #[test]
    fn the_help_speaks_german() {
        use super::super::chrome::help_groups_in;
        let en = help_groups_in(Lang::En);
        let de = help_groups_in(Lang::De);
        assert_eq!(en.len(), de.len());
        for (e, d) in en.iter().zip(&de) {
            assert_ne!(e.title, d.title, "{}", e.title);
            assert_eq!(e.entries.len(), d.entries.len(), "{}", e.title);
            for (ee, de) in e.entries.iter().zip(&d.entries) {
                assert_ne!(ee.desc, de.desc, "{}", ee.desc);
            }
        }
    }

    /// The image status lines and the name rows keep their English words
    /// and say them in German.
    #[test]
    fn image_and_name_words_resolve_in_german() {
        use crate::image::{status_line, status_text, SlotState};
        assert_eq!(status_line("a.png", &SlotState::Queued), "[a.png: queued]");
        assert_eq!(
            status_text("a.png", &SlotState::TooMany).resolve(Lang::De),
            "[a.png: nicht geholt]"
        );
        assert_eq!(crate::names::ago_text(7200, 0).resolve(Lang::De), "vor 2 h");
    }

    /// The first locale variable that is set wins, an empty one is skipped,
    /// and nothing set is English.
    #[test]
    fn the_locale_names_the_language() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(locale_lang(env(&[])), Lang::En);
        assert_eq!(locale_lang(env(&[("LANG", "de_DE.UTF-8")])), Lang::De);
        assert_eq!(
            locale_lang(env(&[("LC_ALL", "en_GB.UTF-8"), ("LANG", "de_DE.UTF-8")])),
            Lang::En
        );
        assert_eq!(
            locale_lang(env(&[
                ("LC_ALL", ""),
                ("LC_MESSAGES", "de_AT"),
                ("LANG", "C")
            ])),
            Lang::De
        );
    }

    #[test]
    fn a_toast_resolves_in_either_language() {
        let text = with(StrId::Copied, "aa11:/page/x.mu");
        assert_eq!(text.resolve(Lang::En), "copied aa11:/page/x.mu");
        assert_eq!(text.resolve(Lang::De), "aa11:/page/x.mu kopiert");
        let hops = Text::Key(StrId::Hops, vec![Arg::Int(3)]);
        assert_eq!(hops.resolve(Lang::En), "3 hops");
    }
}

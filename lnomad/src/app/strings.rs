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
    Loading = "browser_loading", "loading %1$s", "lade %1$s";
    Downloading = "browser_downloading", "downloading %1$s", "lade %1$s herunter";
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
    Nodes = "browser_nodes", "Discovered nodes", "Gefundene Knoten";
    NoNodesYet = "browser_no_nodes_yet", "(listening for announces…)", "(lausche auf Ankündigungen…)";
    PlaceBookmark = "browser_place_bookmark", "%1$s  ·  %2$s", "%1$s  ·  %2$s";
    PlaceNode = "browser_place_node", "%1$s  ·  %2$s  ·  %3$s", "%1$s  ·  %2$s  ·  %3$s";
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
    AnonymousTo = "browser_anonymous_to", "Now anonymous to %1$s", "Jetzt anonym bei %1$s";
    WontOpen = "browser_wont_open", "won't open %1$s: link", "öffne keinen %1$s:-Link";
    LxmfCopied = "browser_lxmf_copied", "LXMF address copied: %1$s", "LXMF-Adresse kopiert: %1$s";
    SavedImage = "browser_saved_image", "saved %1$s (%2$d bytes)", "%1$s gespeichert (%2$d Bytes)";
    OpenedImage = "browser_opened_image", "opened %1$s externally", "%1$s extern geöffnet";
    FailedToOpenImage = "browser_failed_to_open_image", "failed to open %1$s", "%1$s ließ sich nicht öffnen";
    IdentifyNotSaved = "browser_identify_not_saved", "could not save identify decision: %1$s", "Identifizier-Entscheidung nicht gespeichert: %1$s";
    OpenedExternally = "browser_opened_externally", "opened externally: %1$s", "extern geöffnet: %1$s";
    FailedToOpen = "browser_failed_to_open", "failed to open: %1$s", "ließ sich nicht öffnen: %1$s";
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

    #[test]
    fn a_toast_resolves_in_either_language() {
        let text = with(StrId::Copied, "aa11:/page/x.mu");
        assert_eq!(text.resolve(Lang::En), "copied aa11:/page/x.mu");
        assert_eq!(text.resolve(Lang::De), "aa11:/page/x.mu kopiert");
        let hops = Text::Key(StrId::Hops, vec![Arg::Int(3)]);
        assert_eq!(hops.resolve(Lang::En), "3 hops");
    }
}

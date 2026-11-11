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
    TabBrowse = "tab_browse", "Browse", "Seiten";
    // 1578: Explore, the pages and the public rooms under one button.
    TabExplore = "tab_explore", "Explore", "Entdecken";
    ExplorePages = "explore_pages", "Pages", "Seiten";
    ExploreRooms = "explore_rooms", "Rooms", "Räume";
    ExploreSearchHint = "explore_search_hint", "Search", "Suchen";
    ExploreSortSeen = "explore_sort_seen", "Last seen", "Zuletzt gesehen";
    BrowseLoading = "browse_loading", "Loading %1$s …", "Lade %1$s …";
    BrowseLoadingSecs = "browse_loading_secs", "Loading %1$s … %2$d s", "Lade %1$s … %2$d s";
    // 1667 (walk 11, finding 59): a page's text field's editor, titled by
    // the field's label or name, never a lone name.
    BrowseFieldTitle = "browse_field_title", "Input: %1$s", "Eingabe: %1$s";
    ExploreSortAz = "explore_sort_az", "A to Z", "A bis Z";
    ExploreBookmarks = "explore_bookmarks", "Bookmarks", "Lesezeichen";
    ExploreToday = "explore_today", "Today", "Heute";
    ExploreThisWeek = "explore_this_week", "This week", "Diese Woche";
    ExploreOlder = "explore_older", "Older", "Älter";
    ExploreAz = "explore_az", "All pages", "Alle Seiten";
    ExploreNoMatch = "explore_no_match", "No page matches “%1$s”.", "Keine Seite passt zu „%1$s“.";
    ExploreNoPages = "explore_no_pages", "No pages known yet: they appear here as soon as your mailbox or this device hears one.", "Noch keine Seiten bekannt: sie erscheinen hier, sobald dein Postfach oder dieses Gerät eine hört.";
    ExplorePagesOff = "explore_pages_off", "Pages cannot be opened on this device yet.", "Seiten lassen sich auf diesem Gerät noch nicht öffnen.";
    // 1621 (walk 2): the rooms the reader is in, then the servers, each
    // section one sentence when empty.
    ExploreNoJoined = "explore_no_joined", "You are in no room. Pick a server below.", "Du bist in keinem Raum. Such dir unten einen Server aus.";
    ExploreNoJoinedOn = "explore_no_joined_on", "You are in no room on %1$s.", "Du bist in keinem Raum auf %1$s.";
    ExploreOpenRooms = "explore_open_rooms", "Open rooms", "Räume öffnen";
    ExploreServers = "explore_servers", "Servers", "Server";
    ExploreNoServers = "explore_no_servers", "No server within reach.", "Kein Server in Reichweite.";
    ExploreNoServerMatch = "explore_no_server_match", "No server matches “%1$s”.", "Kein Server passt zu „%1$s“.";
    ExploreNotSeen = "explore_not_seen", "not seen yet", "noch nicht gesehen";
    ExploreFreshOne = "explore_fresh_one", "1 new page", "1 neue Seite";
    ExploreFreshMany = "explore_fresh_many", "%1$d new pages", "%1$d neue Seiten";
    // The chip's two words (1632, walk 4 new 18): "N new pages" whenever
    // pages that are not on the list wait behind it; "Update the order"
    // only when none did and the waiting answer only moves rows that are
    // there already.
    ExploreFreshOrder = "explore_fresh_order", "Update the order", "Reihenfolge aktualisieren";
    ExploreSeenNow = "explore_seen_now", "seen just now", "gesehen gerade eben";
    ExploreSeenMinutes = "explore_seen_minutes", "seen %1$d min ago", "gesehen vor %1$d Min.";
    ExploreSeenHours = "explore_seen_hours", "seen %1$d h ago", "gesehen vor %1$d Std.";
    ExploreSeenDay = "explore_seen_day", "seen yesterday", "gesehen gestern";
    ExploreSeenDays = "explore_seen_days", "seen %1$d days ago", "gesehen vor %1$d Tagen";
    // How old the mailbox's list is, on top of Explore (1644): "As of 3 h
    // ago" when the last answer is over an hour old, "Not connected · as of
    // yesterday 14:20" while the link is down.
    ExploreAsOf = "explore_as_of", "As of %1$s", "Stand: %1$s";
    ExploreDownAsOf = "explore_down_as_of", "Not connected · %1$s", "Nicht verbunden · %1$s";
    ExploreAsOfToday = "explore_as_of_today", "as of today %1$s", "Stand von heute %1$s";
    ExploreAsOfYesterday = "explore_as_of_yesterday", "as of yesterday %1$s", "Stand von gestern %1$s";
    ExploreAsOfWeekday = "explore_as_of_weekday", "as of %1$s %2$s", "Stand von %1$s %2$s";
    ExploreAsOfDate = "explore_as_of_date", "as of %1$s %2$s", "Stand vom %1$s %2$s";
    ExploreHop = "explore_hop", "directly reachable", "direkt erreichbar";
    ExploreHops = "explore_hops", "reachable over %1$d stations", "über %1$d Stationen erreichbar";
    ExploreOpenAddress = "explore_open_address", "Open %1$s", "%1$s öffnen";
    ExploreBookmark = "explore_bookmark", "Bookmark", "Lesezeichen setzen";
    ExploreUnbookmark = "explore_unbookmark", "Remove bookmark", "Lesezeichen entfernen";
    // 1618 (walk of 17024c3a): the page's menu, a nameless page, and a server's sheet.
    ExploreOpen = "explore_open", "Open", "Öffnen";
    ExploreCopyAddress = "explore_copy_address", "Copy address", "Adresse kopieren";
    ExploreCopied = "explore_copied", "Address copied", "Adresse kopiert";
    ExploreUnnamedPage = "explore_unnamed_page", "Unnamed page", "Unbenannte Seite";
    ExploreServerUse = "explore_server_use", "Use this server for rooms", "Diesen Server für Räume nutzen";
    ExploreServerInUse = "explore_server_in_use", "Your rooms are on this server.", "Deine Räume laufen über diesen Server.";
    ExploreServerRooms = "explore_server_rooms", "Its rooms", "Seine Räume";
    ExploreServerAsking = "explore_server_asking", "Asking the server for its rooms…", "Frage den Server nach seinen Räumen…";
    ExploreServerAskingFor = "explore_server_asking_for", "Asking the server for its rooms… %1$d s", "Frage den Server nach seinen Räumen… %1$d s";
    ExploreServerClose = "explore_server_close", "Close", "Schließen";
    // The Rooms (1379): Reticulum Relay Chat on a hub, live, beside the groups.
    TabRooms = "tab_rooms", "Rooms", "Räume";
    RoomsLive = "rooms_live", "A room is live: no mailbox keeps it, there are no receipts and no ticks, and what you missed is gone.", "Ein Raum ist live: kein Postfach hebt ihn auf, es gibt keine Lesebestätigungen und keine Haken, und was du verpasst hast, ist weg.";
    RoomsNoHub = "rooms_no_hub", "No public rooms yet. Choose a server under Explore.", "Noch keine öffentlichen Räume. Wähl einen Server unter Entdecken.";
    RoomsLinking = "rooms_linking", "Connecting…", "Verbinde…";
    RoomsLinkingFor = "rooms_linking_for", "Connecting… %1$d s", "Verbinde… %1$d s";
    // 1659 (walk 10, 53): from 20 s until the patience of 45 s.
    RoomsSlowFor = "rooms_slow_for", "The server is answering slowly… %1$d s", "Server antwortet langsam… %1$d s";
    RoomsNotLinked = "rooms_not_linked", "Not connected yet.", "Noch nicht verbunden.";
    RoomsLinked = "rooms_linked", "Connected", "Verbunden";
    RoomsConnect = "rooms_connect", "Connect", "Verbinden";
    RoomsRetry = "rooms_retry", "Try again", "Erneut versuchen";
    RoomsOnHub = "rooms_on_hub", "Rooms on %1$s", "Räume auf %1$s";
    RoomsAsking = "rooms_asking", "Asking the server for its rooms…", "Frage den Server nach seinen Räumen…";
    RoomsNoneNamed = "rooms_none_named", "This server names no public rooms.", "Dieser Server nennt keine öffentlichen Räume.";
    RoomsOther = "rooms_other", "Join another room", "Anderen Raum beitreten";
    RoomsNoPath = "rooms_no_path", "The rooms cannot be reached. Is their server on this network?", "Die Räume sind nicht erreichbar. Ist ihr Server in diesem Netz?";
    RoomsNoAnswer = "rooms_no_answer", "The server does not answer.", "Server antwortet nicht.";
    RoomsLost = "rooms_lost", "The connection to the server broke.", "Die Verbindung zum Server ist abgerissen.";
    RoomsLeft = "rooms_left", "Not connected.", "Nicht verbunden.";
    RoomsOff = "rooms_off", "There are no public rooms here yet. Choose a server under Explore.", "Hier gibt es noch keine öffentlichen Räume. Wähl einen Server unter Entdecken.";
    RoomsHowToJoin = "rooms_how_to_join", "Type a room's name, like #lab, to join it.", "Tipp den Namen eines Raums, etwa #lab, um ihm beizutreten.";
    RoomsNotReady = "rooms_not_ready", "Not connected yet. Wait a moment and try again.", "Noch nicht verbunden. Warte kurz und versuch es noch einmal.";
    RoomsNotIn = "rooms_not_in", "You are not in %1$s. Join it first.", "Du bist nicht in %1$s. Tritt ihm zuerst bei.";
    RoomsTooLong = "rooms_too_long", "This line is %1$d bytes long and a room takes at most %2$d. Shorten it.", "Diese Zeile ist %1$d Bytes lang, ein Raum nimmt höchstens %2$d. Kürze sie.";
    RoomsTooBig = "rooms_too_big", "This line needs %1$d bytes and the link carries %2$d. Shorten it.", "Diese Zeile braucht %1$d Bytes, die Verbindung trägt %2$d. Kürze sie.";
    RoomsBadRoom = "rooms_bad_room", "A room name needs a few letters on one line.", "Ein Raumname braucht ein paar Buchstaben in einer Zeile.";
    RoomsTooMany = "rooms_too_many", "You are in %1$d rooms, as many as their server allows. Leave one first.", "Du bist in %1$d Räumen, so vielen, wie ihr Server erlaubt. Verlass zuerst einen.";
    RoomsYou = "rooms_you", "you", "du";
    RoomsCameIn = "rooms_came_in", "%1$s came in", "%1$s ist da";
    // Our own join (1384): "du ist da" was the nick put into the line above.
    RoomsWeCameIn = "rooms_we_came_in", "you came in", "du bist da";
    RoomsWentOut = "rooms_went_out", "%1$s left", "%1$s ist gegangen";
    // 1450: a room drawn as a chat, its header, its people and its menu.
    RoomsJoinHint = "rooms_join_hint", "Room name, like #lab", "Raumname, etwa #lab";
    RoomsOnlyYou = "rooms_only_you", "Only you so far", "Bisher nur du";
    RoomsWithYou = "rooms_with_you", "%1$s, you", "%1$s, du";
    RoomsPeople = "rooms_people", "Who is here", "Wer hier ist";
    RoomsPeopleNote = "rooms_people_note", "Everyone who came in or wrote while you were here.", "Alle, die hereinkamen oder schrieben, während du hier warst.";
    RoomsLeave = "rooms_leave", "Leave room", "Raum verlassen";
    // 1577: the public rooms' hub, chosen in the settings, and the mark of a public room.
    RoomsPublic = "rooms_public", "public", "öffentlich";
    RoomsHub = "rooms_hub_setting", "Server for public rooms", "Server für öffentliche Räume";
    RoomsHubNone = "rooms_hub_none", "None", "Keiner";
    RoomsHubOwn = "rooms_hub_own", "Your own server", "Eigener Server";
    RoomsHubPaste = "rooms_hub_paste", "Another server: paste its address", "Anderer Server: Adresse einfügen";
    RoomsHubPasteHint = "rooms_hub_paste_hint", "The server's address", "Adresse des Servers";
    RoomsHubBad = "rooms_hub_bad", "That is no server's address.", "Das ist keine Adresse eines Servers.";
    RoomsHubChosen = "rooms_hub_chosen", "Public rooms on %1$s.", "Öffentliche Räume auf %1$s.";
    RoomsHubOff = "rooms_hub_off", "No public rooms.", "Keine öffentlichen Räume.";
    // 1599: hubs heard on the mesh, offered closest and newest first.
    RoomsHubUnnamed = "rooms_hub_unnamed", "Server without a name", "Server ohne Namen";
    RoomsHubHop = "rooms_hub_hop", "directly reachable", "direkt erreichbar";
    RoomsHubHops = "rooms_hub_hops", "reachable over %1$d stations", "über %1$d Stationen erreichbar";
    RoomsHubSuggested = "rooms_hub_suggested", "suggested", "empfohlen";
    RoomsHubNoAnswer = "rooms_hub_no_answer", "no answer", "keine Antwort";
    RoomsHubMore = "rooms_hub_more", "More…", "Mehr…";
    RoomsNameVisible = "rooms_name_visible", "Your name is visible to everyone in the room.", "Dein Name ist im Raum für alle sichtbar.";
    RoomsNoName = "rooms_no_name", "You have no name on this server yet: in the room you appear without one.", "Auf diesem Server hast du noch keinen Namen: Im Raum erscheinst du ohne Namen.";
    RoomsNoNameProfile = "rooms_no_name_profile", "You have no name on this server yet (your profile is called %1$s): in the room you appear without one.", "Auf diesem Server hast du noch keinen Namen (dein Profil heißt %1$s): Im Raum erscheinst du ohne Namen.";
    Settings = "settings", "Settings", "Einstellungen";
    // 1450 (walk 1, finding 21): the settings' nine sections, one list on
    // every target (`super::settings`), in the phone's order.
    SettingsProfile = "settings_profile", "Profile", "Profil";
    SettingsProfileSummary = "settings_profile_summary", "Name, address, QR code", "Name, Adresse, QR-Code";
    Mailbox = "mailbox", "Mailbox", "Postfach";
    SettingsMailboxSummary = "settings_mailbox_summary", "Which mailbox, connection, devices", "Welches Postfach, Verbindung, Geräte";
    SettingsAppearance = "settings_appearance", "Appearance", "Darstellung";
    SettingsAppearanceSummary = "settings_appearance_summary", "Light or dark, the map", "Hell oder dunkel, die Karte";
    SettingsChats = "settings_chats", "Chats", "Chats";
    SettingsChatsSummary = "settings_chats_summary", "Photo size", "Fotogröße";
    SettingsNotifications = "settings_notifications", "Notifications", "Benachrichtigungen";
    SettingsNotificationsSummary = "settings_notifications_summary", "New messages, muted chats", "Neue Nachrichten, stumme Chats";
    SettingsPrivacy = "settings_privacy", "Privacy", "Privatsphäre";
    SettingsPrivacySummary = "settings_privacy_summary", "Read receipts, blocked contacts, positions you shared", "Lesebestätigungen, blockierte Kontakte, geteilte Positionen";
    SettingsMapsSummary = "settings_maps_summary", "Maps on this device, automatic download", "Karten auf diesem Gerät, automatisch laden";
    SettingsBackup = "settings_backup", "Backup", "Sicherung";
    SettingsBackupSummary = "settings_backup_summary", "Back up the mailbox", "Das Postfach sichern";
    Advanced = "advanced", "Advanced", "Erweitert";
    SettingsAdvancedSummary = "settings_advanced_summary", "Relay and more", "Relais und mehr";
    PropagationNode = "propagation_node", "Relay", "Relais";
    PropagationNodeText = "propagation_node_text", "While your mailbox is offline, mail for you waits at a relay in the network, which Loquax chooses by itself.", "Solange dein Postfach offline ist, wartet Post für dich bei einem Relais im Netz, das Loquax selbst wählt.";
    SettingsLight = "settings_light", "Light", "Hell";
    SettingsDark = "settings_dark", "Dark", "Dunkel";
    SettingsOn = "settings_on", "On", "An";
    SettingsOff = "settings_off", "Off", "Aus";
    SettingsNotificationsDesktop = "settings_notifications_desktop", "Loquax tells you of every new message with a notification of the desktop; a muted chat stays quiet. To mute one, open its menu in the list.", "Loquax meldet jede neue Nachricht mit einer Benachrichtigung des Desktops; ein stummer Chat bleibt still. Zum Stummschalten öffne sein Menü in der Liste.";
    SettingsBackupDesktop = "settings_backup_desktop", "On a computer, loquax export writes your address, contacts and history to a folder, and loquax import reads them back.", "Auf einem Computer schreibt loquax export deine Adresse, deine Kontakte und den Verlauf in einen Ordner, loquax import liest sie wieder ein.";
    SettingsNothingHere = "settings_nothing_here", "Nothing to set here on this device yet.", "Auf diesem Gerät gibt es hier noch nichts einzustellen.";
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
    Mute = "mute", "Mute", "Stummschalten";
    Unmute = "unmute", "Unmute", "Stummschaltung aufheben";
    Archive = "archive", "Archive", "Archivieren";
    Unarchive = "unarchive", "Unarchive", "Aus dem Archiv holen";
    MarkRead = "mark_read", "Mark as read", "Als gelesen markieren";
    MarkUnread = "mark_unread", "Mark as unread", "Als ungelesen markieren";
    Delete = "delete", "Delete", "Löschen";
    // Names (1452): the name alone, "Unknown" without one, and where two
    // peers share a name, the sentence that says so beside the hash tails.
    UnknownContact = "unknown_contact", "Unknown", "Unbekannt";
    SameNameTwo = "same_name_two", "Two contacts are called %1$s", "Zwei Kontakte heißen %1$s";
    SameNameMany = "same_name_many", "%1$d contacts are called %2$s", "%1$d Kontakte heißen %2$s";
    NoNameTwo = "no_name_two", "Two contacts have no name yet", "Zwei Kontakte haben noch keinen Namen";
    NoNameMany = "no_name_many", "%1$d contacts have no name yet", "%1$d Kontakte haben noch keinen Namen";
    // Times and the liveness words (1209).
    Today = "today", "Today", "Heute";
    Yesterday = "yesterday", "Yesterday", "Gestern";
    JustNow = "just_now", "Now", "Jetzt";
    MinutesAgo = "minutes_ago", "%1$d min", "%1$d Min.";
    HoursAgo = "hours_ago", "%1$d h", "%1$d Std.";
    DaysAgo = "days_ago", "%1$d d", "%1$d T.";
    Position = "position", "Position", "Position";
    Live = "live", "live", "live";
    LiveEvery = "live_every", "every %1$s", "alle %1$s";
    LiveAgo = "live_ago", "%1$s ago", "vor %1$s";
    LiveNow = "live_now", "now", "gerade eben";
    LastPosition = "last_position", "last position %1$s", "letzte Position %1$s";
    // A shared position in a chat (1449 item 5, walk 1 finding 11): one
    // sentence, never the coordinates, which the Info shows.
    PositionLive = "position_live", "Live position", "Live-Position";
    PositionToday = "position_today", "Position from today %1$s", "Position von heute %1$s";
    PositionYesterday = "position_yesterday", "Position from yesterday %1$s", "Position von gestern %1$s";
    PositionWeekday = "position_weekday", "Position from %1$s %2$s", "Position von %1$s %2$s";
    PositionDate = "position_date", "Position from %1$s %2$s", "Position vom %1$s %2$s";
    InfoPlace = "info_place", "Place", "Ort";
    InfoAccuracy = "info_accuracy", "Accuracy", "Genauigkeit";
    PositionOf = "position_of", "Position of %1$s, %2$s", "Position von %1$s, %2$s";
    PositionMarker = "position_marker", "⌖", "⌖";
    // Our own pin on the map (1447): "you" was English on every screen.
    MapYou = "map_you", "you", "du";
    // What stopping the phone's service costs (1447, walk 1 finding 27), under "Dienst beenden".
    StopServiceText = "stop_service_text", "Loquax then receives nothing until you open it again.", "Loquax empfängt dann nichts mehr, bis du es wieder öffnest.";
    // The radar's line where no map is loaded (1447, walk 1 finding 19).
    RadarNote = "radar_note", "No map loaded for this area · Rings %1$s", "Keine Karte für diese Gegend geladen · Ringe %1$s";
    RadarNoteOn = "radar_note_on", "No map loaded for this area · Rings %1$s around %2$s", "Keine Karte für diese Gegend geladen · Ringe %1$s um %2$s";
    // The M9's map where no region lies under the view (1476): the words of
    // 1447 §5 instead of plain grey, and how a region gets onto the board.
    MapNoRegion = "map_no_region", "No map for this area", "Keine Karte für diese Gegend";
    MapRegionHow = "map_region_how", "lnflash loads maps over USB", "Karten lädt lnflash per USB";
    MapOverview = "map_overview", "Overview only", "Nur Übersicht";
    MapSkySearching = "map_sky_searching", "Searching satellites… %1$d", "Suche Satelliten… %1$d";
    MapSkyFix = "map_sky_fix", "Fix %1$s ago", "Fix vor %1$s";
    // An empty conversation in the terminal (1447, walk 1 finding 22).
    NoMessagesYet = "no_messages_yet", "No messages yet", "Noch keine Nachrichten";
    // The terminal's title without a name of our own (1447).
    AppName = "app_name_title", "Loquax", "Loquax";
    // The map with nobody else's position (1447): the window, the phone and the terminal.
    MapEmpty = "map_empty", "Nobody shares a position with you yet", "Noch niemand teilt eine Position mit dir";
    NoPosition = "no_position", "No position from this person yet", "Noch keine Position von dieser Person";
    // A node the mesh shows on the map that is no contact (1302).
    NearbyCard = "nearby_card", "Nearby, not in your contacts", "In der Nähe, nicht in deinen Kontakten";
    NearbyAdd = "nearby_add", "Add", "Hinzufügen";
    NearbyEnterAdds = "nearby_enter_adds", "Enter adds", "Enter fügt hinzu";
    // The map on the M9 (1375): its pins in ASCII, the atlas has no ◉, and
    // the keys a board without a palette or a pointer works it with.
    // 1447 (walk 1 finding 23): each fits the M9's 31 cells; the keys come in two pages.
    MapKeys = "map_keys", "T in  Y out  arrows pan", "T näher  Y weiter  Pfeile schieben";
    MapKeysPin = "map_keys_pin", "space next pin  enter open", "Leer nächster Pin  Enter öffnen";
    // A row's last line while a message is typed there and not sent (1219).
    DraftRow = "draft_row", "Draft: %1$s", "Entwurf: %1$s";
    // The terminal's list line of our own last message (1381).
    RowYou = "row_you", "you: %1$s", "du: %1$s";
    Reacted = "reacted", "reacted %1$s", "hat mit %1$s reagiert";
    // A reaction's line with its subject (1447, walk 1 finding 24).
    ReactedBy = "reacted_by", "%1$s reacted %2$s", "%1$s hat mit %2$s reagiert";
    YouReacted = "you_reacted", "You reacted %1$s", "Du hast mit %1$s reagiert";
    RunUpdates = "run_updates", "%1$d updates since %2$s", "%1$d Meldungen seit %2$s";
    QuoteYou = "quote_you", "You", "Du";
    // A relayed group (1482): a bot's chat, a reaction or a reply whose target is not on this device.
    RelayedGroup = "relayed_group", "%1$s · group via relay", "%1$s · Gruppe über Relais";
    ReactedElsewhere = "reacted_elsewhere", "reacted %1$s to a message that is not here", "hat mit %1$s auf eine Nachricht reagiert, die hier nicht liegt";
    QuoteNotHere = "quote_not_here", "A message that is not here", "Eine Nachricht, die hier nicht liegt";
    InfoRelayedFrom = "info_relayed_from", "%1$s %2$s, via %3$s", "%1$s %2$s, über %3$s";
    // A row's last line for a message without words (1377): the list and
    // the phone's notification say these in the reader's language.
    RowPhoto = "row_photo", "Photo", "Foto";
    RowPositionShared = "row_position_shared", "Position shared", "Position geteilt";
    RowPositionReport = "row_position_report", "Position report", "Positionsmeldung";
    // The chat: compose, the marks, a bubble's menu.
    ComposeHint = "compose_hint", "Message", "Nachricht";
    Send = "send", "Send", "Senden";
    // The ladder's words for a screen reader (1232,
    // loquax-read-receipts.md, section 6): the clock twice, one tick, two,
    // two in the accent, the red circle.
    StateSending = "state_sending", "Sending", "Wird gesendet";
    StateUnconfirmed = "state_unconfirmed", "Sent, not confirmed yet", "Gesendet, noch nicht bestätigt";
    StateMailbox = "state_mailbox", "Waiting in the mailbox", "Wartet im Postfach";
    // One tick from the recipient's proof (1462): its mailbox or its
    // device, the word is neutral; two ticks are the device's own word.
    StateArrived = "state_arrived", "Arrived", "Angekommen";
    StateDelivered = "state_delivered", "On the recipient's device", "Auf dem Gerät";
    // A group's two ticks (1466): every member's device said it has it.
    StateGroupDelivered = "state_group_delivered", "On the devices", "Auf den Geräten";
    StateRead = "state_read", "Read at %1$s", "Gelesen um %1$s";
    // The arrived rung's words when the propagation node, not a proof,
    // witnessed it (1395, 1462, loquax-propagation-receipts.md).
    StateCollected = "state_collected", "Collected from the propagation network", "Aus dem Propagation-Netz abgeholt";
    StateFailed = "state_failed", "Not sent. Double tap to send again", "Nicht gesendet. Doppeltippen, um es erneut zu senden";
    SendAgain = "scene_send_again", "Send again", "Erneut senden";
    ActionReply = "action_reply", "Reply", "Antworten";
    ActionCopy = "action_copy", "Copy", "Kopieren";
    ActionForward = "action_forward", "Forward", "Weiterleiten";
    ActionInfo = "action_info", "Info", "Info";
    ActionDelete = "action_delete", "Delete", "Löschen";
    ActionSelect = "action_select", "Select", "Auswählen";
    // A message's Info (1381): plain rows in the reader's language; the
    // hex and the device's log only under Advanced.
    InfoAdvanced = "info_advanced", "Advanced", "Erweitert";
    InfoFrom = "info_from", "From", "Von";
    InfoTo = "info_to", "To", "An";
    InfoSent = "info_sent", "Sent", "Gesendet";
    InfoReceived = "info_received", "Received", "Empfangen";
    InfoState = "info_state", "Status", "Status";
    InfoWaiting = "info_waiting", "Waiting to be sent", "Wartet auf den Versand";
    InfoPreparing = "info_preparing", "Being prepared for sending", "Wird zum Senden vorbereitet";
    InfoSending = "info_sending", "Being sent now", "Wird gerade gesendet";
    InfoMailbox = "info_mailbox", "In the mailbox, delivered once %1$s is online", "Im Postfach, wird zugestellt, sobald %1$s online ist";
    InfoDelivered = "info_delivered", "On %1$s's device", "Auf dem Gerät von %1$s";
    // The times of the ladder (1462): when the proof came, when the device
    // said it has it, when it was read.
    InfoArrived = "info_arrived", "Arrived", "Angekommen";
    InfoOnDevice = "info_on_device", "On the device", "Auf dem Gerät";
    InfoReadAt = "info_read_at", "Read", "Gelesen";
    InfoCollected = "info_collected", "Collected from the propagation network at %1$s", "Um %1$s aus dem Propagation-Netz abgeholt";
    InfoNotCollected = "info_not_collected", "Not collected at the propagation node; perhaps delivered via another", "Am Propagation-Knoten nicht abgeholt; vielleicht über einen anderen zugestellt";
    InfoRefused = "info_refused", "Not sent: it was refused. Double tap to send again", "Nicht gesendet: es wurde abgelehnt. Doppeltippen, um es erneut zu senden";
    InfoSender = "info_sender", "Sender", "Absender";
    InfoSignatureOk = "info_signature_ok", "Confirmed by its signature", "Durch die Signatur bestätigt";
    InfoSignatureUnchecked = "info_signature_unchecked", "Not confirmed yet: the sender's key is missing", "Noch nicht bestätigt: der Schlüssel des Absenders fehlt";
    InfoSignatureBad = "info_signature_bad", "Signature invalid: it may not be from %1$s", "Signatur ungültig: sie ist vielleicht nicht von %1$s";
    InfoRead = "info_read", "Read", "Gelesen";
    InfoReceiptsOff = "info_receipts_off", "Read receipts are off: you send none and see none", "Lesebestätigungen sind aus: du sendest keine und siehst keine";
    InfoNoReceipts = "info_no_receipts", "%1$s sends no read receipts", "%1$s sendet keine Lesebestätigungen";
    InfoGroup = "info_group", "Group", "Gruppe";
    InfoGroupNone = "info_group_none", "No member's copy is confirmed yet", "Noch bei keinem Mitglied bestätigt";
    InfoGroupDelivered = "info_group_delivered", "Delivered to %1$d of %2$d", "Bei %1$d von %2$d angekommen";
    InfoGroupRead = "info_group_read", "Read by %1$d of %2$d", "Von %1$d von %2$d gelesen";
    InfoGroupNoReaders = "info_group_no_readers", "Nobody in it sends read receipts", "Niemand darin sendet Lesebestätigungen";
    InfoGroupQuiet = "info_group_quiet", "%1$d send no read receipts and are not counted", "%1$d senden keine Lesebestätigungen und zählen nicht mit";
    // A member's row in a group line's Info (1466): its rung and since
    // when; a copy nobody proved yet says so.
    InfoMemberWaiting = "info_member_waiting", "Not arrived yet", "Noch nicht angekommen";
    InfoSize = "info_size", "Size", "Größe";
    InfoKb = "info_kb", "%1$d.%2$d KB", "%1$d,%2$d KB";
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
    // Group chats (1307, the study's sections 3.0 and 4).
    NewGroup = "group_new", "New group", "Neue Gruppe";
    GroupNext = "group_next", "Next", "Weiter";
    GroupCreate = "group_create", "Create", "Erstellen";
    GroupNameHint = "group_name_hint", "Group name", "Name der Gruppe";
    GroupPickTitle = "group_pick_title", "Add members", "Mitglieder hinzufügen";
    GroupPicked = "group_picked", "%1$d selected", "%1$d ausgewählt";
    GroupTrustCreate = "group_trust_create", "%1$s is kept on your mailbox. Messages in the group can be read there.", "%1$s liegt in deinem Postfach. Dort sind die Nachrichten der Gruppe lesbar.";
    GroupTrustJoin = "group_trust_join", "%1$s is kept on %2$s's mailbox. %2$s could read the group's messages.", "%1$s liegt im Postfach von %2$s. %2$s könnte die Nachrichten der Gruppe lesen.";
    GroupInvited = "group_invited", "%1$s invited you to %2$s", "%1$s lädt dich zu %2$s ein";
    GroupYouInvited = "group_you_invited", "You invited them to %1$s", "Du hast zu %1$s eingeladen";
    GroupJoin = "group_join", "Join", "Beitreten";
    GroupJoined = "group_joined_card", "You joined %1$s", "Du bist %1$s beigetreten";
    GroupInvitation = "group_invitation", "Invitation", "Einladung";
    GroupMembers = "group_members", "Members", "Mitglieder";
    GroupMemberCount = "group_member_count", "%1$d members", "%1$d Mitglieder";
    GroupLeave = "group_leave", "Leave group", "Gruppe verlassen";
    GroupLeaveAsk = "group_leave_ask", "Leave %1$s?", "%1$s verlassen?";
    GroupRemove = "group_remove", "Remove", "Entfernen";
    GroupAdd = "group_add", "Add members", "Mitglieder hinzufügen";
    GroupAddConfirm = "group_add_confirm", "Add", "Hinzufügen";
    // The header's relay, by the name it announces, never its hash (1587,
    // feedback_no_hashes_on_screen).
    RelayNamed = "relay_named", "relay %1$s", "Relais %1$s";
    RelayUnnamed = "relay_unnamed", "a relay", "ein Relais";
    GroupRemoveAsk = "group_remove_ask", "Remove %1$s from %2$s?", "%1$s aus %2$s entfernen?";
    GroupYes = "group_yes", "Yes", "Ja";
    GroupNo = "group_no", "No", "Nein";
    GroupYou = "group_you", "You", "Du";
    // 1449 item 3 (walk 1 finding 9): a group's chat menu names the group,
    // never a contact.
    GroupInfo = "group_info", "Group info", "Gruppeninfo";
    GroupAdmin = "group_admin", "admin", "Admin";
    GroupStateJoined = "group_state_joined", "joined", "beigetreten";
    GroupStateInvited = "group_state_invited", "invited", "eingeladen";
    // The member sheet says where an invitation of the reader's stands (1307 pass six).
    GroupStateDelivering = "group_state_delivering", "invited, delivering", "eingeladen, wird zugestellt";
    GroupStateWaiting = "group_state_waiting", "invited, waiting for them to come online", "eingeladen, wartet, bis das Mitglied online ist";
    GroupStateUndelivered = "group_state_undelivered", "invited, not delivered", "eingeladen, nicht zugestellt";
    GroupStateLeft = "group_state_left", "left", "ausgetreten";
    GroupHomeTooOld = "group_home_too_old", "Your mailbox cannot hold groups yet. Update Loquax there.", "Dein Postfach kann noch keine Gruppen führen. Aktualisiere Loquax dort.";
    GroupYouLeft = "group_you_left", "You left %1$s", "Du hast %1$s verlassen";
    GroupDelete = "group_delete", "Delete group", "Gruppe löschen";
    GroupDeleteAsk = "group_delete_ask", "Delete %1$s for everyone?", "%1$s für alle löschen?";
    GroupRemoveFromList = "group_remove_from_list", "Remove from list", "Aus der Liste entfernen";
    GroupAgain = "group_again", "Start again with the same people", "Mit denselben Leuten neu starten";
    GroupEnded = "group_ended", "%1$s ended the group", "%1$s hat die Gruppe beendet";
    GroupArrivedLate = "group_arrived_late", "arrived late", "kam verspätet";
    // A group's own lines and a digest's left-out lines, said from the `lev` data in the reader's language (1348): the wire carries the English.
    GroupNoteJoined = "group_note_joined", "%1$s joined.", "%1$s ist beigetreten.";
    GroupNoteLeft = "group_note_left", "%1$s left.", "%1$s ist ausgetreten.";
    GroupNoteAdded = "group_note_added", "%1$s was added.", "%1$s wurde hinzugefügt.";
    GroupNoteYouJoined = "group_note_you_joined", "You joined.", "Du bist beigetreten.";
    GroupNoteYouLeft = "group_note_you_left", "You left.", "Du bist ausgetreten.";
    GroupNoteYouAdded = "group_note_you_added", "You were added.", "Du wurdest hinzugefügt.";
    GroupNoteEnded = "group_note_ended", "%1$s ended the group.", "%1$s hat die Gruppe beendet.";
    GroupNoteYouEnded = "group_note_you_ended", "You ended the group.", "Du hast die Gruppe beendet.";
    GroupNoteAdminEnded = "group_note_admin_ended", "The admin ended the group.", "Der Admin hat die Gruppe beendet.";
    GroupLeftOutPhoto = "group_left_out_photo", "sent a photo (too large for the radio)", "hat ein Foto geschickt (zu groß für die Funkstrecke)";
    GroupLeftOutVoice = "group_left_out_voice", "sent a voice message (too large for the radio)", "hat eine Sprachnachricht geschickt (zu groß für die Funkstrecke)";
    GroupLeftOutFile = "group_left_out_file", "sent a file (too large for the radio)", "hat eine Datei geschickt (zu groß für die Funkstrecke)";
    GroupLeftOutGone = "group_left_out_gone", "a message (no longer here)", "eine Nachricht (nicht mehr da)";
    // A group's status of our message in its ledger (1381): kept as numbers, said in the reader's language.
    GroupStatusDevicesNote = "group_status_devices_note", "Group: arrived at %1$d of %2$d, on the devices of %3$d, read by %4$d of %5$d", "Gruppe: bei %1$d von %2$d angekommen, auf den Geräten von %3$d, von %4$d von %5$d gelesen";
    GroupStatusNote = "group_status_note", "Group: delivered to %1$d of %2$d, read by %3$d of %4$d", "Gruppe: bei %1$d von %2$d angekommen, von %3$d von %4$d gelesen";
    MyCode = "scene_my_code", "My code", "Mein Code";
    // The profile behind the avatar (1443): one scene on every target; the keys are the phone's as they were.
    ProfileTitle = "profile_title", "Your profile", "Dein Profil";
    ProfileNameElsewhere = "profile_name_elsewhere", "Your name is set where your mailbox runs.", "Dein Name wird dort festgelegt, wo dein Postfach läuft.";
    ProfileAddress = "profile_address", "Your address", "Deine Adresse";
    ProfileQrCaption = "profile_qr_caption", "Scan this code to add me as a contact", "Scanne diesen Code, um mich als Kontakt hinzuzufügen";
    ProfileMailboxHere = "profile_mailbox_here", "Mailbox: on this device", "Postfach: auf diesem Gerät";
    // 1447 (walk 1 finding 13): the mailbox by the name it announces, never the server it is reached through.
    ProfileMailboxAway = "profile_mailbox_named", "Mailbox: %1$s", "Postfach: %1$s";
    ProfileMailboxConnected = "profile_mailbox_connected", "%1$s (connected)", "%1$s (verbunden)";
    ProfileMailboxElsewhere = "profile_mailbox_elsewhere", "Mailbox: on another device", "Postfach: auf einem anderen Gerät";
    ProfileThisDeviceUnnamed = "profile_this_device_unnamed", "This device", "Dieses Gerät";
    ProfileDeviceUnnamed = "profile_device_unnamed", "A device without a name", "Ein Gerät ohne Namen";
    ProfileMailboxUnknown = "profile_mailbox_unknown", "Mailbox: not set up yet", "Postfach: noch nicht eingerichtet";
    ProfileVia = "profile_via", "Connected over %1$s", "Verbunden über %1$s";
    ProfileDevices = "profile_devices", "Your devices", "Deine Geräte";
    ProfileThisDevice = "profile_this_device", "%1$s (this device)", "%1$s (dieses Gerät)";
    ProfileShare = "profile_share", "Share my address", "Meine Adresse teilen";
    ProfileCopied = "profile_copied", "Your address is on the clipboard", "Deine Adresse ist in der Zwischenablage";
    // 1642: the mailbox page, when the mailbox is on another device: which one, how it is reached, who uses it.
    MailboxWhich = "mailbox_which", "Which mailbox", "Welches Postfach";
    MailboxName = "mailbox_name", "Name", "Name";
    MailboxNameUnheard = "mailbox_name_unheard", "not heard yet", "noch nicht gehört";
    MailboxAddress = "mailbox_address", "Address", "Adresse";
    MailboxAddressOwn = "mailbox_address_own", "This is your address too: the mailbox and you are one identity.", "Das ist auch deine Adresse: Postfach und du sind eine Identität.";
    MailboxHost = "mailbox_host", "Runs on", "Läuft auf";
    MailboxVersion = "mailbox_version", "Version", "Version";
    MailboxVersionNewer = "mailbox_version_newer", "%1$s, newer", "%1$s, neuer";
    MailboxVersionOlder = "mailbox_version_older", "%1$s, older", "%1$s, älter";
    MailboxVersionBuild = "mailbox_version_build", "%1$s, another build", "%1$s, anderer Stand";
    MailboxVersionOld = "mailbox_version_old", "older than this device", "älter als dieses Gerät";
    MailboxOldText = "mailbox_old_text", "Your mailbox runs an older version. Update it there so that Explore and this page are complete.", "Dein Postfach läuft mit einer älteren Version. Aktualisiere es dort, damit Entdecken und diese Seite vollständig sind.";
    MailboxReach = "mailbox_reach", "Connection", "Verbindung";
    MailboxUp = "mailbox_up", "This device is connected", "Dieses Gerät ist verbunden";
    MailboxUpFor = "mailbox_up_for", "This device connected for %1$s", "Dieses Gerät verbunden seit %1$s";
    MailboxDown = "mailbox_down", "Not connected", "Nicht verbunden";
    MailboxDownHere = "mailbox_down_here", "This device is not connected", "Dieses Gerät ist nicht verbunden";
    MailboxDownFor = "mailbox_down_for", "This device not connected for %1$s", "Dieses Gerät nicht verbunden seit %1$s";
    MailboxMoment = "mailbox_moment", "less than a minute", "weniger als einer Minute";
    MailboxContact = "mailbox_contact", "Last contact", "Letzter Kontakt";
    MailboxContactNone = "mailbox_contact_none", "none yet", "noch keiner";
    MailboxWayShielded = "mailbox_way_shielded", "Connection to %1$s, shielded (the phone hears only its mailbox)", "Verbindung zu %1$s, geschirmt (das Handy hört nur sein Postfach)";
    MailboxWayOpen = "mailbox_way_open", "Connection to %1$s, not shielded (the phone hears the whole network, which costs battery and data)", "Verbindung zu %1$s, ungeschirmt (das Handy hört das ganze Netz, kostet Akku und Daten)";
    MailboxWayOther = "mailbox_way_other", "Connection to %1$s, port %2$d", "Verbindung zu %1$s, Port %2$d";
    MailboxWayNone = "mailbox_way_none", "No server reached yet", "Noch kein Server erreicht";
    MailboxWayLocal = "mailbox_way_local", "Over this computer's network connection", "Über die Netzverbindung dieses Rechners";
    MailboxAccess = "mailbox_access", "Devices of this mailbox", "Geräte dieses Postfachs";
    MailboxSeen = "mailbox_seen", "last there %1$s", "zuletzt da %1$s";
    MailboxSeenNever = "mailbox_seen_never", "never connected yet", "noch nie verbunden";
    MailboxThisDevice = "mailbox_this_device", "this device", "dieses Gerät";
    MailboxDevicesAsking = "mailbox_devices_asking", "Asking your mailbox which devices use it …", "Dein Postfach wird gefragt, welche Geräte es nutzen …";
    MailboxDevicesOld = "mailbox_devices_old", "Your mailbox does not list its devices yet: that needs a newer version there.", "Dein Postfach nennt seine Geräte noch nicht: dafür braucht es dort eine neuere Version.";
    MailboxAccessThere = "mailbox_access_there", "A device is added or removed on the mailbox itself.", "Ein Gerät wird am Postfach selbst hinzugefügt oder entfernt.";
    MailboxShowCode = "mailbox_show_code", "Show my code", "Meinen Code zeigen";
    MailboxCopy = "mailbox_copy", "Copy", "Kopieren";
    MailboxCopied = "mailbox_copied", "The mailbox's address is on the clipboard", "Die Adresse des Postfachs ist in der Zwischenablage";
    // The ladder's header words (loquax-reachability.md), as "Connected over %1$s" names them.
    RungLocal = "rung_local", "this computer", "diesen Rechner";
    RungBluetooth = "rung_bluetooth", "Bluetooth", "Bluetooth";
    RungHomeWifi = "rung_home_wifi", "the home WiFi", "das Heim-WLAN";
    RungDirect = "rung_direct", "a direct connection", "eine direkte Verbindung";
    RungRelayed = "rung_relayed", "a relay server", "einen Vermittlungsserver";
    RungMesh = "rung_mesh", "the mesh", "das Mesh";
    Name = "scene_name", "Your name", "Dein Name";
    Pull = "scene_pull", "Fetch messages", "Nachrichten abholen";
    Front = "scene_front", "App in front", "App im Vordergrund";
    Theme = "scene_theme", "Light or dark", "Hell oder dunkel";
    SendOnEnter = "scene_send_on_enter", "Enter sends", "Enter sendet";
    Mouse = "scene_mouse", "Mouse", "Maus";
    Import = "scene_import", "Import", "Importieren";
    // The privacy page's switch (1233, loquax-read-receipts.md, section 4).
    ReadReceipts = "read_receipts", "Send read receipts", "Lesebestätigungen senden";
    ReadReceiptsText = "read_receipts_text", "Others see when you have read their messages. Off, you do not see it from others either.", "Andere sehen, wann du ihre Nachrichten gelesen hast. Ausgeschaltet siehst du es auch bei anderen nicht.";
    // The Maps page (1201, 1290): its title, the storage line, and the
    // row at the list's foot that deletes every region, with its one
    // question, which says whether the autoload brings them back.
    SettingsMaps = "settings_maps", "Maps", "Karten";
    MapsUsed = "maps_used", "Storage used: %1$s", "Belegter Speicher: %1$s";
    DeleteAllRegions = "maps_delete_all", "Delete all downloaded maps (%1$s)", "Alle geladenen Karten löschen (%1$s)";
    DeleteAllRegionsAsk = "maps_delete_all_ask", "Delete all %1$d maps? Areas you visit will be downloaded again when you look at them.", "Alle %1$d Karten löschen? Gebiete, die du besuchst, werden wieder geladen, wenn du sie anschaust.";
    DeleteAllRegionsAskManual = "maps_delete_all_ask_manual", "Delete all %1$d maps? They come back only when you load an area again.", "Alle %1$d Karten löschen? Sie kommen erst wieder, wenn du ein Gebiet neu lädst.";
    DeleteAllRegionsYes = "maps_delete_all_yes", "Delete all", "Alle löschen";
    Cancel = "scene_cancel", "Cancel", "Abbrechen";
    // Photos (1220): the bubble's words, the paperclip's entry, the
    // setting under Settings › Chats with its one line.
    Photo = "scene_photo", "Photo, %1$s", "Foto, %1$s";
    AttachPhoto = "attach_photo", "Gallery", "Galerie";
    ViewPhoto = "scene_view_photo", "Show photo", "Foto zeigen";
    PhotoSize = "photo_size", "Photo size", "Fotogröße";
    PhotoSizeText = "photo_size_text", "Photos are made smaller before they go. Their place and camera data never go.", "Fotos werden vor dem Senden verkleinert. Ort und Kameradaten gehen nie mit.";
    PhotoSmall = "photo_small", "Small, for slow connections", "Klein, für langsame Verbindungen";
    PhotoPhone = "photo_phone", "Phone (recommended)", "Handy (empfohlen)";
    PhotoOriginal = "photo_original", "Original size", "Originalgröße";
    PhotoUnreadable = "photo_unreadable", "This photo could not be shown.", "Dieses Foto ließ sich nicht zeigen.";
    // Files (1221): the bubble's row, the paperclip's entry, the cap's
    // refusal with both sizes, and what the phone says when it cannot.
    File = "scene_file", "File %1$s, %2$s", "Datei %1$s, %2$s";
    AttachFile = "attach_file", "File", "Datei";
    OpenFile = "scene_open_file", "Open file", "Datei öffnen";
    FileTooLarge = "file_too_large", "This file is %1$s. A message carries at most %2$s.", "Diese Datei hat %1$s. Eine Nachricht trägt höchstens %2$s.";
    FileUnreadable = "file_unreadable", "This file could not be read.", "Diese Datei ließ sich nicht lesen.";
    FileNoApp = "file_no_app", "No app on this device opens this file.", "Keine App auf diesem Gerät kann diese Datei öffnen.";
    // A photo or a file under way, in its bubble (1662).
    TransferSending = "transfer_sending", "sending · %1$d %%", "wird gesendet · %1$d %%";
    TransferFetching = "transfer_fetching", "fetching · %1$s of %2$s", "wird geholt · %1$s von %2$s";
    Megabytes = "megabytes", "%1$d.%2$d MB", "%1$d,%2$d MB";
    // Voice messages (1222): the bubble's words, its buttons, the
    // microphone's hint, the slide that cancels, the refusals, and what
    // the phone says without the microphone.
    Voice = "scene_voice", "Voice message, %1$s", "Sprachnachricht, %1$s";
    VoiceUnplayable = "scene_voice_unplayable", "Voice message, %1$s, which cannot be played here", "Sprachnachricht, %1$s, hier nicht abspielbar";
    PlayVoice = "scene_play_voice", "Play", "Abspielen";
    PauseVoice = "scene_pause_voice", "Pause", "Pause";
    RecordVoice = "record_voice", "Hold to record a voice message", "Halten für eine Sprachnachricht";
    VoiceHold = "voice_hold", "Hold the microphone to record, release to send.", "Mikrofon halten zum Aufnehmen, loslassen zum Senden.";
    VoiceCancel = "voice_cancel", "Slide left to cancel", "Zum Abbrechen nach links wischen";
    VoiceTooLong = "voice_too_long", "A voice message lasts at most %1$s.", "Eine Sprachnachricht dauert höchstens %1$s.";
    VoiceNoMicrophone = "voice_no_microphone", "Loquax needs the microphone to record a voice message.", "Loquax braucht das Mikrofon für Sprachnachrichten.";
    // The recording bar (1276): the timer, the waveform, the hints, the
    // lock chip, the buttons, the silence hint and the cap.
    RecBar = "rec_bar", "Recording a voice message", "Sprachnachricht wird aufgenommen";
    RecTimer = "rec_timer", "REC %1$s", "REC %1$s";
    RecWave = "rec_wave", "Sound level", "Lautstärke";
    RecHeld = "rec_held", "Release to send", "Loslassen zum Senden";
    RecReleaseCancel = "rec_release_cancel", "Release to cancel", "Loslassen zum Abbrechen";
    RecLock = "rec_lock", "Lock the recording", "Aufnahme feststellen";
    RecLocked = "rec_locked", "Recording locked", "Aufnahme festgestellt";
    RecPause = "rec_pause", "Pause recording", "Aufnahme pausieren";
    RecResume = "rec_resume", "Continue recording", "Aufnahme fortsetzen";
    RecPlay = "rec_play", "Listen", "Anhören";
    RecStopPlay = "rec_stop_play", "Stop listening", "Anhören beenden";
    RecSend = "rec_send", "Send voice message", "Sprachnachricht senden";
    RecDiscard = "rec_discard", "Discard recording", "Aufnahme verwerfen";
    RecSilent = "rec_silent", "Nothing heard. Is the microphone allowed?", "Nichts zu hören. Ist das Mikrofon erlaubt?";
    RecAllow = "rec_allow", "Allow the microphone", "Mikrofon erlauben";
    RecCap = "rec_cap", "That is as long as a voice message can be", "Länger kann eine Sprachnachricht nicht sein";
    // The compose bar after Signal (1270): the plus and its sheet, the
    // position button and its sheet, the camera and the staged photo.
    Attach = "scene_attach", "Attach", "Anhängen";
    Locate = "scene_locate", "My position", "Meine Position";
    SendPositionTo = "scene_send_position_to", "Send position to %1$s", "Position an %1$s senden";
    Camera = "scene_camera", "Take a photo", "Foto aufnehmen";
    CameraNoApp = "camera_no_app", "There is no camera app on this device.", "Auf diesem Gerät gibt es keine Kamera-App.";
    Staged = "scene_staged", "Photo to send, %1$s", "Foto zum Senden, %1$s";
    Unstage = "scene_unstage", "Remove photo", "Foto entfernen";
    CaptionHint = "caption_hint", "Add a caption", "Bildunterschrift hinzufügen";
    ShareNow = "share_now", "Now, once", "Jetzt, einmal";
    // Sharing a position for a while (1273): the sheet's four rows, the
    // chat's chip and its Stop, the list's sign, Settings, Privacy.
    ShareFor15 = "share_15_min", "For 15 minutes", "Für 15 Minuten";
    ShareFor1h = "share_1_h", "For 1 hour", "Für 1 Stunde";
    ShareFor8h = "share_8_h", "For 8 hours", "Für 8 Stunden";
    ShareUntilStopped = "share_until_stopped", "Until I stop", "Bis ich es beende";
    SharingLeft = "sharing_left", "Sharing your position, %1$s left", "Deine Position wird geteilt, noch %1$s";
    SharingUntilStop = "sharing_until_stop", "Sharing your position until you stop", "Deine Position wird geteilt, bis du es beendest";
    ShareStop = "share_stop", "Stop", "Beenden";
    SharingSign = "sharing_sign", "Sharing your position", "Du teilst deine Position";
    PositionSharing = "position_sharing", "Position sharing", "Positionsfreigabe";
    ShareNotice = "share_notice", "Loquax shares your position", "Loquax teilt deine Position";
    NoShares = "no_shares", "Your position is not shared with anyone.", "Deine Position wird mit niemandem geteilt.";
    LocationOn = "location_on", "Turn location on", "Standort einschalten";
    // The position by hand on a desktop (1371).
    EnterPosition = "enter_position", "Type my position", "Meine Position eingeben";
    MapCentre = "map_centre", "The map's centre", "Die Kartenmitte";
    PositionHint = "position_hint", "52.5163, 13.3777", "52.5163, 13.3777";
    FixByHand = "fix_by_hand", "Set by hand", "Von Hand gesetzt";
    NoFix = "no_fix", "Loquax does not know where you are yet.", "Loquax weiß noch nicht, wo du bist.";
    FixAge = "fix_age", "Found %1$s", "Ermittelt %1$s";
    FixNow = "fix_now", "Found just now", "Gerade eben ermittelt";
    // A code read (1263, `crate::code`): what a scanner, the pencil and
    // the palette say for one they cannot take.
    CodeForeign = "code_foreign", "That is not a Loquax code.", "Das ist kein Loquax-Code.";
    CodeDamaged = "code_damaged", "This code is damaged. Ask for it again.", "Dieser Code ist beschädigt. Lass ihn dir noch einmal geben.";
    // A code read from a picture (1341, 1377): the chooser's title and
    // what a picture without one says, without the file's path.
    CodeFromPicture = "code_from_picture", "Read a code from a picture", "Code aus einem Bild lesen";
    CodeNotInPicture = "code_not_in_picture", "There is no code in this picture.", "In diesem Bild ist kein Code.";
    CodeIsMailbox = "code_is_mailbox", "That code is for a mailbox, not for a contact.", "Dieser Code ist für ein Postfach, nicht für einen Kontakt.";
    // The pencil's screen (1263): scan first, paste second, typing under
    // Advanced.
    NewScan = "new_scan", "Scan a code", "Code scannen";
    NewPaste = "new_paste", "Paste a code", "Code einfügen";
    NewHint = "new_hint", "Ask your contact for their code: in Loquax they tap their own picture at the top.", "Lass dir den Code zeigen: In Loquax tippt dein Kontakt oben auf sein eigenes Bild.";
    NewTyped = "new_typed", "Or type the address your contact gave you.", "Oder tippe die Adresse ein, die dein Kontakt dir gegeben hat.";
    // 1449 item 8: the terminal's words that still said "friend".
    AskForwardTo = "ask_forward_to", "Forward to: a contact's name", "Weiterleiten an: Name eines Kontakts";
    AskContactCode = "ask_contact_code", "Your contact's code or address", "Code oder Adresse deines Kontakts";
    AskGroupTick = "ask_group_tick", "Tick contacts: type a name, Enter ticks or unticks it", "Kontakte ankreuzen: Namen tippen, Enter kreuzt an oder ab";
    NoContactCalled = "no_contact_called", "No contact called %1$s", "Kein Kontakt namens %1$s";
    NoConversationsTyped = "no_conversations_typed", "No conversations yet: :new conversation starts one with a contact's code.", "Noch keine Unterhaltungen: :new conversation beginnt eine mit dem Code eines Kontakts.";
    AppConnecting = "app_connecting", "Loquax (connecting)", "Loquax (verbindet)";
    NewNothing = "new_nothing", "There is nothing to paste. Copy your contact's code first.", "Es gibt nichts zum Einfügen. Kopiere zuerst den Code deines Kontakts.";
    // Joining a mailbox (1263, finding 8): one sentence each, the
    // daemon's words under Details.
    JoinNoAnswer = "join_no_answer", "The mailbox did not answer. Is the computer on?", "Das Postfach hat nicht geantwortet. Ist der Computer an?";
    JoinSlow = "join_slow", "The mailbox did not answer in time. Try again.", "Das Postfach hat nicht rechtzeitig geantwortet. Versuch es noch einmal.";
    JoinNotMailbox = "join_not_mailbox", "That code is not a mailbox code.", "Das ist kein Postfach-Code.";
    JoinRefused = "join_refused", "The mailbox did not take this code. Make a new one on the computer.", "Das Postfach hat den Code nicht angenommen. Mach am Computer einen neuen.";
    JoinNoCode = "join_no_code", "Scan or paste the code your mailbox shows.", "Scanne den Code, den dein Postfach zeigt, oder füge ihn ein.";
    JoinWhere = "join_where", "Loquax does not know where your mailbox is. Type where it is below.", "Loquax weiß nicht, wo dein Postfach ist. Gib es unten ein.";
    JoinBadWhere = "join_bad_where", "Loquax cannot read where the mailbox is. It looks like 192.168.1.20:4242.", "Loquax kann nicht lesen, wo das Postfach ist. Das sieht so aus: 192.168.1.20:4242.";
    JoinFailed = "join_failed", "Joining the mailbox did not work.", "Das Postfach ließ sich nicht verbinden.";
    JoinOtherMailbox = "join_other_mailbox", "This phone already reads another mailbox.", "Dieses Handy liest schon ein anderes Postfach.";
    // A device of a home that has no window to say more (1415, the M9's
    // top bar): joining with its code, the archive coming in, and the
    // home out of reach. The pull counts entries of the archive, which
    // are messages and what became of them.
    HomeJoining = "home_joining", "Joining your mailbox", "Verbinde mit deinem Postfach";
    HomeFetched = "home_fetched", "Fetched %1$d of %2$d", "%1$d von %2$d abgeholt";
    HomeFetchedSome = "home_fetched_some", "Fetched %1$d", "%1$d abgeholt";
    HomeUnreachable = "home_unreachable", "Mailbox unreachable", "Postfach nicht erreichbar";
    // Linking a device (1264): the phone holds no mailbox to link to.
    LinkOnComputer = "link_on_computer", "Your mailbox is on a computer. Link the other device there.", "Dein Postfach ist auf einem Computer. Verbinde das andere Gerät dort.";
    // Moving to a new phone (1281, walk finding 24).
    MoveOnComputer = "move_on_computer", "Your mailbox is on a computer. Your new phone joins it there.", "Dein Postfach ist auf einem Computer. Dein neues Handy verbindest du dort.";
    MoveFailed = "move_failed", "The move did not work. Your old phone still has everything.", "Der Umzug hat nicht geklappt. Dein altes Handy hat noch alles.";
    MoveHasAddress = "move_has_address", "This phone already has an address. Only a new phone can take one over.", "Dieses Handy hat schon eine Adresse. Nur ein neues Handy kann eine übernehmen.";
    MovedAway = "moved_away", "This phone has moved. Your mailbox is on your new phone now, and this phone sends nothing.", "Dieses Handy ist umgezogen. Dein Postfach ist jetzt auf deinem neuen Handy, und dieses Handy sendet nichts mehr.";
    RestoreLive = "restore_live", "This address is in use on another device right now. If you restore it here too, both devices lose messages. Move from the old phone instead, or stop Loquax there first.", "Diese Adresse ist gerade auf einem anderen Gerät in Betrieb. Stellst du sie hier auch wieder her, verlieren beide Geräte Nachrichten. Zieh lieber vom alten Handy um, oder beende Loquax dort zuerst.";
    Details = "details", "Details", "Details";
    ZoomIn = "scene_zoom_in", "Zoom in", "Vergrößern";
    ZoomOut = "scene_zoom_out", "Zoom out", "Verkleinern";
    Pan = "scene_pan", "Move the map", "Karte verschieben";
    Fit = "scene_fit", "Show every pin", "Alle Stecknadeln zeigen";
    StepPin = "scene_step_pin", "Next pin", "Nächste Stecknadel";
    SelectPin = "scene_select_pin", "Select pin", "Stecknadel wählen";
    MapDrawing = "scene_map_drawing", "Map drawing", "Kartendarstellung";
    GlyphTest = "scene_glyph_test", "Glyph test", "Zeichentest";
    // 1385, 1419: the tracks' time window, an entry of the map's menu and
    // the single choice it opens; the default is the current movement.
    TrackWindow = "scene_track_window", "Time window", "Zeitraum";
    TracksNow = "scene_tracks_now", "Current movement", "Aktuelle Bewegung";
    TracksToday = "scene_tracks_today", "Today", "Heute";
    TracksYesterday = "scene_tracks_yesterday", "Yesterday", "Gestern";
    TracksWeek = "scene_tracks_week", "This week", "Diese Woche";
    TracksMonth = "scene_tracks_month", "This month", "Dieser Monat";
    TracksAll = "scene_tracks_all", "All", "Alles";
    // 1386: every peer with a position inside the time window in view.
    FitWindow = "scene_fit_window", "Fit all", "Alle zeigen";
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
    // The list's time (1449 item 4, walk 1 finding 10): one format on every
    // target, "17:37" today, "Gestern", the weekday short within the week,
    // else the date; never "17 Min." or "7h".
    // The month is its short word, "Oct" in English and "10." in German.
    ListDate = "scene_list_date", "%1$d %2$s", "%1$d.%2$s";
    ListDateYear = "scene_list_date_year", "%1$d %2$s %3$d", "%1$d.%2$s%3$d";
    JanuaryShort = "scene_january_short", "Jan", "1.";
    FebruaryShort = "scene_february_short", "Feb", "2.";
    MarchShort = "scene_march_short", "Mar", "3.";
    AprilShort = "scene_april_short", "Apr", "4.";
    MayShort = "scene_may_short", "May", "5.";
    JuneShort = "scene_june_short", "Jun", "6.";
    JulyShort = "scene_july_short", "Jul", "7.";
    AugustShort = "scene_august_short", "Aug", "8.";
    SeptemberShort = "scene_september_short", "Sep", "9.";
    OctoberShort = "scene_october_short", "Oct", "10.";
    NovemberShort = "scene_november_short", "Nov", "11.";
    DecemberShort = "scene_december_short", "Dec", "12.";
    MondayShort = "scene_monday_short", "Mon", "Mo.";
    TuesdayShort = "scene_tuesday_short", "Tue", "Di.";
    WednesdayShort = "scene_wednesday_short", "Wed", "Mi.";
    ThursdayShort = "scene_thursday_short", "Thu", "Do.";
    FridayShort = "scene_friday_short", "Fri", "Fr.";
    SaturdayShort = "scene_saturday_short", "Sat", "Sa.";
    SundayShort = "scene_sunday_short", "Sun", "So.";
    // A start with no daemon to attach to (1265, walk finding 5): one
    // sentence and the one command, the stack's detail behind --verbose.
    NeedsService = "needs_service", "Loquax needs its network service. Start it with: %1$s", "Loquax braucht seinen Netzwerkdienst. Starte ihn mit: %1$s";
    // The M9's error states (1493): a failed write is a word on the
    // screen, never only a log line.
    M9NotSaved = "m9_not_saved", "Not saved: memory error", "Nicht gespeichert: Speicherfehler";
    M9JoinNotSaved = "m9_join_not_saved", "Connected, but not saved: pair again after a restart", "Verbunden, aber nicht gespeichert: nach einem Neustart neu koppeln";
    M9NoIdentity = "m9_no_identity", "Memory error: this device cannot read its identity and cannot start. Restart it; if this stays, it needs service.", "Speicherfehler: Dieses Gerät kann seine Identität nicht lesen und nicht starten. Starte es neu; bleibt das so, braucht es Service.";
    // The M9's overview behind the house key and its type switch (1532).
    M9Overview = "m9_overview", "Overview", "Übersicht";
    M9Type = "m9_type", "Text", "Schrift";
    M9TypeSmall = "m9_type_small", "small", "klein";
    M9TypeLarge = "m9_type_large", "large", "groß";
    M9TypeNowSmall = "m9_type_now_small", "Text: small", "Schrift: klein";
    M9TypeNowLarge = "m9_type_now_large", "Text: large", "Schrift: groß";
    // The M9's top bar (1564): the page's name and at most one short word
    // of state that stands on its own, never a sentence cut by the bar.
    M9BarConnected = "m9_bar_connected", "Connected", "Verbunden";
    M9BarDisconnected = "m9_bar_disconnected", "Disconnected", "Getrennt";
    M9BarConnecting = "m9_bar_connecting", "Connecting…", "Verbindet…";
    M9BarFetching = "m9_bar_fetching", "Fetching…", "Holt ab…";
    M9BarRefused = "m9_bar_refused", "Refused", "Abgelehnt";
    M9BarNotSaved = "m9_bar_not_saved", "Not saved", "Nicht gespeichert";
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

    /// A reader never reads the protocol's name (1577): "public rooms",
    /// never RRC or Relay Chat, in either language.
    #[test]
    fn no_string_names_the_protocol() {
        for (_, key, en, de) in CATALOGUE {
            for text in [en, de] {
                let lower = text.to_lowercase();
                assert!(
                    !lower.contains("rrc")
                        && !lower.contains("relay chat")
                        && !lower.contains("rooms_hub"),
                    "{key}: {text}"
                );
                // 1618: a WhatsApp reader knows neither word; the UI says
                // "server" and how far, never "hub" or "hops".
                assert!(
                    !plain_words(text).iter().any(|w| forbidden_word(w)),
                    "{key}: {text}"
                );
            }
        }
        // The control: the check reads the catalogue's words.
        assert!(CATALOGUE
            .iter()
            .any(|(_, _, en, _)| en.to_lowercase().contains("public rooms")));
        // The control of 1618's words: the check catches them in both
        // languages, and spares a word that only holds the letters.
        assert!(plain_words("Hubs in deinem Netz")
            .iter()
            .any(|w| forbidden_word(w)));
        assert!(plain_words("1 Hop entfernt")
            .iter()
            .any(|w| forbidden_word(w)));
        assert!(plain_words("%1$d hops away")
            .iter()
            .any(|w| forbidden_word(w)));
        assert!(!plain_words("Schublade, Shop, Hopfen")
            .iter()
            .any(|w| forbidden_word(w)));
    }

    /// The words of `text`, lower case, split at whatever is no letter.
    fn plain_words(text: &str) -> Vec<String> {
        text.split(|c: char| !c.is_alphabetic())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect()
    }

    /// Lew 2026-11-06: nobody on the screen is a "friend"; a contact is a
    /// contact, in both languages (1449 item 8).
    #[test]
    fn no_string_calls_anyone_a_friend() {
        let friendly = |text: &str| {
            plain_words(text)
                .iter()
                .any(|w| w.starts_with("friend") || w.starts_with("freund"))
        };
        for (_, key, en, de) in CATALOGUE {
            assert!(!friendly(en) && !friendly(de), "{key}: {en} / {de}");
        }
        // The control: the check catches the word in either language.
        assert!(friendly("Copy your friend's code") && friendly("Freunde auf der Karte"));
        assert!(!friendly("your contact's code"));
    }

    /// "hub", "hubs", "hop", "hops" (1618).
    fn forbidden_word(word: &str) -> bool {
        matches!(word, "hub" | "hubs" | "hop" | "hops")
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

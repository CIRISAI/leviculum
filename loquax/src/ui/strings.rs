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
    // The Rooms (1379): Reticulum Relay Chat on a hub, live, beside the groups.
    TabRooms = "tab_rooms", "Rooms", "Räume";
    RoomsLive = "rooms_live", "A room is live: no mailbox keeps it, there are no receipts and no ticks, and what you missed is gone.", "Ein Raum ist live: kein Postfach hebt ihn auf, es gibt keine Lesebestätigungen und keine Haken, und was du verpasst hast, ist weg.";
    RoomsNoHub = "rooms_no_hub", "No hub yet. rooms_hub in the config names one.", "Noch kein Hub. rooms_hub in der Konfiguration nennt einen.";
    RoomsLinking = "rooms_linking", "Connecting to the hub…", "Verbinde mit dem Hub…";
    RoomsHub = "rooms_hub", "Hub %1$s", "Hub %1$s";
    RoomsNoPath = "rooms_no_path", "The hub cannot be reached. Is it announced on this network?", "Der Hub ist nicht erreichbar. Wird er in diesem Netz angekündigt?";
    RoomsNoAnswer = "rooms_no_answer", "The hub did not answer. Try again later.", "Der Hub hat nicht geantwortet. Versuch es später noch einmal.";
    RoomsLost = "rooms_lost", "The connection to the hub broke. Open the Rooms again to reconnect.", "Die Verbindung zum Hub ist abgerissen. Öffne die Räume erneut, um neu zu verbinden.";
    RoomsLeft = "rooms_left", "You left the hub.", "Du hast den Hub verlassen.";
    RoomsOff = "rooms_off", "There are no Rooms here; rooms_hub in the config turns them on.", "Hier gibt es keine Räume; rooms_hub in der Konfiguration schaltet sie ein.";
    RoomsHowToJoin = "rooms_how_to_join", "Type a room's name, like #lab, to join it.", "Tipp den Namen eines Raums, etwa #lab, um ihm beizutreten.";
    RoomsHintRoom = "rooms_hint_room", "Enter sends to the room. /me for an action, /part to leave.", "Enter schickt in den Raum. /me für eine Aktion, /part zum Verlassen.";
    RoomsNotReady = "rooms_not_ready", "The hub has not answered yet. Wait a moment and try again.", "Der Hub hat noch nicht geantwortet. Warte kurz und versuch es noch einmal.";
    RoomsNotIn = "rooms_not_in", "You are not in %1$s. Join it first.", "Du bist nicht in %1$s. Tritt ihm zuerst bei.";
    RoomsTooLong = "rooms_too_long", "This line is %1$d bytes long and a room takes at most %2$d. Shorten it.", "Diese Zeile ist %1$d Bytes lang, ein Raum nimmt höchstens %2$d. Kürze sie.";
    RoomsTooBig = "rooms_too_big", "This line needs %1$d bytes and the link carries %2$d. Shorten it.", "Diese Zeile braucht %1$d Bytes, die Verbindung trägt %2$d. Kürze sie.";
    RoomsBadRoom = "rooms_bad_room", "A room name needs a few letters and no spaces.", "Ein Raumname braucht ein paar Buchstaben und keine Leerzeichen.";
    RoomsTooMany = "rooms_too_many", "You are in %1$d rooms, as many as this hub allows. Leave one first.", "Du bist in %1$d Räumen, so vielen, wie dieser Hub erlaubt. Verlass zuerst einen.";
    RoomsYou = "rooms_you", "you", "du";
    RoomsCameIn = "rooms_came_in", "%1$s came in", "%1$s ist da";
    // Our own join (1384): "du ist da" was the nick put into the line above.
    RoomsWeCameIn = "rooms_we_came_in", "you came in", "du bist da";
    RoomsWentOut = "rooms_went_out", "%1$s left", "%1$s ist gegangen";
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
    Mute = "mute", "Mute", "Stummschalten";
    Unmute = "unmute", "Unmute", "Stummschaltung aufheben";
    Archive = "archive", "Archive", "Archivieren";
    Unarchive = "unarchive", "Unarchive", "Aus dem Archiv holen";
    MarkRead = "mark_read", "Mark as read", "Als gelesen markieren";
    MarkUnread = "mark_unread", "Mark as unread", "Als ungelesen markieren";
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
    MapLegend = "map_legend", "* person  @ chosen  grey: old", "* Person  @ gewählt  grau: alt";
    MapKeys = "map_keys", "arrows pan  +- zoom", "Pfeile schieben  +- Zoom";
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
    GroupRemoveAsk = "group_remove_ask", "Remove %1$s from %2$s?", "%1$s aus %2$s entfernen?";
    GroupYes = "group_yes", "Yes", "Ja";
    GroupNo = "group_no", "No", "Nein";
    GroupYou = "group_you", "You", "Du";
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
    CodeIsMailbox = "code_is_mailbox", "That code is for a mailbox, not for a friend.", "Dieser Code ist für ein Postfach, nicht für einen Kontakt.";
    // The pencil's screen (1263): scan first, paste second, typing under
    // Advanced.
    NewScan = "new_scan", "Scan a code", "Code scannen";
    NewPaste = "new_paste", "Paste a code", "Code einfügen";
    NewHint = "new_hint", "Your friend's code is on their profile, behind their picture.", "Den Code deines Kontakts findest du in seinem Profil, hinter dem Bild.";
    NewTyped = "new_typed", "Or type the address your friend gave you.", "Oder tippe die Adresse ein, die dein Kontakt dir gegeben hat.";
    NewNothing = "new_nothing", "There is nothing to paste. Copy your friend's code first.", "Es gibt nichts zum Einfügen. Kopiere zuerst den Code deines Kontakts.";
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
    // A start with no daemon to attach to (1265, walk finding 5): one
    // sentence and the one command, the stack's detail behind --verbose.
    NeedsService = "needs_service", "Loquax needs its network service. Start it with: %1$s", "Loquax braucht seinen Netzwerkdienst. Starte ihn mit: %1$s";
    // The M9's error states (1493): a failed write is a word on the
    // screen, never only a log line.
    M9NotSaved = "m9_not_saved", "Not saved: memory error", "Nicht gespeichert: Speicherfehler";
    M9JoinNotSaved = "m9_join_not_saved", "Connected, but not saved: pair again after a restart", "Verbunden, aber nicht gespeichert: nach einem Neustart neu koppeln";
    M9NoIdentity = "m9_no_identity", "Memory error: this device cannot read its identity and cannot start. Restart it; if this stays, it needs service.", "Speicherfehler: Dieses Gerät kann seine Identität nicht lesen und nicht starten. Starte es neu; bleibt das so, braucht es Service.";
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

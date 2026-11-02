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
    // A node the mesh shows on the map that is no contact (1302).
    NearbyCard = "nearby_card", "Nearby, not in your contacts", "In der Nähe, nicht in deinen Kontakten";
    NearbyAdd = "nearby_add", "Add", "Hinzufügen";
    NearbyEnterAdds = "nearby_enter_adds", "Enter adds", "Enter fügt hinzu";
    // A row's last line while a message is typed there and not sent (1219).
    DraftRow = "draft_row", "Draft: %1$s", "Entwurf: %1$s";
    Reacted = "reacted", "reacted %1$s", "hat mit %1$s reagiert";
    YouReacted = "you_reacted", "You reacted %1$s", "Du hast mit %1$s reagiert";
    RunUpdates = "run_updates", "%1$d updates since %2$s", "%1$d Meldungen seit %2$s";
    QuoteYou = "quote_you", "You", "Du";
    // The chat: compose, the marks, a bubble's menu.
    ComposeHint = "compose_hint", "Message", "Nachricht";
    Send = "send", "Send", "Senden";
    // The ladder's words for a screen reader (1232,
    // loquax-read-receipts.md, section 6): the clock twice, one tick, two,
    // two in the accent, the red circle.
    StateSending = "state_sending", "Sending", "Wird gesendet";
    StateUnconfirmed = "state_unconfirmed", "Sent, not confirmed yet", "Gesendet, noch nicht bestätigt";
    StateMailbox = "state_mailbox", "Waiting in the mailbox", "Wartet im Postfach";
    StateDelivered = "state_delivered", "Delivered", "Zugestellt";
    StateRead = "state_read", "Read at %1$s", "Gelesen um %1$s";
    StateFailed = "state_failed", "Not sent. Double tap to send again", "Nicht gesendet. Doppeltippen, um es erneut zu senden";
    SendAgain = "scene_send_again", "Send again", "Erneut senden";
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
    // Group chats (1307, the study's sections 3.0 and 4).
    NewGroup = "group_new", "New group", "Neue Gruppe";
    GroupNext = "group_next", "Next", "Weiter";
    GroupCreate = "group_create", "Create", "Erstellen";
    GroupNameHint = "group_name_hint", "Group name", "Name der Gruppe";
    GroupPickTitle = "group_pick_title", "Add members", "Mitglieder hinzufügen";
    GroupPicked = "group_picked", "%1$d selected", "%1$d ausgewählt";
    GroupTrustCreate = "group_trust_create", "%1$s is kept on your mailbox. Messages in the group can be read there.", "%1$s liegt auf deinem Postfach. Dort sind die Nachrichten der Gruppe lesbar.";
    GroupTrustJoin = "group_trust_join", "%1$s is kept on %2$s's mailbox. %2$s could read the group's messages.", "%1$s liegt auf dem Postfach von %2$s. %2$s könnte die Nachrichten der Gruppe lesen.";
    GroupInvited = "group_invited", "%1$s invited you to %2$s", "%1$s hat dich zu %2$s eingeladen";
    GroupYouInvited = "group_you_invited", "You invited them to %1$s", "Du hast eingeladen zu %1$s";
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
    GroupHomeTooOld = "group_home_too_old", "Your home cannot host groups yet. Update it.", "Dein Postfach kann noch keine Gruppen führen. Aktualisiere es.";
    GroupYouLeft = "group_you_left", "You left %1$s", "Du hast %1$s verlassen";
    GroupDelete = "group_delete", "Delete group", "Gruppe löschen";
    GroupDeleteAsk = "group_delete_ask", "Delete %1$s for everyone?", "%1$s für alle löschen?";
    GroupRemoveFromList = "group_remove_from_list", "Remove from list", "Aus der Liste entfernen";
    GroupAgain = "group_again", "Start again with the same people", "Mit denselben Leuten neu starten";
    GroupEnded = "group_ended", "%1$s ended the group", "%1$s hat die Gruppe beendet";
    GroupArrivedLate = "group_arrived_late", "arrived late", "kam verspätet";
    MyCode = "scene_my_code", "My code", "Mein Code";
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
    FileNoApp = "file_no_app", "No app here opens this file.", "Keine App hier öffnet diese Datei.";
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
    CameraNoApp = "camera_no_app", "No camera app here takes a photo.", "Keine Kamera-App hier nimmt ein Foto auf.";
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
    NoFix = "no_fix", "Loquax does not know where you are yet.", "Loquax weiß noch nicht, wo du bist.";
    FixAge = "fix_age", "Found %1$s", "Ermittelt %1$s";
    FixNow = "fix_now", "Found just now", "Gerade eben ermittelt";
    // A code read (1263, `crate::code`): what a scanner, the pencil and
    // the palette say for one they cannot take.
    CodeForeign = "code_foreign", "That is not a Loquax code.", "Das ist kein Loquax-Code.";
    CodeDamaged = "code_damaged", "This code is damaged. Ask for it again.", "Dieser Code ist beschädigt. Lass ihn dir noch einmal geben.";
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
    // Linking a device (1264): the phone holds no mailbox to link to.
    LinkOnComputer = "link_on_computer", "Your mailbox is on a computer. Link the other device there.", "Dein Postfach ist auf einem Computer. Verbinde das andere Gerät dort.";
    // Moving to a new phone (1281, walk finding 24).
    MoveOnComputer = "move_on_computer", "Your mailbox is on a computer. Your new phone joins it there.", "Dein Postfach ist auf einem Computer. Dein neues Handy verbindest du dort.";
    MoveFailed = "move_failed", "The move did not work. Your old phone still has everything.", "Der Umzug hat nicht geklappt. Dein altes Handy hat noch alles.";
    MoveHasAddress = "move_has_address", "This phone already has an address. Only a new phone can take one over.", "Dieses Handy hat schon eine Adresse. Nur ein neues Handy kann eine übernehmen.";
    MovedAway = "moved_away", "This phone has moved. Your mailbox is on your new phone now, and this phone sends nothing.", "Dieses Handy ist umgezogen. Dein Postfach ist jetzt auf deinem neuen Handy, und dieses Handy sendet nichts mehr.";
    RestoreLive = "restore_live", "This address is in use on another device right now. Restored here too, both lose messages. Move from the old phone instead, or stop Loquax there first.", "Diese Adresse ist gerade auf einem anderen Gerät in Betrieb. Hier auch wiederhergestellt, verlieren beide Nachrichten. Zieh lieber vom alten Handy um, oder beende Loquax dort zuerst.";
    Details = "details", "Details", "Details";
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
    // A start with no daemon to attach to (1265, walk finding 5): one
    // sentence and the one command, the stack's detail behind --verbose.
    NeedsService = "needs_service", "Loquax needs its network service. Start it with: %1$s", "Loquax braucht seinen Netzwerkdienst. Starte ihn mit: %1$s";
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

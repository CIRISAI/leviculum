# The ninety percent walk, round two

Order 1377, Lew's rule of 2026-10-31 (a user who knows nothing of
Reticulum; defaults decide). Round one was 1259
(`ninety-percent-walk-2026-11.md`, 31 findings, all fixed since). This
round walks what landed after it: groups (create, invite, join, members,
add, delete, status, read-by, digest, note lines), read receipts and their
ladder, the English wire texts with localised cards, the swipeable notice
and its settings row, the position by hand, a code read from a picture,
the jackdaw icon, the window as the default, Browse off, the leaf-mode
mailbox and the home setup with `--phone-port`.

## 1. How it was walked

- **No Waydroid, no adb** on schneckenschreck (`which waydroid adb`: none),
  so, as in round one, the phone was read in the source: the core's
  catalogue (`loquax/src/ui/strings.rs`), `strings.xml` in both languages,
  the activities that put them on screen.
- **The window's goldens were the eyes** for the list and the chat
  (`loquax/tests/golden/gui-*.png`); every golden that moved in this pass
  was looked at before it was taken.
- **The terminal** was read in its row and detail code
  (`loquax/src/ui/layout.rs`, `loquax/src/ui.rs` `detail_view`).
- Every finding names what a first user sees and in which words.

## 2. Findings

Fixed means in this pass, with the commit; order means it needs its own
pass, with the sentence a user would say.

| # | Screen | What a first user sees | Fixed / order |
|---|---|---|---|
| 1 | Chat list and the phone's message notification, every target | On a German phone a row whose last message has no words says "photo", "voice message 0:02", "position shared", "you reacted 👍" in English; the notification repeats it | fixed, `4416b064` |
| 2 | Chat list, the inviter's row | "Anna invited you to Verein: lxma://…", the wire's English with the group's code, under a German list too | fixed, `4416b064`: the row says the card's title, "Anna lädt dich zu Verein ein" |
| 3 | Window, a voice bubble | "♪ voice message · 0:07", English on every locale | fixed, `4416b064`: the scene's "Sprachnachricht, 0:07" |
| 4 | Phone, German, every screen | "Telefon" on the phone's own screens, "Handy" in the core's (one screen could say both) | fixed, `dc2f4cb2`, and the German help page |
| 5 | Phone, joining a mailbox, the address field | "Die Adresse deines Postfachs: Host:Port seines lnsd-TCP-Servers" | fixed, `dc2f4cb2`: "Wo dein Postfach ist: Adresse und Port des Computers" |
| 6 | New group on an old mailbox | "Your home cannot host groups yet. Update it." ("home" is our word, not theirs) | fixed, `dc2f4cb2`: "Your mailbox cannot hold groups yet. Update Loquax there." |
| 7 | Group invitation, the sender's card (German) | "Du hast eingeladen zu Familie" | fixed, `dc2f4cb2`: "Du hast zu Familie eingeladen" |
| 8 | Group invitation, the trust line (German) | "liegt auf deinem Postfach", "auf dem Postfach von Anna" | fixed, `dc2f4cb2`: "in deinem", "im Postfach von" |
| 9 | Row menu (German) | "Stumm schalten", "Ton an", "Gelesen" and "Ungelesen" as verbs | fixed, `dc2f4cb2`: "Stummschalten", "Stummschaltung aufheben", "Als gelesen markieren" |
| 10 | Restore while the address runs elsewhere | "Restored here too, both lose messages." / "Hier auch wiederhergestellt, verlieren beide Nachrichten." | fixed, `dc2f4cb2`: "If you restore it here too, both devices lose messages." |
| 11 | Move to a new phone | "this phone stops then", "dieses Handy hört dann auf", "hat aufgehört" | fixed, `dc2f4cb2`: "After that, this phone sends nothing." |
| 12 | A group's digest line (German) | "hat ein Foto geschickt (zu groß für Funk)" | fixed, `dc2f4cb2`: "(zu groß für die Funkstrecke)"; the English wire text is unchanged |
| 13 | No camera app, no app for a file | "No camera app here takes a photo." / "Keine Kamera-App hier nimmt ein Foto auf." | fixed, `dc2f4cb2`: "There is no camera app on this device." |
| 14 | Window and terminal, a code read from a picture without one | "/home/anna/Bilder/x.png: no QR code in this picture", English and a path | fixed, `dc2f4cb2`: "In diesem Bild ist kein Code." |
| 15 | Help page | "Sending photos, files and voice messages: Not yet." All three landed | fixed, `84f589ef` |
| 16 | Help page | "tap the paper clip"; "Sharing your position for a while ... is not in the app yet." Both changed | fixed, `84f589ef` |
| 17 | Help page | No word on groups at all | fixed, `84f589ef`: create, the trust, members, other apps |
| 18 | Help page | A second device only "when the mailbox runs on a computer"; Link a device and the move are missing | fixed, `84f589ef` |
| 19 | Phone, long press, Info | The terminal's detail in monospace English: "peer 3a3a…", "id none yet: the router has not queued it", "size 412 bytes", "signature verified", and in a group "group  delivered to 2 of 3", "read   by 1 of 2" | **order**: "I want to see who in the group read my message, and I get hex lines in English." A localised Info sheet (delivered and read, per member in a group), the technical lines behind Details |
| 20 | Phone, a group message's history | The note "the group: delivered to 2 of 3, read by 1 of 2" (`loquax/src/ui/groups.rs`, `learn_status`) is English in the detail | **order**, with 19 |
| 21 | Terminal, the inviter's row | Still the wire's English with the code: the terminal's row is drawn by its own preview (`loquax/src/ui/layout.rs`, the list row), not the shared one | **order**: "the terminal shows me a long code where it should say who invited me." Move the terminal row onto `chat::list_row` |
| 22 | Terminal on a German desktop | Everything in English: the terminal never sets the model's language, the window does (`loquax/src/gui.rs`, `locale()`) | **order**: "on my German laptop the window speaks German and the terminal does not." |
| 23 | `loquax mailbox setup` in the account store | "restart the Reticulum daemon (lnsd or rnsd) to open it" | kept: the account store is the non-default answer of a computer setup; the default system store restarts lnsd itself |
| 24 | Profile, Advanced | "Identity hash", "Home node", "Rendezvous" | kept: collapsed under Advanced, which says it is the part that needs Reticulum knowledge |

What is good:

- **New group: 4 taps plus one per member, plus the name.** That is
  WhatsApp's count: pencil, New group, the members, Next, the name,
  Create.
- **The trust line on every invitation** says in one sentence whose
  mailbox can read the group, with no Reticulum words.
- **Read receipts** are on by default, and the switch says both sides
  of the bargain in one line.
- **The ladder's words** a screen reader says ("Waiting in the mailbox",
  "Read at 09:20") are plain in both languages.

## 3. Positive controls

- **Row line, finding 2:** `ui::groups::tests::the_invitation_card_speaks_the_readers_language`
  now asserts the row equals the card's title. With the old line put back,
  the row is "Lew invited you to Familie: lxma://code Reply /leave to
  leave." and the test is red.
- **Row words, findings 1 and 3:** the window's goldens moved where a row
  or a voice bubble carries them (`gui-chat-*.png`, `gui-group-*.png`,
  `gui-recording-*.png`, `gui-position-*.png`, `gui-my-code-light.png`,
  `gui-attach-sheet-light.png`); each moved picture was looked at.
- **German words, findings 4, 5 and 11:**
  `loquax-android/tests/strings.rs`
  `the_german_app_says_handy_and_the_join_screen_names_no_daemon`.
  With the old German `strings.xml` it goes red on `battery_text`.

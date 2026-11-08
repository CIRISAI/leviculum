# Loquax on Android: the scripted device walk

Status: slice one, order 1175 (Lew, 2026-10-31; Refs #232). The script is
`loquax-android/scripts/device-walk.py`; the reviewer runs it on hamster
every night and reads its log.

## Why

The three device bugs of 2026-10-30/31 (an import the running session did
not see, names that did not come, Info that failed on an imported row)
were found by Lew's thumb, on a real phone, with a real store. The host
tests had small, fresh, well formed stores and passed. The ugly store of
order 1173 (`loquax/tests/support/ugly_store.rs`) brought the store's
shape into the host tests; the walk brings the screens. It installs the
app on an Android device the reviewer controls through `adb`, against a
mailbox on the same host that holds the ugly store, and does what the
thumb did, step by step, reading the screen after every step.

## What it proves

One walk, one APK, one device, one fresh mailbox. Each step prints one
line, `WALK step=<name> ok=<0|1> ms=<n> detail=<...>`, and the walk ends
with `WALK_SUMMARY ok=<n> failed=<n> ms=<total>` and exit 1 on any
failure.

| Step | What it asserts on screen |
|---|---|
| `fixture` | on the host: lnsd with a TCP server on `--server`, `loquax mailbox run` over the ugly store's archive (800 messages, 80 imported), an admission code for one device |
| `install` | the app data cleared, the APK installed with its run time permissions granted |
| `join_error_words` | before anything is joined (1263, finding 8 of the ninety percent walk): the onboarding's "On a computer at home", Continue, "Paste a code", a code that is no mailbox's (`lqxh1nonsense`) typed, Join; the join form says the one sentence "That code is not a mailbox code." with no daemon word (LXMF, hex digit, lnsd, TCP, host:port, `loquax home`, Reticulum), Retry `join_retry` and the fold `join_details_toggle` show, the fold is closed and a tap opens `join_details` with the parser's own words; the app is force-stopped, nothing joined |
| `first_start` | the onboarding's "Where should your mailbox be?", "On a computer at home", Continue, "Paste a code", the code typed in full, Join; the notification and battery questions dismissed if they show; the address page `welcome_qr` shows once after the join (1278, walk finding 13) and its Done leads to the conversation list; a list without that page fails |
| `list` | within 10 s of the list and without a restart, a named row shows; scrolling, every conversation that is neither archived nor blocked shows (18 of the store's 20), each named one by its name and never as the address form `<8 hex…>` |
| `notification_text` | the always-on notification (1278, walk finding 19): within 30 s the session's untagged record in `dumpsys notification --noredact` says "Connected to your mailbox", with no unread count (1345: the count is the message notifications', so a new message does not post the notice again), and never `--server` or its host |
| `open_imported` | the conversation imported from Ratspeak opens; scrolled to the top, its topmost body is the oldest imported one and a day separator shows |
| `info` | a long press on an imported message, Info: the "Message details" sheet shows and no text on screen says "did not decode" |
| `contact_info` | the chat bar's menu and the contact screen (1283, walk findings 28 and 29): the menu `menu` offers "Contact info" and no "Refresh"; Contact info opens the screen with its Advanced header `contact_advanced` shown and its body `contact_advanced_body` closed, and neither "Identity", "Network", a "hash" nor "the name it announces" on screen; a tap opens the body, and scrolled down (`contact_scroll`) the headers "Identity" and "Network" show; Back, then the chat |
| `marks` | the imported chat, scrolled down from the top, shows a mark on each rung of the tick ladder (1232, [read receipts](loquax-read-receipts.md) section 6), found by the mark view's id, which is its rung's: `state_clock`, `state_tick`, `state_ticks`, `state_ticks_read`, `state_failed`; each says its rung's words to a screen reader ("Sending" or "Sent, not confirmed yet", "Waiting in the mailbox", "Delivered", "Read at …", "Not sent. Double tap to send again"). The ugly store gives the imported conversations one sent message of each state, read among them |
| `reply_send` | a long press, Reply, a typed body, Send: the bubble shows with a ticked mark (`state_tick`, `state_ticks` or `state_ticks_read`: the home node took it, the mailbox's one tick) within 5 s, and the mailbox's log says `HOME send accepted` |
| `compose_bar` | the compose bar as Signal has it (1270, the compose node of [the UI](loquax-ui.md)): with the field emptied, the bar's buttons by the scene's ids are, left to right, `emoji` left of the field, `locate`, `camera`, `record` right of it and the round `plus` outside it, five, each with words for a screen reader; one letter typed turns the bar into `emoji`, `locate`, `camera`, `send` within 5 s (the microphone hides); the letter deleted turns it back. The detail says the time of the swap |
| `image` | a photo through the plus's Gallery (1220, [photos](loquax-photos.md)): the walk pushes the repository's fixture (`loquax/tests/fixtures/photo-portrait.jpg`, 1800 x 2400 shown, stored turned with EXIF orientation 6, a GPS position and a camera in its EXIF) to `/sdcard/Pictures` under a name of the run and asks the media scanner for it; the compose bar's plus `plus`, the sheet's "Gallery" `sheet_gallery` (1270), the system's picker, the file by its name; within 15 s one more bubble photo (`photo`) than before is on screen, every one of them at least 48 px a side and named "Photo, *size*" to a screen reader; the mailbox's log says one more `HOME send accepted`; a tap on the photo shows it whole (`photo_full`), Back returns to the chat. The time from the pick to the bubble is said in the detail. The picker is the system's (DocumentsUI), so the name is looked for in whatever list it opens on |
| `picture_zoom` | the image step's photo zooms (1269, [photos](loquax-photos.md)): a tap on it opens `photo_full`; the viewer's own log line `PHOTO_ZOOM scale=.. left=.. top=.. width=.. height=.. view=WxH` (tag `loquax`, written once a gesture settled) says 1x; a pinch out about the picture's centre, two fingers from a tenth to four tenths of its width either side, written with `sendevent` to the first direct multi-touch device of `getevent -pl` (`input` has one pointer only; none found fails the step), leaves a scale above 1.2, and at least 5 % of the sampled pixels changed; a swipe across a third of the width moves the picture by more than 10 px; a double tap goes back to 1x; Back returns to the chat. After every gesture the logged picture covers the screen on each axis it is wider on and is centred on the others. The zoom's numbers come from the log, that the picture moved from the screen |
| `file` | a file through the plus's File (1221, [files](loquax-files.md)): the walk writes a 2 KB text file named for the run, pushes it to `/sdcard/Download` and asks the media scanner for it; the compose bar's plus `plus`, the sheet's "File" `sheet_file` (1270), the system's picker, the file by its name; within 15 s one more file row than before is on screen, the newest named "File *name*, *size*" to a screen reader; the mailbox's log says one more `HOME send accepted`; a tap on the row saves it, and `run-as` finds its bytes under the app's `files/files/*/`*name*; within 2 s the chat is gone under an app that opens it, which Back leaves, or the chat says "No app on this device opens this file." The detail says which, and the time from the pick to the row |
| `voice` | a voice message through the microphone (1222, [voice messages](loquax-voice.md)): the walk writes eight seconds of speech-like 16 kHz PCM to the debug app's `files/voice-fixture.pcm` through `run-as`, which the debug build records instead of the microphone, and grants `RECORD_AUDIO` as the first press would ask; with the compose field empty it holds the microphone in the field, `record` (1270), for 3 s; within 15 s one more voice player than before is on screen, the newest at least 2 s long as "Voice message, *m:ss*" to a screen reader; the mailbox's log says one more `HOME send accepted`; a tap on its "Play" turns it to "Pause" within 5 s and back to "Play" at its end; a hold slid 400 px left sends nothing; the fixture is removed. The detail says the time from the hold to the player |
| `voice_hold_release` | recording a voice message that looks like recording (1276, [the recording bar](loquax-voice-recording.md)): ten seconds of speech as `voice-fixture.pcm`, which the debug build's recorder feeds window by window in real time, its 50 ms levels too; a 6 s hold of `record` runs in the background while the walk dumps the screen at 1.5 s: the compose field (`compose`) is gone and the recording bar is there, `rec_bar`, `rec_dot` selected (it pulses), `rec_timer` saying `REC 00:0N`, `rec_wave`, `rec_cancel` saying "‹ Slide left to cancel", `rec_held` saying "Release to send" and the open chip `rec_lock` ("Lock the recording", not selected), with no `rec_send`, `rec_pause`, `rec_discard`; after the release the bar gives way to the field within 10 s, the newest voice player is at least 5 s long and the mailbox logs one more `HOME send accepted` |
| `voice_slide_cancel` | the same fixture; the hold slides 500 px left over 3 s (past 96 dp up to density 5) and lifts. The walk reads `RecordingBar`'s `REC_BAR` lines of logcat (tag `loquax`, debug build only, one whenever the phase, the dot, the chip, the hint's arming, the buttons or the line change): `held`, then `held` armed ("Release to cancel"), then `cancelled`; the bar goes within 10 s, and 4 s later there is no new voice player and no new `HOME send accepted` |
| `voice_lock_send` | twelve seconds of speech; a 400 px swipe up from `record` (past 72 dp up to density 5.5) and the finger lifts: the bar is `locked` (`rec_lock` selected and "Recording locked", `rec_discard`, `rec_pause`, `rec_send` with their words, no `rec_held`, no `rec_cancel`); `rec_pause` makes it `paused` (`rec_play`, `rec_resume`, `rec_send`, the dot not selected); `rec_resume` makes it `locked` again; `rec_send` takes the bar away, one more voice player at least 3 s long shows, and the mailbox accepts one more send |
| `voice_silence_hint` | eight seconds of digital silence as the fixture: what Android hands an app whose microphone is blocked. Locked by the swipe up, within 10 s the line `rec_notice` says "Nothing heard. Is the microphone allowed?", its button `rec_allow` says "Allow the microphone", `rec_dot` is no longer selected, and the timer says at least 2 s; `rec_discard` takes the bar away and the mailbox accepts nothing more |
| `camera_photo` | a photo through the compose bar's camera (1270): the walk puts the photo fixture into the debug app's `files/camera-fixture.jpg` through `run-as`, which the debug build takes instead of the camera app (an emulator has none); a tap on `camera` stages it: `staged_words` reads "Photo to send, *size*" within 15 s and the round button is `send`; a caption typed and Send make, within 15 s, one more photo than before, in one bubble with the caption as its text row and a delivery mark; the mailbox's log says one more `HOME send accepted`; the staged row is gone and the bar is the five of an empty field again; the fixture is removed |
| `share_position_from_bar` | the position one tap away (1270, Lew 2026-11-01): the walk seeds a fix (`43.7731,11.2560`) in the debug app's `files/fix-fixture.txt`, which the debug build reads as the phone's fix; a tap on `locate` opens the sheet, whose `sheet_fix_words` begin "Found"; a tap on "Now, once" `sheet_share_now` makes the mailbox's log say one more `HOME send accepted ... to=<peer>` for the open conversation's peer within 30 s, and the sheet closes. The walk runs no session for the peer, so the mailbox's acceptance for it is where the step sees the position report go; the fixture is removed |
| `share_for_15_min` | a share for a while (1273, [the map](loquax-map.md), "Sharing for a while"): the fix seeded as in `share_position_from_bar`; `locate`, then "For 15 minutes" `sheet_share_15_min`; within 10 s the chat's chip `share_chip` shows, its words `share_words` "Sharing your position, *n* min left" and its Stop `share_stop` with words; within 30 s the mailbox accepts the share's first report for the peer; the seeded fix moved 44 m north (`43.7735,11.2560`), a second report is accepted within 2 min (the share's minute, the share service reading the seeded fix every 15 s); a tap on Stop takes the chip away within 5 s, and within 30 s the mailbox's outbox (`home_outbox` of its store, read-only) holds one more message for the peer carrying the end mark (`"ended": true` in msgpack). The walk runs no session for the peer, so the outbox is where it sees the end report go; the fixture is removed |
| `share_until_stopped` | as `share_for_15_min` with "Until I stop" `sheet_share_until_stopped`: the chip says "Sharing your position until you stop", the first report is accepted, Stop takes the chip away and the end report lands in the outbox |
| `receive_wake` | a second `loquax` on the host, with the Ratspeak peer's identity, sends to the mailbox; the bubble appears in the open chat without a tap, seen by a UI dump begun within 3 s of the send |
| `receive_report` | back to the chats, the same peer sends a position report (`loquax send --position`, no body): within 5 s the row's last line reads "📍 live"; `dumpsys notification` lists no new or raised again notification tagged with the peer; and at least half of 16 points sampled on the middle of the row avatar's ring stayed as they were, since the list draws no ring (1209: a report is a state, not a message); the row shows its map pin (`pin`), labelled "Position of ..." with a tap target of at least 48 dp at the screen's `wm density` (1224) |
| `position_pin` | on the chats, a tap on that row's map pin opens the Map tab (no `write`, no `compose` in the dump) with the card of that friend, named as the row; Back returns to the chats with the card closed, not to the map's previous view (1224) |
| `draft` | in the open conversation a line is typed and not sent; Back to the chats shows the row's last line as `Draft: ` and that line; the app force-stopped and started again shows the row so still, and the chat opens with the line in its field; emptied, the row's draft goes (1219) |
| `group_create` | the pencil, "New group", the host's sender ticked, Next, the group's name, Create: 4 + 1 taps and the name; the mailbox's log says `GROUP hosted name=<name> address=<hex>` and the list shows the group's row (1307) |
| `group_join` | the host's sender writes to the group's address with `loquax send`, joining by writing: the group's chat shows the line, and the line "<name> joined." the new membership makes (1307 G2) |
| `group_message` | the phone writes in its group: the bubble gets its tick from the group's proof, which the mailbox gives once the line is archived (1307) |
| `group_card_join` | a second home on the host (`card`, its owner a scripted terminal client of it) makes a group with the phone in it; the phone lists the group as an invitation, its chat shows the card, and Join is the one tap after opening it: 2 taps; the card home's `group_members` row of the phone must say joined and understanding groups (flags 8 and 1) within 60 s (1307 pass four) |
| `network_wifi_toggle` | a network change with no tap (1298, [reachability](loquax-reachability.md), "A network change"): on the chats, `svc wifi disable`; the default network of `dumpsys connectivity` must change (a device where it does not fails the step and says so), the status line `status` is read every second for 10 s, then `svc wifi enable` and at once a `loquax send --via direct` from the host's sender. Counted from the enable: when the line "Reconnecting…" was gone (`never_shown` when it never showed) and when the message showed in the chats; no message within 90 s fails. The detail is `change= off_s= kinds=before>during>after shown= line_gone_ms= bubble_ms=`, and `network_rows` in the script makes the report's table from a `walk.log` |
| `network_mobile_switch` | WLAN to mobile data and back: `svc data enable`, `svc wifi disable`; a device whose default network does not become mobile within 15 s (Waydroid, a phone without a SIM) says `skipped=no_mobile_network` and measures nothing; otherwise the WLAN back, then the same measure as `network_wifi_toggle` with mobile data carrying for 20 s |
| `flight_mode` | `cmd connectivity airplane-mode enable` for `--flight-secs` (the order's 120 s by default), then `disable`, and the same measure |
| `map` | the Map tab opens the map; its view is not uniform and has no black part of sixteen; the dump shows exactly the two tabs Chats and Map and none of the old header controls ("Load area", "All", the sun or moon, 1199), and the crosshair (1201); after each of two zooms and a pan, at least 5 % of the sampled pixels changed and no part is black; Settings, Appearance set to light and then dark changes the map's mean brightness |
| `fling` | the Map tab again; a swipe across three quarters of the map's width in 60 ms, then the map grabbed about 100 ms and 400 ms after the swipe returned and at 4 s: each pair of the three differs in at least 2 % of the sampled pixels, so the map still moved after the finger lifted and slowed to rest later (1188), and the frame at rest has no black part. The grabs' measured times are said in the detail, since a screencap takes its own while |
| `friends` | the Map tab's Friends button opens the sheet; every name in it is a peer of the fixture that sent a position (the manifest's `friends`), none twice, under the "Older than a day" divider (the ugly store's fixes are months old); a tap on the first closes the sheet and opens that friend's card with its name; Chat on the card opens the chat, back returns to the map with the card, back closes the card, back goes to the chats (1200). Whether Navigate shows (an app takes `geo:`) is said in the detail, not asserted |
| `offgrid_positions` | positions the mesh announces need no mailbox (1302, [map](loquax-map.md), "Positions the mesh announces"): on the Map tab the host's mailbox is stopped (`SIGSTOP`, resumed at the step's end; skipped once the phone holds it), and two fresh identities run `loquax announce --position` at two places by the Florence fixture under the names `Walk Nearby A<nonce>` and `B`. Within 20 s the map's log line `map pins=<n> nearby=<m>` (tag `loquax`, `MapPane.refresh`) counts two more pins, both nearby; the Friends sheet lists both names; a tap on the first opens its card, whose `card_line` says "Nearby, not in your contacts", with `card_add` and no `card_chat`. Runs right after `friends`, whose sheet would list the strangers |
| `settings` | More, Settings: the list shows the nine entries Profile, Mailbox, Appearance, Chats, Notifications, Privacy, Maps, Backup, Advanced in that order, each with a one line description, no raw hash (32 hex digits or `<8 hex…>`) on a main row and nothing that "did not decode"; each page opens, shows nothing that did not decode and its own mark where it has one (Profile the QR code `profile_qr`, which it shows since 1187 opened the profile screen from there, Appearance "Like the phone", Chats the photo size's phone choice `photo_size_phone` (1220), Privacy "Positions you shared", Maps the automatic download switch `maps_auto`), and back returns to the list; back goes to the chats (1201). Before the pages the walk puts the repository's Florence region (`leviculum-map/fixtures/firenze.lmr`) into the app's `files/maps` through `run-as`; the Maps page must show the automatic download switch on (the default, 1089), a storage line with a size that is not 0 B, a row of that region ("Around 43.77° N, 11.25° E") whose line has its size, and nothing that did not decode; the switch is turned off, the page left and opened again still off (stored), and turned on again, every dump on the way free of "did not decode" (1207, after Lew's phone showed the switch off, 0 B and no maps). Then Delete all (1290): the row `maps_delete_all` below every region, on, its label "Delete all downloaded maps (size)" with the storage line's size; a tap shows the question "Delete all N maps? Areas you visit will be downloaded again when you look at them." with N the regions listed and a "Delete all" button; after the yes the page lists no region, and the row says 0 B and is off. A toast is not in every `uiautomator dump`, so the stored state is what proves the request decoded. The Maps page's line is said in the detail |
| `receipts` | More, Settings, Privacy: the first row is "Send read receipts" with its line, above the blocked contacts, its switch `privacy_receipts` on; switched off, back to the imported chat, which shows no read mark (`state_ticks_read`) up to its top and down to its end, and a delivered one (`state_ticks`); Privacy again shows the switch off (stored), switched on, and the chat shows the read mark again (1233) |
| `profile` | from the chats, a tap on our avatar at the top left opens the profile (1187): its address row `profile_address` is 32 hex digits and the fixture mailbox's address, the QR code `profile_qr` and Share `profile_share` are there, nothing "did not decode"; back goes to the chats |
| `first_start_qr` | one contact code everywhere (1263, finding 1): the first start's address page, opened on the running session through the debug APK's exported `WelcomeActivity` (`--es show address`), shows `welcome_qr`; `zbarimg` reads exactly one code off the screenshot, `lxma://` and the fixture mailbox's address; the profile's `profile_qr` reads as the same code. The screenshots stay beside the log. Needs `zbarimg` on the host (`zbar-tools`) |
| `pencil_scan` | the pencil (1263, finding 3): "Scan a code" `new_scan` above "Paste a code" `new_paste` where the phone has a camera (`pm list features`), "Paste a code" alone where it has none; Advanced `new_advanced` closed, so no address field; no daemon word on screen; Scan opens the scanner and Back returns; under Advanced, "hello" typed and Open says "That is not a Loquax code."; back goes to the chats |
| `share_in_contact` | a friend's code from another app (1278, walk findings 12 and 15): the manifest's `friend.code`, a contact code of an identity the phone does not know, shared as the text "Add me on Loquax: <code>" (`am start -a SEND`) opens "What do you call this contact?" with one field; "Walk Friend" typed and "Add contact" opens that chat, without the banner `book_banner` that asks to save the friend to the phone's contacts. The same code as an `lxma://` link (`am start -a VIEW -d`) opens the dialog again, the field holding "Walk Friend"; Back, then the chats |
| `buildings` | the map's buildings (1193, [the map](loquax-map.md), "Drawing the buildings"): the walk puts its own region of central Frankfurt (`leviculum-map/fixtures/frankfurt/roemer.lmr`, z15 with its building sections) into `files/maps` through `run-as`, and More, Settings, Maps turns the automatic download off, so nothing else lands while it looks; then it opens the map on the Römer (50.10775° N, 8.682° E) at z15 and at z16, each by `am start` of the map screen with `lat_e6`, `lon_e6` and `zoom_milli`, which the debug APK alone exports. Of 32 x 64 samples over the middle half of the screen, at z16 at least 5 % are the building fill of either table (the light `rgb(221,213,205)` and the dark `rgb(62,62,70)` as the frame holds them through RGB565, (222,215,206) and (57,60,66), within 3 per channel), at z15, where no building is drawn, at most 0.5 %. The frame time proxy: the time from the screen asked for to two grabs in a row the same (the map at rest, and not the screen before it) at z16 is at most 1.5 times z15's plus 1 s. Both shares and both times are said in the detail. `the_walks_frankfurt_shows_the_building_fill_at_z16_and_not_at_z15` in `loquax-android/tests/map.rs` holds the walk's colours, place and shares to the renderer's frame of the same view on the host (13.9 % light, 13.8 % dark, 0 at z15) |
| `pan_into_empty` | a pan into the empty loads at once (1292, [the map](loquax-map.md), "The screen first"): the walk's Frankfurt region again, and through `run-as` the config's `map_autoload = on` and `map_source` a socket on the walk's host (the `--server` host, a free port) that takes every connection and never answers, so a load that starts stays running and its connection is its start as the host sees it; the app stopped so it reads the config afresh. The map opened on the Römer at z18, where the region holds the screen, then two right-to-left swipes of three quarters of the map's width in 400 ms each, a screen and a half east. The chip `chip` must not say "Loading the map of this area" before the pan; the first connection after the pan began must come within 700 ms of the last swipe's end (the scheduler's 300 ms and a frame's draw and the network's way); within 10 s the chip must say "Loading the map of this area". The connections before the pan (the cells next to the screen) and after it, the pan's length and both chips are said in the detail. The config is put back and the app stopped after |
| `region_fills_in` | a region shows itself while it downloads (1291, [the map](loquax-map.md), "Showing it"): no autoloaded region left on the phone, and through `run-as` the config's `map_autoload = on` and `map_source` the Grunewald archive (`leviculum-mapcut/fixtures/grunewald.pmtiles`, z12 to z15, 80 kB) served from the walk's host (the `--server` host, a free port) by a range server throttled to 8 kB/s over all its connections, about ten seconds of it; the app stopped so it reads the config afresh. The map opened over Grunewald at z15, no region there, and the middle half of the screen grabbed 16 by 16 again and again until it has stayed the same for 3 s after the source sent its last byte (90 s at most). A grab differing from the last new picture in more than 2 % of its samples is a new picture; at least three must come before the last byte: the outline and the coarser zooms standing in for z15 as they arrive. The new pictures' times, the last byte's, the bytes sent and the grabs are said in the detail. The autoloaded region is removed, the config put back and the app stopped after |
| `first_start_name` | the first start's name (1278, walk finding 9), near the end because it starts the app afresh: `pm clear`, then a servers file under `files/mailbox` through `run-as` naming `--server`, so the phone's own mailbox dials the walk's lnsd instead of the public rendezvous; "On this phone", "Use this phone", Continue through Android's dialogs. On the address page the name field `welcome_name` is the page's one field and stands above `welcome_qr`; the walk types "Walk Phone" into it and taps Done, to the chats; the profile's `profile_name` then holds "Walk Phone" |
| `link_device` | link a device (1264, finding 7), on the phone mailbox `first_start_name` made (when that step did not run, this one makes it the same way). The avatar, then "Link a device" `profile_link_device`: the sheet's QR `link_qr` read by zbarimg is the code's text `link_code` (case aside: a join code's QR is in capitals), a join code (`lqxh1`) ending in `@` and `--server`. The host's `loquax mailbox join` with that code, over the walk's lnsd, under `LNMSG_DISPLAY_NAME=walk laptop`, exits 0, and within 15 s `link_status` says "Device linked: walk laptop" |
| `move_phone` | move to a new phone (1281, walk finding 24), last before the teardown since it ends the phone's mailbox. The avatar, then "Move to a new phone" `profile_move_phone`: the sheet's QR `move_qr` read by zbarimg is the code's text `move_code`, a join code ending in `@` and `--server`. The host's `walk_move` example (`--walk-move`, or `cargo run`) takes the mailbox over with that code as the new phone, under the name "walk new phone", and exits 0 with a mailbox in its directory; within 15 s the sheet says "Moved to walk new phone. This phone has stopped."; Done opens the first screen, which says "This phone has moved", offers "Use this phone again" `join_retry` and no field; that button and its confirmation take the mailbox back, and the chats come within 60 s |
| `teardown` | the device revoked on the mailbox, lnsd and the mailbox stopped, the app uninstalled |

Every assertion reads the screen: the texts and content descriptions of a
`uiautomator dump`, or pixels sampled from a raw `screencap`. A timing is
only ever measured and reported, never taken as proof that something
happened. A failing step writes `<step>.png` and `<step>.xml` (the screen
and its UI dump) beside `walk.log`, and the walk goes on where the next
step can still start.

## What it does not prove

- **No real radio, no real phone.** Waydroid runs Android in a container
  on hamster's kernel. There is no battery, no doze as a phone lying
  still does it, no mobile network, no BLE, no camera. The wake step proves
  the wake path over a LAN TCP link that never drops.
- **x86_64 only.** The APK of `make waydroid` carries only the x86_64
  library. The arm64 and armv7 libraries of `make debug` are the same Rust
  source cross-built, but a bug that only one target's code generation
  makes, or one that only a 32-bit `usize` makes, stays invisible.
- **One gesture is not the thumb's.** `adb shell input` has one pointer,
  so the map's zoom is a double tap (`MapView.onDoubleTap`, the same zoom
  path as a pinch), not a pinch. A bug in the pinch detector itself stays
  for the thumb.
- **One mailbox, one fixed store.** The walk joins over the mailbox's TCP
  server on the LAN. Rendezvous, mesh paths
  ([reaching the mailbox](loquax-reachability.md)) and the mailbox on the
  phone are not walked.
- **The frame time is a proxy.** The `buildings` step times the map to
  rest by polling the screen, so a screencap's own while is its grain,
  and it compares z16 with z15 on the same device rather than holding a
  number in milliseconds. It catches a frame that takes seconds, not one
  that is a few milliseconds slower; the host's `map_fps` rows
  ([the map](loquax-map.md)) are the measurement.
- **A held finger is a swipe that does not move.** `adb shell input`
  holds with `input swipe x y x y ms`, which lifts at its end; the
  recording bar's chip is tapped by a second finger while the first holds,
  which one pointer cannot do, so the voice steps lock by the swipe up.
  The haptic ticks and the preview's sound are not observed; the facade
  test sees the ticks as effects.
- **English only.** The step texts are `android/res/values/strings.xml`'s;
  a device set to German shows `values-de/`'s and the walk fails at the
  first text.

## How the reviewer runs it on hamster

Once: Waydroid's session up, `adb connect` to it, so `adb devices` lists
its serial. The device must reach the host's TCP server: Waydroid's
bridge gives the host `192.168.240.1` by default.

```sh
cargo build -p loquax --features tui --bin loquax
cargo build -p leviculum-cli --bin lnsd
(cd loquax-android && make waydroid)
python3 -I loquax-android/scripts/device-walk.py \
    --serial 192.168.240.112:5555 \
    --server 192.168.240.1:4242 \
    --out ~/scratch/walk-$(date +%F)
```

`--loquax` and `--lnsd` name other host binaries (default: the workspace's
debug ones), `--apk` another APK, `--listen` the address the TCP server
binds (default `0.0.0.0`), `--walk-mailbox` a built `walk_mailbox` example
(default: `cargo run` it), `--steps` a subset of the steps in the walk's
order, `--server-mode` and `--link-keepalive` set the TCP server's interface
mode and the host's link keepalive (the power proxy's levers,
[tier P](loquax-phone-power.md)), `--keep` leaves the fixture, the device's admission and the app in
place for a look by hand. The fixture's homes, logs and the code live in
`OUT/fixture-*/`.

## How the fixture is made

`loquax-android/examples/walk_mailbox.rs` includes the ugly store's module
as the host tests do and writes three things: the store into the archive
of a mailbox home, through the same writer the home node tests use; the
Ratspeak peer's identity into a second home, the sender of
`receive_wake`; and a manifest in JSON with the mailbox's address, every
conversation's peer, the name a screen shows for it, whether it is
archived, blocked or imported, and the bodies the `open_imported` step
looks for. The script asserts against the manifest only, so a change to
the ugly store changes what the walk expects in the same commit.

## How to add a step

1. Add its name to `STEPS` in the walk's order, and a method
   `step_<name>(self) -> str` on `Walk`. It returns its detail; it raises
   `StepFailed` with the reason when an assertion fails. The step's line,
   its time and the evidence on failure come from `run_step`.
2. Read the screen with `self.need_device().dump()` and `find`, wait with
   `wait_text` or `wait_id`, never with a bare sleep as proof. Pixels come
   from `dev.screen()` and `sample_grid`.
3. A step that needs a new fact about the store takes it from the
   manifest: add the fact to `walk_mailbox.rs`, from the ugly store's own
   functions, not as a literal in the script.
4. A new parser gets a test in `loquax-android/scripts/tests/`;
   `cargo test -p loquax-android` runs them through
   `loquax-android/tests/device_walk_script.rs`.

## The parsers and their tests

The UI dump goes through `parse_dump`, which also repairs what Android's
serializer writes for a character outside the BMP: two character
references to its UTF-16 surrogates, which XML forbids. Every emoji in
the ugly store's names is such a character, so without the repair the
first dump of the list would not parse. The raw screencap goes through
`parse_screencap`, with either header Android writes (12 bytes before
Android 9, 16 after). The tests pin both, the sampling (uniform, black
parts, the share of changed samples, the brightness), the admission code
and the `WALK` line format. The recording bar's (1276) readers,
`rec_bar_problems` (its parts by phase), `rec_seconds`, `rec_pulsing`
(the dot's `selected`, which `parse_dump` now reads), `rec_notice_problems`,
`parse_rec_lines` and `rec_went` (the `REC_BAR` lines in order), are pinned
by the class `Recording`.

## The M9's walk on the host

The ThinkNode M9 has a walk of its own that needs no device:
`loquax-m9-sim` (1530) runs the board's UI loop and its store on the
host, headless with a key script and PNG and text dumps, or in a window.
`just m9-sim-walk` runs its walks over the fixture and the 1489 store;
the usage is in [the M9 section of the handheld
messenger](handheld-messenger.md#the-m9-on-the-host-loquax-m9-sim-1530).

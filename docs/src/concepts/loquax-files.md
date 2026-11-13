# Loquax: files

Status: slice one built (1221, 2026-11-01). Lew, 2026-10-31: attach a file
to a message, compatible with the other messengers; the received file is
kept on the phone and opens in the app the phone has for it.

## Decisions

| Question | Decision |
|---|---|
| Wire | The LXMF file attachments field, `0x05`, holding `[[name, bytes], ...]`, the name a str, as the reference names it and Columba and Sideband write and read it. No field of our own. |
| A file and text | A file is one message with an empty text, as the plus sheet's File row sends it. A received message with several files shows a row for each. |
| Cap | 990 000 bytes a file: a receiver refuses a message past 1000 KB, and the message around the file needs room. Refused with both sizes said, before anything goes. |
| Names | What the sender wrote is shown and saved only after [`safe_name`](#3-the-core): no path, no control character, no leading dot. |
| Where it is kept | Under the home: `files/<peer>-<key>-<n>/<name>`, written once when the file is first opened. On the phone that is the app's own files; a provider of the app's lends that one file read only to the app that opens it. Deleting the message removes it (1662); a copy the reader exported elsewhere stays. |
| Where the work is done | In Rust: the field, the cap and its words, the names, the save. Java keeps the system picker, the open and share intents and the provider. |

## 1. The wire

The reference defines `FIELD_FILE_ATTACHMENTS = 0x05`
(`reference/LXMF/LXMF/LXMF.py:12`). Columba writes
`fields[LxmfFields.FIELD_FILE_ATTACHMENTS] = options.fileAttachments.map {
(name, data) -> listOf(name, data) }`
(`columba/rns-backend-kt/src/main/kotlin/network/columba/app/rns/backend/kt/NativeMessageSender.kt:141-145`,
the same in
`columba/rns-backend-py/src/main/kotlin/network/columba/app/rns/backend/py/PythonRnsLxmf.kt:520-531`),
whose comment says why the name is a str: Sideband renders it with
`str(name)`, so a bin name shows as its `b'...'` repr. Columba reads an
entry of two or more elements, the name either a str or hex of a bin
(`columba/app/src/main/java/network/columba/app/ui/model/MessageMapper.kt:784-800`),
and its interop suite carries files both ways to Sideband
(`columba/tests/interop/test_file.py`). Sideband's source is not on this
host; the Python reference passes the field through unchanged.

The bytes Loquax writes for `fields[5] = [["a.txt", 01 02]]` are those
`umsgpack.packb([["a.txt", b"\x01\x02"]])` gives:

```
05  91  92  a5 61 2e 74 78 74  c4 02 01 02
key arr arr "a.txt"            bin 2
```

`ui::files::tests::the_field_is_the_references_list_of_name_and_bytes`
pins them. Reading is lenient (`ui::files::file_parts`): a name may be a
str or a bin, elements past the bytes are skipped, and an entry that does
not read is skipped while the rest are still read.

## 2. The caps

| Where | Limit | Source |
|---|---|---|
| A file Loquax sends | 990 000 bytes | `loquax::ui::files::LARGEST`, the same budget as an original photo |
| Any receiver, direct or over a link | 1000 KB a message | `DELIVERY_LIMIT`, `reference/LXMF/LXMF/LXMRouter.py:60`, applied at `:1979`; ours `leviculum_lxmf::constants::DELIVERY_LIMIT_BYTES` |
| A stock Python propagation node (lxmd) | 256 KB a message | `PROPAGATION_LIMIT`, `reference/LXMF/LXMF/LXMRouter.py:55`; lxmd's `propagation_transfer_max_accepted_size` default, `reference/LXMF/LXMF/Utilities/lxmd.py:165-177` |
| lnpnd | 256 KB a message, with or without a config, as lxmd; up to 1000 KB stored local only, above it refused | `DEFAULT_TRANSFER_LIMIT_KB` and `DEFAULT_DELIVERY_LIMIT_KB`, `lnpnd/src/config.rs` (256 since 1294, 4 before); pinned by `a_node_without_a_config_takes_a_sixty_kb_voice_message_and_refuses_one_over_the_delivery_limit` (1663) |
| A board's propagation node | 4 KB a message | `BOARD_TRANSFER_LIMIT_KB`, `leviculum-nrf/heap-budget`; its heap, see [the board's node](propagation-node-on-a-board.md), section 6 |
| A Loquax Mailbox's archive | 1024 MB in all, oldest first out | `DEFAULT_ARCHIVE_CAP_MB`, `loquax/src/home.rs:362`, `archive_cap_mb` in the config |
| Columba | none of its own | `MAX_SINGLE_FILE_SIZE = Int.MAX_VALUE`, `columba/app/src/main/java/network/columba/app/util/FileUtils.kt:37`; its receiver defaults to 1 MB (`columba/app/src/main/java/network/columba/app/repository/SettingsRepository.kt:1849`) |

So a file up to 990 KB goes to a peer that is online, and to a peer that
is offline through a Loquax Mailbox. Through a propagation node, lxmd or
lnpnd, a file up to 256 KB is stored and carried on to the node's peers;
past 256 KB it is stored local only, so its recipient collects it from
that node and from no other, and lnpnd refuses one past 1000 KB
(`PN_REJECT reason=too_large`). A board's node takes 4 KB. The same open
question as a phone-size photo ([photos](loquax-photos.md), section 7).

The refusal says both sizes, in the reader's language: "This file is
4.2 MB. A message carries at most 990 KB." The phone reads at most the
first megabyte of a picked file and only counts the rest, so a film is not
held in memory to be refused.

## 3. The core

`loquax/src/ui/files.rs`:

- `FileRef { name, bytes }`, `files_of`, `file_at`, `file_fields`.
- `send_name`: a picked file goes under its last path element, cut to
  255 bytes on a character.
- `safe_name`: what a received name may be shown and saved as. `/`, `\`,
  `:`, `*`, `?`, `"`, `<`, `>`, `|` and control characters become `_`;
  leading dots and spaces, and trailing ones, go; the name is cut to 120
  bytes keeping its extension; nothing left is `file`. `..` is `file`,
  `../../etc/passwd` is `_.._etc_passwd`.
- `refusal(bytes)`: the cap's words, or none.
- `UiEvent::SendFile { peer, name, data }` sends one message whose only
  field is the file; past the cap it adds a toast and nothing.
- `UiEvent::OpenFile { peer, key, index }` gives `UiEffect::OpenFile`.
- `chat::Bubble::files`; `scene::FileNode` with id `file_<key>_<n>`, its
  safe name, bytes, size words, its words "File report.pdf, 313 KB" or
  "Datei report.pdf, 313 KB", and the tap `Intent::OpenFile`, which
  `Scene::action` resolves. A bubble with no text is said as its first
  file. The list says the first file's name, its kind is `file`.
- `loquax::file_save::save` (std) writes the file under the home.
- A saved file goes with its message (1662): deleting a message (`d`, the
  selection's delete, the phone's `Request::Delete`), another device of the
  home deleting it, sending a failed one again and deleting the
  conversation each give `UiEffect::ForgetSaved { peer, key }` (no key for
  the conversation). `loquax::file_save::forget` removes the message's
  folders under `files/` and the copies a desktop wrote for its viewer and
  player (`photos/<peer>-<key>.<ext>`, `voice/<peer>-<key>.wav`); the
  delimiter after the key keeps key 1 from taking key 12, a link is
  removed and never what it points at. A message still being sent is
  kept, and what it saved with it. The window and the terminal run the
  same shell, so their saved copies go the same way; the phone's session
  removes them under the app's files, where the provider lends them from.
  Nothing outside the home is touched: a file the reader shared or saved to
  Downloads, or a copy the system's handler made, is the reader's.
- A picture sent as a file (1662): `photos::picture_of` takes the image
  field's photo, else the first file whose bytes are a picture this build
  decodes, told by its first bytes (`photos::image_format`: PNG, JPEG,
  WebP, GIF), never by its name. The bubble's `photo` is then that file's,
  `PhotoRef::file` and `PhotoNode::file` its place among the files; the
  file's line stays under it, and a screen reader says the file. The phone
  draws the thumbnail as a photo's (`MessageRow::photo`, `Query::Photo`
  read through `photos::picture_bytes`); the window, the terminal and the
  M9, which draw no thumbnail, draw the file's line alone.

The terminal draws `▤ report.pdf · 313 KB` (`[f] report.pdf - 313 KB` in
ASCII) above the text or alone; Enter saves the first file and hands it to
the system's handler (`Output::File`). The window draws the same line,
pressable. `loquax send --file FILE` sends one from a script.

## 4. Android

- The compose bar's plus sheet (1270): Gallery, File, My position. File opens the system
  picker with `ACTION_GET_CONTENT` and `*/*`; Java reads the display name
  and the bytes off the UI thread and sends `Request::File { peer, name,
  data, size }`. A refusal shows on the chat's status line.
- `MessageRow::files` carries each file's safe name, bytes, size words and
  words. The rows sit in the bubble above its words (1268), as a photo
  does; a tap asks `Query::SaveFile`, gets the path, and starts `ACTION_VIEW`
  with the type from the extension; where nothing opens that kind the
  share sheet is offered, and where nothing takes it either the chat says
  "No app on this device opens this file." A long press shares it.
- `FilesProvider` (authority `<package>.files`, not exported, grants URI
  permissions) serves only files two levels under `files/files/`, read
  only, resolved through canonical paths.

## 5. Delivery and progress

A file goes as a photo goes: over a link as a resource from the desktop,
through the mailbox's `/send` as a request resource from the phone
(1220's fallback in `leviculum-std`). The archive stores it packed and
counts it against `archive_cap_mb` like any other message.

Progress is in the bubble (1662), for photos and files alike: a bar and
its words under the media, "wird gesendet · 43 %", while the message still
sends; "wird geholt · 1,2 MB von 3,0 MB" for one being fetched. The model
keeps one `ui::transfer::Transfer` a bubble and takes a new one at most
every 250 ms of its clock (`transfer::EVERY_MS`), the complete one always.
Two feeds reach it:

- Over a link the router keeps the resource's share in its outbound entry
  and reports no event of it; the engine reads the entry after every hook
  and says `OutboxEvent::Progress { message_id, permille }` when it moved.
- A client's `/send` to its mailbox is a request that goes as a resource:
  `home::client::send_watching` asks the link
  (`ReticulumNode::link_outgoing_resource_progress`) every 250 ms while the
  answer is awaited, and the runner hands the screen
  `UiEvent::SendProgress`. The terminal and the phone fold it as it comes.

The terminal draws `▰▰▰▰▰▱▱▱▱▱ sending · 43 %` (`[#####-----]` in ASCII)
under the file's line, the window the words and a thin bar, the phone the
words and a horizontal `ProgressBar` (`MessageRow::progress_words`,
`progress_percent`). Fetching has its words and its event
(`UiEvent::Transfer`), and no feed yet: a message is fetched whole before
its bubble exists, on the phone in the pull's pages and on a desktop as
the delivery resource whose sender is known only once it is opened. A
transfer is not kept: it goes with the runner's end and with a restart.

## 6. Proof

- `loquax-android/tests/facade.rs::a_file_travels_both_ways_with_python_lxmf`:
  Python sends two files as Sideband does, one named `../../evil.sh`; the
  phone shows both rows, the second as `_.._evil.sh`; the list says
  `Protokoll.pdf`, `file`; `Query::SaveFile` writes both under the app's
  `files/` with their bytes, a third index is an error. The phone sends
  `Grüße.txt`, 300 000 bytes, and Python's router gets field 5 with one
  entry, the name a str, the bytes unchanged. A file of 990 001 bytes,
  and one Java stopped reading at 1 MB of 48.3 MB, are refused with both
  sizes and nothing reaches Python. Positive control: `safe_name`
  returning the name as it came turns the test red.
- `loquax/tests/python_interop.rs::a_file_reaches_a_python_lxmf_receiver_as_the_attachments_field`:
  `loquax send --file` with a 300 KB file named `Protokoll Grüße.pdf`
  reaches Python as field 5, name a str, bytes unchanged; a file past
  990 KB exits 2 with the refusal. Positive control: a fixed name `x`
  turns it red.
- Unit tests: the wire bytes, lenient reading, malformed fields, the safe
  and the sent name, the cap's words in both languages, the scene node and
  its tap, the terminal's line and Enter, the save under the home.
- The device walk's `file` step ([device walk](loquax-device-walk.md)).
- 1662: `ui::tests::a_files_progress_is_in_its_bubble_while_it_goes` (both
  feeds, the 250 ms rule, the bar gone once sent; positive control:
  `EVERY_MS = 0` turns it red), the terminal golden
  `tui-file-progress.txt` and its German twin at 43 %,
  `facade.rs::a_file_under_way_carries_its_words_and_bar_to_the_phone`;
  `photos::tests::a_picture_sent_as_a_file_is_told_by_its_bytes`,
  `facade.rs::a_png_sent_as_a_file_shows_its_thumbnail_with_the_file_line`
  (positive control: the bubble on `photo_of` turns it red);
  `file_save::tests::a_saved_file_goes_with_its_message_and_an_exported_copy_stays`,
  `ui::tests::a_deleted_message_asks_the_shell_to_forget_what_it_saved`,
  and `a_file_travels_both_ways_with_python_lxmf` deletes the received
  message after saving it: both paths gone, the exported copy whole
  (positive control: forgetting under another folder turns it red).

## 7. Open

1. The propagation node's 256 KB per message (section 2), shared with
   photos: above it a file reaches its recipient only through the node
   it was uploaded to. lnpnd without a config takes the same 256 KB
   since 1294 (checked in 1663).
2. One file per message from the plus sheet; several picked at once are a
   later slice. Received messages with several show all.
3. Fetching progress has its words and its event and no feed (section 5):
   a message is fetched whole before its bubble exists.
4. A message deleted by another device while it is not loaded here (an
   older page) leaves its saved files: the mark names its id, and the
   folders are named by the device's own key, which only a loaded message
   gives.

Done: progress in the bubble, a picture sent as a file shows its
thumbnail, a saved file goes with its message (1662).

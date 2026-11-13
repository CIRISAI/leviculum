# Loquax: photos

Status: slice one built (1220, 2026-11-01). Lew, 2026-10-31: send and
receive images like Columba and Sideband; large ones are scaled down by
default to a size that still looks good on a phone; a setting off the
main path offers three steps.

## Decisions

| Question | Decision |
|---|---|
| Wire | The LXMF image field, `0x06`, holding `[format, bytes]`, exactly as the reference names it and Columba writes and reads it. No field of our own. |
| Sizes | `phone` (the default): longest edge 1600 px, JPEG quality 80, at most 250 000 bytes (1288). `small`: 800 px, quality 70, the same bound. `original`: the picture as taken, only its metadata removed. |
| Privacy | No photo carries EXIF in any size: the place, the time, the camera never go. The setting's one line says so. |
| Orientation | Every photo goes upright, its EXIF orientation applied, since Columba draws the bytes without reading the tag. |
| Where the pixels are done | In Rust, `loquax::photo` (feature `photos`), on every target. Java keeps the system picker and a viewer screen that shows a Bitmap. |
| A photo and text | A photo is one message with an empty text, as Columba sends one without a caption. |
| Setting | Settings › Chats on the phone, `photo_size` in the config, `--photo-size` for `loquax send`. Never on the main path. |

## 1. The wire

The reference defines `FIELD_IMAGE = 0x06`
(`reference/LXMF/LXMF/LXMF.py:13`). Columba writes it as
`fields[LxmfFields.FIELD_IMAGE] = listOf(options.imageFormat,
options.imageData)` (Columba's
`columba/rns-backend-kt/src/main/kotlin/network/columba/app/rns/backend/kt/NativeMessageSender.kt:133`,
the same shape in `columba/rns-backend-py/src/main/kotlin/network/columba/app/rns/backend/py/PythonRnsLxmf.kt:517-518`), with format words `jpg`,
`png`, `webp` or `gif`; its own compressor sends `webp`
(`columba/app/src/main/java/network/columba/app/util/ImageUtils.kt:205`).
Columba reads the second element of an array of two or more and decodes
the bytes with Android's `BitmapFactory`, whatever the format word says
(`columba/app/src/main/java/network/columba/app/ui/model/MessageMapper.kt:511-547`
and `:563-605`). Columba's own interop suite sends `["png", bytes]` to
Sideband and back (`columba/tests/interop/test_image.py`). Sideband's source is not
on this host; the Python reference passes any field through unchanged, so
what reaches a Sideband is what the reference's router took.

The bytes Loquax writes for `fields[6] = ["jpg", ff d8]` are those
`umsgpack.packb(["jpg", b"\xff\xd8"])` gives:

```
06                       key 6 in the fields map
92                       array of 2
a3 6a 70 67              str "jpg"
c4 02 ff d8              bin, 2 bytes
```

`photos::photo_fields` writes them through
`leviculum_lxmf::attachments::MessageAttachments::into_fields`; the unit
test `the_field_is_the_references_format_and_bytes` pins them.
`photos::photo_parts` reads leniently, as Columba reads: the format may be
a str or a bin, and elements after the bytes are ignored.

A received photo is kept with its message as every field is (the store
keeps a message's fields whole), so the phone, the terminal and the
window show it from the same row.

## 2. Preparing a photo (`loquax::photo::prepare`)

1. The format is read from the first bytes: JPEG, PNG, WebP, GIF. Anything
   else, a HEIC from a phone's camera among it, is refused with "not a
   picture Loquax can send".
2. `phone` and `small`: decoded, turned upright by the EXIF orientation,
   scaled with a triangle filter if the longer side is past the step's
   edge (never up), encoded as JPEG at the step's quality. The JPEG
   encoder writes no metadata. A picture past 250 000 bytes
   (`photo::SHOWN_BY_COLUMBA`, see 2a) steps its quality down by 10 to
   60, as Columba's compressor steps
   (`columba/app/src/main/java/network/columba/app/util/ImageUtils.kt:186-191`),
   and then its edge, by the root of how far it is over, since a JPEG's
   bytes go with its pixels.
3. `original`, upright JPEG: every APP segment but JFIF (APP0), the ICC
   profile (APP2) and Adobe's colour transform (APP14) is dropped, and
   comments with them; the scan is byte for byte the camera's
   (`photo::strip_jpeg`, test `an_upright_original_keeps_its_scan_and_loses_its_metadata`).
4. `original`, PNG: the text, time and EXIF chunks go, the chunks that
   change how it looks stay (`photo::strip_png`). GIF goes as it is (it
   has no EXIF). A JPEG not stored upright, and a WebP, are turned and
   encoded at quality 95 at full size.
5. A prepared photo past 990 000 bytes is refused: a receiver refuses a
   delivery resource above `DELIVERY_LIMIT`, 1000 KB
   (`reference/LXMF/LXMF/LXMRouter.py:60`, applied at `:1979`), and the
   message around the picture costs its header, signature and framing.
   Only `original` can meet this; the error says to choose a smaller size.

### 2a. What Columba shows (1288)

Lew sent three photos from Loquax to Columba on 2026-11-01; one showed,
two showed a grey box saying "Not available". The cause is in Columba,
for every received picture past 255 995 bytes, whatever its format:

1. The kt backend writes a received message's fields as JSON, bytes as
   lower hex, so `fields[6]` becomes `["jpg","ffd8..."]`
   (`columba/rns-api/src/main/java/network/columba/app/rns/api/util/AppDataParser.kt:204-232`).
2. A fields JSON of 512 000 characters or more has each field longer
   than 512 000 written to a file as its JSON text, and replaced by
   `{"_file_ref": path}`
   (`columba/data/src/main/java/network/columba/app/data/repository/ConversationRepository.kt:957-983`,
   the threshold `500 * 1024` in
   `columba/data/src/main/java/network/columba/app/data/storage/AttachmentStorageManager.kt:38`).
3. Reading it back, the file's text is taken for the picture's hex
   (`columba/app/src/main/java/network/columba/app/ui/model/MessageMapper.kt:532-543`);
   `hexStringToByteArray` throws at the `[` (`:1076-1089`), the error is
   caught (`:544`) and no bytes come back.
4. The view model marks the image loaded without a picture
   (`columba/app/src/main/java/network/columba/app/viewmodel/MessagingViewModel.kt:2427-2431`),
   and the bubble draws "Not available"
   (`columba/app/src/main/java/network/columba/app/ui/screens/MessagingScreen.kt:2356-2376`).

`2 * bytes + 10 > 512 000` is a picture of 255 996 bytes or more. A 12 MP
photo at 1600 px and quality 80 is 180 to 610 KB, so most went past it.
Loquax therefore keeps `phone` and `small` at most 250 000 bytes, which
also fits under a stock propagation node's 256 KB a transfer. The unit
tests `columba_reads_a_picture_inline_and_loses_one_past_its_threshold`
(a model of the four steps, pinned at 255 995 and 255 996) and
`a_detailed_phone_photo_goes_at_a_size_columba_shows` pin it; with the old
bound the second fails. `original` keeps the picture and can still be
past it: Columba then shows "Not available", Sideband and Loquax show it.

### Measured sizes and times

`cargo run --release -p loquax --features photos --example photo_cost`,
on the coder host (AMD Ryzen 9 7950X, musl release build), best of three.
A phone is several times slower; no phone was measured in slice one.

| picture | step | sent | prepare | decode of the original | thumbnail of what went (480 px) |
|---|---|---|---|---|---|
| 3456 x 3456, 11.9 MP, 2.9 MB camera JPEG | small | 800 x 800, 54 KB | 141 ms | 78 ms | 7 ms |
| | phone | 1600 x 1600, 223 KB (279 KB before 1288) | 266 ms | | 17 ms |
| | original | refused (2.9 MB) | | | |
| 3468 x 4624, 16 MP, 4.9 MB phone JPEG, turned | small | 600 x 800, 82 KB | 190 ms | 126 ms | 6 ms |
| | phone | 1200 x 1600, 403 KB before 1288, now at most 250 KB | 272 ms | | 14 ms |
| fixture `photo-portrait.jpg`, 4.3 MP, 360 KB, turned | small | 600 x 800, 23 KB | 44 ms | 13 ms | 5 ms |
| | phone | 1200 x 1600, 104 KB | 98 ms | | 11 ms |
| | original | 1800 x 2400, 668 KB | 77 ms | | 25 ms |

The `image` crate (0.25, JPEG through zune-jpeg, PNG, WebP, GIF, no
default formats, no rayon) was already in the lock through lnomad. The
Android library for arm64 (release, `make native ABIS=arm64-v8a`) grew
from 17 408 864 to 18 294 800 bytes, 865 KiB or 5.1 %, measured on the
same tree without and with the `photos` feature.

## 3. The core

- `UiModel::photo_size`, from the config's `photo_size` at start;
  `UiEvent::PhotoSize` sets it and remembers it
  (`UiEffect::Remember { key: "photo_size" }`).
- `UiEvent::SendPhoto { peer, format, data }`: the shell has prepared the
  picture already; the model sends one message with the image field alone
  (`photos::photo_fields`), the view on it. The read receipt mark rides
  on it as on every send.
- `chat::Bubble::photo` and `scene::BubbleNode::photo`, a `PhotoNode`
  with its id `photo_<key>`, its format and bytes, its words
  (`Photo, 279 KB`, `Foto, 279 KB`) and its tap,
  `Intent::ViewPhoto`. The bubble's screen reader label is the photo's
  words when it has no text. The list's last line says `photo`.
- A message is one bubble whatever it carries (1268):
  `scene::BubbleNode::text` is its words, none when the body holds
  nothing to read, and every renderer draws the media first, the words
  under them and the time and mark closing the last row, with no text row
  where `text` is none. Before 1268 the phone drew the photo above an
  empty text bubble.
- `UiEvent::ViewPhoto` answers `UiEffect::ShowPhoto` for a message that
  carries one. The terminal writes the picture to
  `$LNMSG_HOME/photos/<peer>-<key>.<format>` and hands it to the system's
  viewer; the window decodes that file and shows it over itself (1269);
  the phone opens its viewer screen.
- The picture whole zooms (1269, Lew 2026-11-01): one state machine,
  `loquax::ui::zoom::Zoom`, for the phone and the window. At 1x the
  picture fits the screen whole; it zooms up to 4x, or to its own size
  when that is larger on screen (`Query::Photo` answers `full_width` and
  `full_height`, the size before the decode scaled it). A pinch keeps the
  picture point under its centre; one finger pans while zoomed; a double
  tap toggles 1x and 2x at the tap; the picture never leaves the screen:
  where it is wider than the screen its edges stay outside, where it is
  narrower it stays centred. Back while zoomed goes to 1x, at 1x it
  closes, as does a swipe down (an eighth of the screen, more down than
  sideways). The platform only reports gestures:

  | Gesture | Android (`PhotoView`) | Window (`gui/viewer.rs`) | `Zoom` |
  |---|---|---|---|
  | zoom about a point | pinch (`ScaleGestureDetector`, its focus) | wheel or touchpad scroll, 1.25x a notch, about the pointer | `pinch` |
  | move | one finger while zoomed (`onScroll`) | drag with the left button | `pan_by` |
  | coast | fling while zoomed (`Fling`'s decision, an `OverScroller` bounded by `room`) | none | `pan_by`, `room` |
  | toggle 1x / 2x | double tap | double click | `double_tap` |
  | back | Back | Esc, the bar's Close closes at once | `back` |
  | close at 1x | tap, swipe down | Esc | `swipe_closes`, `back` |
  | screen size | `onSizeChanged` | the canvas's size | `resize` |

  The phone keeps the state between gestures and hands it back with each
  (`Query::Zoom`, which needs no session); the terminal keeps its still
  picture.
- The terminal draws `▣ photo · 279 KB` above the text or alone
  (`[#] photo - 279 KB` in ASCII); `Enter` on it shows it. The window
  draws the photo's words as a pressable line; its thumbnail in the window
  is open.
- A forward carries the photo (the image field is a content field of
  `replies`), and send again resends it.

## 4. Delivery

- **Desktop** (the engine): the delivery method counts the fields with
  the body (`engine::content_len`), so a photo goes over a link as a
  resource, the way the reference and Columba send it, and not
  opportunistically into the router's oversized fallback.
- **Phone** (through its mailbox's `/send`): the submission with the
  picture is past one packet, so the request travels as a request resource
  (`ReticulumNode::send_request_awaited`, as the reference's
  `Link.request()` sends any request past the MDU,
  `reference/Reticulum/RNS/Link.py:514-517`). Before 1220 it was refused
  with `PayloadTooLarge`. The mailbox's answers past one packet already
  went as resources.
- **The mailbox** keeps the message packed as it came, like any other, and
  its archive cap counts it by its bytes
  (`SUM(LENGTH(packed))`, `loquax/src/store.rs:3611`).

## 5. Android

- The compose bar's plus opens the sheet whose first row is "Gallery"
  (1270, [the UI](loquax-ui.md), "The compose bar"): the system picker
  (`ACTION_GET_CONTENT`, `image/*`). The camera in the field takes a
  photo through the platform's capture intent and stages it for a caption;
  its bytes take the same `loquax::photo::prepare` as a picked one
  (`Request::Stage`). The picked
  bytes, read off the UI thread up to 64 MB, go to the library
  (`Request::Photo`); the library's refusal is said on the status line.
- `MessageRow` carries `photo`, `photo_bytes`, `photo_w`, `photo_h` (the
  size it shows at, upright, from the picture's header) and
  `photo_words`. The row draws the thumbnail first in the bubble (1268),
  240 dp on the longer side (less where the bubble is narrower), its shape the picture's, from
  `Query::Photo { peer, key, edge }`, which answers RGBA rows that
  `Bitmap.copyPixelsFromBuffer` takes as they are; a cache of 16 MiB
  keeps them. A tap opens `PhotoActivity`, which asks for twice the
  screen's longer side (at most 4096), so a zoom has pixels to show, and
  shows the Bitmap in `PhotoView`, zoomed as above (1269).
- Settings › Chats: the three sizes as a choice, phone checked by default,
  and the line "Photos are made smaller before they go. Their place and
  camera data never go." (`Query::PhotoSize`).

## 6. Proof

- `loquax-android/tests/facade.rs`,
  `a_photo_travels_both_ways_with_python_lxmf_at_each_size`: the Python
  reference sends `["jpg", fixture]` to the phone's mailbox; the row says
  photo, 1800 x 2400 upright, "Photo, 361 KB", the list says photo, and
  the library draws it upright at 360 x 480. The phone sends the fixture
  at small, phone and original; Python's router receives the image field
  with the longest edge 800, 1600 and 2400, portrait, its top left the
  fixture's red block, and no `Exif`, camera or `GPS` bytes. A document
  is refused with "not a picture" and nothing reaches Python. Positive
  control: without the request resource fallback no photo reaches Python.
- `loquax/tests/python_interop.rs`,
  `a_photo_reaches_a_python_lxmf_receiver_as_the_image_field` (with
  `--features photos`): `loquax send --photo` at small and phone, the same
  assertions on what Python got; a file that is no picture exits 2.
  Positive control: sending the original regardless of the size fails the
  edge assertion.
- `loquax::photo` unit tests: turned photos come out upright and scaled,
  never up; no EXIF, comment or camera bytes in any step; an upright
  original keeps its scan; a PNG original is exactly the PNG without its
  text and EXIF chunks; no picture and too large are refused, and noise
  steps its quality down to fit.
- The scene, the terminal and the config each have their test; the device
  walk's `image` step drives the plus, the picker, the thumbnail and
  the viewer on a phone ([the device walk](loquax-device-walk.md)), its
  `picture_zoom` step a pinch, a pan, a double tap and Back on it.
- The zoom (1269): `loquax::ui::zoom` unit tests (the fit, the most of 4x
  or the native size, a pinch's clamp and anchor, an anchor at the edge,
  a pan held at the edge, the double tap, Back, the swipe down, a resize,
  and 2000 held random gestures after each of which the picture covers
  the screen or is centred); `loquax-android/tests/viewer.rs` the same in
  Java's bytes through the JNI's `query` without a session;
  `gui/viewer.rs` and `gui.rs`'s `a_photo_opens_in_the_windows_viewer_and_zooms`
  the window's wheel, drag, double click, Esc and Close.

## 7. Open

- **The propagation limit.** A stock propagation node takes at most
  256 KB a transfer (`PROPAGATION_LIMIT`,
  `reference/LXMF/LXMF/LXMRouter.py:55`). Since 1288 a `phone` photo is
  at most 250 000 bytes, so it fits; an `original` can be refused there.
- **Progress in the bubble** while a photo uploads is not built: the
  request resource's progress is not yet plumbed to the scene.
- **HEIC** from a phone's camera is refused; the `image` crate has no HEIC
  decoder. Android could hand JPEG through its own decoder, against the
  rule that the pixels are Rust's.
- **A GIF** keeps its animation only as `original`; scaled, it is its
  first frame as JPEG.
- **The window's thumbnail** and a camera on the phone are later slices.
- **Measured on a phone**: the decode and prepare times above are the
  desktop's.

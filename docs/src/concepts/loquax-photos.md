# Loquax: photos

Status: slice one built (1220, 2026-11-01). Lew, 2026-10-31: send and
receive images like Columba and Sideband; large ones are scaled down by
default to a size that still looks good on a phone; a setting off the
main path offers three steps.

## Decisions

| Question | Decision |
|---|---|
| Wire | The LXMF image field, `0x06`, holding `[format, bytes]`, exactly as the reference names it and Columba writes and reads it. No field of our own. |
| Sizes | `phone` (the default): longest edge 1600 px, JPEG quality 80. `small`: 800 px, quality 70. `original`: the picture as taken, only its metadata removed. |
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
   encoder writes no metadata. A picture of fine detail that is still
   past 990 000 bytes steps its quality down by 10 to 40, as Columba's
   compressor steps (`columba/app/src/main/java/network/columba/app/util/ImageUtils.kt:186-191`).
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

### Measured sizes and times

`cargo run --release -p loquax --features photos --example photo_cost`,
on the coder host (AMD Ryzen 9 7950X, musl release build), best of three.
A phone is several times slower; no phone was measured in slice one.

| picture | step | sent | prepare | decode of the original | thumbnail of what went (480 px) |
|---|---|---|---|---|---|
| 3456 x 3456, 11.9 MP, 2.9 MB camera JPEG | small | 800 x 800, 54 KB | 141 ms | 78 ms | 7 ms |
| | phone | 1600 x 1600, 279 KB | 247 ms | | 17 ms |
| | original | refused (2.9 MB) | | | |
| 3468 x 4624, 16 MP, 4.9 MB phone JPEG, turned | small | 600 x 800, 82 KB | 190 ms | 126 ms | 6 ms |
| | phone | 1200 x 1600, 403 KB | 272 ms | | 14 ms |
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
  carries one. The terminal and the window write the picture to
  `$LNMSG_HOME/photos/<peer>-<key>.<format>` and hand it to the system's
  viewer; the phone opens its viewer screen.
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
  (`SUM(LENGTH(packed))`, `loquax/src/store.rs:2342`).

## 5. Android

- The paperclip's menu has "Photo" above "My position": the system
  picker (`ACTION_GET_CONTENT`, `image/*`), no camera yet. The picked
  bytes, read off the UI thread up to 64 MB, go to the library
  (`Request::Photo`); the library's refusal is said on the status line.
- `MessageRow` carries `photo`, `photo_bytes`, `photo_w`, `photo_h` (the
  size it shows at, upright, from the picture's header) and
  `photo_words`. The row draws the thumbnail first in the bubble (1268),
  240 dp on the longer side (less where the bubble is narrower), its shape the picture's, from
  `Query::Photo { peer, key, edge }`, which answers RGBA rows that
  `Bitmap.copyPixelsFromBuffer` takes as they are; a cache of 16 MiB
  keeps them. A tap opens `PhotoActivity`, which asks for the screen's
  longer side and shows the Bitmap; a tap or Back closes it.
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
  walk's `image` step drives the paperclip, the picker, the thumbnail and
  the viewer on a phone ([the device walk](loquax-device-walk.md)).

## 7. Open

- **The propagation limit.** A stock propagation node takes at most
  256 KB a transfer (`PROPAGATION_LIMIT`,
  `reference/LXMF/LXMF/LXMRouter.py:55`). A `phone` photo of a 12 MP
  picture is about 280 KB, so it can be refused when it goes through a
  propagation node to a peer who is offline. Directly and through a
  Loquax Mailbox it goes. Whether `phone` should aim under 256 KB is
  Lew's call.
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

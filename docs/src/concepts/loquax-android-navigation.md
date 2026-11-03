# Loquax on Android: navigation and views

Status: concept, decided by Lew on 2026-10-31 after a brainstorming with
the reviewer (order 1198, Refs #232). The implementation is orders 1199
to 1201; the rules below are their acceptance. The colours and sizes are
the ones of [the user interface](loquax-ui.md) (1129), the map is
[the map](loquax-map.md).

## The problem

Loquax is for the 90 % of users who never heard of Reticulum. Today the
phone grew one screen at a time and it shows: the chat list has an avatar,
a title, search and a menu in its bar; the map has a moon, "All" and
"Load area" in a row of buttons and a status line; the map is reached
from a menu item. Nothing tells a new user that the map is a place of its
own, and the two headers look like two apps. This page fixes the shape
every view takes, so that the next view fits without a new decision.

## Two levels only

- **Main views** sit side by side under a **bottom tab bar**: Chats and
  Map, Calls later. Switching tabs keeps each view's state (scroll
  position, map viewport).
- **Sub views** are pushed on top of a main view: a chat, the profile,
  the settings, a contact, a settings page. They have a back arrow at the
  top left and no tab bar. Back (arrow or system gesture) returns to
  exactly where the user came from, including from the map to a chat.

There is no third level of navigation: a settings page may scroll, but it
opens no sub view of its own beyond a dialog or a bottom sheet.

## The top app bar

Every main view has the same bar, in the same place, of the same height:

```text
+----------------------------------+
| (LP)•  Chats              Q   :  |
+----------------------------------+
```

- **The user's avatar circle** at the left, the one the chat list draws
  for a contact. A tap opens the profile (1187).
- **The status dot** on the avatar's lower right edge: green while the
  mailbox link is up, grey otherwise. No words in the bar. The profile
  says it in full: "Mailbox on <host>, connected since <time>", or since
  when it is not.
- **The title**: the tab's name.
- **A search icon**: on Chats it searches conversations; on Map it finds
  a friend or a place in the loaded regions.
- **A three dot menu** with at most two entries: Settings and Help.
  Refresh goes (the mailbox link pulls by itself), the map leaves the menu
  for the tab bar, the archive moves to the end of the chat list where its
  row already is.

A sub view's bar keeps the height and the typography, with the back
arrow in place of the avatar and the view's own actions at the right.

## The tab bar

```text
+----------------------------------+
|    [chat]             (pin)      |
|    Chats               Map       |
+----------------------------------+
```

An icon with a label under it, the selected tab in the accent colour, an
unread count as a badge on Chats. Two tabs today; Calls becomes the third
when it exists, never as a disabled placeholder.

## Chats

```text
+----------------------------------+
| (LP)•  Chats              Q   :  |
+----------------------------------+
| (AN) Anna                  12:04 |
|      Bis gleich!              2  |
| (BO) Bob                   11:30 |
|      Position · 3                |
| (CA) Carla                   Mo  |
|      Danke                       |
|                                  |
|                                  |
|                                  |
|                            (/)   |
+----------------------------------+
|    [chat]             (pin)      |
|    Chats               Map       |
+----------------------------------+
```

The list as it is now (rows, long press, pins, archive), under the common
bar. The pencil stays the round button bottom right. It opens "New
conversation": "Scan a code" first, "Paste a code" second, the typed
address under a closed Advanced (1263).

## Map

The map gets the whole screen between the bar and the tab bar. The moon,
"All" and "Load area" go.

```text
+----------------------------------+
| (LP)•  Map                Q   :  |
+----------------------------------+
|   [ Loading Berlin ... 34 % ]    |
|                                  |
|        (AN)                      |
|        5 min      (3)            |
|                                  |
|              (BO)                |
|              1 h                 |
|                            (+)   |
| (c) OpenStreetMap          (@@)  |
+----------------------------------+
|    [chat]             (pin)      |
|    Chats               Map       |
+----------------------------------+
```

- **Regions load by themselves** where the user looks (1089), and the user
  sees it: a small chip at the top edge, "Loading Berlin … 34 %", which
  disappears by itself when the region is in. Tiles appear progressively,
  the coarser steps scaled up first; nothing is ever black (1143).
- **Chips appear only when something happens**: "Loading …", "Offline",
  "No map here yet". Otherwise there is no chrome on the map apart from
  the two round buttons and the map credit.
- **Map theme** moves to Settings, Appearance: "Map: like the phone /
  light / dark". The config key of 1133 stays; only its control moves.

### The two round buttons

Bottom right, stacked, in the one round button style:

- **Crosshair** `(+)`. First tap: centre on the phone's own position and
  zoom to about z16. Second tap: follow mode, the map moves with the phone
  until the user pans. The first tap ever asks for the location
  permission (see below).
- **Friends** `(@@)`, a people icon. Opens a bottom sheet:

```text
+----------------------------------+
|              ----                |
|  Friends                         |
|  (AN) Anna       5 min    1.2 km |
|  (BO) Bob        1 h      4.8 km |
|  (DI) Dieter     9 h       31 km |
+----------------------------------+
```

  Everyone who sent a position in the last 24 h, newest first, with
  avatar, name, age and distance from the phone (no distance without the
  phone's own fix). A tap closes the sheet and flies the map to them, with
  their card open. Empty, the sheet says how a friend gets on the map and
  offers "Share my position".

### Pins and the card

- **Pins are avatar circles**, the same circle as the chat list's, with
  the age as a small label under it ("5 min", "1 h"). The red dot and the
  hash go.
- **Overlapping pins cluster** into one circle with a count, `(3)`. A tap
  zooms in until they separate, or, at the deepest zoom, opens the
  Friends sheet filtered to them.
- **A tap on a pin** opens a small card at the bottom:

```text
+----------------------------------+
|  (AN) Anna                       |
|       5 min ago · 1.2 km         |
|   [ Chat ]        [ Navigate ]   |
+----------------------------------+
```

  **Chat** opens the conversation. **Navigate** hands a `geo:` intent to
  the phone's navigation app; Loquax builds no routing.
- **From a chat**, a tap on a position bubble opens the map focused on
  that friend with the card open. Back returns to the chat.
- **From the list** (1224), a row whose friend has a position younger
  than a day carries a small map pin under the time; a tap on it switches
  to the Map tab centred on that friend with the card open, and Back
  returns to the chats. The newest report's line in a chat does the same.
- **Empty map**: one line and one button, "No friends on the map yet" and
  "Share my position".

## Location: the permission flow

Lew, 2026-10-31: Loquax may read the phone's location. Rules:

1. The permission is asked at the first tap on the crosshair or on "Share
   my position", never at start and never from the welcome screen.
2. A short line before the system dialog says why: "To show where you
   are on the map. Loquax sends your position only when you share it."
3. Refused, the crosshair keeps working as "centre on the last place I
   looked at" and a chip says "Location is off"; the next tap may ask
   again as long as Android lets it.
4. **Sending is always explicit.** The position leaves the phone only when
   the user shares it: from the map's empty state button, from a pin's
   card ("Share mine" with that friend), or from the chat's attachment
   menu. Reading the location for the crosshair sends nothing.
5. Sharing for a while (1273, Lew 2026-11-01: once, for a certain time,
   or until stopped, and ending it simple): the chat's position sheet
   offers "Now, once", "For 15 minutes", "For 1 hour", "For 8 hours" and
   "Until I stop". While a share runs the chat shows a chip at its top
   ("Sharing your position, 42 min left", or "… until you stop") with a
   Stop, the list's row a small ⌖ in the accent colour, Settings, Privacy,
   "Position sharing" lists every running share with its Stop, and a
   foreground service shows "Loquax shares your position" with a Stop of
   its own. The rules and the numbers are in [the map](loquax-map.md),
   "Sharing for a while". The battery side is measured by tier R (1177)
   afterwards, not assumed.

## Settings

After the modern messenger cut, one list, each entry a settings page:

```text
Settings
  Profile         the profile behind the avatar (1187), below
  Mailbox         where it lives, connected since, join another
  Appearance      app theme; Map: like the phone / light / dark
  Notifications   on/off, sound, per chat muting, the connection notice
  Privacy         read receipts (1233, first), blocked contacts, positions shared
  Maps            regions with sizes, delete, automatic download on/off
  Backup          back up the mailbox, restore
  Advanced        Relay (the propagation node), the map's fling, stop the service, logs
```

"Load area" becomes the Maps entry: management, not an action on the
map. Help is its own entry in the three dot menu, not a settings page.

The connection notice (1345) is the foreground service's notification,
"Connected to your mailbox", which Android does not let a service run
without. It carries no unread count (each conversation's notification
has its own), and it is posted again only when its sentence changes
(connected, not connected, the mailbox on this phone), so a swipe on
Android 14 and later holds until the next change. Notifications,
"Connection notice" opens the channel's system page, where the user
picks minimised or off; on Android 13 and earlier, where the notice
cannot be swiped, that is the way to quiet it. The app cannot set the
channel below LOW itself: Android lifts a foreground service's channel
to LOW unless the user lowered it (`LoquaxService.channels`). After a
reboot `BootReceiver` starts the service again when the user had it
running, that is, it ran and was not ended by its Stop.

## The profile

1187, after the modern messenger layout: the avatar at the top left of
the chat list opens our own screen, and so does Settings, Profile. The
design rule is the end user one: a user who knows nothing of Reticulum
sees the name, the address, a way to hand it over and where the mailbox
is; what needs Reticulum knowledge sits under Advanced, collapsed.
Columba's "Your Identity" page was the reference; ours is cut to Loquax.

```text
Your profile
  avatar, large
  name             the name the announce carries; edited in place with
                   the mailbox on this phone, else "set where your
                   mailbox runs"
  Your address     the LXMF address, monospaced, selectable    [Copy]
  QR code          on a white card: the contact code
                   lxma://<address>:<public key>
                   "Scan this code to add me as a contact"
  [Share]          the system share sheet with the contact code as text
  [Share as picture]  the QR code as a PNG (QrProvider)
  Copy address
  Scan a contact's code   the scanner: lxma:// adds a contact, lqxh1 joins
  Mailbox: on this phone | on <host>, since <date>
  Use another mailbox     the join form
  Advanced ▾       identity hash, home node, Relay: the propagation node
                   (or "Chosen by your mailbox"), backup, app version
```

The word on the screen is "Relay" wherever the phone names the
propagation node (1283, walk finding 30), as the onboarding draft's
section 1 asked; the rest of Advanced keeps its words.

The contact code is the one Columba and Sideband read (in Columba's
checkout: `encodeToQrString` of `IdentityQrCodeUtils`, and
`LXMF_IDENTITY_PREFIX` and `PUBLIC_KEY_LENGTH`, 64, of
`ValidationConstants`): the address and the identity's public key as
lowercase hex. `loquax::lxma` writes and
parses it once for every frontend; its test pins the string byte for
byte against Columba's encoder, and the QR code is byte mode, verbatim,
because Columba compares the prefix case for case.

A scanned or pasted code (the pencil's "Scan a code" and "Paste a code"
take one too) asks for the contact's name, keeps it as the reader's name
for the address, hands the key to the mailbox (`/key`,
docs/src/concepts/loquax-home-node.md) and opens the chat. The facade has
`Query::Profile`, `Query::Lxma` (what a code is and whose, before the
name is asked) and `Request::AddContact`.

One reader takes every code (1263, finding 1 of the ninety percent walk):
`loquax::code::read`, used by the phone's scanners and pencil, the
terminal's palette and the window's field. It takes a contact code, an
address alone (32 hex digits, either case, groups of four, `lxmf@`; the
first start's QR held one before 1263, and an old screenshot must still
work) and a mailbox's `lqxh1` code, which goes to the join. A text that
is none is said in one sentence of the catalogue: "That is not a Loquax
code." or, for one of ours that is damaged (a contact code whose address
is not its key's, a cut code), "This code is damaged. Ask for it again."
The scanner shows that sentence instead of ignoring the code. The first
start's "Your address" shows the profile's contact code as its QR, and
its Share sends it.

No new colours: the card is white because a QR code is black on white,
and every text on the screen uses the background's text and secondary
text colours of the contrast table in `res/values/colors.xml`.

## Consistency rules

Every future view follows these, and a review refuses a view that does
not:

- One top app bar style (above); one tab style; one round button style,
  bottom right (the pencil on Chats, the crosshair and Friends on Map).
- The typography and colour tokens of 1129 (`loquax/src/ui/chat.rs`
  `LIGHT`, `DARK`, `size`), in both themes; no colour outside the table.
- Controls in the lower third, information at the top.
- Chips for transient state, the status dot for the link, never a
  permanent status line.
- An avatar circle for every person, wherever a person appears.

## Everyday actions

Ranked in the brainstorming. Targets: 1 to 5 within one tap, 6 within
two, 7 may be deep. Taps counted from the main view the user is on.

| # | Action | Taps today | Taps after |
|---|---|---|---|
| 1 | Read and answer | 1 (a row) | 1 (a row) |
| 2 | See where friends are, jump there | 2 (menu, Map) plus panning; no list | 1 (Map tab), 2 to fly to one (Friends, name) |
| 3 | Find a chat | 1 (search) | 1 (search) |
| 4 | Share own position | not possible on the phone | 1 on the map's empty state or a card, 2 from a chat (attach, position) |
| 5 | Know everything is connected | 0, but as words in a status line | 0, the dot on the avatar |
| 6 | Add someone (QR) and start a chat | no contact QR; pencil and paste an address | 2 (pencil, Scan a QR code) |
| 7 | Settings, regions, backup | 2, 3 (Map, Load area), 3 | 2, 3 (Settings, Maps), 3 |

Action 2 counts the tab as one tap and the Friends sheet as the second;
a friend already on screen is one tap away from the map (the pin).

## Deferred

- **The Calls tab**, with LXST; it appears when calls work.
- **Live sharing** was here; Lew lifted it on 2026-11-01 (1273, above).
  Its cost, the radio and the GPS kept awake, is measured by tier R
  (1177) afterwards, as for the keepalives of
  [the home node](loquax-home-node.md).
- **Routing**: never ours; Navigate hands off to the phone's app.

## The slices

- **1199: the frame.** Bottom tab bar with Chats and Map, the common top
  app bar (avatar with status dot, title, search, menu with Settings and
  Help), back behaviour for sub views, Refresh and Map out of the menu.
  Map gets the whole screen; moon, "All" and "Load area" go; the map
  theme control moves to Settings, Appearance; the settings list gets its
  eight entries (pages that do not exist yet show what exists today);
  1220 added a ninth, Chats, with the photo size.
  Built in 1199 with two choices of its own: on the Map tab the search
  icon stays hidden until the Friends sheet of 1200 gives it something to
  find, and the map's one chip says "Loading the map of this area…" or
  a load's failure without the region's name and progress, which 1200's
  loading chip adds. The status dot is the list's `home_link`, the
  watching link of the session (`loquax-android/src/schema.rs`).
- **1200: the map.** Avatar pins with age labels, clustering, the pin
  card with Chat and Navigate (`geo:`), the chat bubble to map to back
  path, the loading and offline chips, the Friends button and sheet.
  Built in 1200 with the reviewer's answers below and these choices of
  its own: the pins are drawn by the app over the library's frame (the
  library draws none when asked with `avatars` and says where they lie),
  so a pin is the list's avatar circle with its photo; a cluster is a
  circle in the badge colours with its count, sitting on its first pin;
  two pins cluster when their centres lie closer than one circle (40 dp),
  a pin joining the first cluster whose first pin is that close; a
  cluster's tap flies to the zoom where its closest pair lies a circle and
  a tenth apart, and at a zoom deeper than the regions allow opens the
  filtered sheet. The Friends sheet is the map's search too (friends
  only). The loading chip still has no region name and progress: the
  loader knows neither (no place index, no progress from the cut); the
  Offline and No map here yet chips came. The empty map's line and
  "Share my position" wait for 1201, which sends.
- **1201: location.** The crosshair (centre, follow), the permission flow
  above, "Share my position" from the empty state, the card and the
  chat's attachment menu; the Maps settings page (regions, sizes, delete,
  automatic download).
  Built in 1201 with these choices of its own: the position is read with
  Android's LocationManager (GPS and the network provider, the newest fix
  wins; no Play services), only while the map is in front and once for a
  share; a share takes the phone's last fix when it is under two minutes
  old, else waits up to 30 s for one. The crosshair's first tap without a
  fix yet centres on the first fix that comes; a finger, a pinch or a
  double tap ends both centring and follow mode; follow mode keeps the
  zoom the user has. The own position is an accent dot in a ring of the
  background, below the friends' pins. The empty map's "Share my
  position" picks the chat first (the empty map has no friend to send
  to), so it is two taps there; the card's "Share mine" and the chat's
  attachment menu (a paper clip in the compose field, one entry "My
  position" then; "Photo" joined it above in 1220,
  [photos](loquax-photos.md), and "File" in 1221,
  [files](loquax-files.md)) send at once. With nothing typed the send
  button is the microphone (1222, [voice messages](loquax-voice.md)):
  held it records, released it sends, slid left it cancels. A share is a report of ours with no
  text, the Telemeter alone, and the chat shows it as our bubble with the
  position line. The Maps page names a map by its middle ("Around 52.52°
  N, 13.40° E"), since the region files carry no place name; Delete asks
  first. Privacy got the list of the positions shared, to whom and when,
  read from our own messages.
  1290 (Lew) put one row at the foot of the region list, "Delete all
  downloaded maps (1.2 GB)", the total in its label and off while the
  list is empty. A tap asks once ("Delete all 7 maps? Areas you visit
  will be downloaded again when you look at them.", or with the
  automatic download off "They come back only when you load an area
  again."), then deletes every region file, the hand placed ones too, and
  the storage line says 0 B. The automatic download stays as it was. The
  action is the core's `Intent::DeleteAllRegions` (the phone sends it as
  `deleteregions`, `loquax::picture::Maps::delete_all` does it), and the
  window's settings page shows the same rows: the Maps title, the
  storage line, the row and its question with Delete all and Cancel. The
  terminal has no Maps page.

## Not decided (for Lew)

- **Search on the map**: friends only, or places in the loaded regions
  too (needs a place index the region files do not carry yet)?
- **Friends sheet window**: a fixed 24 h, or "since the last position of
  each contact" with older ones greyed?
- **Distance** without the phone's fix: hide it, or measure from the map
  centre?
- **Cluster tap** at the deepest zoom: filtered sheet (proposed) or a
  fan of pins?
- **"Share mine" on a card**: send to that friend only (proposed), or
  open a chooser?
- **Unread badge** on the Chats tab: count of messages or of chats?
- **Help**: a page in the app, or a link to the book on
  leviculum.network? Since 1278 the menu opens the user help page,
  leviculum.network/help, or /help-de on a German phone (`help_url`), not
  the book.

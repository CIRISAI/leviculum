# Golden pictures

`gui-names-florence-z16.png`: the window's map picture with the map's
names set into it (1100; redrawn in 1126, when a street's pieces were
joined and its name tries each straight run and further along it: five
street names where there were two; redrawn in 1144, when the window began
composing its frames from the tile cache as the phone does: the same names
in the same places, 10402 pixels differ: edge pixels along the lines and
the dashed paths, each tile drawn as a view of its own, and the names'
edges, blended as kept sprites rather than set into the whole view; to
the eye the same picture), drawn by `gui::map::tests::the_map_names_are_set_into_the_picture`
from `../../../leviculum-map/fixtures/firenze.lmr` at z16, 480 x 320, the
PC style, one pin. Compared pixel for pixel; a picture that moved is left
in the temp dir as `loquax-golden/<name>` for a look. The names are set in
Fira Sans as iced embeds it, so the picture does not depend on the host's
fonts.

The picture is a produced work of OpenStreetMap data: © OpenStreetMap
contributors, <https://www.openstreetmap.org/copyright>, under the Open
Database License (ODbL) 1.0; see `leviculum-map/fixtures/README.md`.

`gui-buildings-light-z16.png`, `gui-buildings-dark-z16.png`,
`gui-buildings-light-z17.png`, `gui-buildings-dark-z17.png`: the window's
map frame at rest with its names over the Florence cut with building
footprints (1192), drawn by
`gui::map::tests::the_window_draws_the_buildings_and_they_move_no_name`
from `../../../leviculum-map/fixtures/buildings/firenze.lmr`, 480 x 320,
`Style::SCREEN`, no pin. The phone's frame of the same view through its
facade is held to these same pictures, pixel for pixel
(`loquax-android/tests/map.rs`,
`the_phone_draws_the_buildings_as_the_window_does`). OpenStreetMap
data as above.

`gui-chat-light.png`, `gui-chat-dark.png`: the window's chat pane in each
theme (1145), drawn off screen by iced's software renderer (tiny-skia) by
`gui::tests::the_chat_pane_is_the_golden_in_both_themes`, 900 x 560 at a
density of one: two conversations, one open with a date line, two grouped
bubbles of theirs, our reaction's pill and our queued bubble, the compose
bar. Compared pixel for pixel as the map's picture is; the text is set in
the window's default font, Fira Sans as iced embeds it, and the picture
holds no emoji, which only a host's font would draw. Redrawn in 1218,
when the window began drawing the scene (`loquax::ui::scene`): 908
pixels in each theme, the first date line alone, which reads
"Yesterday" where the window's own date rule said "Thursday 29
October"; the scene's day words are the phone's. Redrawn again in 1218
when the window first showed the scene's shared structure: the tab rail
at the left (Chats with the unread count, Map), the reader's top bar
(the avatar with Mia's initials, "Chats", search, new conversation and
the theme toggle as drawn icons) in place of the window's own "Loquax"
bar with the words Map, Dark and New; the list and the chat moved right
by the rail's 72 pixels.

`gui-chat-compact-chat.png`, `gui-chat-compact-list.png`: the same
window in the Compact class (1218), 420 x 640, light, drawn by
`gui::tests::a_narrow_window_stacks_and_a_resize_switches`: the open
chat alone with a back in its bar, and once the model closed it the
list with the tabs at the bottom, as the phone stacks them.

`gui-recording-held.png`, `gui-recording-locked.png`,
`gui-recording-paused.png`: the window's recording bar (1276) over the
chat of `gui-chat-light.png`, 900 x 560, light, drawn by
`gui::tests::the_recording_bar_is_the_golden_held_locked_and_paused`:
held three seconds into a recording with the pointer 40 px left (the red
dot, `REC 00:03`, the live waveform, the cancel hint, the grown red
microphone and the open lock chip above it); locked seven seconds in,
after a three second voice message went whose bubble draws the
recording's own waveform (the bin, the pause, the big Send, the closed
lock); paused after three silent seconds (the hollow dot, the play, the
bin, resume, and the amber "Nothing heard" line with its Allow). The
pulse is a frame of the timer's clock, so each picture is the same when
taken again.


`scene-compact-list.json`, `scene-compact-chat.json`, `scene-wide.json`:
the scene of the list and the chat (1217, `loquax::ui::scene`) built from
the ugly store (1173) by `tests/scene_golden.rs`: Compact with no
conversation open, Compact and Wide with conversation 2 open. JSON with
sorted keys; texts are catalogue keys with their arguments, not resolved,
so one file pins both languages. Compared line for line; a scene that
moved is left in the temp dir as `loquax-golden/<name>`, and
`LOQUAX_WRITE_GOLDEN=1` writes the goldens for a change that means to move
them.

`gui-browse-list.png`, `gui-browse-page.png`: the window's Browse tab
(1369), lnomad's window view beside the rail, drawn off screen by
`gui::tests::the_browse_tab_is_lnomads_window_beside_the_rail`, 1100 x
640 in the light theme: the places list the tab opens on with two
NomadNet nodes, then lnomad's sample page (`lnomad/tests/fixtures/sample.mu`)
with a picture link under it and its picture, a gradient made by the test,
drawn by lnomad's picture code. Compared pixel for pixel as above; the page
is set in Fira Mono as lnomad embeds it.

`tui-rooms-room.txt`: the terminal's Rooms tab (1379) over the whole 80 x 24
screen: the hub's line, `#lab` in the list, the open room under its one line
on what a room is not, two joins, Bob's line and action, the hub's notice
and the echo of our own line as "you", drawn by
`tui::tests::the_rooms_tab_draws_the_open_room_under_its_one_line`.
Compared as text.

`gui-rooms-room.png`: the window's Rooms tab (1379) beside the rail, 1100 x
640, light: the hub's line, `#lab` selected in the list, the open room with
its one line, two joins, Bob's line and ours as "you", and the input, drawn
from the scene's Rooms node by
`gui::tests::the_rooms_tab_is_the_scenes_rooms_beside_the_rail`. Compared
pixel for pixel.

`tui-browse-list.txt`, `tui-browse-page.txt`: the terminal's Browse tab
(1369), lnomad's terminal view over the whole 80 x 24 screen, the same two
screens without the picture, drawn by
`tui::tests::the_browse_tab_is_lnomads_terminal_screen`. Compared as
text; one that moved is left in the temp dir likewise.

`gui-tracks-hiking.png`, `gui-tracks-park.png`, `gui-tracks-park-dark.png`,
`gui-tracks-convoy.png`, `gui-tracks-week-old.png`,
`gui-tracks-outlier.png`, `gui-tracks-handbag.png`: the six track scenes
of `loquax::ui::tracks::scenes` (1385, `docs/src/concepts/loquax-tracks.md`)
through the window's own frames over the Florence fixture, 480 x 320,
`Style::SCREEN`, the pins drawn in, drawn by
`gui::map::tests::the_track_scenes_are_the_goldens`. Most of each scene
lies outside the fixture's box, so the ground there is the outline's
plain land: the tracks are what the pictures pin. OpenStreetMap data as
above where the box shows.

`tracks-hiking.txt` and the five like it: the same scenes in the
terminal's braille cells, 72 x 22, the tracks and the pins as the text
frame draws them, drawn by
`ui::tracks::scenes::tests::the_track_scenes_in_cells_are_the_goldens`;
`LOQUAX_WRITE_GOLDEN=1` writes them anew.
Redrawn in 1447 (walk 1 finding 7): a pin's age is the catalogue's, "13 min
ago", "now", where it was "13 min", "<1 min"; the quiet line ("no fix for
3 h") under dino's pin is gone, the pin's own age says it; every
`gui-tracks-*.png` moved by those words alone (hiking 3265 pixels, park
1485, radar 513, the others alike).

`gui-tracks-radar.png`, `gui-tracks-way-back.png`, `gui-tracks-late.png`,
`gui-tracks-battery.png`, `gui-tracks-fit-all.png`, and
`tracks-radar.txt` and the four like it: 1386's five scenes
(`loquax::ui::tracks::scenes::marks`) in the window's pixels over the
Florence fixture, drawn by `gui::map::tests::the_marks_scenes_are_the_goldens`,
and in cells, drawn by
`ui::tracks::scenes::tests::the_marks_scenes_in_cells_are_the_goldens`.
The radar scene lies on the Seceda ridge, where no region is, so its
ground is the radar's blank one with its rings.

`gui-map-window.png`, `gui-map-window-menu.png`, `gui-map-window-week.png`:
the window's map screen (1419) beside the rail, 900 x 560, light, the
chat fixture's model with no map frame drawn: the bar with no time strip
and the title "Map"; the map's menu open over the map's corner, "Time
window" with the six windows, the current movement marked, and "Fit all";
the bar's title "Map · This week" once that window is chosen. Drawn by
`gui::tests::the_time_window_is_in_the_maps_menu`.

`scene-group-ticks.json`, `tui-group-ticks.txt`, `gui-group-ticks.png`:
the group's ticks tell the truth too (1466). "Familie" with Anna and Ben
and two lines of ours, from one fixture (`tests/support/group_ticks.rs`,
included by every target): "Brot ist da", which every member's device
said it has (two ticks, `delivered`, "On the devices"), and "Wer kommt
mit?", every copy proven but Ben's device silent (one tick, `arrived`).
Drawn by `scene_golden::a_groups_one_and_two_ticks_are_the_golden_scene`
(the Compact scene, as JSON),
`tui::tests::a_groups_one_and_two_ticks_are_the_terminals_golden` (80 x 24,
as text) and `gui::tests::a_groups_one_and_two_ticks_are_the_windows_golden`
(900 x 560, light, pixel for pixel). The M9's frame of the same scene is
`loquax-m9/tests/golden/m9-group-ticks.png`, the phone's rows and Info
`loquax-android/tests/golden/group-ticks.json`.

`scene-relay-group.json`, `tui-relay-group.txt`, `gui-relay-group.png`: a
relayed post shows its writer (1482). A relay bot, "ReticulumNet NL", hands
us Anna's and Ben's posts with its default prefix `Name\n<hash>\n` in front,
from one fixture (`tests/support/relay_group.rs`): each post under its
writer and without the prefix, the bar "ReticulumNet NL · group via relay",
Ben's reaction to a message this device does not hold said in words, our
own line, and Anna's reply quoting "Ben: Moin Anna". Pinned by
`scene_golden::a_relayed_group_is_the_golden_scene`,
`tui::tests::a_relayed_group_is_the_terminals_golden` (80 by 24, as text;
the terminal names the writer in the line and draws no quotes) and
`gui::tests::a_relayed_group_is_the_windows_golden` (900 x 560, light,
pixel for pixel). The phone's title, rows and Info in German are
`loquax-android/tests/golden/relay-group.json`.

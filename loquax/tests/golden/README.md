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

`gui-chat-light.png`, `gui-chat-dark.png`: the window's chat pane in each
theme (1145), drawn off screen by iced's software renderer (tiny-skia) by
`gui::tests::the_chat_pane_is_the_golden_in_both_themes`, 900 x 560 at a
density of one: two conversations, one open with a date line, two grouped
bubbles of theirs, our reaction's pill and our queued bubble, the compose
bar. Compared pixel for pixel as the map's picture is; the text is set in
the window's default font, Fira Sans as iced embeds it, and the picture
holds no emoji, which only a host's font would draw.


`scene-compact-list.json`, `scene-compact-chat.json`, `scene-wide.json`:
the scene of the list and the chat (1217, `loquax::ui::scene`) built from
the ugly store (1173) by `tests/scene_golden.rs`: Compact with no
conversation open, Compact and Wide with conversation 2 open. JSON with
sorted keys; texts are catalogue keys with their arguments, not resolved,
so one file pins both languages. Compared line for line; a scene that
moved is left in the temp dir as `loquax-golden/<name>`, and
`LOQUAX_WRITE_GOLDEN=1` writes the goldens for a change that means to move
them.

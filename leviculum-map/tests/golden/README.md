# Golden images

The pictures and cell dumps `../golden.rs` holds the map's draw to, one
per profile (1099): drawn from `../../fixtures/firenze.lmr`, compared pixel
by pixel. The `screen-*` pictures are the buildings (1192), drawn from
`../../fixtures/buildings/firenze.lmr` at the window's and the phone's
line widths. A change that moves one leaves this run's picture in
`$TMPDIR/leviculum-golden/`; look at it, and copy it here when the change
is the one intended.

**© OpenStreetMap contributors**, <https://www.openstreetmap.org/copyright>.
The pictures are produced works of a database under the Open Database
License (ODbL) 1.0, <https://opendatacommons.org/licenses/odbl/1-0/>, and
carry its attribution, as `../../fixtures/README.md` says.

The `world-*` pictures and cells (1683) are the world view where no
region covers it: central Europe at z2 and z6, drawn from the crate's own
outline tables (`../../data/`, not from a region) the way the M9 and the
window draw it, and as the terminal's cells. Their data is Natural Earth
5.1.2, public domain, and the drawer credits "Natural Earth" on screen as
it credits OpenStreetMap over a region (`../../fixtures/README.md`, "The
world outline").

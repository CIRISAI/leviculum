#!/usr/bin/env python3
"""Writes Loquax's icon, the jackdaw (1289's candidate 2, Lew 2026-11-02),
everywhere it is used, from one geometry and the suite's chrome colours.

    python3 -I loquax-android/scripts/render-icon.py           # write
    python3 -I loquax-android/scripts/render-icon.py --check   # compare

The geometry is drawn on Android's adaptive-icon canvas: 108 x 108 units,
of which a launcher shows the inner 72 under its mask and keeps the circle
of radius 33 around (54, 54) safe; the bird stays inside that circle. Its
colours are read from leviculum-ui/src/pixel.rs (`LIGHT`): the bird in the
chrome's ground (`background`) on the chrome's `accent`, so a change of
the accent there moves the icon with it at the next run.

What it writes, from the tree's root:

- loquax-android/icon/loquax.svg and its layers (foreground, background,
  monochrome): the sources, as SVG.
- The Android resources: the adaptive icon (mipmap-anydpi-v26), its
  foreground and monochrome as vector drawables, its background colour,
  and the legacy ic_launcher.png for every mipmap density.
- packaging/loquax/loquax.png, 256 px, for the Linux package.
- leviculum-ui-iced/assets/window-icon.png, 128 px, the windows' icon.
- loquax-m9/assets/splash-logo.rgba, 112 px, the M9's power-on splash
  (1485): the same render as raw RGBA, 8 bits a channel, not
  premultiplied, row by row, no header (50,176 B), because the board has
  no PNG decoder; `loquax_m9::splash` lays it on the panel's ground.

The PNGs are rendered by resvg (0.45.1 measured; `sudo apt install
resvg`), which writes the same bytes for the same SVG, so `--check`
re-renders into target/tmp and fails on any file that differs from the
committed one. Flat colour and plain geometry only: no fonts, no rasters,
no gradients.
"""

import re
import shutil
import struct
import subprocess
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRATCH = ROOT / "target" / "tmp" / "render-icon"
ICON = ROOT / "loquax-android" / "icon"
RES = ROOT / "loquax-android" / "android" / "res"
PIXEL_RS = ROOT / "leviculum-ui" / "src" / "pixel.rs"

# The legacy launcher icon is 48 dp; one PNG per density.
DENSITIES = {
    "mdpi": 48,
    "hdpi": 72,
    "xhdpi": 96,
    "xxhdpi": 144,
    "xxxhdpi": 192,
}
PACKAGE_PNG = (ROOT / "packaging" / "loquax" / "loquax.png", 256)
WINDOW_PNG = (ROOT / "leviculum-ui-iced" / "assets" / "window-icon.png", 128)
# The M9's splash (1485): raw RGBA, its size `loquax_m9::splash::LOGO_PX`.
SPLASH_RGBA = (ROOT / "loquax-m9" / "assets" / "splash-logo.rgba", 112)

# The jackdaw in profile, beak open: head, the two mandibles, the body with
# its tail, then the legs as strokes and the eye in the ground's colour.
# (kind, geometry): "circle" is (cx, cy, r), "path" an SVG path.
BODY = (
    ("circle", (61, 39, 10)),
    ("path", "M68,33.5 L82,37.5 L68.5,39.5 Z"),
    ("path", "M68.5,42 L80,46 L67,47.5 Z"),
    ("path", "M54,32 C43,38 37,50 40,62 L30,77 L46,71 C58,72 70,63 68,44 Z"),
)
LEGS = "M51,70 L49,80 M58,68 L58,80"
LEG_WIDTH = 3
EYE = (64, 37, 2.2)

# What a launcher shows: the inner 72 units under a circular mask.
LAUNCHER_VIEW = "18 18 72 72"


def chrome_light():
    """The light chrome's accent and ground, as #rrggbb, from pixel.rs."""
    text = PIXEL_RS.read_text()
    block = re.search(r"pub const LIGHT: Chrome = Chrome \{(.*?)\};", text, re.S)
    if block is None:
        sys.exit(f"render-icon: no `pub const LIGHT: Chrome` in {PIXEL_RS}")

    def field(name):
        m = re.search(rf"\b{name}: 0xff([0-9a-fA-F]{{6}}),", block.group(1))
        if m is None:
            sys.exit(f"render-icon: no opaque `{name}` in LIGHT ({PIXEL_RS})")
        return "#" + m.group(1).lower()

    return field("accent"), field("background")


def circle_path(cx, cy, r):
    """A circle as path data, for the vector drawable, which has no circle."""
    return (
        f"M{cx - r:g},{cy:g} a{r:g},{r:g} 0 1,0 {2 * r:g},0 "
        f"a{r:g},{r:g} 0 1,0 {-2 * r:g},0 Z"
    )


def svg(body, view="0 0 108 108"):
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{view}" '
        f'width="108" height="108">\n{body}\n</svg>\n'
    )


def svg_bird(ink, eye):
    shapes = []
    for kind, g in BODY:
        if kind == "circle":
            shapes.append(f'<circle cx="{g[0]}" cy="{g[1]}" r="{g[2]}"/>')
        else:
            shapes.append(f'<path d="{g}"/>')
    out = (
        f'<g fill="{ink}">{"".join(shapes)}</g>'
        f'<path d="{LEGS}" stroke="{ink}" stroke-width="{LEG_WIDTH}" '
        'stroke-linecap="round" fill="none"/>'
    )
    if eye is not None:
        out += f'<circle cx="{EYE[0]}" cy="{EYE[1]}" r="{EYE[2]}" fill="{eye}"/>'
    return out


def svg_ground(fill):
    return f'<rect x="0" y="0" width="108" height="108" fill="{fill}"/>'


def svg_composed(accent, ground):
    """The icon as a round launcher shows it: the inner 72 units, a circle."""
    return svg(
        '<clipPath id="launcher"><circle cx="54" cy="54" r="36"/></clipPath>'
        f'<g clip-path="url(#launcher)">{svg_ground(accent)}'
        f"{svg_bird(ground, accent)}</g>",
        view=LAUNCHER_VIEW,
    )


def vector(ink, eye):
    """The bird as an Android vector drawable on the 108 dp canvas."""
    paths = []
    for kind, g in BODY:
        data = circle_path(*g) if kind == "circle" else g
        paths.append(f'  <path a:fillColor="{ink}" a:pathData="{data}"/>')
    paths.append(
        f'  <path a:strokeColor="{ink}" a:strokeWidth="{LEG_WIDTH}" '
        f'a:strokeLineCap="round" a:pathData="{LEGS}"/>'
    )
    if eye is not None:
        paths.append(f'  <path a:fillColor="{eye}" a:pathData="{circle_path(*EYE)}"/>')
    return (
        "<?xml version='1.0' encoding='utf-8'?>\n"
        "<!-- Written by loquax-android/scripts/render-icon.py; edit the script. -->\n"
        "<vector xmlns:a='http://schemas.android.com/apk/res/android'\n"
        "        a:width='108dp' a:height='108dp'\n"
        "        a:viewportWidth='108' a:viewportHeight='108'>\n"
        + "\n".join(paths)
        + "\n</vector>\n"
    )


ADAPTIVE = """<?xml version='1.0' encoding='utf-8'?>
<!-- Written by loquax-android/scripts/render-icon.py; edit the script.
     The jackdaw on the accent; the monochrome layer is what Android 13 on
     tints for a themed home screen. -->
<adaptive-icon xmlns:a='http://schemas.android.com/apk/res/android'>
  <background a:drawable='@color/ic_launcher_background'/>
  <foreground a:drawable='@drawable/ic_launcher_foreground'/>
  <monochrome a:drawable='@drawable/ic_launcher_monochrome'/>
</adaptive-icon>
"""


def background_colour(accent):
    return (
        "<?xml version='1.0' encoding='utf-8'?>\n"
        "<!-- Written by loquax-android/scripts/render-icon.py: the adaptive\n"
        "     icon's ground, leviculum_ui::pixel::LIGHT.accent. -->\n"
        "<resources>\n"
        f'    <color name="ic_launcher_background">{accent}</color>\n'
        "</resources>\n"
    )


def sources(accent, ground):
    """Every text file the script writes, by path."""
    return {
        ICON / "loquax.svg": svg_composed(accent, ground),
        ICON / "loquax-foreground.svg": svg(svg_bird(ground, accent)),
        ICON / "loquax-background.svg": svg(svg_ground(accent)),
        ICON / "loquax-monochrome.svg": svg(svg_bird("#000000", None)),
        RES / "mipmap-anydpi-v26" / "ic_launcher.xml": ADAPTIVE,
        RES / "drawable" / "ic_launcher_foreground.xml": vector(ground, accent),
        RES / "drawable" / "ic_launcher_monochrome.xml": vector("#000000", None),
        RES / "values" / "ic_launcher.xml": background_colour(accent),
    }


def rasters():
    """Every PNG the script renders from loquax.svg, by path and size."""
    out = {
        RES / f"mipmap-{d}" / "ic_launcher.png": px for d, px in DENSITIES.items()
    }
    out[PACKAGE_PNG[0]] = PACKAGE_PNG[1]
    out[WINDOW_PNG[0]] = WINDOW_PNG[1]
    return out


def png_rgba(path):
    """The pixels of an 8-bit RGBA, non-interlaced PNG (what resvg writes),
    as raw RGBA bytes row by row: the five scanline filters undone."""
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        sys.exit(f"render-icon: {path} is no PNG")
    at, idat, head = 8, b"", None
    while at < len(data):
        (n,) = struct.unpack(">I", data[at : at + 4])
        kind, body = data[at + 4 : at + 8], data[at + 8 : at + 8 + n]
        at += 12 + n
        if kind == b"IHDR":
            head = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat += body
    if head is None or head[2:] != (8, 6, 0, 0, 0):
        sys.exit(f"render-icon: {path} is not 8-bit RGBA non-interlaced: {head}")
    width, height = head[0], head[1]
    raw, stride, bpp = zlib.decompress(idat), head[0] * 4, 4
    out, prev = bytearray(), bytearray(stride)
    for y in range(height):
        kind = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1 : (y + 1) * (stride + 1)])
        for i in range(stride):
            a = line[i - bpp] if i >= bpp else 0
            b = prev[i]
            c = prev[i - bpp] if i >= bpp else 0
            if kind == 1:
                line[i] = (line[i] + a) & 0xFF
            elif kind == 2:
                line[i] = (line[i] + b) & 0xFF
            elif kind == 3:
                line[i] = (line[i] + (a + b) // 2) & 0xFF
            elif kind == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + pred) & 0xFF
            elif kind != 0:
                sys.exit(f"render-icon: {path}: filter {kind} in row {y}")
        out += line
        prev = line
    assert len(out) == width * height * 4
    return bytes(out)


def render(src, size, out):
    out.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["resvg", "-w", str(size), "-h", str(size), str(src), str(out)],
        check=True,
    )


def main():
    check = sys.argv[1:] == ["--check"]
    if sys.argv[1:] not in ([], ["--check"]):
        sys.exit("usage: render-icon.py [--check]")
    if shutil.which("resvg") is None:
        sys.exit("render-icon: resvg not found; sudo apt install resvg")
    accent, ground = chrome_light()
    texts = sources(accent, ground)
    if SCRATCH.exists():
        shutil.rmtree(SCRATCH)
    SCRATCH.mkdir(parents=True)
    master = SCRATCH / "loquax.svg"
    master.write_text(texts[ICON / "loquax.svg"])
    differ = []
    for path, text in texts.items():
        if check:
            if not path.exists() or path.read_text() != text:
                differ.append(path)
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
    for path, size in rasters().items():
        out = SCRATCH / "png" / path.relative_to(ROOT)
        render(master, size, out)
        if check:
            if not path.exists() or path.read_bytes() != out.read_bytes():
                differ.append(path)
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(out, path)
    path, size = SPLASH_RGBA
    out = SCRATCH / "splash.png"
    render(master, size, out)
    raw = png_rgba(out)
    if check:
        if not path.exists() or path.read_bytes() != raw:
            differ.append(path)
    else:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(raw)
    shutil.rmtree(SCRATCH)
    for path in differ:
        print(f"render-icon: {path.relative_to(ROOT)} differs from the script's")
    if differ:
        sys.exit(1)


if __name__ == "__main__":
    main()

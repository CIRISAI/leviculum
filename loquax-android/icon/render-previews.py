#!/usr/bin/env python3
"""Writes the three Loquax icon candidates (1289) and renders their previews.

Each candidate is hand-drawn geometry on Android's adaptive-icon canvas:
108 x 108 units, of which a launcher shows the inner 72 (18..90) under its
mask and keeps the inner circle of radius 33 around (54, 54) safe. Every
motif stays inside that circle. The geometry lives here once; this script
writes it out as the layers Android wants (background, foreground,
monochrome) and as the composed icon the launcher shows, for the light
and the dark theme, then renders previews with resvg.

    python3 -I loquax-android/icon/render-previews.py

Flat colour and plain geometry only: no fonts, no rasters, no gradients,
so a render is the same at 16 px and at 512 px and in every renderer.
"""

import shutil
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent
SCRATCH = HERE.parents[1] / "target" / "tmp" / "icon-previews"

# The app's accent (values/colors.xml, values-night/colors.xml) and the
# grounds: near-white, and near-black a step above the dark theme's
# background so the icon's edge shows on a dark home screen.
LIGHT = {"accent": "#3a7d44", "ground": "#f6f7f5", "on_accent": "#ffffff"}
DARK = {"accent": "#81c784", "ground": "#222326", "on_accent": "#17181b"}
PAGE = {"light": "#ffffff", "dark": "#17181b"}

SIZES = (48, 96, 192, 512)


def svg(body, view="0 0 108 108"):
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{view}" '
        f'width="108" height="108">\n{body}\n</svg>\n'
    )


def rect(fill):
    return f'<rect x="0" y="0" width="108" height="108" fill="{fill}"/>'


# 1. The speech bubble with a bird's beak: a pill-shaped bubble whose tail
# is an open beak of two mandibles, two dots inside as the talking.
def bubble_beak(c, mono=False):
    shape = (
        '<rect x="30" y="32" width="54" height="38" rx="19"/>'
        '<path d="M35,45 L21,54.5 L35,54 Z"/>'
        '<path d="M35,57.5 L23,61 L35,65 Z"/>'
    )
    dots = '<circle cx="57" cy="51" r="4.5"/><circle cx="70" cy="51" r="4.5"/>'
    if mono:
        return (
            '<mask id="m"><rect x="0" y="0" width="108" height="108" fill="#fff"/>'
            f'<g fill="#000">{dots}</g></mask>'
            f'<g fill="#000" mask="url(#m)">{shape}</g>'
        )
    return f'<g fill="{c["accent"]}">{shape}</g><g fill="{c["ground"]}">{dots}</g>'


def bubble_beak_bg(c):
    return rect(c["ground"])


# 2. The bird: a jackdaw in profile, beak open, on the accent; the launcher's
# mask makes the circle.
def bird(c, mono=False):
    ink = "#000" if mono else c["on_accent"]
    body = (
        '<circle cx="61" cy="39" r="10"/>'
        '<path d="M68,33.5 L82,37.5 L68.5,39.5 Z"/>'
        '<path d="M68.5,42 L80,46 L67,47.5 Z"/>'
        '<path d="M54,32 C43,38 37,50 40,62 L30,77 L46,71 '
        'C58,72 70,63 68,44 Z"/>'
    )
    legs = (
        f'<path d="M51,70 L49,80 M58,68 L58,80" stroke="{ink}" '
        'stroke-width="3" stroke-linecap="round" fill="none"/>'
    )
    if mono:
        return f'<g fill="{ink}">{body}</g>{legs}'
    eye = f'<circle cx="64" cy="37" r="2.2" fill="{c["accent"]}"/>'
    return f'<g fill="{ink}">{body}</g>{legs}{eye}'


def bird_bg(c):
    return rect(c["accent"])


# 3. The mesh bubble: a speech bubble's outline drawn as four nodes and the
# links between them, the tail's tip being one of the nodes.
def mesh_bubble(c, mono=False):
    ink = "#000" if mono else c["accent"]
    links = (
        f'<path d="M34,38 L75,38 L75,64 L49,64 L37,74 Z" stroke="{ink}" '
        'stroke-width="4.5" stroke-linejoin="round" fill="none"/>'
    )
    nodes = (
        f'<g fill="{ink}"><circle cx="34" cy="38" r="6"/>'
        '<circle cx="75" cy="38" r="6"/><circle cx="75" cy="64" r="6"/>'
        '<circle cx="37" cy="74" r="6"/></g>'
    )
    return links + nodes


def mesh_bubble_bg(c):
    return rect(c["ground"])


CANDIDATES = {
    1: (bubble_beak, bubble_beak_bg),
    2: (bird, bird_bg),
    3: (mesh_bubble, mesh_bubble_bg),
}

# What a launcher shows: the inner 72 units under a circular mask.
LAUNCHER_VIEW = "18 18 72 72"
LAUNCHER_CLIP = '<clipPath id="launcher"><circle cx="54" cy="54" r="36"/></clipPath>'


def composed(fg, bg):
    return svg(
        f'{LAUNCHER_CLIP}<g clip-path="url(#launcher)">{bg}{fg}</g>',
        view=LAUNCHER_VIEW,
    )


def themed(mono):
    """The monochrome layer as Android 13 tints it: a pale ground, dark ink."""
    ink = mono.replace('"#000"', '"#0b2110"')
    return svg(
        f'{LAUNCHER_CLIP}<g clip-path="url(#launcher)">{rect("#d6eadb")}{ink}</g>',
        view=LAUNCHER_VIEW,
    )


def safe_zone(fg, bg):
    """The whole canvas with the launcher's visible square and the safe circle."""
    guide = (
        '<rect x="18" y="18" width="72" height="72" fill="none" '
        'stroke="#c62828" stroke-width="0.6"/>'
        '<circle cx="54" cy="54" r="33" fill="none" stroke="#c62828" '
        'stroke-width="0.6" stroke-dasharray="2 1.5"/>'
    )
    return svg(bg + fg + guide)


def write_sources():
    files = {}
    for n, (fg, bg) in CANDIDATES.items():
        files[f"candidate-{n}.svg"] = composed(fg(LIGHT), bg(LIGHT))
        files[f"candidate-{n}-dark.svg"] = composed(fg(DARK), bg(DARK))
        files[f"candidate-{n}-foreground.svg"] = svg(fg(LIGHT))
        files[f"candidate-{n}-foreground-dark.svg"] = svg(fg(DARK))
        files[f"candidate-{n}-background.svg"] = svg(bg(LIGHT))
        files[f"candidate-{n}-background-dark.svg"] = svg(bg(DARK))
        files[f"candidate-{n}-monochrome.svg"] = svg(fg(LIGHT, mono=True))
    for name, text in files.items():
        (HERE / name).write_text(text)
    return files


def render(svg_text, size, name):
    src = SCRATCH / f"{name}.svg"
    out = SCRATCH / f"{name}-{size}.png"
    src.write_text(svg_text)
    subprocess.run(
        ["resvg", "-w", str(size), "-h", str(size), str(src), str(out)],
        check=True,
    )
    return Image.open(out).convert("RGBA")


def sheet(n, fg, bg):
    """One picture per candidate: both themes at every size, the themed
    monochrome, and the full canvas with the safe zone."""
    pad = 24
    row_h = max(SIZES) + 2 * pad
    width = sum(SIZES) + pad * (len(SIZES) + 1)
    extra_h = 192 + 2 * pad
    img = Image.new("RGBA", (width, 2 * row_h + extra_h), "#ffffff")
    draw = ImageDraw.Draw(img)
    for row, (theme, colours) in enumerate((("light", LIGHT), ("dark", DARK))):
        top = row * row_h
        draw.rectangle([0, top, width, top + row_h], fill=PAGE[theme])
        x = pad
        for size in SIZES:
            icon = render(composed(fg(colours), bg(colours)), size, f"c{n}-{theme}")
            img.alpha_composite(icon, (x, top + row_h - pad - size))
            x += size + pad
    top = 2 * row_h
    draw.rectangle([0, top, width, top + extra_h], fill="#eceeea")
    x = pad
    for size in (48, 96):
        icon = render(themed(fg(LIGHT, mono=True)), size, f"c{n}-mono")
        img.alpha_composite(icon, (x, top + extra_h - pad - size))
        x += size + pad
    for theme, colours in (("light", LIGHT), ("dark", DARK)):
        full = render(safe_zone(fg(colours), bg(colours)), 192, f"c{n}-safe-{theme}")
        img.alpha_composite(full, (x, top + pad))
        x += 192 + pad
    return img.convert("RGB")


def side_by_side():
    pad = 24
    n = len(CANDIDATES)
    img = Image.new("RGB", (n * (96 + pad) + pad, 2 * (96 + 2 * pad)), "#ffffff")
    draw = ImageDraw.Draw(img)
    draw.rectangle([0, 96 + 2 * pad, img.width, img.height], fill=PAGE["dark"])
    for row, colours in enumerate((LIGHT, DARK)):
        for i, (k, (fg, bg)) in enumerate(CANDIDATES.items()):
            icon = render(composed(fg(colours), bg(colours)), 96, f"all-{k}-{row}")
            img.paste(icon, (pad + i * (96 + pad), pad + row * (96 + 2 * pad)), icon)
    return img


def main():
    if shutil.which("resvg") is None:
        sys.exit("render-previews: resvg not found; sudo apt install resvg")
    SCRATCH.mkdir(parents=True, exist_ok=True)
    write_sources()
    for n, (fg, bg) in CANDIDATES.items():
        sheet(n, fg, bg).save(HERE / f"preview-{n}.png", optimize=True)
    side_by_side().save(HERE / "preview-all.png", optimize=True)
    shutil.rmtree(SCRATCH)


if __name__ == "__main__":
    main()

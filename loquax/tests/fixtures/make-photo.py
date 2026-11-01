#!/usr/bin/env python3
"""Writes photo-portrait.jpg, the photo fixture of 1220: what a phone's
camera hands the picker, 2400 x 1800 stored with EXIF orientation 6, so it
shows 1800 x 2400 upright, a GPS position and a camera model in its EXIF
(which a sent photo must not carry), a red block in the top left corner as
it shows (which tells a turned picture from an upright one), and some
grain, so it compresses like a photo and not like a drawing.

    python3 -I make-photo.py   # in this directory
"""

import random

from PIL import Image, ImageDraw

W, H = 1800, 2400  # as shown
random.seed(1220)
shown = Image.new("RGB", (W, H))
pixels = shown.load()
for y in range(H):
    for x in range(W):
        g = random.randint(-4, 4)
        pixels[x, y] = (
            max(0, min(255, 40 + x * 160 // W + g)),
            max(0, min(255, 90 + y * 120 // H + g)),
            max(0, min(255, 170 - x * 60 // W + g)),
        )
draw = ImageDraw.Draw(shown)
draw.rectangle([0, 0, W // 4, H // 4], fill=(250, 10, 10))
for i in range(12):
    draw.ellipse([200 + i * 110, 900 + (i % 3) * 300, 380 + i * 110, 1080 + (i % 3) * 300],
                 fill=(240, 220 - i * 10, 60 + i * 12))
# Stored as the sensor saw it: turned so orientation 6 turns it back.
stored = shown.transpose(Image.Transpose.ROTATE_90)
exif = Image.Exif()
exif[0x0112] = 6            # Orientation: rotate 90 CW to show
exif[0x0110] = "Pixel 8"    # Model
exif[0x8825] = {            # GPS IFD
    1: "N", 2: (52.0, 30.0, 58.0),
    3: "E", 4: (13.0, 22.0, 39.0),
}
stored.save("photo-portrait.jpg", "JPEG", quality=85, exif=exif)

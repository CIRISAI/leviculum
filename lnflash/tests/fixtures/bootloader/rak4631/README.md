# RAK4631 bootloader images: 0.4.4 and the way back to 0.4.3

Three files, byte for byte as RAKwireless published them in
github.com/RAKWireless/WisBlock at commit
`d3da249c0e4df56fb972021cf6e99239f27857bf` (2026-07-23, "Update RAK4631
bootloader to V0.4.4 with BLE OTA recovery."):

| File | Path in that commit | git blob | sha256 |
| --- | --- | --- | --- |
| `update-wiscore_rak4631_board_bootloader-0.4.4_nosd.uf2` | `bootloader/RAK4630/Latest/` | `b1622756849fedb37ca9924e2ab3368ea8a8b8bc` | `6795b91cfbb7ccd711e8c2602404d8a621dd01deb368e8b471fabb17d8297001` |
| `wiscore_rak4631_board_bootloader-0.4.4.hex` | `bootloader/RAK4630/Latest/` | `0d8085df5db7dcbddd6d96bd10be062fd351e256` | `29635d90eccd181ce206bfa10aa0ca92b9cff61735a6205321efe241e0f1298a` |
| `wiscore_rak4631_board_bootloader-0.4.3.hex` | `bootloader/RAK4630/Old/V0.4.3/` | `ee3a252912f9f2fba11a723f3e3159e384c1d92a` | `36525835a69f5705e17bad5e7d0f59022abd83582d06b9d80036bbbbc8b885f6` |

The UF2 is the self-update: 136 blocks, family `0xd663823c` (nRF52
bootloader), INFO string `UF2 Bootloader 0.4.4`, `Date: Jun 17 2026`, no
SoftDevice. The two hexes are bootloader only (code, config page, the two
UICR words), the images the SWD recovery writes. None of RAK's
`_s140_6.1.1` files is here: they carry SoftDevice 6.1.1, and our boards run
7.3.0.

They are here for two readers: `lnflash/tests/bootloader_update.rs`, which
asserts which flash pages the UF2 reaches and that the 0.4.4 hex is what it
installs, and the bench (`just bootloader-update-rak`, the SWD recovery). They are not part of any package and
lnflash never writes a bootloader image: `uf2::Image::encode` refuses the family. The
procedure and the reasoning are in
`docs/src/concepts/ota-stage-1-ble-dfu.md`, "Bootloader update".

The images are builds of the Adafruit nRF52 bootloader, MIT-licensed by
Adafruit Industries, published by RAKwireless under the MIT licence of the
WisBlock repository, reproduced below.

```
MIT License

Copyright (c) 2021 RAKwireless

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.```

The Adafruit licence, from the bootloader source RAK publishes beside it
(`bootloader/RAK4630/Latest/WisCore_RAK4631_Bootloader/LICENSE` at the same commit):

```
MIT License

Copyright (c) 2016 Adafruit Industries

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

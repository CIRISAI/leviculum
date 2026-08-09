# `lnflash` payload sources

What `just lnflash-bundle` assembles the bundle's `firmware/` directory
from. Everything here except the manifest is a file that travels to the
user unchanged; nothing here is linked into the binary.

## Why these files are in the repo at all

A bundle built from a Meshtastic checkout is exactly the hidden
dependency our clone-and-deploy policy forbids: the only copy of the
SoftDevice on our machines used to live at
`~/coding/meshtastic/bin/s140_nrf52_7.3.0_softdevice.hex`, and a build
recipe reaching sideways into an unrelated checkout is not reproducible
for anyone else. So the blob is vendored.

`s140_nrf52_7.3.0_softdevice.hex` is byte-identical to that file
(sha256 `ef75b7621c2b64a5e8101a8fb8a74d07b5f7b530396d0f368644f6e8b7415660`).
It is Nordic's, not ours, and it is distributed under
`LICENSE-NORDIC` — Nordic's five-clause BSD variant. Our copy of that
licence is byte-identical to the one `nrf-softdevice` ships with its
S140 crate (md5 `d86fff2d6237b5a565289c1fa208f1ec`), which settles its
provenance; the file itself names no product or version.

## The two rules that shape this directory

**The SoftDevice is never linked into the binary.** Clause 4 restricts
what the software may be used for and clause 5 forbids modification,
decompilation and disassembly. AGPL-3.0 permits no such additional
restrictions on the combined work, so an `include_bytes!` of this blob
would put the two licences in direct conflict. Shipping it beside the
binary is ordinary aggregation and does not have that problem. Our own
firmware travels the same way — being ours it could be embedded, but one
uniform payload layout beats a split where some images live inside the
binary and others outside.

**The licence cannot be left behind.** A `remedy` entry in the manifest
carries a mandatory `license` field, and the manifest loader rejects an
entry whose licence file is missing. Shipping a third-party blob without
its licence is therefore impossible by construction rather than by
remembering. Clause 2 requires it.

The reasoning in full: `docs/src/concepts/lnode-flashing.md`,
"Why we cannot simply ship it".

## What is not here

The application image, `leviculum-t114-<version>.uf2`. It is built from
this tree by `just lnflash-bundle`, not vendored, so a bundle always
carries the firmware the commit it was built from produces.

The manifest is generated at bundle time for the same reason: it records
the sha256 of an image that does not exist until the firmware is built.

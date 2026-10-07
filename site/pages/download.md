+++
title = "Download"
+++

Nightly builds of `master`, not releases. Nightly means nightly: what broke yesterday may be fixed tonight, and the other way round.

Each file has a `.sha256` beside it. The second link is the same file on Codeberg, until leviculum.network carries the builds.

## The stack: `lnsd` and the tools

- [leviculum-nightly-amd64.deb](https://leviculum.network/releases/nightly/latest/leviculum-nightly-amd64.deb) ([sha256](https://leviculum.network/releases/nightly/latest/leviculum-nightly-amd64.deb.sha256)), mirror: [Codeberg](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/leviculum-nightly-amd64.deb) ([sha256](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/leviculum-nightly-amd64.deb.sha256))
- [leviculum-nightly-arm64.deb](https://leviculum.network/releases/nightly/latest/leviculum-nightly-arm64.deb) ([sha256](https://leviculum.network/releases/nightly/latest/leviculum-nightly-arm64.deb.sha256)), mirror: [Codeberg](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/leviculum-nightly-arm64.deb) ([sha256](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/leviculum-nightly-arm64.deb.sha256))

## Propagation node: `lnpnd`

- [lnpnd-nightly-amd64.deb](https://leviculum.network/releases/nightly/latest/lnpnd-nightly-amd64.deb) ([sha256](https://leviculum.network/releases/nightly/latest/lnpnd-nightly-amd64.deb.sha256)), mirror: [Codeberg](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/lnpnd-nightly-amd64.deb) ([sha256](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/lnpnd-nightly-amd64.deb.sha256))
- [lnpnd-nightly-arm64.deb](https://leviculum.network/releases/nightly/latest/lnpnd-nightly-arm64.deb) ([sha256](https://leviculum.network/releases/nightly/latest/lnpnd-nightly-arm64.deb.sha256)), mirror: [Codeberg](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/lnpnd-nightly-arm64.deb) ([sha256](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/lnpnd-nightly-arm64.deb.sha256))

## NomadNet browser: `lnomad`

- [lnomad-nightly-amd64.deb](https://leviculum.network/releases/nightly/latest/lnomad-nightly-amd64.deb) ([sha256](https://leviculum.network/releases/nightly/latest/lnomad-nightly-amd64.deb.sha256)), mirror: [Codeberg](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/lnomad-nightly-amd64.deb) ([sha256](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/lnomad-nightly-amd64.deb.sha256))
- [lnomad-nightly-arm64.deb](https://leviculum.network/releases/nightly/latest/lnomad-nightly-arm64.deb) ([sha256](https://leviculum.network/releases/nightly/latest/lnomad-nightly-arm64.deb.sha256)), mirror: [Codeberg](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/lnomad-nightly-arm64.deb) ([sha256](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/lnomad-nightly-arm64.deb.sha256))

## Blog server: `lblogd`

- [lblogd-nightly-amd64.deb](https://leviculum.network/releases/nightly/latest/lblogd-nightly-amd64.deb) ([sha256](https://leviculum.network/releases/nightly/latest/lblogd-nightly-amd64.deb.sha256)), mirror: [Codeberg](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/lblogd-nightly-amd64.deb) ([sha256](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/lblogd-nightly-amd64.deb.sha256))
- [lblogd-nightly-arm64.deb](https://leviculum.network/releases/nightly/latest/lblogd-nightly-arm64.deb) ([sha256](https://leviculum.network/releases/nightly/latest/lblogd-nightly-arm64.deb.sha256)), mirror: [Codeberg](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/lblogd-nightly-arm64.deb) ([sha256](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/lblogd-nightly-arm64.deb.sha256))

Install:

```
sudo apt install ./leviculum-nightly-amd64.deb
```

Static binaries, so any Debian from 9 or Ubuntu from 16.04 will do. A `.tar.gz` of the bare binaries sits beside each package in the [Codeberg release](https://codeberg.org/Lew_Palm/leviculum/releases/tag/nightly).

## Firmware

`lnflash` carries the flasher and one image per supported [board](/boards), amd64 host only.

- [lnflash-nightly-amd64.tar.gz](https://leviculum.network/releases/nightly/latest/lnflash-nightly-amd64.tar.gz) ([sha256](https://leviculum.network/releases/nightly/latest/lnflash-nightly-amd64.tar.gz.sha256)), mirror: [Codeberg](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/lnflash-nightly-amd64.tar.gz) ([sha256](https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/lnflash-nightly-amd64.tar.gz.sha256))

Flash:

```
tar xzf lnflash-nightly-amd64.tar.gz && cd lnflash-*
sudo ./lnflash
```

Details: [Building and flashing](https://codeberg.org/Lew_Palm/leviculum/src/branch/master/docs/src/firmware/flashing.md).

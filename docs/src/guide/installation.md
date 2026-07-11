# Installation

## Requirements

- Rust stable toolchain
- Git

Optional, depending on what you want to test:

- Python 3 (for interop tests)
- Docker (for integration tests)
- 2-4 RNode modems via USB (for LoRa integration tests)

No system C libraries are required. All cryptography is compiled from Rust source.

### Debian/Ubuntu setup

```sh
# Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# Interop tests
sudo apt install python3

# Integration tests
sudo apt install docker.io
sudo usermod -aG docker $USER

# LoRa tests and embedded firmware (USB serial access)
sudo usermod -aG dialout $USER
```

## Build from source

```sh
git clone https://codeberg.org/Lew_Palm/leviculum.git
cd leviculum
cargo build --release --bin lnsd --bin lnstest --bin lncp
```

The binaries are in `target/release/`.

## Running the daemon

```sh
./target/release/lnsd -v
```

Reads its config from `~/.reticulum/config`, the same location as Python Reticulum.

## Development

### Cargo aliases

Common workflows are available as cargo aliases (defined in `.cargo/config.toml`):

| Command | What it does |
|---------|-------------|
| `cargo test-core` | Run all leviculum-core unit tests |
| `cargo test-std` | Run all leviculum-std unit tests |
| `cargo test-interop` | Run interop tests against Python Reticulum |
| `cargo test-integ` | Run Docker-based integration tests |
| `cargo lint` | Run clippy on all crates |
| `cargo fmt --all -- --check` | Check formatting |

### Test levels

Tests are organized by what they require:

**Unit tests** -- just Rust, no extra dependencies:

```sh
cargo test-core
cargo test-std
```

**Interop tests** -- require Python 3 and the vendored Reticulum:

```sh
git submodule update --init reference/Reticulum
cargo test-interop
```

**Integration tests** -- require Docker and pre-built release binaries:

```sh
cargo build --release --bin lnsd --bin lnstest --bin lncp
cargo test-integ
```

**LoRa integration tests** -- require physical RNode modems connected via USB:

LoRa tests are `#[ignore]`d by default and must be run explicitly. They
exercise real over-the-air transfers between RNode radios running Reticulum
firmware. Tests are skipped automatically if the required devices are not
connected.

| Devices needed | Test count | Examples |
|----------------|-----------|----------|
| 2 RNodes | 40 | `lora_link_rust`, `lora_lncp_push`, `lora_ratchet_basic` |
| 3 RNodes | 3 | `lora_3node_transfer`, `lora_3node_contention` |
| 4 RNodes | 7 | `lora_4node_contention_rust`, `lora_multihop_transfer` |

Hardware setup:

- Connect RNodes via USB. They appear as `/dev/ttyACM0`, `/dev/ttyACM1`, etc.
- Your user must be in the `dialout` group: `sudo usermod -aG dialout $USER`
- Override device paths with environment variables if needed:
  `LEVICULUM_RNODE_0=/dev/ttyUSB0 LEVICULUM_RNODE_1=/dev/ttyUSB1`

Running LoRa tests:

```sh
# Single test
cargo test -p reticulum-integ -- --exact executor::tests::lora_link_rust --ignored --nocapture

# All 2-device tests
cargo test -p reticulum-integ -- lora_ --ignored --nocapture --test-threads=1

# Override radio parameters (bandwidth in Hz)
LORA_BANDWIDTH=125000 cargo test -p reticulum-integ -- --exact executor::tests::lora_lncp_push --ignored --nocapture
```

Each LoRa test must pass on all three bandwidth profiles (62.5 kHz, 125 kHz,
250 kHz). The TOML files define 62.5 kHz; use `LORA_BANDWIDTH` to switch.

Some tests use the `lora-proxy` binary for fault injection (dropping frames
to test retransmit recovery). Build it before running proxy tests:

```sh
cargo build --release --bin lora-proxy
```

### Embedded cross-compilation

Embedded targets are not downloaded automatically. Install them when needed:

```sh
rustup target add thumbv7em-none-eabihf   # nRF52840
rustup target add thumbv6m-none-eabi       # RP2040
cargo check-nrf52
cargo check-embedded
```

### Before submitting changes

```sh
cargo fmt --all -- --check
cargo lint
cargo test-core
cargo test-interop
```

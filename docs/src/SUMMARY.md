# Summary

[Introduction](introduction.md)

# Concepts

- [Architecture overview](architecture.md)
- [Interface isolation](concepts/interface-isolation.md)
- [Python-RNS compatibility](concepts/python-rns-compatibility.md)
- [Cryptographic identity and forward secrecy](concepts/identity-and-forward-secrecy.md)
- [Storage and embedding](concepts/storage-and-embedding.md)

# User Guide

- [Installation](guide/installation.md)
- [Configuration reference](guide/configuration.md)
- [Running lnsd](lnsd-quickstart.md)
- [lns command-line utility](guide/lns.md)
- [lncp file transfer](guide/lncp.md)
- [Manual page: lnsd(1)](man/lnsd.1.md)
- [Manual page: lns(1)](man/lns.1.md)
- [Manual page: lncp(1)](man/lncp.1.md)

# Firmware (LNode)

- [Supported boards](firmware/boards.md)
- [Building and flashing](firmware/flashing.md)
- [Serial ports and udev](firmware/serial-ports.md)
- [Recovery and troubleshooting](firmware/recovery.md)

# Developer Guide

- [Choosing a layer](developer/choosing-a-layer.md)
- [Rust API tutorial](developer/rust-api-tutorial.md)
- [Rust API specification](developer/rust-api-spec.md)
- [Embedded integration](developer/embedded.md)
- [C API: overview and concepts](c-api/overview.md)
- [C API: tutorial](c-api/tutorial.md)
- [C API: how-to](c-api/howto.md)
- [C API: reference](c-api/reference.md)

# Reference and Internals

- [RNode serial protocol](rnode-protocol.md)
- [SX1262 datasheet reference](sx1262-datasheet-reference.md)
- [Structured event logs](structured-event-logs.md)
- [jl / jldiff tools](jl-jldiff.md)
- [Storage trait analysis](storage-trait-analysis.md)
- [Broadcast Python-RNS parity](architecture-broadcast-python-parity.md)
- [Testing quick reference](development-testing.md)
- [CI pipeline](development-ci.md)

# Appendix

- [Reticulum Protocol Specification](appendix/reticulum-specification.md)
  - [Introduction and scope](appendix/reticulum/00-introduction.md)
  - [Cryptographic primitives](appendix/reticulum/01-primitives.md)
  - [Identity](appendix/reticulum/02-identity.md)
  - [Destination](appendix/reticulum/03-destination.md)
  - [Packet format](appendix/reticulum/04-packet.md)
  - [Announce](appendix/reticulum/05-announce.md)
  - [Link](appendix/reticulum/06-link.md)
  - [Resource](appendix/reticulum/07-resource.md)
  - [Channel and Buffer](appendix/reticulum/08-channel-buffer.md)
  - [Transport](appendix/reticulum/09-transport.md)
  - [Framing and IFAC](appendix/reticulum/10-framing-ifac.md)
  - [Constants reference](appendix/reticulum/11-constants.md)
  - [Coverage ledger](appendix/reticulum/12-coverage-ledger.md)
  - [Test vectors](appendix/reticulum/13-test-vectors.md)
  - [Symbol inventory (frozen)](appendix/reticulum/_inventory.md)
- [LXMF Protocol Specification](appendix/lxmf-specification.md)
  - [Introduction and scope](appendix/lxmf/00-introduction.md)
  - [Cryptographic primitives](appendix/lxmf/01-primitives.md)
  - [Identifiers and sizes](appendix/lxmf/02-identifiers-and-sizes.md)
  - [Message binary format](appendix/lxmf/03-message-format.md)
  - [Fields](appendix/lxmf/04-fields.md)
  - [Delivery methods and sizing](appendix/lxmf/05-delivery-and-sizing.md)
  - [On-air sequences](appendix/lxmf/06-on-air-sequences.md)
  - [Stamps and proof-of-work](appendix/lxmf/07-stamps-pow.md)
  - [Tickets](appendix/lxmf/08-tickets.md)
  - [Announce application data](appendix/lxmf/09-announce-appdata.md)
  - [Propagation](appendix/lxmf/10-propagation.md)
  - [Router internals (informative)](appendix/lxmf/11-router-informative.md)
  - [Constants reference](appendix/lxmf/12-constants.md)
  - [Coverage ledger](appendix/lxmf/13-coverage-ledger.md)
  - [Test vectors](appendix/lxmf/14-test-vectors.md)
  - [Symbol inventory (frozen)](appendix/lxmf/_inventory.md)

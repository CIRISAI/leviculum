//! Interoperability tests with Python Reticulum.
//!
//! These tests verify that our Rust implementation correctly interoperates
//! with the Python Reticulum reference implementation. Tests are organized
//! into two categories:
//!
//! ## Test Categories
//!
//! 1. **Unit Tests** - Pure logic tests that don't require a daemon:
//!    - `protocol_tests` - Flag encoding, hash derivation, packet layout
//!
//! 2. **Interop Tests** - Tests using the TestDaemon infrastructure:
//!    - `announce_interop_tests` - Core announce/path tests
//!    - `discovery_tests` - Bidirectional discovery tests
//!    - `link_tests` - Link establishment tests (Rust as initiator)
//!    - `responder_tests` - Link responder tests (Rust as responder)
//!    - `flow_tests` - End-to-end flow tests
//!
//! ## Fixtures
//!
//! - `harness::TestDaemon` - `scripts/test_daemon.py`, a Python RNS daemon
//!   with a JSON command port; most tests here drive it.
//! - `rpc_interop_tests::start_rust_daemon_with_rpc` - our shared instance
//!   with the Python CLI tools (`rnstatus`, `rnpath`, `rnprobe`) as clients.
//! - `lxmd_peer` - the stock reference `lxmd` (`reference/LXMF`, run as
//!   `python3 -m LXMF.Utilities.lxmd` with a config the test writes) as a
//!   propagation-node peer of lnpnd's production engine, both local clients
//!   of one in-process shared instance. Reads lxmd's log and message store,
//!   lnpnd's engine events and `/pn/get/stats`. Used by
//!   `lxmd_drop_in_tests` (peering both ways, D6 re-pricing, rotation).
//!
//! ## Running Tests
//!
//! ```sh
//! # Run all interop tests (includes daemon tests that auto-spawn Python daemon)
//! cargo test --package leviculum-std --test rnsd_interop
//!
//! # Run with verbose output
//! cargo test --package leviculum-std --test rnsd_interop -- --nocapture
//!
//! # Run specific test module
//! cargo test --package leviculum-std --test rnsd_interop announce_interop_tests
//! cargo test --package leviculum-std --test rnsd_interop responder_tests
//! cargo test --package leviculum-std --test rnsd_interop protocol_tests
//! ```

mod announce_emission_interop_tests;
mod announce_interop_tests;
mod auto_interop_tests;
mod backbone_interop_tests;
mod blackhole_interop_tests;
mod ble_torn_announce_tests;
mod channel_tests;
mod common;
mod comprehensive_network_test;
mod discovery_interop_tests;
mod discovery_tests;
mod edge_case_tests;
mod encryption_tests;
mod explicit_hash_interop_tests;
mod flood_tests;
mod flow_tests;
mod group_crypto_tests;
mod harness;
/// The host-wide allocator, where `ports::free_port` draws.
use harness::port_alloc;
mod held_announce_interop_tests;
mod identify_loss_proxy;
mod ifac_interop_tests;
mod interface_mode_tests;
mod lifecycle_tests;
mod link_keepalive_close_tests;
mod link_manager_tests;
mod link_packet_interop_tests;
mod link_tests;
mod lncp_identify_loss_interop_tests;
mod lncp_listener_identify_loss_interop_tests;
mod lnomad_fetch_interop_tests;
mod loadtest_tcp_hub_tests;
mod local_client_announce_latency_ab;
mod lrproof_hop_undercount_interop_tests;
mod lxmd_drop_in_tests;
mod lxmd_peer;
mod lxmf_interop_tests;
mod mtu_tests;
mod multihop_tests;
mod node_api_tests;
mod nomad_page_tests;
mod packet_radio_stats_tests;
mod path_gap_tests;
mod path_recovery_tests;
mod pipe_interop_tests;
mod plain_broadcast_tests;
/// Who picks a port and when: the consumer binds port 0 wherever it can.
#[path = "../support/ports.rs"]
#[allow(dead_code)]
mod ports;
mod probe_tests;
mod proof_tests;
mod propagation_node_interop_tests;
mod protocol_tests;
mod python_parity_tests;
mod python_rnstatus_server_tests;
mod ratchet_rotation_tests;
mod ratchet_tests;
mod relay_integration_tests;
mod remote_management_server_tests;
mod remote_management_tests;
mod request_resource_interop_tests;
mod request_tests;
mod resource_tests;
mod responder_node_tests;
mod responder_tests;
mod response_resource_tests;
mod retain_interop_tests;
mod reverse_rpc_interop_tests;
mod rpc_interop_tests;
mod rust_relay_tests;
mod selftest_announce_cap_tests;
mod serial_interop_tests;
mod shared_instance_tests;
mod status_parity_tests;
mod stress_tests;
mod transport_interop_tests;
mod tunnel_interop_tests;
mod udp_interop_tests;

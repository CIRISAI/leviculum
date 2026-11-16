//! The board firmware's orchestration stratum (Codeberg #469).
//!
//! Stratum 2 of `docs/src/concepts/firmware-layers.md` (Firmware layers):
//! the node loop, once, over seams that each chip family's bind crate
//! implements. Stratum 1 below it is policy (`leviculum-core`, the
//! host-tested member crates); stratum 3 above it is a bind crate with pins,
//! backends and `main` (`leviculum-nrf` today).
//!
//! [`seams`] declares every seam of the page's section 3. Every seam has a
//! host fake in `fakes` (test builds only), so the modules that arrive are
//! host-tested like the policy crates. Step 1 moves the leaf modules that
//! need no backend but a seam: [`log`], [`boot_count`], [`postmortem`],
//! [`time_floor`], and the policy half of [`record_store`]. The leaves that
//! need not even a seam follow (957): [`iface_bytes`] (its counters; the
//! names stay with the bind crate), [`dispatch`], [`clock`], [`boot_trace`]
//! (all but the reset-cause register) and [`identity`]. Cuts that need no
//! new seam follow (958): the calendar source the board reports joins
//! [`clock`], the interface names become a table the bind crate sets once
//! at boot ([`iface_bytes::set_names`]), and the serial [`interface`]
//! arrives over it. In 959 the board state the sensor tasks feed and the
//! orchestration reads moves whole (`board_state`, behind the capability
//! features; the tasks stay below), and the orchestration draws its
//! randomness through [`entropy::Entropy`], over the fill function the bind
//! crate registers once at boot ([`entropy::set_source`]). In 961 the
//! global allocator and the two heap facts the orchestration reads move
//! here ([`heap`]); the pool's memory stays with the bind crate. In 973 the
//! feedback policy moves here ([`feedback`]) over the [`seams::Sinks`]
//! trait; the LED and buzzer players stay below. In 976 the button's
//! press handling moves here ([`button`]) over the [`seams::ButtonInput`]
//! and [`seams::Clock`] seams; the pin and its pull stay below. In 980 the
//! status display's page and power move here (`display`, behind the
//! `display` capability) over the `seams::Panel` sink; the SSD1306 and
//! ST7789 drivers stay below. In 991 the battery monitor's cadences and
//! lines move here (`battery`, behind the `battery` capability) over the
//! `seams::BatteryGauge` seam; the SAADC and the divider stay below. In 998
//! the BLE link's node side begins to move here ([`ble_link`], step 4's
//! first slice): the carrier gate the media profile pushes, the pacing
//! value, the link/drain table and the heap-census facts the backend
//! publishes. In 999 the message half follows (step 4's second slice):
//! the node-facing channels, the peer events and inbound packets as
//! [`seams::BleLinkEvent`], the BLE `Interface` implementation, and the
//! `[MEDIA]` lines both carrier interfaces write ([`media`]); the
//! identity-to-slot registry and the fan-out stay below until the
//! registry split lands.
//!
//! What the crate may depend on is held by two guards rather than by this
//! comment: `just firmware-no-hal` refuses a HAL or chip crate in its
//! `cargo tree`, `just firmware-no-chip-cfg` a BSP `cfg` or a chip name in
//! its sources outside doc comments.

#![no_std]

extern crate alloc;

#[cfg(feature = "battery")]
pub mod battery;
pub mod ble_link;
#[cfg(any(feature = "display", feature = "gnss", feature = "battery"))]
pub mod board_state;
pub mod boot_count;
pub mod boot_trace;
pub mod button;
pub mod clock;
pub mod dispatch;
#[cfg(feature = "display")]
pub mod display;
pub mod entropy;
pub mod feedback;
pub mod heap;
pub mod identity;
pub mod iface_bytes;
pub mod interface;
pub mod log;
pub mod media;
pub mod postmortem;
pub mod record_store;
pub mod seams;
pub mod thread_watch;
pub mod time_floor;

#[cfg(test)]
mod fakes;

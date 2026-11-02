//! What the `lev.position` announce costs on the air, pinned (order 1301).
//!
//! The decision this table carries: a separate aspect is one more frame in
//! every announce set, not "zero extra airtime" (study 1296, section 1),
//! and the sealed form is larger than one LoRa frame. The numbers come from
//! the codec ([`leviculum_lxmf::lev_position`]), core's announce geometry
//! and the duty ledger's own airtime formula (`packet_airtime_ms`, the call
//! `leviculum_nrf::lora::AnnounceCap` charges announces at), at our LoRa
//! defaults: 869.463 MHz, BW 125 kHz, SF8, CR 4/5, the RNode-derived
//! 18-symbol preamble. If one moves, `announce-cadence.md`'s table and
//! `PositionShare::frames` move with it.

use leviculum_core::announce_payload_fixed_len;
use leviculum_core::constants::{DEFAULT_ANNOUNCE_CAP_PERCENT, HEADER_MINSIZE};
use leviculum_core::rnode::{derive_preamble_symbols, packet_airtime_ms, MAX_SINGLE_PAYLOAD};
use leviculum_lxmf::announce::DeliveryAnnounce;
use leviculum_lxmf::lev_position::{sealed_len, Position, Source};

/// The derived board name, 14 bytes, as both announces carry it.
const NAME: &str = "LNode-1a2b3c4d";

/// A walking board's fix with every optional key.
fn walking() -> Position {
    Position {
        lat_e7: 437_731_000,
        lon_e7: 112_560_000,
        alt_m: Some(50),
        speed_cm_s: Some(140),
        heading_e2: Some(27_000),
        fix_age_s: 2,
        source: Some(Source::Gnss),
        name: Some(NAME.into()),
    }
}

/// The transport header a relay adds (HEADER_2's transport id).
const RELAY_BYTES: usize = 16;

fn announce_len(app_data: usize) -> usize {
    HEADER_MINSIZE + announce_payload_fixed_len(false) + app_data
}

fn delivery_app_data() -> usize {
    DeliveryAnnounce {
        display_name: Some(NAME.as_bytes().to_vec()),
        stamp_cost: None,
        compression_supported: false,
    }
    .encode()
    .len()
}

fn ms(len: usize) -> u64 {
    packet_airtime_ms(len, 125_000, 8, 5, derive_preamble_symbols(8, 5, 125_000))
}

/// Per hour, milliseconds, of `sets` announce sets of these frames.
fn per_hour(sets: u64, frames: &[usize]) -> u64 {
    sets * frames.iter().map(|len| ms(*len)).sum::<u64>()
}

#[test]
fn the_three_announces_cost_these_bytes_and_milliseconds() {
    assert_eq!(derive_preamble_symbols(8, 5, 125_000), 18);
    let delivery = announce_len(delivery_app_data());
    let plain = announce_len(walking().encode().len());
    let sealed = announce_len(sealed_len(walking().encode().len()));
    assert_eq!((delivery, plain, sealed), (186, 209, 297));
    // Direct, and as a relay forwards it.
    assert_eq!((ms(delivery), ms(delivery + RELAY_BYTES)), (554, 595));
    assert_eq!((ms(plain), ms(plain + RELAY_BYTES)), (605, 646));
    assert_eq!((ms(sealed), ms(sealed + RELAY_BYTES)), (913, 954));
    // The seal: 88 bytes and 308 ms over the plain form, because 297 bytes
    // do not fit one LoRa frame (254) and go as two, each with its own
    // preamble. No sealed form can: the fixed part is 82 bytes and the
    // smallest padded map 16, on a 167-byte announce.
    assert_eq!(sealed - plain, 88);
    assert!(plain <= MAX_SINGLE_PAYLOAD && sealed > MAX_SINGLE_PAYLOAD);
    assert!(announce_len(sealed_len(0)) > MAX_SINGLE_PAYLOAD);
}

#[test]
fn a_moving_board_spends_this_much_more_an_hour() {
    let delivery = announce_len(delivery_app_data());
    let plain = announce_len(walking().encode().len());
    let sealed = announce_len(sealed_len(walking().encode().len()));
    // Twelve sets an hour at the five-minute cadence.
    let before = per_hour(12, &[delivery]);
    let with_plain = per_hour(12, &[delivery, plain]);
    let with_sealed = per_hour(12, &[delivery, sealed]);
    assert_eq!((before, with_plain, with_sealed), (6_648, 13_908, 17_604));
    // A still board: one set an hour.
    assert_eq!(per_hour(1, &[delivery, sealed]), 1_467);
    // Own duty: 0.18 % before, 0.49 % with the sealed position, against
    // the tenth of the 10 % sub-band its own announces may spend (1 %,
    // 36 s an hour). The 25 m advance at the budget's spacing adds about
    // nine more position announces an hour while walking
    // (`leviculum_announce_policy`'s scripted walk), 0.72 % in all.
    assert!(with_sealed < 36_000);
    assert!(per_hour(12, &[delivery]) + per_hour(21, &[sealed]) < 36_000);
}

/// The mesh: N moving boards, each heard and forwarded once by one relay
/// (the hardware cell's T114). The relay forwards under the announce cap,
/// 2 % of its interface, 72 s an hour.
#[test]
fn the_mesh_reaches_the_announce_cap_at_four_moving_boards() {
    let delivery = announce_len(delivery_app_data());
    let sealed = announce_len(sealed_len(walking().encode().len()));
    let own = per_hour(12, &[delivery, sealed]);
    let relayed = per_hour(12, &[delivery + RELAY_BYTES, sealed + RELAY_BYTES]);
    assert_eq!((own, relayed), (17_604, 18_588));
    let cap_ms = 3_600_000 * DEFAULT_ANNOUNCE_CAP_PERCENT as u64 / 100;
    assert_eq!(cap_ms, 72_000);
    // Channel occupancy, hundredths of a percent, for 1, 5 and 10 boards.
    let occupancy = |n: u64| n * (own + relayed) * 10_000 / 3_600_000;
    assert_eq!(
        (occupancy(1), occupancy(5), occupancy(10)),
        (100, 502, 1_005)
    );
    // The relay's forwarded announces against its cap.
    let fits = |n: u64| n * relayed <= cap_ms;
    assert!(fits(3) && !fits(4), "the cap is full at four moving boards");
    // Without the position announce it was ten.
    let before = per_hour(12, &[delivery + RELAY_BYTES]);
    assert!(10 * before <= cap_ms && 11 * before > cap_ms);
    // The thinning (`leviculum_announce_policy::thinned_interval_ms`)
    // starts where the measured channel load passes the cap's 2 %: two
    // moving boards here. Above it the mesh's position announces stay at
    // about 24 an hour however many boards move, so with the delivery
    // announces the relay's cap holds up to six moving boards.
    assert!(occupancy(2) >= 200 && occupancy(1) < 200);
    let thinned = |n: u64| 24 * ms(sealed + RELAY_BYTES) + n * 12 * ms(delivery + RELAY_BYTES);
    assert!(thinned(6) <= cap_ms && thinned(7) > cap_ms);
}

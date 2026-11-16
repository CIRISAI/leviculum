//! mvr: lnsd mutes a single-UART board with the first 0x13 it writes (1238,
//! 1242, Refs #483).
//!
//! The ThinkNode M9's one UART carries its debug text and its HDLC transport
//! under XON/XOFF, and the UART obeys a received XOFF in hardware: it deletes
//! the byte and stops transmitting until an XON arrives. Measured on the rig
//! (1238, 3 of 3): a frame carrying a raw 0x13 left the board running and
//! silent until reset. lnsd's serial interface escaped only FLAG and ESCAPE,
//! so any frame with a 0x13 in it (a length, a hash, a tx power of 19 dBm) did
//! the same.
//!
//! The board here is that UART and nothing more: a raw 0x13 halts its
//! transmitter, a raw 0x11 releases it, both are deleted from what it
//! receives, and what is left goes to the core `Deframer`, as on the board. It
//! acks the legacy radio config and answers the radio query with the profile,
//! escaping its own 0x11/0x13 and sending a raw XON before every answer, as
//! the board's writer and its UART may.
//!
//! Red before: with the escape in `SerialMuxPort::poll_write` replaced by a
//! pass-through, the mux arm comes back `Silent`, which is the plain arm's
//! outcome. Green after: `Adopted`.

use leviculum_core::envelope;
use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_core::rnode::RadioConfigWire;
use leviculum_serial_mux::{escape_flow, XOFF, XON};
use leviculum_std::interfaces::serial_mux::SerialMuxPort;
use leviculum_std::interfaces::{radio_bring_up, RadioBringUp};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

/// 19 dBm is 0x13: the legacy config frame carries a raw XOFF in its
/// tx-power byte.
fn phy() -> RadioConfigWire {
    RadioConfigWire {
        frequency_hz: 869_463_000,
        bandwidth_hz: 125_000,
        sf: 8,
        cr: 5,
        tx_power_dbm: 19,
        preamble_len: 18,
        csma_enabled: true,
        radio_silent: false,
        st_alock: 0,
        lt_alock: 0,
        lt_alock_present: true,
        silence_lease_s: 0,
    }
}

/// The fake board; `halted` reads whether its transmitter is stopped.
fn single_uart_board(mut port: DuplexStream, requested: RadioConfigWire) -> Arc<AtomicBool> {
    let halted = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&halted);
    tokio::spawn(async move {
        let mut deframer = Deframer::with_max_frame(564);
        let mut buf = vec![0u8; 1024];
        let mut held: Vec<u8> = Vec::new();
        loop {
            let n = match port.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            let mut rx = Vec::with_capacity(n);
            for &b in &buf[..n] {
                match b {
                    XOFF => flag.store(true, Ordering::SeqCst),
                    XON => flag.store(false, Ordering::SeqCst),
                    _ => rx.push(b),
                }
            }
            for r in deframer.process(&rx) {
                let DeframeResult::Frame(data) = r else {
                    continue;
                };
                let reply = match envelope::decode_frame(&data) {
                    Err(_) if data.starts_with(&leviculum_core::rnode::RADIO_CONFIG_MAGIC) => {
                        leviculum_core::rnode::RADIO_CONFIG_ACK.to_vec()
                    }
                    Ok(f) if f.frame_type == envelope::TYPE_RADIO_QUERY => {
                        envelope::encode_radio_report(&requested)
                    }
                    _ => continue,
                };
                let mut framed = Vec::new();
                frame(&reply, &mut framed);
                held.push(XON);
                escape_flow(&framed, &mut |run| held.extend_from_slice(run));
            }
            if !flag.load(Ordering::SeqCst) && !held.is_empty() {
                if port.write_all(&held).await.is_err() {
                    return;
                }
                held.clear();
            }
        }
    });
    halted
}

async fn bring_up(mux: bool) -> (RadioBringUp, bool) {
    let (host, board) = tokio::io::duplex(8192);
    let halted = single_uart_board(board, phy());
    let mut port = SerialMuxPort::new(host, mux);
    let outcome = radio_bring_up(&mut port, &phy(), "mvr-m9").await;
    (outcome, halted.load(Ordering::SeqCst))
}

/// The fixture is what it claims: the config frame carries a raw 0x13.
#[test]
fn the_config_frame_carries_a_raw_xoff() {
    let mut framed = Vec::new();
    frame(
        &leviculum_core::rnode::build_radio_config_frame(&phy()),
        &mut framed,
    );
    assert!(framed.contains(&XOFF));
}

/// Without `serial_mux` the board's transmitter stops at the config's 0x13
/// and nothing it says ever comes back: the interface sees a silent board.
#[tokio::test(start_paused = true)]
async fn a_plain_line_halts_the_board() {
    let (outcome, halted) = bring_up(false).await;
    assert!(halted, "the raw XOFF must have stopped the board");
    assert_eq!(outcome, RadioBringUp::Silent);
}

/// With `serial_mux` the same config arrives escaped, the board stays
/// transmitting, its ack and its report (with their raw XONs) come back, and
/// the interface comes up on the profile.
#[tokio::test(start_paused = true)]
async fn a_mux_line_keeps_the_board_talking() {
    let (outcome, halted) = bring_up(true).await;
    assert!(!halted, "a mux line must never put a raw XOFF on the wire");
    assert_eq!(outcome, RadioBringUp::Adopted);
}

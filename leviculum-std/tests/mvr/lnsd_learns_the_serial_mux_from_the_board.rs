//! mvr: lnsd learns from the board whether its UART needs the serial mux
//! (1249, Refs #483, #471).
//!
//! 1242 made lnsd escape XON/XOFF for a block that says `serial_mux = yes`,
//! and nothing else: a user who plugged in a ThinkNode M9 without the key got
//! a board muted by the first raw 0x13 (the radio config at 19 dBm). The
//! CH340 has no USB strings, so the port cannot tell. The board can: its
//! capability report states `envelope::FACT_SERIAL_MUX`, and with the key
//! unset the bring-up asks for that report before any other frame.
//!
//! The boards here:
//! * the single-UART board of `a_raw_xoff_mutes_a_single_uart_board`: a raw
//!   0x13 halts its transmitter, a raw 0x11 releases it, both are deleted
//!   from what it receives; it answers the capability query, the legacy
//!   radio config and the radio query, with a raw XON before every answer
//!   and, in its report, a raw XON in the middle as its UART may insert one;
//! * a USB CDC LNode: it keeps every byte, and its report states no fact;
//! * a Python `SerialInterface` peer: it never answers a control frame, and
//!   sends one Reticulum packet during the wait.
//!
//! Red before: with `bring_up` not consulting the report (the 1242 state, mux
//! off unless configured), the fact-stating board comes back `Silent` and
//! halted, as `a_plain_line_halts_the_board` shows for 1242.

use leviculum_core::envelope;
use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_core::rnode::RadioConfigWire;
use leviculum_serial_mux::{escape_flow, XOFF, XON};
use leviculum_std::interfaces::serial_mux::{
    bring_up, detect_serial_mux, MuxDetection, SerialMuxPort, CAPABILITY_ANSWER_TIMEOUT,
    CAPABILITY_QUERY_ATTEMPTS,
};
use leviculum_std::interfaces::{radio_bring_up, RadioBringUp};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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

/// The accepted types a board reports; 0x11 and 0x13 among them, as on the
/// M9 (1238: `... 0e 13 14 0f ...`), so the report itself carries flow bytes.
const ACCEPTED: &[u8] = &[
    envelope::TYPE_RADIO_CONFIG,
    envelope::TYPE_CAPABILITIES,
    envelope::TYPE_RADIO_QUERY,
    envelope::TYPE_STORE_STORM,
    envelope::TYPE_MGMT_ALLOW,
];

#[derive(Clone, Copy)]
enum Board {
    /// One UART under XON/XOFF; `fact` is whether its report states it.
    SingleUart { fact: bool },
    /// A USB CDC LNode: every byte raw, no fact.
    Cdc,
    /// A Python `SerialInterface` peer.
    Python,
}

/// What the fake board saw: whether its transmitter is halted, and every
/// byte it received, as received.
struct Seen {
    halted: AtomicBool,
    rx: Mutex<Vec<u8>>,
}

/// A packet-shaped frame the Python peer sends during the wait.
const PACKET: &[u8] = &[0x00, 0x00, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x01, 0x02];

fn spawn_board(mut port: DuplexStream, board: Board) -> Arc<Seen> {
    let seen = Arc::new(Seen {
        halted: AtomicBool::new(false),
        rx: Mutex::new(Vec::new()),
    });
    let out = Arc::clone(&seen);
    tokio::spawn(async move {
        if let Board::Python = board {
            let mut framed = Vec::new();
            frame(PACKET, &mut framed);
            if port.write_all(&framed).await.is_err() {
                return;
            }
        }
        let single_uart = matches!(board, Board::SingleUart { .. });
        let mut deframer = Deframer::with_max_frame(564);
        let mut buf = vec![0u8; 1024];
        let mut held: Vec<u8> = Vec::new();
        loop {
            let n = match port.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            out.rx.lock().expect("rx").extend_from_slice(&buf[..n]);
            let mut rx = Vec::with_capacity(n);
            for &b in &buf[..n] {
                match b {
                    XOFF if single_uart => out.halted.store(true, Ordering::SeqCst),
                    XON if single_uart => out.halted.store(false, Ordering::SeqCst),
                    _ => rx.push(b),
                }
            }
            if let Board::Python = board {
                continue;
            }
            for r in deframer.process(&rx) {
                let DeframeResult::Frame(data) = r else {
                    continue;
                };
                let reply = match envelope::decode_frame(&data) {
                    Err(_) if data.starts_with(&leviculum_core::rnode::RADIO_CONFIG_MAGIC) => {
                        leviculum_core::rnode::RADIO_CONFIG_ACK.to_vec()
                    }
                    Ok(f) if f.frame_type == envelope::TYPE_CAPABILITIES => match board {
                        Board::SingleUart { fact: true } => {
                            envelope::encode_capability_report_with_facts(
                                ACCEPTED,
                                &[envelope::FACT_SERIAL_MUX],
                            )
                        }
                        _ => envelope::encode_capability_report(ACCEPTED),
                    },
                    Ok(f) if f.frame_type == envelope::TYPE_RADIO_QUERY => {
                        envelope::encode_radio_report(&phy())
                    }
                    _ => continue,
                };
                let mut framed = Vec::new();
                frame(&reply, &mut framed);
                if single_uart {
                    let mut escaped = Vec::new();
                    escape_flow(&framed, &mut |run| escaped.extend_from_slice(run));
                    held.push(XON);
                    let mid = escaped.len() / 2;
                    held.extend_from_slice(&escaped[..mid]);
                    held.push(XON);
                    held.extend_from_slice(&escaped[mid..]);
                } else {
                    held.extend_from_slice(&framed);
                }
            }
            if !out.halted.load(Ordering::SeqCst) && !held.is_empty() {
                if port.write_all(&held).await.is_err() {
                    return;
                }
                held.clear();
            }
        }
    });
    seen
}

/// The serial interface's bring-up order: settle the mux, then the radio.
async fn bring_up_on(
    board: Board,
    configured: Option<bool>,
) -> (Vec<Vec<u8>>, bool, RadioBringUp, Arc<Seen>) {
    let (host, far) = tokio::io::duplex(8192);
    let seen = spawn_board(far, board);
    let mut port = SerialMuxPort::new(host, false);
    let heard = bring_up(&mut port, configured, "mvr-1249").await;
    let mux = port.mux();
    let outcome = radio_bring_up(&mut port, &phy(), "mvr-1249").await;
    (heard, mux, outcome, seen)
}

/// The board states the fact: the mux goes on before the config, the 0x13
/// in the config goes out escaped, the board keeps talking, and the
/// interface comes up on the profile, with no key in the config.
#[tokio::test(start_paused = true)]
async fn a_board_that_states_the_fact_gets_the_mux_without_a_key() {
    let (_, mux, outcome, seen) = bring_up_on(Board::SingleUart { fact: true }, None).await;
    assert!(mux, "the report stated serial_mux=1");
    assert!(
        !seen.halted.load(Ordering::SeqCst),
        "no raw XOFF on the line"
    );
    assert!(!seen.rx.lock().expect("rx").contains(&XOFF));
    assert_eq!(outcome, RadioBringUp::Adopted);
}

/// The same board on 1242's footing (a build without the fact) is muted,
/// which is what the fact is for. Guards the fixture: the board does halt.
#[tokio::test(start_paused = true)]
async fn a_single_uart_board_without_the_fact_is_muted_as_before() {
    let (_, mux, outcome, seen) = bring_up_on(Board::SingleUart { fact: false }, None).await;
    assert!(!mux);
    assert!(seen.halted.load(Ordering::SeqCst));
    assert_eq!(outcome, RadioBringUp::Silent);
}

/// The key overrides the detection either way: `yes` turns the mux on
/// without asking, `no` keeps it off for a board that states the fact.
#[tokio::test(start_paused = true)]
async fn the_key_overrides_the_detection() {
    let (_, mux, outcome, seen) = bring_up_on(Board::SingleUart { fact: false }, Some(true)).await;
    assert!(mux);
    assert_eq!(outcome, RadioBringUp::Adopted);
    let mut query = Vec::new();
    frame(&envelope::encode_capability_query(), &mut query);
    let rx = seen.rx.lock().expect("rx").clone();
    assert!(
        !rx.windows(query.len()).any(|w| w == query.as_slice()),
        "a configured key sends no capability query"
    );

    let (_, mux, outcome, seen) = bring_up_on(Board::SingleUart { fact: true }, Some(false)).await;
    assert!(!mux);
    assert!(seen.halted.load(Ordering::SeqCst));
    assert_eq!(outcome, RadioBringUp::Silent);
}

/// A USB CDC LNode answers without the fact: the mux stays off and the
/// config's 0x13 reaches it raw, byte for byte as before 1249.
#[tokio::test(start_paused = true)]
async fn a_cdc_lnode_keeps_its_raw_bytes() {
    let (_, mux, outcome, seen) = bring_up_on(Board::Cdc, None).await;
    assert!(!mux);
    assert_eq!(outcome, RadioBringUp::Adopted);
    let mut config = Vec::new();
    frame(
        &leviculum_core::rnode::build_radio_config_frame(&phy()),
        &mut config,
    );
    let rx = seen.rx.lock().expect("rx").clone();
    assert!(
        rx.windows(config.len()).any(|w| w == config.as_slice()),
        "the config arrives unescaped, its raw 0x13 included"
    );

    // Its report carries 0x11 and 0x13 raw, which only the as-received
    // reading decodes: it is an answer, not silence.
    let (host, far) = tokio::io::duplex(8192);
    let _seen = spawn_board(far, Board::Cdc);
    let mut port = SerialMuxPort::new(host, false);
    let (verdict, _) = detect_serial_mux(&mut port, "mvr-1249").await;
    assert_eq!(verdict, MuxDetection::NotStated);
}

/// A Python peer never answers: the detection gives up after exactly
/// `CAPABILITY_QUERY_ATTEMPTS` queries of `CAPABILITY_ANSWER_TIMEOUT` each,
/// the mux stays off, the packet it sent during the wait is handed back for
/// the transport, and what it received is the queries and nothing escaped.
#[tokio::test(start_paused = true)]
async fn a_peer_that_never_answers_costs_the_bound_and_keeps_its_bytes() {
    let (host, far) = tokio::io::duplex(8192);
    let seen = spawn_board(far, Board::Python);
    let mut port = SerialMuxPort::new(host, false);
    let started = tokio::time::Instant::now();
    let (verdict, heard) = detect_serial_mux(&mut port, "mvr-1249").await;
    let waited = started.elapsed();
    assert_eq!(verdict, MuxDetection::NoAnswer);
    assert_eq!(
        waited,
        CAPABILITY_ANSWER_TIMEOUT * u32::from(CAPABILITY_QUERY_ATTEMPTS)
    );
    assert_eq!(heard, vec![PACKET.to_vec()]);

    port.set_mux(verdict.mux());
    port.write_all(&[0x7e, XOFF, 0x7e]).await.expect("write");
    tokio::task::yield_now().await;
    let mut query = Vec::new();
    frame(&envelope::encode_capability_query(), &mut query);
    let mut expected = query.repeat(usize::from(CAPABILITY_QUERY_ATTEMPTS));
    expected.extend_from_slice(&[0x7e, XOFF, 0x7e]);
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    assert_eq!(*seen.rx.lock().expect("rx"), expected);
}

/// The query itself carries no flow byte, so it is safe raw on any line.
#[test]
fn the_capability_query_carries_no_flow_byte() {
    let mut query = Vec::new();
    frame(&envelope::encode_capability_query(), &mut query);
    assert_eq!(query, [0x7e, 0xa4, 0xa5, 0x04, 0x00, 0x00, 0x7e]);
    assert!(!query.contains(&XON) && !query.contains(&XOFF));
}

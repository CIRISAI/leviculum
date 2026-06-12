//! USB composite device with two CDC-ACM serial ports
//!
//! Port 0 (debug): carries human-readable log output from `info!`/`warn!` macros.
//! Port 1 (transport): Reticulum serial interface with HDLC framing.
//!
//! The host sees two `/dev/ttyACM*` devices. The debug port sends log messages
//! formatted with `\r\n` line endings for terminal compatibility.

extern crate alloc;

use alloc::vec::Vec;
use embassy_time::Duration;

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_nrf::usb::vbus_detect::SoftwareVbusDetect;
use embassy_nrf::{peripherals, usb, Peri};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_time::with_timeout;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb::control::{OutResponse, Recipient, Request, RequestType};
use embassy_usb::{Builder, Config, Handler, UsbDevice};
use reticulum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use static_cell::StaticCell;

use crate::boards::BoardConfig;
use crate::log::{log_fmt, LOG_RING, LOG_SIGNAL};

// Interrupt bindings are centralized in ble.rs (CLOCK_POWER shared with MPSL)
use crate::ble::Irqs;

static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
static MSOS_DESC: StaticCell<[u8; 0]> = StaticCell::new();
static CONTROL_BUF: StaticCell<[u8; 128]> = StaticCell::new();
static CDC_DEBUG_STATE: StaticCell<State<'static>> = StaticCell::new();
static CDC_RETIC_STATE: StaticCell<State<'static>> = StaticCell::new();
static BAUD_TOUCH: StaticCell<BaudTouchHandler> = StaticCell::new();

/// Channels for serial interface: NodeCore ↔ USB CDC-ACM
static INCOMING_CHANNEL: Channel<CriticalSectionRawMutex, Vec<u8>, 8> = Channel::new();
static OUTGOING_CHANNEL: Channel<CriticalSectionRawMutex, Vec<u8>, 8> = Channel::new();

/// nRF52840 FICR base address
const FICR_BASE: u32 = 0x1000_0000;
/// DEVICEID[0] register offset
const FICR_DEVICEID0_OFFSET: u32 = 0x060;
/// DEVICEID[1] register offset
const FICR_DEVICEID1_OFFSET: u32 = 0x064;

/// Read the nRF52840 factory-programmed unique device ID from FICR registers
/// and format as a 16-character uppercase hex string (static lifetime).
fn serial_number() -> &'static str {
    static SERIAL: StaticCell<[u8; 16]> = StaticCell::new();
    // SAFETY: FICR registers are read-only factory-programmed values, always safe to read
    let id0 =
        unsafe { core::ptr::read_volatile((FICR_BASE + FICR_DEVICEID0_OFFSET) as *const u32) };
    let id1 =
        unsafe { core::ptr::read_volatile((FICR_BASE + FICR_DEVICEID1_OFFSET) as *const u32) };
    let buf = SERIAL.init([0u8; 16]);
    hex_u32(&mut buf[0..8], id0);
    hex_u32(&mut buf[8..16], id1);
    // buf contains only ASCII hex digits, always valid UTF-8
    match core::str::from_utf8(buf) {
        Ok(s) => s,
        Err(_) => "0000000000000000",
    }
}

fn hex_u32(buf: &mut [u8], val: u32) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for i in 0..8 {
        buf[i] = HEX[((val >> (28 - i * 4)) & 0xF) as usize];
    }
}

/// Channel endpoints for the main loop to communicate with the serial task.
pub struct SerialChannels {
    /// Receive packets from USB (deframed by serial task)
    pub incoming_rx: Receiver<'static, CriticalSectionRawMutex, Vec<u8>, 8>,
    /// Send packets to USB (serial task frames and writes)
    pub outgoing_tx: Sender<'static, CriticalSectionRawMutex, Vec<u8>, 8>,
}

/// Initialize USB composite device and spawn driver tasks.
///
/// Returns channel endpoints for the Reticulum serial interface.
///
/// Spawns three tasks:
/// - `usb_task`: drives the USB bus state machine
/// - `debug_writer_task`: drains log channel to CDC-ACM port 0
/// - `retic_serial_task`: HDLC-framed bidirectional I/O on CDC-ACM port 1
pub fn init(
    spawner: &Spawner,
    usbd: Peri<'static, peripherals::USBD>,
    vbus: &'static SoftwareVbusDetect,
    board: &'static BoardConfig,
) -> SerialChannels {
    let driver = usb::Driver::new(usbd, Irqs, vbus);

    // TODO: register a proper PID at https://pid.codes/
    let mut config = Config::new(board.usb_vid, board.usb_pid);
    config.manufacturer = Some(board.usb_manufacturer);
    config.product = Some(board.usb_product);
    config.serial_number = Some(serial_number());
    config.max_power = 100;
    config.composite_with_iads = true;

    let mut builder = Builder::new(
        driver,
        config,
        CONFIG_DESC.init([0; 256]),
        BOS_DESC.init([0; 256]),
        MSOS_DESC.init([0; 0]),
        CONTROL_BUF.init([0; 128]),
    );

    // 1200-baud-touch: Adafruit nRF52 bootloader trigger.
    // Registered before any CdcAcmClass so the handler sees SET_LINE_CODING
    // requests for either CDC port. See `BaudTouchHandler` below.
    builder.handler(BAUD_TOUCH.init(BaudTouchHandler));

    // CDC-ACM #0: Debug log output (interfaces 00+01)
    let cdc_debug = CdcAcmClass::new(&mut builder, CDC_DEBUG_STATE.init(State::new()), 64);

    // CDC-ACM #1: Reticulum serial interface (interfaces 02+03)
    let cdc_retic = CdcAcmClass::new(&mut builder, CDC_RETIC_STATE.init(State::new()), 64);

    let usb_dev = builder.build();

    spawner.must_spawn(usb_task(usb_dev));
    spawner.must_spawn(debug_writer_task(cdc_debug));
    spawner.must_spawn(runtime_drain_open_timeout_task());
    spawner.must_spawn(retic_serial_task(
        cdc_retic,
        INCOMING_CHANNEL.sender(),
        OUTGOING_CHANNEL.receiver(),
        crate::lora::config_sender(),
    ));

    SerialChannels {
        incoming_rx: INCOMING_CHANNEL.receiver(),
        outgoing_tx: OUTGOING_CHANNEL.sender(),
    }
}

type UsbDriver = usb::Driver<'static, &'static SoftwareVbusDetect>;

/// CDC PSTN spec §6.3.10: SET_LINE_CODING is class request 0x20,
/// addressed to the CDC communication interface. Payload is the 7-byte
/// LINE_CODING structure: u32 dwDTERate (LE), u8 bCharFormat,
/// u8 bParityType, u8 bDataBits.
const REQ_SET_LINE_CODING: u8 = 0x20;

/// USB control-request handler that triggers the Adafruit nRF52 bootloader
/// when the host opens any CDC port at exactly 1200 baud (the
/// "1200-baud-touch" convention). On match: writes the UF2-stay-in-bootloader
/// magic to GPREGRET and issues a soft reset. The bootloader reads GPREGRET
/// on next boot and stays in UF2 mass-storage mode for flashing.
///
/// All non-1200 SET_LINE_CODING requests fall through to the CdcAcm handler
/// untouched. Strict equality only, fuzzy matching would risk spurious
/// resets when terminal apps round 1215 → 1200.
struct BaudTouchHandler;

impl Handler for BaudTouchHandler {
    fn control_out(&mut self, req: Request, data: &[u8]) -> Option<OutResponse> {
        if req.request_type == RequestType::Class
            && req.recipient == Recipient::Interface
            && req.request == REQ_SET_LINE_CODING
            && data.len() >= 7
        {
            let data_rate = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            if data_rate == 1200 {
                // Hot path: no `await`, no allocation, no logging between the
                // GPREGRET write and `sys_reset()`. Any preemption here would
                // risk the magic being lost before the reset fires.
                // SAFETY: GPREGRET is a memory-mapped retained-register in the
                // POWER peripheral; safe to write any u32 value at any time.
                unsafe {
                    const GPREGRET: *mut u32 = 0x4000_051C as *mut u32;
                    const DFU_MAGIC_UF2_RESET: u32 = 0x57;
                    core::ptr::write_volatile(GPREGRET, DFU_MAGIC_UF2_RESET);
                }
                cortex_m::peripheral::SCB::sys_reset();
                // sys_reset() is `-> !` and never returns.
            }
        }
        // Pass-through for everything else: CdcAcm's own handler will accept
        // the SET_LINE_CODING and update its cached line_coding, or reject as
        // appropriate.
        None
    }
}

#[embassy_executor::task]
async fn usb_task(mut usb: UsbDevice<'static, UsbDriver>) {
    usb.run().await;
}

#[embassy_executor::task]
async fn debug_writer_task(mut cdc: CdcAcmClass<'static, UsbDriver>) {
    use embassy_time::{with_timeout, Duration};

    let mut chunk = [0u8; 64];
    loop {
        // Wait for new data or check every 100ms (catches DTR changes)
        let _ = with_timeout(Duration::from_millis(100), LOG_SIGNAL.wait()).await;

        // Only send if host has the port open
        if !cdc.dtr() {
            continue;
        }
        // First DTR-assert observation opens the runtime-drain gate so
        // the LoRa task / tracing subscriber stop dropping their output.
        crate::log::open_runtime_drain();

        // Drain ring buffer to USB
        loop {
            let n = LOG_RING.read(&mut chunk);
            if n == 0 {
                break;
            }
            match with_timeout(Duration::from_millis(50), cdc.write_packet(&chunk[..n])).await {
                Ok(Ok(())) => {}
                _ => break, // USB error or timeout
            }
        }
    }
}

/// Headless-fallback opener for the runtime-drain gate. If no host
/// asserts DTR within 30 s of boot, open the gate anyway so field
/// deployments without a debug host don't accumulate dropped output
/// indefinitely.
#[embassy_executor::task]
pub(crate) async fn runtime_drain_open_timeout_task() {
    use embassy_time::{Duration, Timer};
    Timer::after(Duration::from_secs(30)).await;
    crate::log::open_runtime_drain();
}

/// Lightweight log helpers for serial task diagnostics (no format_args overhead)
fn log(msg: &str) {
    log_fmt("[SER ] ", format_args!("{}", msg));
}

fn log_u32(msg: &str, val: u32) {
    log_fmt("[SER ] ", format_args!("{} {}", msg, val));
}

/// Serial HW_MTU (matches Python SerialInterface)
const SERIAL_HW_MTU: usize = 564;

/// Incomplete frame timeout. Python uses 100ms but also sets low_latency mode
/// so frames arrive as bulk USB packets. Without low_latency, byte-by-byte USB
/// delivery needs more time. 500ms gives 33x margin for a 167-byte frame at 115200.
const FRAME_TIMEOUT_MS: u64 = 500;

/// Reticulum serial interface task: HDLC-framed bidirectional I/O on USB CDC-ACM.
///
/// Read path: CDC read → HDLC deframe → incoming channel → NodeCore
/// Write path: NodeCore → outgoing channel → HDLC frame → CDC write
#[embassy_executor::task]
async fn retic_serial_task(
    mut cdc: CdcAcmClass<'static, UsbDriver>,
    incoming_tx: Sender<'static, CriticalSectionRawMutex, Vec<u8>, 8>,
    outgoing_rx: Receiver<'static, CriticalSectionRawMutex, Vec<u8>, 8>,
    config_tx: Sender<'static, CriticalSectionRawMutex, crate::lora::RadioConfig, 1>,
) {
    let mut deframer = Deframer::new();
    let mut read_buf = [0u8; 64];
    let mut frame_buf = Vec::with_capacity(1200);

    loop {
        log("SER: wait_connection");
        cdc.wait_connection().await;
        log("SER: connected");
        deframer.reset();

        loop {
            // Conditional timeout: 100ms when mid-frame, ~infinite when idle
            let timeout_dur = if deframer.is_in_frame() {
                Duration::from_millis(FRAME_TIMEOUT_MS)
            } else {
                Duration::from_secs(3600)
            };

            match select(
                with_timeout(timeout_dur, cdc.read_packet(&mut read_buf)),
                outgoing_rx.receive(),
            )
            .await
            {
                // Read succeeded within timeout
                Either::First(Ok(Ok(n))) => {
                    log_u32("SER: USB read", n as u32);
                    let results = deframer.process(&read_buf[..n]);
                    for r in results {
                        if let DeframeResult::Frame(ref data) = r {
                            // Check for radio config frame (test infrastructure)
                            if data.len() == crate::lora::CONFIG_FRAME_LEN
                                && data[0] == crate::lora::CONFIG_MAGIC[0]
                                && data[1] == crate::lora::CONFIG_MAGIC[1]
                            {
                                if let Some(cfg) = crate::lora::RadioConfig::from_wire(&data[2..]) {
                                    log("SER: radio config received");
                                    config_tx.send(cfg).await;
                                    // Send ACK back over serial
                                    frame(&crate::lora::CONFIG_ACK[..], &mut frame_buf);
                                    let mut ack_ok = true;
                                    for chunk in frame_buf.chunks(64) {
                                        if cdc.write_packet(chunk).await.is_err() {
                                            log("SER: config ACK write failed");
                                            ack_ok = false;
                                            break;
                                        }
                                    }
                                    if ack_ok && !frame_buf.is_empty() && frame_buf.len() % 64 == 0
                                    {
                                        let _ = cdc.write_packet(&[]).await;
                                    }
                                } else {
                                    log("SER: invalid config frame");
                                }
                            } else {
                                log_u32("SER: frame complete", data.len() as u32);
                                incoming_tx.send(data.clone()).await;
                            }
                        }
                    }
                    // HW_MTU enforcement
                    if deframer.buffer_len() > SERIAL_HW_MTU {
                        log("SER: HW_MTU exceeded, reset");
                        deframer.reset();
                    }
                }
                // USB disconnect
                Either::First(Ok(Err(_))) => {
                    log("SER: USB disconnect");
                    break;
                }
                // Frame timeout
                Either::First(Err(_)) => {
                    if deframer.is_in_frame() {
                        log("SER: frame timeout, reset");
                        deframer.reset();
                    }
                }
                // Outgoing packet to send
                Either::Second(data) => {
                    // Stage 5/6: log first 8 bytes of payload being sent to lnsd
                    let n = data.len().min(8);
                    let mut p8 = [0u8; 8];
                    p8[..n].copy_from_slice(&data[..n]);
                    crate::log::log_fmt(
                        "[T114_SERIAL_TX] ",
                        format_args!(
                            "pkt_hash8={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x} len={}",
                            p8[0],
                            p8[1],
                            p8[2],
                            p8[3],
                            p8[4],
                            p8[5],
                            p8[6],
                            p8[7],
                            data.len()
                        ),
                    );
                    log_u32("SER: TX", data.len() as u32);
                    frame(&data, &mut frame_buf);
                    log_u32("SER: HDLC framed", frame_buf.len() as u32);
                    let mut write_ok = true;
                    for chunk in frame_buf.chunks(64) {
                        if cdc.write_packet(chunk).await.is_err() {
                            log("SER: write_packet failed");
                            write_ok = false;
                            break;
                        }
                    }
                    // ZLP if last chunk was exactly 64 bytes
                    if write_ok && !frame_buf.is_empty() && frame_buf.len() % 64 == 0 {
                        let _ = cdc.write_packet(&[]).await;
                    }
                }
            }
        }
    }
}

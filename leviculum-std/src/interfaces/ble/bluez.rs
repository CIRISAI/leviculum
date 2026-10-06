//! The BlueZ half of the Columba BLE interface: everything that talks to
//! bluer. Thin by design — protocol decisions live in [`super::links`],
//! orchestration in [`super`]; this module performs advertising, the
//! GATT server, windowed scanning and the central-role connections, and
//! reports what happened as [`Ev`] events.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use bluer::adv::{Advertisement, AdvertisementHandle, Type as AdvType};
use bluer::gatt::local::{
    characteristic_control, Application, ApplicationHandle, Characteristic, CharacteristicControl,
    CharacteristicControlEvent, CharacteristicNotify, CharacteristicNotifyMethod,
    CharacteristicRead, CharacteristicWrite, CharacteristicWriteMethod, Service,
};
use bluer::gatt::remote::CharacteristicWriteRequest;
use bluer::gatt::CharacteristicReader;
use bluer::gatt::WriteOp;
use bluer::{Adapter, AdapterEvent, Address, DiscoveryFilter, DiscoveryTransport, Uuid};
use futures::{FutureExt, Stream, StreamExt};
use tokio::sync::{mpsc, oneshot};

use super::links::{IdentityHash, SERVICE_UUID_U128};
use super::{Ev, LINK_QUEUE_DEPTH};
use crate::event_log::Scalar;
use leviculum_ble_tx::COMPANY_ID;

/// The Columba GATT service and its three characteristics
/// (BLE_PROTOCOL_v2.2 §GATT Service Structure).
pub(crate) const SERVICE_UUID: Uuid = Uuid::from_u128(SERVICE_UUID_U128);
/// TX: peripheral → central, read + notify.
pub(crate) const TX_UUID: Uuid = Uuid::from_u128(0x37145b00_442d_4a94_917f_8f42c5da28e4);
/// RX: central → peripheral, write + write-without-response.
pub(crate) const RX_UUID: Uuid = Uuid::from_u128(0x37145b00_442d_4a94_917f_8f42c5da28e5);
/// Identity: the 16-byte identity hash, read-only.
pub(crate) const IDENTITY_UUID: Uuid = Uuid::from_u128(0x37145b00_442d_4a94_917f_8f42c5da28e6);

/// One scan window per discovery cycle. The reference scans in windows
/// too (its `balanced` mode: 1 s scan, `discovery_interval` pause);
/// windowed discovery also keeps BlueZ's scanner out of the way of our
/// own connection attempts, which mostly land in the pauses.
const SCAN_WINDOW: Duration = Duration::from_secs(2);

/// Budget for one central-role connection setup: connect, resolve,
/// identity read, subscribe, handshake.
const SETUP_TIMEOUT: Duration = Duration::from_secs(20);

/// Register the Columba advertisement.
///
/// Separate from [`serve_gatt`] because the two have different
/// lifetimes: the GATT application serves live sessions for the whole
/// BlueZ session, while the advertisement is gated on link-table
/// occupancy (`LinkTable::should_advertise`, the firmware's `ADV_LOCK`
/// policy) — deregistered when the table fills, re-registered when a
/// slot frees. Dropping the returned handle deregisters it.
///
/// The advertisement carries the service UUID and the v0.3.0 capability
/// record with flags 0x00 — dual-role, and deliberately *present* rather
/// than omitted, so peers sort us by the confirmed rule instead of the
/// assumed one. Since #412 the record also carries the first four bytes
/// of our identity hash, so a scanning peer can tell us from a stranger
/// before it spends a dial; the firmware advertises the identical
/// record, built by the identical function. BlueZ places the
/// `LN-<hex8>` local name in the scan response, where it does not
/// compete with the 31 advertisement bytes (same layout as the
/// firmware's).
///
/// `free_slots` is the table's free INCOMING count (#432): the same
/// quantity a board publishes, since lnsd now has a board's slot shape.
/// It is a parameter rather than read here, because this function is
/// also the only re-registration path — the caller compares what is on
/// the air with what the table says and calls again when they differ.
pub(crate) async fn register_advertisement(
    adapter: &Adapter,
    identity: IdentityHash,
    iface: &str,
    free_slots: u8,
) -> bluer::Result<AdvertisementHandle> {
    let name = super::links::local_name(&identity);
    // The manufacturer-record payload minus the company ID: BlueZ keys
    // the record by CID and prepends it on the wire.
    let record = super::links::capability_record(&identity, free_slots)[2..].to_vec();

    let advertisement = Advertisement {
        advertisement_type: AdvType::Peripheral,
        service_uuids: BTreeSet::from([SERVICE_UUID]),
        manufacturer_data: BTreeMap::from([(COMPANY_ID, record)]),
        discoverable: Some(true),
        local_name: Some(name.clone()),
        ..Default::default()
    };
    let adv = adapter.advertise(advertisement).await?;
    tracing::info!(
        "BLE {iface}: advertising as {name} (service {SERVICE_UUID}, dual-role record present, \
         identity hint {}, free incoming slots {free_slots})",
        super::links::hint_str(Some(leviculum_ble_tx::identity_hint(&identity))),
    );
    Ok(adv)
}

/// Register the Columba GATT application; it serves every
/// peripheral-role session for the whole BlueZ session. Dropping the
/// returned handle deregisters it.
///
/// The RX characteristic takes its writes over AcquireWrite
/// ([`CharacteristicWriteMethod::Io`], #436): BlueZ hands us one
/// SEQPACKET socket per connected central, and [`periph_reader`] reads
/// it in one task, so a central's frames reach the event channel in
/// the order the kernel received them. Through WriteValue each write
/// was its own D-Bus call on its own tokio task (bluer-0.17.4
/// `session.rs` spawns every crossroads method), and two fragments
/// issued back to back could arrive inverted. Once a central's socket
/// is acquired BlueZ routes every write on that connection through it,
/// with or without response (bluez-5.82 `src/gatt-database.c`,
/// `chrc_write_cb`: a `write_ios` entry for the ATT channel short-cuts
/// to `sock_io_send` before any WriteValue), so the identity
/// handshake, a Write Request, arrives on the socket too: it is the
/// write that triggers the AcquireWrite, and BlueZ forwards it once the
/// socket exists. The WriteValue method bluer still exports answers
/// NotSupported under `Io`; BlueZ only falls back to it when an
/// AcquireWrite fails.
pub(crate) async fn serve_gatt(
    adapter: &Adapter,
    identity: IdentityHash,
    ev_tx: mpsc::Sender<Ev>,
    iface: String,
) -> bluer::Result<ApplicationHandle> {
    let write_tx = ev_tx.clone();
    let notify_tx = ev_tx;
    let (rx_control, rx_control_handle) = characteristic_control();
    let app = Application {
        services: vec![Service {
            uuid: SERVICE_UUID,
            primary: true,
            characteristics: vec![
                Characteristic {
                    uuid: TX_UUID,
                    read: Some(CharacteristicRead {
                        read: true,
                        // The TX value is only ever pushed as
                        // notifications; a read returns nothing. The
                        // read flag exists because the reference
                        // declares it (`ble-reticulum@07d94130` `linux_bluetooth_driver.py`, TX char flags).
                        fun: Box::new(|_req| async move { Ok(Vec::new()) }.boxed()),
                        ..Default::default()
                    }),
                    notify: Some(CharacteristicNotify {
                        notify: true,
                        method: CharacteristicNotifyMethod::Fun(Box::new(move |notifier| {
                            let tx = notify_tx.clone();
                            async move {
                                let _ = tx.send(Ev::PeriphNotify(notifier)).await;
                            }
                            .boxed()
                        })),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Characteristic {
                    uuid: RX_UUID,
                    write: Some(CharacteristicWrite {
                        write: true,
                        write_without_response: true,
                        method: CharacteristicWriteMethod::Io,
                        ..Default::default()
                    }),
                    control_handle: rx_control_handle,
                    ..Default::default()
                },
                Characteristic {
                    uuid: IDENTITY_UUID,
                    read: Some(CharacteristicRead {
                        read: true,
                        fun: Box::new(move |_req| async move { Ok(identity.to_vec()) }.boxed()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }],
        ..Default::default()
    };
    let handle = adapter.serve_gatt_application(app).await?;
    // The acceptor ends with the registration: deregistering drops the
    // control handle, which ends the event stream.
    tokio::spawn(accept_writers(rx_control, write_tx, iface));
    Ok(handle)
}

/// Accept every central's AcquireWrite on the RX characteristic and
/// give each socket its own [`periph_reader`]. bluer's AcquireWrite
/// handler waits for this answer, so the loop does nothing else.
async fn accept_writers(
    mut control: CharacteristicControl,
    ev_tx: mpsc::Sender<Ev>,
    iface: String,
) {
    while let Some(event) = control.next().await {
        let CharacteristicControlEvent::Write(request) = event else {
            continue;
        };
        let addr = request.device_address();
        match request.accept() {
            Ok(reader) => {
                let mtu = reader.mtu();
                tracing::debug!("BLE {iface}: write socket acquired for {addr} (mtu {mtu})");
                tokio::spawn(periph_reader(
                    addr,
                    mtu,
                    reader_frames(reader),
                    ev_tx.clone(),
                ));
            }
            Err(e) => {
                // BlueZ falls back to WriteValue, which `Io` refuses: the
                // central's writes fail and the handshake timeout clears it.
                tracing::warn!("BLE {iface}: write socket for {addr} not created: {e}");
            }
        }
    }
}

/// One acquired write socket as a stream of written values, one item
/// per ATT write.
fn reader_frames(reader: CharacteristicReader) -> impl Stream<Item = std::io::Result<Vec<u8>>> {
    futures::stream::unfold(reader, |reader| async move {
        let frame = reader.recv().await;
        Some((frame, reader))
    })
}

/// One central's write stream, read by ONE task: each value goes to the
/// event channel as [`Ev::PeriphWrite`] in the order the socket yields
/// it, which is the order the kernel received the writes (#436). The
/// stream's end is the central's disconnect — BlueZ closes its end of
/// the socket when the ATT channel goes (bluez-5.82
/// `src/gatt-database.c`, `client_io_new` registers
/// `att_disconnect_cb`, which frees the client and its io) — and is
/// reported as [`Ev::PeriphGone`], always the reader's last event.
///
/// A zero-length read is that end of stream on SEQPACKET. A central
/// writing an empty value would read the same; the v2.2 protocol never
/// does (its shortest frame is the one-byte keepalive).
pub(crate) async fn periph_reader<S>(addr: Address, mtu: usize, frames: S, ev_tx: mpsc::Sender<Ev>)
where
    S: Stream<Item = std::io::Result<Vec<u8>>>,
{
    let mut frames = std::pin::pin!(frames);
    while let Some(Ok(data)) = frames.next().await {
        if data.is_empty() {
            break;
        }
        if ev_tx
            .send(Ev::PeriphWrite { addr, mtu, data })
            .await
            .is_err()
        {
            return;
        }
    }
    let _ = ev_tx.send(Ev::PeriphGone { addr }).await;
}

/// Windowed discovery: scan for [`SCAN_WINDOW`], report every sighting,
/// pause for the configured interval, repeat. Dropping the discovery
/// stream stops the BlueZ scan between windows.
pub(crate) struct ScanTask {
    pub adapter: Adapter,
    pub ev_tx: mpsc::Sender<Ev>,
    pub interval: Duration,
    pub iface: String,
}

impl ScanTask {
    pub(crate) async fn run(self) {
        let filter = DiscoveryFilter {
            uuids: [SERVICE_UUID].into_iter().collect(),
            transport: DiscoveryTransport::Le,
            ..Default::default()
        };
        if let Err(e) = self.adapter.set_discovery_filter(filter).await {
            tracing::warn!("BLE {}: discovery filter rejected: {e}", self.iface);
        }
        loop {
            // `discover_devices_with_changes`, not `discover_devices`:
            // plain `DeviceAdded` fires only when bluetoothd CREATES a
            // device object. For a device it already holds (stored after
            // an earlier connect, or created by a previous window), the
            // stream opens with a synthetic `DeviceAdded` carrying no
            // RSSI — BlueZ drops RSSI outside discovery, so the receiver
            // discards it as a cache entry — and the adv reports that
            // then land in the window only update properties, which the
            // plain stream never surfaces. Every window after the first
            // was blind to every known peer (BLE path hunt round 2,
            // 2026-08-31). The `_with_changes` stream re-emits
            // `DeviceAdded` on each property change, so the first adv
            // report of the window re-offers the device with its RSSI
            // set.
            match self.adapter.discover_devices_with_changes().await {
                Ok(events) => {
                    let mut events = std::pin::pin!(events);
                    let window_end = tokio::time::Instant::now() + SCAN_WINDOW;
                    loop {
                        let event = tokio::select! {
                            ev = events.next() => ev,
                            _ = tokio::time::sleep_until(window_end) => break,
                        };
                        match event {
                            Some(AdapterEvent::DeviceAdded(addr)) => {
                                if let Some(ev) = self.probe(addr).await {
                                    if self.ev_tx.send(ev).await.is_err() {
                                        return;
                                    }
                                }
                            }
                            Some(_) => {}
                            None => break,
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!("BLE {}: discovery failed: {e}", self.iface);
                }
            }
            tokio::time::sleep(self.interval).await;
        }
    }

    /// Read the sighting's decision inputs from BlueZ's parsed properties.
    async fn probe(&self, addr: Address) -> Option<Ev> {
        let device = self.adapter.device(addr).ok()?;
        let rssi = device.rssi().await.ok()?;
        let offers_service = device
            .uuids()
            .await
            .ok()?
            .is_some_and(|uuids| uuids.contains(&SERVICE_UUID));
        let record = device
            .manufacturer_data()
            .await
            .ok()
            .flatten()
            .and_then(|mut m| m.remove(&COMPANY_ID));
        Some(Ev::Scan {
            addr,
            rssi,
            offers_service,
            record,
        })
    }
}

/// One central-role connection: connect, read the peer's identity, ask
/// the orchestrator for admission, subscribe, handshake, then pump
/// frames in both directions until either side ends the session.
/// Always reports [`Ev::CentralGone`] as its last event.
pub(crate) struct CentralTask {
    pub adapter: Adapter,
    pub addr: Address,
    pub own_identity: IdentityHash,
    pub ev_tx: mpsc::Sender<Ev>,
    pub iface: String,
}

impl CentralTask {
    pub(crate) async fn run(self) {
        match tokio::time::timeout(SETUP_TIMEOUT, self.establish()).await {
            Ok(Ok(Some(session))) => self.session_loop(session).await,
            Ok(Ok(None)) => {
                // Admission refused; the orchestrator logged why.
            }
            Ok(Err(e)) => {
                tracing::debug!("BLE {}: central {} failed: {e}", self.iface, self.addr);
            }
            Err(_) => {
                tracing::debug!("BLE {}: central {} setup timed out", self.iface, self.addr);
            }
        }
        if let Ok(device) = self.adapter.device(self.addr) {
            let _ = device.disconnect().await;
        }
        let _ = self.ev_tx.send(Ev::CentralGone { addr: self.addr }).await;
    }

    async fn establish(&self) -> bluer::Result<Option<CentralSession>> {
        let device = self.adapter.device(self.addr)?;
        if !device.is_connected().await? {
            device.connect().await?;
        }

        // `Device::services` waits for BlueZ's ServicesResolved itself.
        let mut tx_char = None;
        let mut rx_char = None;
        let mut id_char = None;
        for service in device.services().await? {
            if service.uuid().await? != SERVICE_UUID {
                continue;
            }
            for characteristic in service.characteristics().await? {
                match characteristic.uuid().await? {
                    u if u == TX_UUID => tx_char = Some(characteristic),
                    u if u == RX_UUID => rx_char = Some(characteristic),
                    u if u == IDENTITY_UUID => id_char = Some(characteristic),
                    _ => {}
                }
            }
        }
        let (Some(tx_char), Some(rx_char), Some(id_char)) = (tx_char, rx_char, id_char) else {
            return Err(missing_gatt());
        };

        // Identity first: admission is decided before any subscription
        // or handshake touches the peer's session state (the firmware
        // orders it the same way).
        let identity_bytes = id_char.read().await?;
        let identity: IdentityHash = identity_bytes
            .as_slice()
            .try_into()
            .map_err(|_| missing_gatt())?;
        // BlueZ ≥ 5.62 exposes the exchanged ATT MTU as a characteristic
        // property; on anything older fall back to the BLE 4.0 floor,
        // the reference's final fallback too.
        let mtu = tx_char
            .mtu()
            .await
            .unwrap_or(leviculum_core::framing::ble::MIN_MTU);

        let (frames_tx, frames_rx) = mpsc::channel(LINK_QUEUE_DEPTH);
        let (ack_tx, ack_rx) = oneshot::channel();
        let sent = self
            .ev_tx
            .send(Ev::CentralIdentity {
                addr: self.addr,
                identity,
                mtu,
                frames: frames_tx,
                ack: ack_tx,
            })
            .await;
        if sent.is_err() || !ack_rx.await.unwrap_or(false) {
            return Ok(None);
        }

        // Subscribe before the handshake so nothing the peer sends lands
        // in an unsubscribed gap.
        let notify_stream = tx_char.notify().await?;

        // The 16-byte identity handshake, written WITH response
        // (BLE_PROTOCOL_v2.2 §Identity Handshake: a Write Request).
        rx_char
            .write_ext(
                &self.own_identity,
                &CharacteristicWriteRequest {
                    op_type: WriteOp::Request,
                    ..Default::default()
                },
            )
            .await?;

        Ok(Some(CentralSession {
            notify_stream: Box::pin(notify_stream),
            frames_rx,
            rx_char,
        }))
    }

    async fn session_loop(&self, mut session: CentralSession) {
        let write_req = CharacteristicWriteRequest {
            op_type: WriteOp::Command,
            ..Default::default()
        };
        // This link's inter-packet gap (#376): the queue carries one
        // packet's fragments per unit, so pacing packet-to-packet is a
        // sleep between units. Keepalives bypass the gap and never
        // slide its window, same rule as the firmware pumps.
        let start = tokio::time::Instant::now();
        let mut pacer = super::links::LinkPacer::new();
        loop {
            tokio::select! {
                frame = session.notify_stream.next() => {
                    let Some(data) = frame else { break };
                    let ev = Ev::CentralFrame { addr: self.addr, data };
                    if self.ev_tx.send(ev).await.is_err() {
                        break;
                    }
                }
                unit = session.frames_rx.recv() => {
                    let Some(frames) = unit else { break };
                    let keepalive = frames.len() == 1
                        && frames[0].len()
                            < leviculum_core::framing::ble::FRAGMENT_HEADER_SIZE;
                    if !keepalive {
                        let now_ms = tokio::time::Instant::now()
                            .duration_since(start)
                            .as_millis() as u64;
                        let wait = pacer.wait_ms(now_ms);
                        if wait > 0 {
                            tracing::info!(
                                event = "BLE_TX_GAP",
                                iface = %Scalar(&self.iface),
                                addr = %self.addr,
                                waited_ms = wait,
                            );
                            tokio::time::sleep(Duration::from_millis(wait)).await;
                        }
                    }
                    let mut failed = false;
                    for payload in &frames {
                        if let Err(e) = session.rx_char.write_ext(payload, &write_req).await {
                            tracing::debug!(
                                "BLE {}: write to {} failed: {e}",
                                self.iface,
                                self.addr
                            );
                            failed = true;
                            break;
                        }
                    }
                    if !keepalive {
                        pacer.packet_done(
                            tokio::time::Instant::now()
                                .duration_since(start)
                                .as_millis() as u64,
                        );
                    }
                    if failed {
                        break;
                    }
                }
            }
        }
    }
}

struct CentralSession {
    notify_stream: std::pin::Pin<Box<dyn futures::Stream<Item = Vec<u8>> + Send>>,
    frames_rx: mpsc::Receiver<Vec<Vec<u8>>>,
    rx_char: bluer::gatt::remote::Characteristic,
}

fn missing_gatt() -> bluer::Error {
    bluer::Error {
        kind: bluer::ErrorKind::ServicesUnresolved,
        message: "Columba GATT layout not found on peer".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interfaces::ble::links::{Inbound, LinkTable, DEFAULT_MAX_LINKS};
    use leviculum_core::framing::ble::fragment_packet;
    use std::os::fd::OwnedFd;

    const OWN: IdentityHash = [0x0E; 16];
    const PEER: IdentityHash = [0xA1; 16];
    const ADDR: Address = Address([0xC0, 0x00, 0x00, 0x00, 0x00, 0x01]);
    const MTU: usize = 185;

    /// What BlueZ and bluer build for one AcquireWrite: a SEQPACKET pair
    /// (bluer-0.17.4 `gatt/mod.rs` `make_socket_pair`). The first half
    /// stands in for bluetoothd's end, the second is ours, as tokio
    /// reads it.
    fn acquired_socket() -> (socket2::Socket, tokio::net::UnixDatagram) {
        let (central, ours) =
            socket2::Socket::pair(socket2::Domain::UNIX, socket2::Type::SEQPACKET, None)
                .expect("socketpair");
        let ours = std::os::unix::net::UnixDatagram::from(OwnedFd::from(ours));
        ours.set_nonblocking(true).expect("nonblocking");
        (
            central,
            tokio::net::UnixDatagram::from_std(ours).expect("tokio socket"),
        )
    }

    /// The test's `reader_frames`: the same one-value-per-read stream,
    /// over the stand-in socket.
    fn socket_frames(
        socket: tokio::net::UnixDatagram,
    ) -> impl Stream<Item = std::io::Result<Vec<u8>>> {
        futures::stream::unfold(socket, |socket| async move {
            let mut buf = vec![0u8; MTU];
            let frame = socket.recv(&mut buf).await.map(|n| {
                buf.truncate(n);
                buf
            });
            Some((frame, socket))
        })
    }

    /// Run the reader over everything the central wrote, then close the
    /// central's end, and return every event the reader reported.
    async fn read_all(writes: &[Vec<u8>]) -> Vec<Ev> {
        let (central, ours) = acquired_socket();
        for w in writes {
            central.send(w).expect("central write");
        }
        drop(central);
        let (ev_tx, mut ev_rx) = mpsc::channel(64);
        periph_reader(ADDR, MTU, socket_frames(ours), ev_tx).await;
        let mut events = Vec::new();
        while let Ok(ev) = ev_rx.try_recv() {
            events.push(ev);
        }
        events
    }

    /// #436: two fragments a central wrote back to back reach the event
    /// channel in the order it wrote them, tagged with its address and
    /// the acquired MTU. Through WriteValue each write had its own task
    /// and the END could overtake its START.
    #[tokio::test]
    async fn the_write_socket_delivers_a_centrals_fragments_in_order() {
        let packet = vec![0x33; 211];
        let frags = fragment_packet(&packet, MTU);
        assert_eq!(frags.len(), 2);

        let events = read_all(&frags).await;
        let writes: Vec<Vec<u8>> = events
            .iter()
            .filter_map(|ev| match ev {
                Ev::PeriphWrite { addr, mtu, data } => {
                    assert_eq!((*addr, *mtu), (ADDR, MTU));
                    Some(data.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(writes, frags, "fragments arrive in write order");
    }

    /// #436: the socket's end is the central's disconnect. The reader
    /// reports it as its last event, and the orchestrator's answer to
    /// that event releases the peripheral link the handshake opened;
    /// before the socket the link waited for the expiry sweep.
    #[tokio::test]
    async fn the_write_sockets_end_releases_the_link() {
        let packet = vec![0x44; 211];
        let mut writes = vec![PEER.to_vec()];
        writes.extend(fragment_packet(&packet, MTU));

        let events = read_all(&writes).await;
        assert!(
            matches!(events.last(), Some(Ev::PeriphGone { addr }) if *addr == ADDR),
            "end of stream is reported, last"
        );

        // The orchestrator's two arms, as `handle_event` runs them.
        let mut table = LinkTable::new(OWN, DEFAULT_MAX_LINKS);
        let mut delivered = Vec::new();
        for ev in events {
            match ev {
                Ev::PeriphWrite { addr, mtu, data } => {
                    match table.peripheral_frame(addr.0, mtu, &data, 0) {
                        Inbound::Packet(p) => delivered.push(p),
                        Inbound::HandshakeComplete { .. } | Inbound::NeedMore => {}
                        other => panic!("unexpected {other:?}"),
                    }
                    assert_eq!(table.link_count(), 1);
                }
                Ev::PeriphGone { addr } => {
                    let gone = table.remove_peripheral_by_addr(&addr.0);
                    assert_eq!(gone.map(|(id, _, _)| id), Some(PEER));
                }
                _ => panic!("reader reports only writes and its end"),
            }
        }
        assert_eq!(delivered, vec![packet]);
        assert_eq!(table.link_count(), 0, "the link is released");
    }
}

//! +ciris (leviculum#101): the port-mapping tests. The schedule runs against
//! mock clients on tokio's paused clock; the real clients run against fake
//! gateways on loopback (a NAT-PMP/PCP responder on UDP 5351, an SSDP plus
//! SOAP IGD). Nothing here talks to a real router.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;

use super::engine::{
    BoxFut, Discover, Discovered, Engine, Lease, MapError, MapRequest, MappingClient, PortKey,
    Shared,
};
use super::*;
use crate::sync_ext::MutexRecover;

// Mocks

type Script = Box<dyn Fn(&MapRequest, usize) -> Result<Lease, MapError> + Send + Sync>;

struct MockClient {
    method: MappingMethod,
    v6: bool,
    script: Script,
    maps: Mutex<Vec<MapRequest>>,
    unmaps: Mutex<Vec<(PortKey, Lease)>>,
}

impl MockClient {
    fn new(method: MappingMethod, script: Script) -> Arc<Self> {
        Arc::new(MockClient {
            method,
            v6: false,
            script,
            maps: Mutex::new(Vec::new()),
            unmaps: Mutex::new(Vec::new()),
        })
    }
    fn v6(method: MappingMethod, script: Script) -> Arc<Self> {
        Arc::new(MockClient {
            method,
            v6: true,
            script,
            maps: Mutex::new(Vec::new()),
            unmaps: Mutex::new(Vec::new()),
        })
    }
    fn map_count(&self) -> usize {
        self.maps.lock_recover().len()
    }
}

impl MappingClient for MockClient {
    fn method(&self) -> MappingMethod {
        self.method
    }
    fn v6(&self) -> bool {
        self.v6
    }
    fn map<'a>(&'a self, req: &'a MapRequest) -> BoxFut<'a, Result<Lease, MapError>> {
        let n = {
            let mut maps = self.maps.lock_recover();
            maps.push(req.clone());
            maps.len()
        };
        let r = (self.script)(req, n);
        Box::pin(async move { r })
    }
    fn unmap<'a>(&'a self, key: PortKey, lease: &'a Lease) -> BoxFut<'a, Result<(), MapError>> {
        self.unmaps.lock_recover().push((key, lease.clone()));
        Box::pin(async { Ok(()) })
    }
}

type ClientList = Arc<Mutex<Vec<Arc<dyn MappingClient>>>>;

/// The clients the network offers now; a test swaps them to move the host
/// to another gateway.
struct MockDiscover(ClientList);

impl Discover for MockDiscover {
    fn discover(&self) -> BoxFut<'_, Discovered> {
        let clients = self.0.lock_recover().clone();
        Box::pin(async move {
            Discovered {
                clients,
                global_v6: Vec::new(),
            }
        })
    }
}

const PUBLIC: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 7);

fn tcp(port: u16) -> PortKey {
    PortKey {
        protocol: MappingProtocol::Tcp,
        internal_port: port,
        v6: false,
    }
}

fn grant(ip: Ipv4Addr, port: u16, secs: u32) -> Result<Lease, MapError> {
    Ok(Lease {
        external: SocketAddr::new(IpAddr::V4(ip), port),
        lifetime_secs: secs,
    })
}

fn same_port(secs: u32) -> Script {
    Box::new(move |req, _| grant(PUBLIC, req.external_port, secs))
}

fn silent() -> Script {
    Box::new(|_, _| {
        Err(MapError::Unsupported(
            FailReason::Timeout,
            "no answer".into(),
        ))
    })
}

struct Rig {
    network: ClientList,
    shared: Arc<Mutex<Shared>>,
    events: Arc<Mutex<Vec<PortMappingStatus>>>,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl Rig {
    fn start(keys: Vec<PortKey>, lease_secs: u32, clients: Vec<Arc<dyn MappingClient>>) -> Rig {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = Arc::clone(&events);
        let network: ClientList = Arc::new(Mutex::new(clients));
        let engine = Engine::new(
            keys,
            lease_secs,
            Box::new(MockDiscover(Arc::clone(&network))),
            Arc::clone(&shared),
            Box::new(move |s| sink_events.lock_recover().push(s.clone())),
        );
        let (stop, rx) = watch::channel(false);
        let task = tokio::spawn(engine.run(rx));
        Rig {
            network,
            shared,
            events,
            stop,
            task,
        }
    }
    fn status(&self, i: usize) -> PortMappingStatus {
        self.shared.lock_recover().statuses[i].clone()
    }
    fn events(&self) -> Vec<PortMappingStatus> {
        self.events.lock_recover().clone()
    }
    async fn finish(self) -> Vec<PortMappingStatus> {
        self.stop.send(true).unwrap();
        self.task.await.unwrap();
        let s = self.shared.lock_recover().statuses.clone();
        s
    }
}

async fn settle() {
    // Let the spawned engine run its due work on the paused clock.
    tokio::time::sleep(Duration::from_millis(10)).await;
}

// The schedule

#[tokio::test(start_paused = true)]
async fn an_unanswered_pcp_falls_back_to_natpmp_and_renews_at_half_life() {
    let pcp = MockClient::new(MappingMethod::Pcp, silent());
    let pmp = MockClient::new(MappingMethod::NatPmp, same_port(120));
    let rig = Rig::start(
        vec![tcp(4242), tcp(8765)],
        7200,
        vec![pcp.clone(), pmp.clone()],
    );
    settle().await;
    for i in 0..2 {
        let s = rig.status(i);
        assert_eq!(s.state, MappingState::Mapped);
        assert_eq!(s.method, Some(MappingMethod::NatPmp));
        assert_eq!(s.lease_secs, 120);
        assert!(s.lease_expires.is_some());
    }
    // PCP's silence on the first port spared the second its timeout.
    assert_eq!(pcp.map_count(), 1);
    assert_eq!(pmp.map_count(), 2);
    assert_eq!(rig.events().len(), 2);
    assert_eq!(
        rig.events()[0].external,
        Some(SocketAddr::new(PUBLIC.into(), 4242))
    );

    // Renewed at half of the granted 120 s, not of the 7200 s asked for.
    tokio::time::sleep(Duration::from_secs(59)).await;
    assert_eq!(pmp.map_count(), 2);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(pmp.map_count(), 4);
    assert_eq!(
        pcp.map_count(),
        1,
        "a held mapping renews with its own client"
    );
    assert_eq!(rig.events().len(), 2, "an unchanged renewal raises nothing");
    rig.finish().await;
}

/// A host that moved to another gateway (new default route, other
/// network) is noticed at the next renewal round: the old mapping is
/// reported lost and the new gateway asked at once.
#[tokio::test(start_paused = true)]
async fn a_changed_gateway_is_noticed_at_the_next_renewal() {
    let old = MockClient::new(MappingMethod::NatPmp, same_port(120));
    let new = MockClient::new(
        MappingMethod::Pcp,
        Box::new(|req, _| grant(Ipv4Addr::new(198, 51, 100, 4), req.external_port, 120)),
    );
    let rig = Rig::start(vec![tcp(4242)], 7200, vec![old.clone()]);
    settle().await;
    assert_eq!(rig.status(0).method, Some(MappingMethod::NatPmp));
    *rig.network.lock_recover() = vec![new.clone()];
    tokio::time::sleep(Duration::from_secs(61)).await;
    assert_eq!(old.map_count(), 1, "the old gateway is not renewed");
    let s = rig.status(0);
    assert_eq!(s.state, MappingState::Mapped);
    assert_eq!(s.method, Some(MappingMethod::Pcp));
    let events = rig.events();
    assert_eq!(events.len(), 3, "gained, lost, gained: {events:?}");
    assert_eq!(events[1].external, None);
    rig.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_router_chosen_port_is_reported_and_asked_for_again() {
    let pmp = MockClient::new(
        MappingMethod::NatPmp,
        Box::new(|req, n| {
            if n == 1 {
                assert_eq!(req.external_port, 4242, "first ask is the internal port");
            } else {
                assert_eq!(req.external_port, 40000, "a renewal asks for the held port");
            }
            grant(PUBLIC, 40000, 100)
        }),
    );
    let rig = Rig::start(vec![tcp(4242)], 7200, vec![pmp.clone()]);
    settle().await;
    let s = rig.status(0);
    assert_eq!(s.external, Some(SocketAddr::new(PUBLIC.into(), 40000)));
    assert!(s.port_differs());
    tokio::time::sleep(Duration::from_secs(51)).await;
    assert_eq!(pmp.map_count(), 2);
    rig.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_moved_external_port_raises_an_event() {
    let pmp = MockClient::new(
        MappingMethod::NatPmp,
        Box::new(|_, n| grant(PUBLIC, if n == 1 { 4242 } else { 5000 }, 100)),
    );
    let rig = Rig::start(vec![tcp(4242)], 7200, vec![pmp.clone()]);
    settle().await;
    tokio::time::sleep(Duration::from_secs(51)).await;
    let events = rig.events();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].external.unwrap().port(), 5000);
    rig.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_failed_renewal_retries_then_loses_the_mapping_at_expiry_and_recovers() {
    // Granted, then refused until the eighth call, then granted again.
    let pmp = MockClient::new(
        MappingMethod::NatPmp,
        Box::new(|req, n| {
            if n == 1 || n >= 8 {
                grant(PUBLIC, req.external_port, 120)
            } else {
                Err(MapError::Refused(
                    FailReason::Other,
                    "network failure".into(),
                ))
            }
        }),
    );
    let rig = Rig::start(vec![tcp(4242)], 7200, vec![pmp.clone()]);
    settle().await;
    assert_eq!(rig.status(0).state, MappingState::Mapped);

    // Renewal at 60 s fails; retries come inside the remaining lease.
    tokio::time::sleep(Duration::from_secs(61)).await;
    assert_eq!(pmp.map_count(), 2);
    assert!(
        matches!(rig.status(0).state, MappingState::Renewing { cause: FailReason::Other, ref detail } if detail.contains("network failure")),
        "still leased, renewal failing: {:?}",
        rig.status(0).state
    );
    assert!(rig.status(0).state.is_held());
    assert_eq!(rig.events().len(), 1, "a failing renewal is not a loss");
    // By 121 s the lease is gone: the mapping is reported lost.
    tokio::time::sleep(Duration::from_secs(62)).await;
    let s = rig.status(0);
    assert!(
        matches!(s.state, MappingState::Failed { cause: FailReason::Other, ref detail } if detail.contains("renewal")),
        "{s:?}"
    );
    assert_eq!(s.external, None);
    let lost = rig.events();
    assert_eq!(lost.len(), 2);
    assert_eq!(lost[1].external, None);

    // Backoff, then the gateway grants again: mapped, and said so.
    tokio::time::sleep(Duration::from_secs(1900)).await;
    assert_eq!(rig.status(0).state, MappingState::Mapped);
    assert_eq!(rig.events().len(), 3);
    rig.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_private_or_shared_external_address_is_deleted_and_reported_as_cgnat() {
    let pmp = MockClient::new(
        MappingMethod::Pcp,
        Box::new(|req, _| grant(Ipv4Addr::new(100, 64, 3, 9), req.external_port, 7200)),
    );
    let rig = Rig::start(vec![tcp(4242)], 7200, vec![pmp.clone()]);
    settle().await;
    let s = rig.status(0);
    assert!(
        matches!(s.state, MappingState::CgnatDetected { external_ip, reason }
            if external_ip == IpAddr::V4(Ipv4Addr::new(100, 64, 3, 9)) && reason.contains("carrier-grade")),
        "{s:?}"
    );
    assert_eq!(s.external, None);
    assert_eq!(
        pmp.unmaps.lock_recover().len(),
        1,
        "the useless mapping is deleted"
    );
    assert!(rig.events().is_empty(), "nothing reachable was gained");
    // Reported once, then looked at again only after six hours.
    tokio::time::sleep(Duration::from_secs(6 * 3600 - 100)).await;
    assert_eq!(pmp.map_count(), 1);
    tokio::time::sleep(Duration::from_secs(200)).await;
    assert_eq!(pmp.map_count(), 2);
    assert!(matches!(
        rig.status(0).state,
        MappingState::CgnatDetected { .. }
    ));
    rig.finish().await;
}

#[tokio::test(start_paused = true)]
async fn no_gateway_and_a_refusal_are_told_apart() {
    let rig = Rig::start(vec![tcp(4242)], 7200, vec![]);
    settle().await;
    assert_eq!(rig.status(0).state, MappingState::NoGateway);
    rig.finish().await;

    let refusing = MockClient::new(
        MappingMethod::UpnpIgd,
        Box::new(|_, _| {
            Err(MapError::Refused(
                FailReason::Refused,
                "ActionNotAuthorized".into(),
            ))
        }),
    );
    let rig = Rig::start(
        vec![tcp(4242)],
        7200,
        vec![MockClient::new(MappingMethod::Pcp, silent()), refusing],
    );
    settle().await;
    assert!(matches!(
        rig.status(0).state,
        MappingState::Failed { cause: FailReason::Refused, ref detail } if detail.contains("upnp-igd: ActionNotAuthorized")
    ));
    rig.finish().await;
}

#[tokio::test(start_paused = true)]
async fn a_stop_deletes_every_held_mapping() {
    let pmp = MockClient::new(MappingMethod::NatPmp, same_port(7200));
    let rig = Rig::start(vec![tcp(4242), tcp(8765)], 7200, vec![pmp.clone()]);
    settle().await;
    let events_before = rig.events().len();
    let after = rig.finish().await;
    assert!(after.iter().all(|s| s.state == MappingState::Removed));
    let unmapped: Vec<u16> = pmp
        .unmaps
        .lock_recover()
        .iter()
        .map(|(k, _)| k.internal_port)
        .collect();
    assert_eq!(unmapped, vec![4242, 8765]);
    assert_eq!(events_before, 2);
}

#[tokio::test(start_paused = true)]
async fn a_permanent_lease_is_rechecked_and_deleted_on_stop() {
    let upnp = MockClient::new(MappingMethod::UpnpIgd, same_port(0));
    let rig = Rig::start(vec![tcp(4242)], 7200, vec![upnp.clone()]);
    settle().await;
    let s = rig.status(0);
    assert_eq!(s.lease_secs, 0);
    assert_eq!(s.lease_expires, None);
    tokio::time::sleep(Duration::from_secs(1201)).await;
    assert_eq!(upnp.map_count(), 2, "re-added after 20 min");
    rig.finish().await;
    assert_eq!(upnp.unmaps.lock_recover().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn an_ipv6_entry_asks_only_the_ipv6_pcp_client() {
    let v4 = MockClient::new(MappingMethod::Pcp, same_port(7200));
    let v6 = MockClient::v6(
        MappingMethod::Pcp,
        Box::new(|req, _| {
            Ok(Lease {
                external: SocketAddr::new("2a01:4f8::7".parse().unwrap(), req.external_port),
                lifetime_secs: 7200,
            })
        }),
    );
    let key6 = PortKey {
        v6: true,
        ..tcp(4242)
    };
    let rig = Rig::start(vec![tcp(4242), key6], 7200, vec![v4.clone(), v6.clone()]);
    settle().await;
    assert_eq!(v4.map_count(), 1);
    assert_eq!(v6.map_count(), 1);
    assert!(rig.status(1).ipv6);
    assert_eq!(rig.status(1).state, MappingState::Mapped);
    rig.finish().await;

    // No IPv6 client: the IPv6 entry has no gateway, the IPv4 one maps.
    let rig = Rig::start(vec![tcp(4242), key6], 7200, vec![v4.clone()]);
    settle().await;
    assert_eq!(rig.status(0).state, MappingState::Mapped);
    assert_eq!(rig.status(1).state, MappingState::NoGateway);
    rig.finish().await;
}

#[test]
fn backoff_doubles_to_its_cap_and_renewal_is_at_half_life() {
    use super::engine::{backoff_for_test as backoff, renew_after_for_test as renew};
    assert_eq!(backoff(1), Duration::from_secs(60));
    assert_eq!(backoff(2), Duration::from_secs(120));
    assert_eq!(backoff(5), Duration::from_secs(960));
    assert_eq!(backoff(6), Duration::from_secs(1920));
    assert_eq!(backoff(7), Duration::from_secs(3600));
    assert_eq!(backoff(60), Duration::from_secs(3600));
    assert_eq!(renew(7200), Duration::from_secs(3600));
    assert_eq!(renew(10), Duration::from_secs(30), "never sooner than 30 s");
    assert_eq!(renew(0), Duration::from_secs(1200));
}

// The report

fn mapped(ip: &str) -> PortMappingStatus {
    PortMappingStatus {
        external: Some(SocketAddr::new(ip.parse().unwrap(), 4242)),
        state: MappingState::Mapped,
        ..PortMappingStatus::new(MappingProtocol::Tcp, 4242, false)
    }
}

#[test]
fn the_reachability_summary_claims_no_more_than_it_knows() {
    let mut r = ReachabilityReport::default();
    assert_eq!(r.is_directly_reachable(), Reachability::Unlikely);
    r.global_ipv6 = vec!["2a01:4f8::7".parse().unwrap()];
    assert_eq!(r.is_directly_reachable(), Reachability::GlobalIpv6);
    r.mappings = vec![mapped("203.0.113.7")];
    assert_eq!(r.is_directly_reachable(), Reachability::Mapped);
    let at: SocketAddr = "203.0.113.7:4242".parse().unwrap();
    r.last_check = Some((at, false, std::time::Instant::now()));
    assert_eq!(r.is_directly_reachable(), Reachability::Unlikely);
    r.last_check = Some((at, true, std::time::Instant::now()));
    assert_eq!(r.is_directly_reachable(), Reachability::Verified);

    let r = ReachabilityReport {
        mappings: vec![mapped("10.0.0.2")],
        ..Default::default()
    };
    assert_eq!(r.is_directly_reachable(), Reachability::Unlikely);
}

// Config

fn reticulum(ini: &str) -> crate::config::ReticulumConfig {
    crate::ini_config::parse_ini(&format!("[reticulum]\n{ini}\n"))
        .expect("parses")
        .reticulum
}

#[test]
fn the_config_key_reads_yes_and_no_and_refuses_other_words() {
    // Unset: the default, which the workspace's cargo config turns off
    // for tests.
    assert_eq!(
        PortMappingConfig::from_config(&reticulum(""))
            .unwrap()
            .enabled,
        default_on()
    );
    let on = PortMappingConfig::from_config(&reticulum(
        "  port_mapping = yes\n  port_mapping_gateway = 192.168.1.1",
    ))
    .unwrap();
    assert!(on.enabled);
    assert_eq!(on.gateway, Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
    assert_eq!(on.methods, PortMappingConfig::default().methods);
    assert!(
        !PortMappingConfig::from_config(&reticulum("  port_mapping = No"))
            .unwrap()
            .enabled
    );
    let bad = PortMappingConfig::from_config(&reticulum("  port_mapping = maybe"));
    assert!(bad.unwrap_err().to_string().contains("yes or no"));
    let bad = PortMappingConfig::from_config(&reticulum(
        "  port_mapping = yes\n  port_mapping_gateway = router.lan",
    ));
    assert!(bad.unwrap_err().to_string().contains("IP address"));

    let toml_on: crate::config::Config =
        toml::from_str("[reticulum]\nport_mapping = true\n").unwrap();
    assert!(
        PortMappingConfig::from_config(&toml_on.reticulum)
            .unwrap()
            .enabled
    );
    let toml_word: crate::config::Config =
        toml::from_str("[reticulum]\nport_mapping = \"yes\"\n").unwrap();
    assert!(
        PortMappingConfig::from_config(&toml_word.reticulum)
            .unwrap()
            .enabled
    );
}

#[test]
fn the_default_is_on_unless_the_environment_moves_it() {
    let on = cfg!(feature = "port-mapping");
    assert!(PortMappingConfig::default().enabled);
    assert_eq!(super::default_on_given(None), on);
    assert_eq!(super::default_on_given(Some("yes")), on);
    for off in ["off", "No", " 0 ", "false"] {
        assert!(!super::default_on_given(Some(off)), "{off}");
    }
}

/// Port mapping on by default must stay quiet on a network that never
/// answers: one request per method per round, rounds backing off to an
/// hour, and nothing in between.
#[tokio::test(start_paused = true)]
async fn a_silent_network_is_asked_rarely() {
    let pcp = MockClient::new(MappingMethod::Pcp, silent());
    let upnp = MockClient::new(MappingMethod::UpnpIgd, silent());
    let rig = Rig::start(
        vec![tcp(4242), tcp(8765), tcp(9000)],
        7200,
        vec![pcp.clone(), upnp.clone()],
    );
    settle().await;
    assert_eq!(upnp.map_count(), 1, "one SSDP round for all three ports");
    assert_eq!(rig.status(2).state, MappingState::NoGateway);
    // A day: rounds at 1, 3, 7, 15, 31, 63 min, then hourly.
    tokio::time::sleep(Duration::from_secs(86_400)).await;
    let rounds = upnp.map_count();
    assert!((20..=30).contains(&rounds), "{rounds} rounds in a day");
    assert_eq!(pcp.map_count(), rounds);
    rig.finish().await;
}

#[test]
fn validation_refuses_what_cannot_work() {
    let off = PortMappingConfig {
        enabled: false,
        ..Default::default()
    };
    assert!(off.validate().is_ok(), "off is always fine");
    let mut c = PortMappingConfig::enabled();
    assert_eq!(c.validate().is_ok(), cfg!(feature = "port-mapping"));
    c.lease = Duration::from_secs(5);
    assert!(c.validate().is_err());
    let c = PortMappingConfig {
        methods: vec![],
        ..PortMappingConfig::enabled()
    };
    assert!(c.validate().is_err());
}

// The real clients against fake gateways on loopback

#[cfg(feature = "port-mapping")]
mod against_fakes {
    use super::*;
    use crate::port_mapping::clients::{NatPmpClient, PcpClient, UpnpClient};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, UdpSocket};

    /// The fake NAT-PMP/PCP gateway needs UDP 5351 on loopback, which only
    /// one test can hold at a time.
    static PMP_PORT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[derive(Clone)]
    struct PmpBehaviour {
        speaks_pcp: bool,
        external: Ipv4Addr,
        /// Assign this external port instead of the suggested one.
        assign: Option<u16>,
        lifetime: u32,
    }

    /// A NAT-PMP (RFC 6886) and PCP (RFC 6887) responder on
    /// 127.0.0.1:5351. Records every request.
    struct FakePmp {
        requests: Arc<Mutex<Vec<Vec<u8>>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for FakePmp {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn bind_5351() -> Option<UdpSocket> {
        for _ in 0..50 {
            match UdpSocket::bind("127.0.0.1:5351").await {
                Ok(s) => return Some(s),
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        None
    }

    impl FakePmp {
        async fn start(b: PmpBehaviour) -> Option<FakePmp> {
            let Some(socket) = bind_5351().await else {
                eprintln!("UDP 127.0.0.1:5351 is taken on this host; skipping");
                return None;
            };
            let requests = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&requests);
            let task = tokio::spawn(async move {
                let mut buf = [0u8; 1100];
                loop {
                    let Ok((n, from)) = socket.recv_from(&mut buf).await else {
                        return;
                    };
                    let req = buf[..n].to_vec();
                    log.lock_recover().push(req.clone());
                    if let Some(resp) = answer(&b, &req) {
                        let _ = socket.send_to(&resp, from).await;
                    }
                }
            });
            Some(FakePmp { requests, task })
        }
        fn requests(&self) -> Vec<Vec<u8>> {
            self.requests.lock_recover().clone()
        }
    }

    fn answer(b: &PmpBehaviour, req: &[u8]) -> Option<Vec<u8>> {
        let epoch = 1000u32.to_be_bytes();
        match req.first()? {
            0 => {
                let op = *req.get(1)?;
                if op == 0 {
                    let mut r = vec![0, 128, 0, 0];
                    r.extend_from_slice(&epoch);
                    r.extend_from_slice(&b.external.octets());
                    return Some(r);
                }
                // MAP UDP (1) or TCP (2): 12-byte request.
                let internal = u16::from_be_bytes([req[4], req[5]]);
                let suggested = u16::from_be_bytes([req[6], req[7]]);
                let lifetime = u32::from_be_bytes([req[8], req[9], req[10], req[11]]);
                let (ext, life) = if lifetime == 0 {
                    (0, 0)
                } else {
                    (b.assign.unwrap_or(suggested.max(internal)), b.lifetime)
                };
                let mut r = vec![0, 128 + op, 0, 0];
                r.extend_from_slice(&epoch);
                r.extend_from_slice(&internal.to_be_bytes());
                r.extend_from_slice(&ext.to_be_bytes());
                r.extend_from_slice(&life.to_be_bytes());
                Some(r)
            }
            2 if !b.speaks_pcp => {
                // NAT-PMP-only: UNSUPP_VERSION in NAT-PMP's format.
                let mut r = vec![0, 128 + req[1], 0, 1];
                r.extend_from_slice(&epoch);
                Some(r)
            }
            2 => {
                // PCP MAP: 24-byte header, 36-byte opcode body.
                if req.len() < 60 || req[1] != 1 {
                    return None;
                }
                let lifetime = u32::from_be_bytes([req[4], req[5], req[6], req[7]]);
                let suggested = u16::from_be_bytes([req[42], req[43]]);
                let internal = u16::from_be_bytes([req[40], req[41]]);
                let (ext, life) = if lifetime == 0 {
                    (0, 0)
                } else {
                    (b.assign.unwrap_or(suggested.max(internal)), b.lifetime)
                };
                let mut r = vec![2, 0x81, 0, 0];
                r.extend_from_slice(&life.to_be_bytes());
                r.extend_from_slice(&epoch);
                r.extend_from_slice(&[0; 12]);
                r.extend_from_slice(&req[24..36]); // nonce
                r.push(req[36]); // protocol
                r.extend_from_slice(&[0; 3]);
                r.extend_from_slice(&internal.to_be_bytes());
                r.extend_from_slice(&ext.to_be_bytes());
                r.extend_from_slice(&std::net::Ipv4Addr::to_ipv6_mapped(&b.external).octets());
                Some(r)
            }
            _ => None,
        }
    }

    const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;

    fn req(port: u16) -> MapRequest {
        MapRequest {
            key: tcp(port),
            lease_secs: 7200,
            external_port: port,
        }
    }

    #[tokio::test]
    async fn pcp_maps_renews_and_deletes_against_a_loopback_gateway() {
        let _port = PMP_PORT.lock().await;
        let Some(fake) = FakePmp::start(PmpBehaviour {
            speaks_pcp: true,
            external: PUBLIC,
            assign: Some(40001),
            lifetime: 3600,
        })
        .await
        else {
            return;
        };
        let client = PcpClient::new(crab_nat::GatewayAddress::IpV4(LOOPBACK), LOOPBACK.into());
        let lease = client.map(&req(4242)).await.expect("PCP maps");
        assert_eq!(lease.external, SocketAddr::new(PUBLIC.into(), 40001));
        assert_eq!(lease.lifetime_secs, 3600);
        // A renewal goes out with the same nonce, as RFC 6887 requires.
        client.map(&req(4242)).await.expect("PCP renews");
        client.unmap(tcp(4242), &lease).await.expect("PCP deletes");
        let reqs = fake.requests();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[0][24..36], reqs[1][24..36], "same nonce");
        assert_eq!(&reqs[2][4..8], &[0, 0, 0, 0], "a delete is lifetime 0");
    }

    #[tokio::test]
    async fn a_natpmp_only_gateway_turns_pcp_away_and_natpmp_maps() {
        let _port = PMP_PORT.lock().await;
        let Some(fake) = FakePmp::start(PmpBehaviour {
            speaks_pcp: false,
            external: PUBLIC,
            assign: None,
            lifetime: 7200,
        })
        .await
        else {
            return;
        };
        let pcp = PcpClient::new(crab_nat::GatewayAddress::IpV4(LOOPBACK), LOOPBACK.into());
        assert!(matches!(
            pcp.map(&req(4242)).await,
            Err(MapError::Unsupported(..))
        ));
        let pmp = NatPmpClient::new(LOOPBACK);
        let lease = pmp.map(&req(4242)).await.expect("NAT-PMP maps");
        assert_eq!(lease.external, SocketAddr::new(PUBLIC.into(), 4242));
        pmp.unmap(tcp(4242), &lease).await.expect("NAT-PMP deletes");
        let last = fake.requests().last().cloned().unwrap();
        assert_eq!(last[0], 0);
        assert_eq!(&last[8..12], &[0, 0, 0, 0], "a delete is lifetime 0");
    }

    /// A node with port mapping on maps its TCP listener through the
    /// loopback gateway, reports it and raises the event, and deletes it on
    /// stop.
    #[tokio::test]
    async fn a_node_maps_its_listener_reports_it_and_deletes_it_on_stop() {
        let _port = PMP_PORT.lock().await;
        let Some(fake) = FakePmp::start(PmpBehaviour {
            speaks_pcp: false,
            external: PUBLIC,
            assign: None,
            lifetime: 7200,
        })
        .await
        else {
            return;
        };
        let listener = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        let mut node = crate::driver::ReticulumNodeBuilder::new()
            .storage_path(dir.path().to_path_buf())
            .add_tcp_server_listener(listener)
            .port_mapping(PortMappingConfig {
                gateway: Some(IpAddr::V4(LOOPBACK)),
                // Never let a test fall through to SSDP on a real LAN.
                methods: vec![MappingMethod::Pcp, MappingMethod::NatPmp],
                ..PortMappingConfig::enabled()
            })
            .build()
            .await
            .unwrap();
        let mut events = node.take_event_receiver().unwrap();
        node.start().await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let event = loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(left, events.recv()).await {
                Ok(Some(e @ leviculum_core::node::NodeEvent::PortMappingChanged { .. })) => {
                    break e
                }
                Ok(Some(_)) => continue,
                other => panic!("no PortMappingChanged: {other:?}"),
            }
        };
        let leviculum_core::node::NodeEvent::PortMappingChanged {
            internal_port,
            udp,
            external,
            method,
        } = event
        else {
            unreachable!()
        };
        assert_eq!(internal_port, port);
        assert!(!udp);
        assert_eq!(external, Some(SocketAddr::new(PUBLIC.into(), port)));
        assert_eq!(method, "nat-pmp");

        let statuses = node.port_mappings();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].state, MappingState::Mapped);
        let report = node.reachability();
        assert_eq!(report.external_ipv4, Some(IpAddr::V4(PUBLIC)));
        assert_eq!(report.is_directly_reachable(), Reachability::Mapped);

        // The host's settings toggle: off deletes now, on maps again, and
        // a repeated call does nothing.
        assert!(node.port_mapping_enabled());
        let before = fake.requests().len();
        node.set_port_mapping(false).await.unwrap();
        assert!(!node.port_mapping_enabled());
        assert_eq!(node.port_mappings()[0].state, MappingState::Removed);
        let after_off = fake.requests();
        let delete = after_off[before..]
            .iter()
            .find(|r| r[0] == 0 && r[1] == 2)
            .expect("turning it off deleted the mapping");
        assert_eq!(&delete[8..12], &[0, 0, 0, 0]);
        let lost = next_mapping_event(&mut events).await;
        assert!(matches!(
            lost,
            leviculum_core::node::NodeEvent::PortMappingChanged { external: None, .. }
        ));
        node.set_port_mapping(false).await.unwrap();
        assert_eq!(
            fake.requests().len(),
            after_off.len(),
            "off twice is a no-op"
        );

        node.set_port_mapping(true).await.unwrap();
        let regained = next_mapping_event(&mut events).await;
        assert!(matches!(
            regained,
            leviculum_core::node::NodeEvent::PortMappingChanged {
                external: Some(_),
                ..
            }
        ));
        assert_eq!(node.port_mappings()[0].state, MappingState::Mapped);
        let after_on = fake.requests().len();
        node.set_port_mapping(true).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(fake.requests().len(), after_on, "on twice is a no-op");

        node.stop().await.unwrap();
        assert_eq!(node.port_mappings()[0].state, MappingState::Removed);
        let delete = fake
            .requests()
            .into_iter()
            .rev()
            .find(|r| r[0] == 0 && r[1] == 2)
            .unwrap();
        assert_eq!(
            &delete[8..12],
            &[0, 0, 0, 0],
            "the stop deleted the mapping"
        );
    }

    async fn next_mapping_event(
        events: &mut crate::driver::EventReceiver,
    ) -> leviculum_core::node::NodeEvent {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(left, events.recv()).await {
                Ok(Some(e @ leviculum_core::node::NodeEvent::PortMappingChanged { .. })) => {
                    return e
                }
                Ok(Some(_)) => continue,
                other => panic!("no PortMappingChanged: {other:?}"),
            }
        }
    }

    // UPnP

    #[derive(Clone, Default)]
    struct IgdBehaviour {
        /// Answer AddPortMapping with a lease other than 0 with 725.
        permanent_only: bool,
        /// Answer AddPortMapping for this external port with 718.
        taken: Option<u16>,
        /// Answer GetGenericPortMappingEntry with 401, as an IGD that
        /// cannot list its table.
        no_listing: bool,
        /// The router's mapping table.
        table: Arc<Mutex<Vec<FakeEntry>>>,
        /// Answer AddAnyPortMapping with 718.
        any_fails: bool,
        /// Answer DeletePortMapping with 606 (not authorized).
        delete_refuses: bool,
        /// Never answer DeletePortMapping.
        delete_hangs: bool,
    }

    #[derive(Clone, Debug, PartialEq)]
    struct FakeEntry {
        ext: u16,
        proto: String,
        int_port: u16,
        client: String,
        desc: String,
    }

    fn entry(ext: u16, proto: &str, int_port: u16, client: &str, desc: &str) -> FakeEntry {
        FakeEntry {
            ext,
            proto: proto.into(),
            int_port,
            client: client.into(),
            desc: desc.into(),
        }
    }

    /// An IGDv2 on loopback: an SSDP responder, the device and service
    /// descriptions, and a SOAP control endpoint. Records each action.
    struct FakeIgd {
        ssdp: SocketAddr,
        actions: Arc<Mutex<Vec<String>>>,
        tasks: Vec<tokio::task::JoinHandle<()>>,
    }

    impl Drop for FakeIgd {
        fn drop(&mut self) {
            for t in &self.tasks {
                t.abort();
            }
        }
    }

    const SERVICE: &str = "urn:schemas-upnp-org:service:WANIPConnection:2";

    fn device_xml() -> String {
        format!(
            r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
 <specVersion><major>1</major><minor>0</minor></specVersion>
 <device>
  <deviceType>urn:schemas-upnp-org:device:InternetGatewayDevice:2</deviceType>
  <deviceList><device>
   <deviceType>urn:schemas-upnp-org:device:WANDevice:2</deviceType>
   <deviceList><device>
    <deviceType>urn:schemas-upnp-org:device:WANConnectionDevice:2</deviceType>
    <serviceList><service>
     <serviceType>{SERVICE}</serviceType>
     <serviceId>urn:upnp-org:serviceId:WANIPConn1</serviceId>
     <SCPDURL>/scpd.xml</SCPDURL>
     <controlURL>/ctl</controlURL>
     <eventSubURL>/evt</eventSubURL>
    </service></serviceList>
   </device></deviceList>
  </device></deviceList>
 </device>
</root>"#
        )
    }

    fn scpd_xml() -> String {
        let action = |name: &str, args: &[&str]| {
            let args: String = args
                .iter()
                .map(|a| {
                    format!(
                        "<argument><name>{a}</name><direction>in</direction>\
                         <relatedStateVariable>X</relatedStateVariable></argument>"
                    )
                })
                .collect();
            format!("<action><name>{name}</name><argumentList>{args}</argumentList></action>")
        };
        let add = [
            "NewRemoteHost",
            "NewExternalPort",
            "NewProtocol",
            "NewInternalPort",
            "NewInternalClient",
            "NewEnabled",
            "NewPortMappingDescription",
            "NewLeaseDuration",
        ];
        format!(
            r#"<?xml version="1.0"?><scpd xmlns="urn:schemas-upnp-org:service-1-0"><actionList>{}{}{}{}</actionList></scpd>"#,
            action("AddPortMapping", &add),
            action("AddAnyPortMapping", &add),
            action(
                "DeletePortMapping",
                &["NewRemoteHost", "NewExternalPort", "NewProtocol"]
            ),
            action("GetExternalIPAddress", &[]),
        )
    }

    fn soap(body: &str) -> String {
        format!(
            r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body>{body}</s:Body></s:Envelope>"#
        )
    }

    fn fault(code: u16, desc: &str) -> String {
        soap(&format!(
            r#"<s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail><UPnPError xmlns="urn:schemas-upnp-org:control-1-0"><errorCode>{code}</errorCode><errorDescription>{desc}</errorDescription></UPnPError></detail></s:Fault>"#
        ))
    }

    fn tag(body: &str, name: &str) -> Option<String> {
        let open = format!("<{name}>");
        let start = body.find(&open)? + open.len();
        let end = body[start..].find('<')? + start;
        Some(body[start..end].to_string())
    }

    fn control(b: &IgdBehaviour, action: &str, body: &str) -> (u16, String) {
        let u = format!("u:{action}Response xmlns:u=\"{SERVICE}\"");
        let ok = |inner: &str| (200, soap(&format!("<{u}>{inner}</u:{action}Response>")));
        match action {
            "GetExternalIPAddress" => ok(&format!(
                "<NewExternalIPAddress>{PUBLIC}</NewExternalIPAddress>"
            )),
            "AddPortMapping" => {
                let lease = tag(body, "NewLeaseDuration").unwrap_or_default();
                let port: u16 = tag(body, "NewExternalPort")
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(0);
                let new = entry(
                    port,
                    &tag(body, "NewProtocol").unwrap_or_default(),
                    tag(body, "NewInternalPort")
                        .and_then(|p| p.parse().ok())
                        .unwrap_or(0),
                    &tag(body, "NewInternalClient").unwrap_or_default(),
                    &tag(body, "NewPortMappingDescription").unwrap_or_default(),
                );
                let mut table = b.table.lock_recover();
                let held = table
                    .iter()
                    .position(|e| e.ext == new.ext && e.proto == new.proto);
                if b.permanent_only && lease != "0" {
                    (500, fault(725, "OnlyPermanentLeasesSupported"))
                } else if b.taken == Some(port)
                    || held.is_some_and(|i| table[i].client != new.client)
                {
                    (500, fault(718, "ConflictInMappingEntry"))
                } else {
                    // Same port, same client: the entry is replaced.
                    match held {
                        Some(i) => table[i] = new,
                        None => table.push(new),
                    }
                    ok("")
                }
            }
            "AddAnyPortMapping" if b.any_fails => (500, fault(718, "ConflictInMappingEntry")),
            "AddAnyPortMapping" => ok("<NewReservedPort>50000</NewReservedPort>"),
            "DeletePortMapping" if b.delete_refuses => (500, fault(606, "Action not authorized")),
            "DeletePortMapping" => {
                let port: u16 = tag(body, "NewExternalPort")
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(0);
                let proto = tag(body, "NewProtocol").unwrap_or_default();
                b.table
                    .lock_recover()
                    .retain(|e| !(e.ext == port && e.proto == proto));
                ok("")
            }
            "GetGenericPortMappingEntry" if b.no_listing => (500, fault(401, "Invalid Action")),
            "GetGenericPortMappingEntry" => {
                let index: usize = tag(body, "NewPortMappingIndex")
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(usize::MAX);
                match b.table.lock_recover().get(index) {
                    None => (500, fault(713, "SpecifiedArrayIndexInvalid")),
                    Some(e) => ok(&format!(
                        "<NewRemoteHost></NewRemoteHost><NewExternalPort>{}</NewExternalPort>\
                         <NewProtocol>{}</NewProtocol><NewInternalPort>{}</NewInternalPort>\
                         <NewInternalClient>{}</NewInternalClient><NewEnabled>1</NewEnabled>\
                         <NewPortMappingDescription>{}</NewPortMappingDescription>\
                         <NewLeaseDuration>0</NewLeaseDuration>",
                        e.ext, e.proto, e.int_port, e.client, e.desc
                    )),
                }
            }
            _ => (500, fault(401, "Invalid Action")),
        }
    }

    async fn serve_http(
        stream: tokio::net::TcpStream,
        b: IgdBehaviour,
        log: Arc<Mutex<Vec<String>>>,
    ) {
        let mut stream = stream;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        // Read the head, then the body by Content-Length.
        let (head, mut body) = loop {
            let Ok(n) = stream.read(&mut chunk).await else {
                return;
            };
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..i]).to_string();
                break (head, buf[i + 4..].to_vec());
            }
        };
        let length = head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        while body.len() < length {
            let Ok(n) = stream.read(&mut chunk).await else {
                return;
            };
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        let body = String::from_utf8_lossy(&body).to_string();
        let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
        let (status, text) = if head.starts_with("GET") && path == "/desc.xml" {
            (200, device_xml())
        } else if head.starts_with("GET") && path == "/scpd.xml" {
            (200, scpd_xml())
        } else if head.starts_with("POST") && path == "/ctl" {
            let action = head
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("soapaction").then(|| {
                        v.trim()
                            .trim_matches('"')
                            .rsplit('#')
                            .next()
                            .map(str::to_string)
                    })?
                })
                .unwrap_or_default();
            log.lock_recover().push(format!(
                "{action} ext={} lease={}",
                tag(&body, "NewExternalPort").unwrap_or_default(),
                tag(&body, "NewLeaseDuration").unwrap_or_default()
            ));
            if action == "DeletePortMapping" && b.delete_hangs {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            control(&b, &action, &body)
        } else {
            (404, String::new())
        };
        let reason = if status == 200 { "OK" } else { "Error" };
        let resp = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        );
        let _ = stream.write_all(resp.as_bytes()).await;
        let _ = stream.shutdown().await;
    }

    impl FakeIgd {
        async fn start(b: IgdBehaviour) -> FakeIgd {
            let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let http_port = http.local_addr().unwrap().port();
            let ssdp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let ssdp_addr = ssdp.local_addr().unwrap();
            let actions = Arc::new(Mutex::new(Vec::new()));
            let ssdp_task = tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                loop {
                    let Ok((n, from)) = ssdp.recv_from(&mut buf).await else {
                        return;
                    };
                    if !String::from_utf8_lossy(&buf[..n]).starts_with("M-SEARCH") {
                        continue;
                    }
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\nUSN: uuid:fake::urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\nEXT:\r\nSERVER: fake/1 UPnP/1.1 fake/1\r\nLOCATION: http://127.0.0.1:{http_port}/desc.xml\r\n\r\n"
                    );
                    let _ = ssdp.send_to(resp.as_bytes(), from).await;
                }
            });
            let log = Arc::clone(&actions);
            let http_task = tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = http.accept().await else {
                        return;
                    };
                    tokio::spawn(serve_http(stream, b.clone(), Arc::clone(&log)));
                }
            });
            FakeIgd {
                ssdp: ssdp_addr,
                actions,
                tasks: vec![ssdp_task, http_task],
            }
        }
        /// The actions, less the table reads of the start-up reclaim.
        fn actions(&self) -> Vec<String> {
            self.actions
                .lock_recover()
                .iter()
                .filter(|a| !a.starts_with("GetGenericPortMappingEntry"))
                .cloned()
                .collect()
        }
        fn listings(&self) -> usize {
            self.actions
                .lock_recover()
                .iter()
                .filter(|a| a.starts_with("GetGenericPortMappingEntry"))
                .count()
        }
    }

    const TAG: &str = "abcdefabcdef";

    fn upnp(igd: &FakeIgd, tcp_ports: &[u16]) -> UpnpClient {
        UpnpClient::new(
            LOOPBACK,
            igd.ssdp,
            TAG.into(),
            tcp_ports
                .iter()
                .map(|p| (MappingProtocol::Tcp, *p))
                .collect(),
        )
        .trusting(LOOPBACK.into())
    }

    /// A crash on an IGDv1 router leaves a permanent mapping behind. At the
    /// next start the node finds its own (our tag, this host, a port it
    /// maps), asks for the same external port again, and deletes a
    /// duplicate; every other entry, another leviculum node's included,
    /// stays as it was.
    #[tokio::test]
    async fn our_stale_upnp_mapping_is_reclaimed_and_nothing_else_is_touched() {
        let ours = format!("leviculum:{TAG}:tcp/4242");
        let others = vec![
            // Another leviculum node on the same host: a different tag.
            entry(
                42000,
                "TCP",
                4242,
                "127.0.0.1",
                "leviculum:ffffffffffff:tcp/4242",
            ),
            // Ours by tag, but another host's address.
            entry(42001, "TCP", 4242, "127.0.0.9", &ours),
            // Ours by tag and host, but a port this run does not map.
            entry(
                42002,
                "TCP",
                5555,
                "127.0.0.1",
                &format!("leviculum:{TAG}:tcp/5555"),
            ),
            // Unrelated.
            entry(3074, "UDP", 3074, "127.0.0.5", "Microsoft Multiplayer"),
        ];
        let mut table = vec![entry(41000, "TCP", 4242, "127.0.0.1", &ours)];
        table.extend(others.clone());
        table.push(entry(41001, "TCP", 4242, "127.0.0.1", &ours));
        let b = IgdBehaviour {
            table: Arc::new(Mutex::new(table)),
            ..Default::default()
        };
        let igd = FakeIgd::start(b.clone()).await;
        let client = upnp(&igd, &[4242]);
        let lease = client.map(&req(4242)).await.expect("UPnP maps");
        assert_eq!(
            lease.external.port(),
            41000,
            "the external port survives the restart"
        );
        assert_eq!(igd.listings(), 7, "six entries read, then the end");
        let after = b.table.lock_recover().clone();
        assert_eq!(after[0], entry(41000, "TCP", 4242, "127.0.0.1", &ours));
        assert_eq!(
            after[1..],
            others[..],
            "the duplicate is gone, the rest untouched"
        );
        assert_eq!(
            igd.actions(),
            vec![
                "DeletePortMapping ext=41001 lease=",
                "AddPortMapping ext=41000 lease=7200",
                "GetExternalIPAddress ext= lease=",
            ]
        );
    }

    /// An IGD that cannot list its table: the plain path, which asks for
    /// the internal port as the external one and so replaces a stale entry
    /// of the same client and port.
    #[tokio::test]
    async fn a_gateway_that_cannot_list_its_table_gets_the_same_port_re_added() {
        let ours = format!("leviculum:{TAG}:tcp/4242");
        let b = IgdBehaviour {
            no_listing: true,
            table: Arc::new(Mutex::new(vec![entry(
                4242,
                "TCP",
                4242,
                "127.0.0.1",
                &ours,
            )])),
            ..Default::default()
        };
        let igd = FakeIgd::start(b.clone()).await;
        let lease = upnp(&igd, &[4242])
            .map(&req(4242))
            .await
            .expect("UPnP maps");
        assert_eq!(lease.external.port(), 4242);
        assert_eq!(igd.listings(), 1, "asked once, refused, not again");
        assert_eq!(
            b.table.lock_recover().len(),
            1,
            "replaced, not added beside"
        );
        assert!(!igd.actions().iter().any(|a| a.starts_with("Delete")));
    }

    #[tokio::test]
    async fn a_taken_port_the_router_cannot_replace_is_port_taken() {
        let igd = FakeIgd::start(IgdBehaviour {
            taken: Some(4242),
            any_fails: true,
            ..Default::default()
        })
        .await;
        let err = upnp(&igd, &[4242]).map(&req(4242)).await.unwrap_err();
        assert!(
            matches!(err, MapError::Refused(FailReason::PortTaken, _)),
            "{err:?}"
        );
    }

    /// Run the engine over one UPnP client until the mapping is held, then
    /// stop it: the clean-stop path, as `set_port_mapping(false)` runs it.
    async fn map_then_stop(b: IgdBehaviour) -> (PortMappingStatus, Vec<PortMappingStatus>) {
        let igd = FakeIgd::start(b).await;
        let client: Arc<dyn MappingClient> = Arc::new(upnp(&igd, &[4242]));
        let rig = Rig::start(vec![tcp(4242)], 7200, vec![client]);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !rig.status(0).state.is_held() {
            assert!(tokio::time::Instant::now() < deadline, "never mapped");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let events = Arc::clone(&rig.events);
        let after = rig.finish().await;
        let events = events.lock_recover().clone();
        (after[0].clone(), events)
    }

    #[tokio::test]
    async fn a_refused_delete_is_remove_failed_and_keeps_the_address() {
        let (s, events) = map_then_stop(IgdBehaviour {
            permanent_only: true,
            delete_refuses: true,
            ..Default::default()
        })
        .await;
        assert_eq!(
            s.state,
            MappingState::RemoveFailed {
                cause: FailReason::Refused,
                permanent: true
            }
        );
        let at = Some(SocketAddr::new(PUBLIC.into(), 4242));
        assert_eq!(s.external, at, "the host needs it to name the port");
        assert_eq!(events.last().unwrap().state, s.state);
        assert_eq!(events.last().unwrap().external, at);
    }

    #[tokio::test]
    async fn a_delete_that_never_answers_is_remove_failed_timeout() {
        let (s, _) = map_then_stop(IgdBehaviour {
            delete_hangs: true,
            ..Default::default()
        })
        .await;
        assert_eq!(
            s.state,
            MappingState::RemoveFailed {
                cause: FailReason::Timeout,
                permanent: false
            }
        );
    }

    #[test]
    fn failures_map_to_their_causes() {
        use crate::port_mapping::clients::{pcp_err, pmp_err, upnp_err};
        let cause = |e: MapError| (matches!(e, MapError::Unsupported(..)), e.cause());
        // Silence: a timeout, and the next method is tried.
        assert_eq!(
            cause(pcp_err(crab_nat::pcp::Failure::Timeout)),
            (true, FailReason::Timeout)
        );
        assert_eq!(
            cause(pmp_err(crab_nat::natpmp::Failure::Timeout)),
            (true, FailReason::Timeout)
        );
        // Nothing listens on 5351 (ICMP refused): no such protocol here.
        let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        assert_eq!(
            cause(pcp_err(crab_nat::pcp::Failure::Socket(refused))),
            (true, FailReason::Unsupported)
        );
        // The gateway said no.
        assert_eq!(
            cause(pcp_err(crab_nat::pcp::Failure::NotAuthorized(0))),
            (false, FailReason::Refused)
        );
        assert_eq!(
            cause(pmp_err(crab_nat::natpmp::Failure::NotAuthorized)),
            (false, FailReason::Refused)
        );
        use igd_next::RequestError as R;
        assert_eq!(
            cause(upnp_err(&R::ErrorCode(
                718,
                "ConflictInMappingEntry".into()
            ))),
            (false, FailReason::PortTaken)
        );
        assert_eq!(
            cause(upnp_err(&R::ErrorCode(606, "Action not authorized".into()))),
            (false, FailReason::Refused)
        );
        // A malformed answer is a protocol fault, and its body (which can
        // hold the external address) stays out of the detail.
        let bad = upnp_err(&R::InvalidResponse("<x>203.0.113.7</x>".into()));
        assert_eq!(bad.cause(), FailReason::Protocol);
        assert!(!bad.detail().contains("203.0.113.7"));
    }

    #[test]
    fn the_upnp_tag_is_stable_short_and_not_the_identity() {
        let id = [0x11u8; 16];
        let tag = crate::port_mapping::upnp_tag(&id);
        assert_eq!(tag.len(), 12);
        assert!(tag.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(tag, crate::port_mapping::upnp_tag(&id));
        assert_ne!(tag, crate::port_mapping::upnp_tag(&[0x12u8; 16]));
        assert!(!"11111111111111111111111111111111".contains(&tag));
    }

    #[tokio::test]
    async fn upnp_maps_the_internal_port_and_deletes_it() {
        let igd = FakeIgd::start(IgdBehaviour::default()).await;
        let client = upnp(&igd, &[4242]);
        let lease = client.map(&req(4242)).await.expect("UPnP maps");
        assert_eq!(lease.external, SocketAddr::new(PUBLIC.into(), 4242));
        assert_eq!(lease.lifetime_secs, 7200);
        client.unmap(tcp(4242), &lease).await.expect("UPnP deletes");
        assert_eq!(
            igd.actions(),
            vec![
                "AddPortMapping ext=4242 lease=7200",
                "GetExternalIPAddress ext= lease=",
                "DeletePortMapping ext=4242 lease=",
            ]
        );
    }

    #[tokio::test]
    async fn upnp_falls_back_to_a_permanent_lease_when_that_is_all_the_router_grants() {
        let igd = FakeIgd::start(IgdBehaviour {
            permanent_only: true,
            ..Default::default()
        })
        .await;
        let client = upnp(&igd, &[4242]);
        let lease = client.map(&req(4242)).await.expect("UPnP maps");
        assert_eq!(lease.lifetime_secs, 0);
        assert_eq!(
            igd.actions()[..2],
            [
                "AddPortMapping ext=4242 lease=7200".to_string(),
                "AddPortMapping ext=4242 lease=0".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn a_taken_upnp_port_gets_a_router_chosen_one() {
        let igd = FakeIgd::start(IgdBehaviour {
            taken: Some(4242),
            ..Default::default()
        })
        .await;
        let client = upnp(&igd, &[4242]);
        let lease = client.map(&req(4242)).await.expect("UPnP maps");
        assert_eq!(lease.external, SocketAddr::new(PUBLIC.into(), 50000));
        client.unmap(tcp(4242), &lease).await.unwrap();
        let actions = igd.actions();
        assert!(actions[1].starts_with("AddAnyPortMapping"), "{actions:?}");
        assert_eq!(
            actions.last().unwrap(),
            "DeletePortMapping ext=50000 lease="
        );
    }

    #[tokio::test]
    async fn no_upnp_answer_is_unsupported_not_refused() {
        // A bound socket that never answers.
        let mute = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client = UpnpClient::new(
            LOOPBACK,
            mute.local_addr().unwrap(),
            TAG.into(),
            vec![(MappingProtocol::Tcp, 4242)],
        );
        assert!(matches!(
            client.map(&req(4242)).await,
            Err(MapError::Unsupported(..))
        ));
    }
}

/// Against the real router of the host running it: discovery only, unless
/// `LEVICULUM_REAL_ROUTER_MAP=1`, which also maps TCP 47123 to this host for
/// two minutes and deletes it again. Never in CI: `#[ignore]`, run by hand
/// with `cargo test -p leviculum-std --lib real_router -- --ignored
/// --nocapture`.
#[cfg(feature = "port-mapping")]
#[tokio::test]
#[ignore = "talks to the real LAN router"]
async fn real_router() {
    use crate::port_mapping::clients::{NatPmpClient, PcpClient, UpnpClient};
    use crate::port_mapping::netinfo;
    let gw = netinfo::default_gateway_v4();
    let local = gw.and_then(|g| netinfo::local_address_toward(SocketAddr::new(g.into(), 5351)));
    println!("default gateway {gw:?}, local address {local:?}");
    println!("global IPv6: {:?}", netinfo::global_ipv6_addresses());
    println!("IPv6 gateway: {:?}", netinfo::default_gateway_v6());
    let (Some(gw), Some(IpAddr::V4(local))) = (gw, local) else {
        return;
    };
    let gateway = crab_nat::GatewayAddress::IpV4(gw);
    // Read-only: NAT-PMP's external-address request maps nothing.
    let ext = crab_nat::natpmp::external_address(
        gateway,
        Some(crab_nat::TimeoutConfig {
            initial_timeout: Duration::from_millis(250),
            max_retries: 2,
            max_retry_timeout: Some(Duration::from_secs(1)),
        }),
    )
    .await;
    println!("NAT-PMP external address: {ext:?}");
    // Read-only: SSDP search and GetExternalIPAddress.
    let mut search = igd_next::SearchOptions::default();
    search.bind_addr = SocketAddr::new(IpAddr::V4(local), 0);
    search.timeout = Some(Duration::from_secs(3));
    match igd_next::aio::tokio::search_gateway(search).await {
        Ok(igd) => {
            println!("UPnP gateway {} service {}", igd.addr, igd.service_type);
            let ip = igd.get_external_ip().await;
            println!(
                "UPnP external address: {:?} ({})",
                ip,
                match &ip {
                    Ok(ip) => netinfo::non_public_reason(*ip).unwrap_or("public"),
                    Err(_) => "-",
                }
            );
        }
        Err(e) => println!("UPnP: {e}"),
    }
    if std::env::var("LEVICULUM_REAL_ROUTER_MAP").as_deref() != Ok("1") {
        return;
    }
    let req = MapRequest {
        key: tcp(47123),
        lease_secs: 120,
        external_port: 47123,
    };
    let clients: Vec<Arc<dyn MappingClient>> = vec![
        Arc::new(PcpClient::new(gateway, local.into())),
        Arc::new(NatPmpClient::new(gw)),
        Arc::new(UpnpClient::new(
            local,
            "239.255.255.250:1900".parse().unwrap(),
            crate::port_mapping::upnp_tag(b"real_router test"),
            vec![(MappingProtocol::Tcp, 47123)],
        )),
    ];
    for c in clients {
        match c.map(&req).await {
            Ok(lease) => {
                println!("{} mapped: {lease:?}", c.method().as_str());
                let removed = c.unmap(req.key, &lease).await;
                println!("{} removed: {removed:?}", c.method().as_str());
                assert!(removed.is_ok(), "delete it on the router by hand");
                return;
            }
            Err(e) => println!("{}: {e:?}", c.method().as_str()),
        }
    }
}

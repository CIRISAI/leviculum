# Port mapping

A home node behind an ordinary router cannot be reached from the internet:
the router drops every connection it did not see go out. Port mapping asks
the router to forward the node's listening ports to it, so peers can connect
in (leviculum#101). Constitution 5.4.6 makes a home node a relay parent when
it is directly reachable and offers relay, so port mapping decides how many
homes can be parents.

The feature is in `leviculum-std`, behind the `port-mapping` cargo feature,
which is on by default. `leviculum-core` gains no dependency; it only carries
the event.

## What is mapped

With port mapping on, each run of the node asks for:

- every `TCPServerInterface` (and listening `BackboneInterface`) port;
- every `WebSocketServerInterface` port;
- the direct-link facilitator's UDP port (`probe_port`), when the node runs
  one;
- any extra ports the host lists in `PortMappingConfig::extra_ports`.

A listener bound to a loopback address is skipped. A listener bound to an
IPv6 address (or `::`) also gets an IPv6 entry; see below.

The UDP sockets of direct-link sessions are not mapped. Each session binds an
ephemeral port and punches its own path, so a mapping would not help.

## How

The node tries three protocols, in this order, and keeps the first that
answers:

| Method | Spoken to | Library |
|---|---|---|
| PCP (RFC 6887) | the default gateway, UDP 5351 | `crab_nat` |
| NAT-PMP (RFC 6886) | the default gateway, UDP 5351 | `crab_nat` |
| UPnP-IGD | whatever answers SSDP on the LAN (WANIPConnection v2 or v1, or WANPPPConnection) | `igd-next` |

A gateway that does not answer one protocol costs under two seconds before
the next is tried, and that protocol is skipped for the other ports in the
same round. The node asks for the external port equal to the internal one. If
the router gives a different port (PCP and NAT-PMP may, and a UPnP router
whose port is taken is asked with `AddAnyPortMapping`), the router's port is
reported, and that is the port a host has to advertise.

The node asks for a two-hour lease and renews at half of what the router
granted. If a renewal fails, it retries while the lease lasts; when the lease
runs out the mapping is reported lost, and the node starts over with backoff
(one minute, doubling, at most 30 minutes). Some IGDv1 routers grant only
permanent mappings; the node takes one (it logs a warning) and re-adds it
every 20 minutes, so a router that rebooted and forgot it gets it back.

On a clean stop (`stop` or `stop_shared`) the node deletes every mapping it
holds and waits up to 12 seconds for the router to confirm. After an unclean
exit a leased mapping lapses with its lease. A permanent UPnP mapping does
not lapse: delete it on the router, or start the node again and stop it
cleanly.

The gateway for PCP and NAT-PMP comes from the routing table
(`/proc/net/route` on Linux, `route -n get default` on macOS and the BSDs,
`route print` on Windows), or from `port_mapping_gateway`.

## Carrier-grade and double NAT

Many ISPs, and nearly every mobile network, put customers behind a
carrier-grade NAT. A home router there can map a port, but only on its own
WAN side, which the internet cannot reach. When a router reports an external
address in the shared range (100.64.0.0/10), a private range (RFC 1918), or
link-local, the node deletes the mapping again and reports
`MappingState::CgnatDetected`. It looks again every 30 minutes.

That check only catches a CGNAT the router knows about. A router can report
a public-looking address and still sit behind filtering, or behind a NAT that
rewrites it. So a mapping is a claim, not proof of reachability.

## IPv6

A host with a global IPv6 address needs no NAT mapping to be addressed, but
the router's firewall usually still drops inbound connections. For each
listener bound to an IPv6 address, the node asks the IPv6 default gateway
for a PCP firewall pinhole (a PCP MAP request over IPv6). The IPv6 gateway is
read from `/proc/net/ipv6_route`, so this works on Linux only. Many home
routers do not speak PCP. Without it, the entry stays `NoGateway`, and
`ReachabilityReport::global_ipv6` lists the host's global addresses so a host
can still try them.

A listener on the default `0.0.0.0` is IPv4 only and gets no IPv6 entry.

## Security

Opening a port on someone's router changes what the internet can reach. So:

- port mapping is off unless the config or the builder turns it on;
- the node logs at info level each port it is about to ask for, each
  mapping the router grants (with its external address and lease), and each
  mapping it removes;
- it maps only the node's own listeners and the host's explicit extras,
  never a range;
- a shared-instance client maps nothing; the shared instance owns the
  listeners.

A mapped listener is exposed to the whole internet. Whatever admission
control the interface has (the TCP server's per-source limits, IFAC) is what
stands between it and every scanner. Turn port mapping on only on a node
meant to accept strangers' connections.

Port mapping is off by default. The Constitution has every owned node offer
relay by default (5.4.3, 5.4.6), but nothing in it or in CEWP says a node
opens ports on its router by default. That stays a choice for whoever runs
the node until a decision says otherwise.

## Config

```ini
[reticulum]
  port_mapping = yes
  # Optional: the PCP/NAT-PMP gateway, instead of the default route's.
  port_mapping_gateway = 192.168.1.1
```

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `port_mapping` | yes/no | no | Ask the router to open this node's listening ports. Any other word is a config error. In TOML, a boolean or a string. |
| `port_mapping_gateway` | IP address | default route | The gateway asked over PCP and NAT-PMP. UPnP finds its gateway by SSDP either way. |

A build without the `port-mapping` feature refuses `port_mapping = yes`.

From code, `ReticulumNodeBuilder::port_mapping(PortMappingConfig)` overrides
the config. `PortMappingConfig` also sets the methods and their order, the
lease, extra ports, whether listeners are mapped, and whether IPv6 pinholes
are asked for. `ReticulumNodeBuilder::enable_port_mapping(bool)` is the
switch alone.

## What a host reads

- `ReticulumNode::port_mappings() -> Vec<PortMappingStatus>`: for each
  requested port, the protocol, internal port, method, external address,
  lease and its expiry, and the state (`Pending`, `Mapped`,
  `Failed { reason }`, `CgnatDetected { external_ip, reason }`, `NoGateway`,
  `Removed`).
- `NodeEvent::PortMappingChanged { internal_port, udp, external, method }`,
  on the control plane, each time a mapping is gained, its external address
  changes, or it is lost or removed (`external: None`).
- `ReticulumNode::reachability() -> ReachabilityReport`: the mappings, the
  router's public IPv4 address, the host's global IPv6 addresses, whether a
  CGNAT was seen, and the last host check.
  `ReachabilityReport::is_directly_reachable()` sums it up as `Verified`,
  `Mapped`, `GlobalIpv6` or `Unlikely`.

`Verified` needs a check from outside. The node cannot test its own
reachability: dialling its own external address from inside the LAN either
fails or is looped back by the router, and proves nothing either way. A host
that can have something outside dial the node (a peer over an existing link,
or a server it trusts) reports the result with
`ReticulumNode::record_reachability_check(external, answered)`. Until a host
does, the best the node says is `Mapped`. A host that advertises a node as a
parent on `Mapped` alone should expect some of those claims to be wrong.

## Testing

The lib tests run the schedule (fallback, renewal, router-chosen ports,
expiry, CGNAT, the stop) against mock clients on tokio's paused clock, and
the real clients against fakes on loopback: a NAT-PMP and PCP responder on
UDP 5351, and an SSDP responder with an IGDv2 description and SOAP control.
One test starts a node with a TCP listener against the loopback gateway and
checks the event, the report and the deletion on stop. None of them talks to
a real router.

`real_router`, an ignored lib test, does read-only discovery against the
LAN's router (`cargo test -p leviculum-std --lib real_router -- --ignored
--nocapture`). With `LEVICULUM_REAL_ROUTER_MAP=1` it also maps TCP 47123 to
the host for a moment and deletes it again.

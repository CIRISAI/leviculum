# The WebSocket interface

A browser cannot open raw TCP or UDP sockets, so a browser node reaches the
mesh over WebSocket (leviculum#109). For that to work anywhere, every node
can accept and dial WebSocket links, not only a browser build:
`WebSocketServerInterface` listens, and `WebSocketClientInterface` dials.
Both are in `leviculum-std` behind the `websocket` cargo feature, which is
on by default. `leviculum-core` does not change and gains no dependency.

Python RNS has no WebSocket interface. These links are leviculum to
leviculum, and a node that has both a WebSocket interface and any other
interface bridges between them like it does for any two interfaces.

## Wire framing

Each connection uses one of two framings. The client chooses it with the
WebSocket subprotocol (`Sec-WebSocket-Protocol`) in its opening handshake.
The two names are a public contract, and other implementations (a browser
host, a server in another language) build against them:

| Subprotocol | Binary messages carry |
|---|---|
| `rns.raw` | One Reticulum packet per message, exactly as the transport hands it to an interface, IFAC included when configured. No HDLC or KISS framing inside. A message longer than the hardware MTU (16384 bytes, the TCP interface's) ends the connection with close code 1009. |
| `rns.hdlc` | A byte stream with the same HDLC framing as the TCP interface (flag `0x7E`, escape `0x7D`, no CRC). Message boundaries are not significant: a frame may span messages, and a message may hold several frames. A frame longer than the hardware MTU is discarded, as on TCP. |

Negotiation:

- The server offers both, and accepts the first name in the client's offer
  that it knows. Its answer names that subprotocol.
- A client that offers no subprotocol, or none the server knows, gets
  `rns.raw`. The answer then names no subprotocol.
- A leviculum client always names its framing: `framing = raw` (the
  default) or `framing = hdlc`.

Raw suits a peer that has a message API (a browser's `WebSocket`, with
`binaryType = "arraybuffer"`). HDLC suits a peer that treats the
WebSocket as one more byte stream behind a generic stream interface.

The rest applies to both framings:

- A **text** message ends the connection with close code 1003 (unsupported
  data). A peer that sends text is not speaking this protocol. Closing
  tells it so, where silently dropping the message would not.
- Each side sends a ping every 30 s. A connection that delivers nothing
  for 90 s, not even a pong, is dropped. A reverse proxy closes a
  WebSocket that carries no data for its read timeout (60 s on nginx by
  default), and TCP keepalives on the socket underneath do not count as
  data. The ping keeps the connection open.
- The server accepts any request path. The client sends the configured
  `path`.

The hardware MTU and bitrate are the TCP interface's: 16384 bytes, and the
10 Mbps guess. Both sides report the medium kind `InterfaceKind::WebSocket`
(`websocket`). A WebSocket runs over one TCP stream, so the resource
window's by-medium policy treats it as it treats TCP.

## Configuration

```
[interfaces]
  [[Browsers]]
    type = WebSocketServerInterface
    listen_ip = 127.0.0.1
    listen_port = 8765

  [[Canonical]]
    type = WebSocketClientInterface
    target_host = mesh.example.org
    target_port = 443
    path = /reticulum
    tls = yes
```

| Key | On | Default | Meaning |
|---|---|---|---|
| `listen_ip`, `listen_port`, `device`, `prefer_ipv6` | server | `0.0.0.0`; port required | The bind address, as on a `TCPServerInterface`. Port 0 binds a port the kernel picks; `ReticulumNode::websocket_listen_addrs` reports it. |
| `max_connections_per_source`, `max_connections`, `silent_timeout_secs`, `silent_timeout_pressure_secs`, `pressure_fraction` | server | 16, 4096, 120, 10, 0.8 | Admission, the same tracker and the same defaults as the TCP server (see below). |
| `url` | client | none | The whole target as `ws://host[:port][/path]` or `wss://...`. Replaces `target_host`, `target_port`, `path` and `tls`. |
| `target_host` | client | required without `url` | Host name or IP literal. The name is looked up again on every dial. |
| `target_port` | client | 80, or 443 with `tls` | The port. |
| `path` | client | `/` | The path of the opening handshake request. |
| `tls` | client | `no` | `yes` dials `wss://`. A value that is not a boolean is a config error. |
| `framing` | client | `raw` | `raw` asks for `rns.raw`, `hdlc` for `rns.hdlc`. Any other value is a config error. |
| `reconnect_interval_secs`, `max_reconnect_tries` | client | 5, never give up | Redial pacing, as on a TCP client. |
| `tcp_keepalive_*`, `tcp_user_timeout_secs` | both | TCP's | Keepalive and user timeout of the TCP socket underneath. |
| `mode`, `transit`, `ingress_control`, `networkname`/`passphrase`, `announce_rate_*`, `buffer_size` | both | as TCP | Every accepted connection inherits the listener's values, as a TCP server's do. |

Ingress control defaults to on for the server, because it accepts
arbitrary peers, and to off for the client, which dials one peer. This
follows the TCP pair.

From code, `ReticulumNodeBuilder` has `add_websocket_server(addr)`,
`add_websocket_server_listener(listener)` (on a listener the caller already
bound), `add_websocket_client(host, port, path, tls)`, and
`add_websocket_interface(url)`, which dials one `ws://` or `wss://` URL with
raw framing. A browser driver will offer the same `add_websocket_interface`
signature. A URL that does not parse is an error when the node builds. For
`framing = hdlc` from code, pass an `InterfaceConfig` to
`add_interface_config`. With the feature
off, a WebSocket section is a configuration error that names the feature.

## Admission

The server uses the TCP server's admission tracker
(`interfaces/tcp_admission.rs`) and its defaults. Each source is an IPv4
address or an IPv6 /64, and may hold 16 connections at once. The server as
a whole holds at most 4096, lowered to fit the descriptor limit. A
connection that has not delivered one binary message within 120 s is
closed, or within 10 s while the server is above 80 % of its cap. The
tracker counts a connection from accept onward, so a client that never
finishes the opening handshake counts against its source and is closed as
silent. The handshake itself must also finish within 10 s. A refused
connection is reset right after accept.

Refusals and silent closes are logged under the TCP server's event names
(`TCP_SOURCE_CAPPED`, `TCP_TOTAL_CAPPED`, `TCP_SILENT_CLOSED`,
`TCP_ACCEPT_BACKOFF`). The `iface` key names the WebSocket section.

Behind a reverse proxy, every connection arrives from the proxy's address,
so the per-source cap counts all clients together. On such a server, set
`max_connections_per_source = 0` or a high value, and limit per client in
the proxy. The server does not read `X-Forwarded-For` yet.

## TLS

The **client** speaks `wss://` through rustls on ring, with the webpki root
certificates compiled in. This is the same TLS stack the rest of the
workspace uses (`lblogd`, `leviculum-mapcut`), so there is no native-tls and
no system CA file to find on a musl-static build. The client builds its
rustls configuration explicitly rather than relying on the process-wide
default provider.

The **server** speaks plain `ws://`. For `wss://`, run it behind a reverse
proxy that terminates TLS (nginx, Caddy, a CDN), and bind it to loopback.
This was chosen for v1: a browser needs a certificate it trusts, which
means ACME and renewal, and a proxy already does that well. Built-in server
TLS can be added later without changing the wire. An nginx location for
it:

```
location /reticulum {
    proxy_pass http://127.0.0.1:8765;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 120s;
}
```

## Not yet

- No tunnel synthesis on the client. A `TCPClientInterface` starts the
  tunnel handshake on every connect. A WebSocket client does not, so its
  paths are relearned from announces after a reconnect.
- No built-in server TLS, and no `X-Forwarded-For` (see above).
- No `wss://` test against a local server. The loopback tests run plain
  `ws://`, and a unit test checks that the TLS connector builds.

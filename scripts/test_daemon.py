#!/usr/bin/env python3
"""
Minimal RNS daemon for Rust interop testing.

This daemon provides:
1. A standalone Reticulum instance with a TCPServerInterface
2. A JSON-RPC command interface for querying internal state

Usage:
    python3 test_daemon.py --rns-port 4242 --cmd-port 9999

The daemon will:
- Create a temporary config directory
- Start Reticulum in standalone mode (no shared instance)
- Listen for Reticulum packets on the RNS port
- Accept JSON-RPC commands on the command port

JSON-RPC methods:
- get_path_table: Returns all known paths
- get_announces: Returns pending announces
- has_path: Check if path exists for a destination hash
- get_interfaces: List active interfaces
- register_destination: Create a destination that accepts links
- get_destinations: List registered destinations
- enable_ratchets: Enable ratchets for a destination
- get_ratchet_info: Get ratchet state for a destination
- add_client_interface: Connect to another daemon's TCPServerInterface
- get_transport_status: Get transport/routing status
- get_link_table: Get link table for relay verification
- rotate_ratchet: Force ratchet rotation for a destination
- close_link: Gracefully close a link by its hash
- get_link_status: Get detailed status of a link
- wait_for_link_state: Wait for a link to reach a specific state
- get_announce_table_detail: Get full announce table entries with rebroadcast info
- get_reverse_table: Get reverse table entries for reverse-path routing
- get_path_request_info: Get discovery path request state
- trigger_path_request: Initiate a path request for a destination hash
- get_announce_cache: Get cached announces for path response verification
- shutdown: Gracefully stop the daemon
"""

import argparse
import json
import os
import socket
import sys
import tempfile
import threading
import time


def find_reticulum_path():
    """Find Reticulum in order: env var, vendor submodule, system."""
    script_dir = os.path.dirname(os.path.abspath(__file__))
    project_root = os.path.dirname(script_dir)

    candidates = [
        os.environ.get("RETICULUM_PATH"),  # 1. Explicit override
        os.path.join(project_root, "vendor", "Reticulum"),  # 2. Vendor submodule
    ]

    for path in candidates:
        if path and os.path.isdir(path) and os.path.exists(os.path.join(path, "RNS")):
            return path

    return None  # Fall back to system-installed


RETICULUM_PATH = find_reticulum_path()
if RETICULUM_PATH:
    sys.path.insert(0, RETICULUM_PATH)

try:
    import RNS
    import RNS.Channel
    from RNS import Transport
    from RNS.Interfaces.TCPInterface import TCPClientInterface
    from RNS.Interfaces.BackboneInterface import BackboneClientInterface
except ImportError:
    print("ERROR: Reticulum (RNS) not found.")
    print("Options:")
    print("  1. Run: git submodule update --init vendor/Reticulum")
    print("  2. Set RETICULUM_PATH=/path/to/Reticulum")
    print("  3. Install system-wide: pip install rns")
    sys.exit(1)


class RawBytesMessage(RNS.Channel.MessageBase):
    """Channel message type compatible with Rust's RawBytesMessage (MSGTYPE=0x0000)."""
    MSGTYPE = 0x0000

    def __init__(self):
        self.data = b""

    def pack(self) -> bytes:
        return self.data

    def unpack(self, raw: bytes):
        self.data = raw


class TestDaemon:
    def __init__(self, rns_port: int, cmd_port: int, verbose: bool = False,
                 udp_listen_port: int = None, udp_forward_port: int = None,
                 auto_interface: bool = False, group_id: str = None,
                 share_instance: bool = False, instance_name: str = None,
                 echo_channel: bool = False, respond_to_probes: bool = False,
                 enable_remote_management: bool = False,
                 remote_management_allowed: list = None):
        self.rns_port = rns_port
        self.cmd_port = cmd_port
        self.udp_listen_port = udp_listen_port
        self.udp_forward_port = udp_forward_port
        self.auto_interface = auto_interface
        self.group_id = group_id or "reticulum"
        self.share_instance = share_instance
        self.instance_name = instance_name or "default"
        self.echo_channel = echo_channel
        self.respond_to_probes = respond_to_probes
        self.enable_remote_management = enable_remote_management
        self.remote_management_allowed = remote_management_allowed or []
        self.verbose = verbose
        self.running = True
        self.destinations = {}  # hash -> (identity, destination)
        self.links = {}  # link_hash -> link
        self.received_packets = []  # [(timestamp, link, data)]
        self.received_single_packets = []  # [(timestamp, dest_hash, data_hex)]
        self.plain_destinations = {}  # hash -> destination (PLAIN type, no identity)
        self.received_plain_packets = []  # [(timestamp, dest_hash, data_hex)]
        self.client_interfaces = {}  # name -> TCPClientInterface
        self.inbound_trace = []  # [(timestamp, flags, packet_type, dest_hash_hex, transport_id_hex)]
        self.received_resources = []  # [{resource_hash, data, metadata, status}]
        self.resource_strategies = {}  # dest_hash -> "accept_all" | "accept_none"

        # Create temp config directory
        self.config_dir = tempfile.mkdtemp(prefix="rns_test_")
        self._write_config()

        if self.verbose:
            print(f"Config dir: {self.config_dir}")
            print(f"RNS port: {self.rns_port}")
            print(f"CMD port: {self.cmd_port}")
            if self.udp_listen_port:
                print(f"UDP listen port: {self.udp_listen_port}")
                print(f"UDP forward port: {self.udp_forward_port}")
            if self.auto_interface:
                print(f"AutoInterface enabled, group_id: {self.group_id}")
            if self.share_instance:
                print(f"Shared instance enabled, name: {self.instance_name}")

        # Initialize Reticulum in standalone mode
        loglevel = RNS.LOG_DEBUG if self.verbose else RNS.LOG_WARNING
        self.rns = RNS.Reticulum(
            configdir=self.config_dir,
            loglevel=loglevel
        )

        if self.verbose:
            print("Reticulum initialized")

        # Print probe destination hash if respond_to_probes is enabled.
        # Reticulum auto-creates the probe destination when the config option
        # is set, so we read it from Transport.probe_destination directly.
        self.probe_dest_hash = None
        if self.respond_to_probes:
            probe_dest = getattr(Transport, 'probe_destination', None)
            if probe_dest is not None:
                self.probe_dest_hash = probe_dest.hash.hex()
                # Print before READY so the harness can parse it from stdout
                print(f"PROBE_DEST:{self.probe_dest_hash}", flush=True)

        # Start JSON-RPC command server
        self._start_cmd_server()

    def _write_config(self):
        """Write minimal Reticulum config."""
        share = "yes" if self.share_instance else "no"
        probes = "yes" if self.respond_to_probes else "no"
        config = f"""[reticulum]
  enable_transport = yes
  share_instance = {share}
  respond_to_probes = {probes}
  panic_on_interface_error = no
"""
        if self.share_instance:
            config += f"  instance_name = {self.instance_name}\n"

        if self.enable_remote_management:
            config += "  enable_remote_management = yes\n"
            if self.remote_management_allowed:
                allowed = ", ".join(self.remote_management_allowed)
                config += f"  remote_management_allowed = {allowed}\n"

        config += f"""
[interfaces]
  [[Test TCP Server]]
    type = TCPServerInterface
    enabled = yes
    listen_ip = 127.0.0.1
    listen_port = {self.rns_port}
    mode = gateway
"""
        if self.udp_listen_port and self.udp_forward_port:
            config += f"""
  [[Test UDP Interface]]
    type = UDPInterface
    enabled = yes
    listen_ip = 127.0.0.1
    listen_port = {self.udp_listen_port}
    forward_ip = 127.0.0.1
    forward_port = {self.udp_forward_port}
"""

        if self.auto_interface:
            config += f"""
  [[Default Interface]]
    type = AutoInterface
    enabled = yes
    group_id = {self.group_id}
"""

        config_path = os.path.join(self.config_dir, "config")
        with open(config_path, 'w') as f:
            f.write(config)

    def _start_cmd_server(self):
        """Start the JSON-RPC command server in a separate thread.

        The requested cmd_port races with other parallel tests between
        find_*_available_ports() releasing its bind(0) discovery listener
        and the moment this bind() runs. When that race is lost, fall back
        to a kernel-allocated port: the harness reads the actually-bound
        port back from the READY line's third field. No retries on the
        requested port — if it's taken, another process holds it for the
        rest of the test and retrying wastes budget.
        """
        self.cmd_socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.cmd_socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            self.cmd_socket.bind(('127.0.0.1', self.cmd_port))
        except OSError:
            self.cmd_socket.bind(('127.0.0.1', 0))
            self.cmd_port = self.cmd_socket.getsockname()[1]
        self.cmd_socket.listen(5)
        self.cmd_socket.settimeout(1.0)

        self.cmd_thread = threading.Thread(target=self._cmd_server_loop, daemon=True)
        self.cmd_thread.start()

        if self.verbose:
            print(f"Command server listening on port {self.cmd_port}")

    def _cmd_server_loop(self):
        """Accept and handle command connections."""
        while self.running:
            try:
                conn, addr = self.cmd_socket.accept()
                threading.Thread(
                    target=self._handle_cmd_connection,
                    args=(conn,),
                    daemon=True
                ).start()
            except socket.timeout:
                continue
            except Exception as e:
                if self.running:
                    print(f"Command server error: {e}")

    def _handle_cmd_connection(self, conn):
        """Handle a single command connection."""
        try:
            conn.settimeout(5.0)
            data = b""
            while True:
                chunk = conn.recv(4096)
                if not chunk:
                    break
                data += chunk
                # Try to parse complete JSON
                try:
                    cmd = json.loads(data.decode('utf-8'))
                    response = self._handle_cmd(cmd)
                    conn.sendall(json.dumps(response).encode('utf-8'))
                    break
                except json.JSONDecodeError:
                    # Need more data
                    continue
        except Exception as e:
            try:
                conn.sendall(json.dumps({"error": str(e)}).encode('utf-8'))
            except:
                pass
        finally:
            conn.close()

    def _handle_cmd(self, cmd):
        """Handle a JSON-RPC command."""
        method = cmd.get("method")
        params = cmd.get("params", {})

        if method == "ping":
            return {"result": "pong"}

        elif method == "get_path_table":
            # Return path table as hex strings
            paths = {}
            for h, entry in Transport.path_table.items():
                # entry = [timestamp, next_hop, hops, expires, ...]
                next_hop = entry[1].hex() if len(entry) > 1 and hasattr(entry[1], 'hex') else (entry[1] if len(entry) > 1 else None)
                paths[h.hex()] = {
                    "timestamp": entry[0],
                    "next_hop": next_hop,
                    "hops": entry[2] if len(entry) > 2 else None,
                    "expires": entry[3] if len(entry) > 3 else None,
                }
            return {"result": paths}

        elif method == "get_announces":
            # Return announce table
            announces = {}
            for h, entry in Transport.announce_table.items():
                # entry = [timestamp, retransmit_timeout, retries, ...]
                announces[h.hex()] = {
                    "timestamp": entry[0] if len(entry) > 0 else None,
                }
            return {"result": announces}

        elif method == "has_path":
            h = bytes.fromhex(params.get("hash", ""))
            return {"result": Transport.has_path(h)}

        elif method == "get_interfaces":
            interfaces = []
            for iface in Transport.interfaces:
                # Codeberg #87: expose the ingress-control held-announce queue
                # length and burst flag so interop tests can A/B the hold-and-
                # release behavior against the Rust stack. held_announces is a
                # dict keyed by destination hash (Interface.py:131).
                held = getattr(iface, 'held_announces', None)
                interfaces.append({
                    "name": str(iface),
                    "online": getattr(iface, 'online', None),
                    "IN": getattr(iface, 'IN', None),
                    "OUT": getattr(iface, 'OUT', None),
                    "txb": getattr(iface, 'txb', None),
                    "rxb": getattr(iface, 'rxb', None),
                    "mode": getattr(iface, 'mode', None),
                    "ifac_identity": hasattr(iface, 'ifac_identity') and iface.ifac_identity is not None,
                    "held_announces": len(held) if held is not None else 0,
                    "burst_active": bool(getattr(iface, 'ic_burst_active', False)),
                })
            return {"result": interfaces}

        elif method == "register_destination":
            # Create a destination that accepts links
            app_name = params.get("app_name", "test")
            aspects = params.get("aspects", [])

            identity = RNS.Identity()
            dest = RNS.Destination(
                identity,
                RNS.Destination.IN,
                RNS.Destination.SINGLE,
                app_name,
                *aspects
            )

            # Set up link acceptance
            dest.set_link_established_callback(
                lambda link, d=dest: self._on_link_established(link, d)
            )

            # Set up single-packet receive callback
            dest.set_packet_callback(
                lambda data, pkt, d=dest: self._on_single_packet(d, data, pkt)
            )

            dest_hash = dest.hash.hex()
            self.destinations[dest_hash] = (identity, dest)

            # Return destination info
            pub_key = identity.get_public_key()
            return {
                "result": {
                    "hash": dest_hash,
                    "public_key": pub_key.hex(),
                    "signing_key": pub_key[32:64].hex(),
                }
            }

        elif method == "verify_signature":
            # Verify an Ed25519 signature made elsewhere (e.g. the C API) using
            # the public key. public_key, message and signature are hex.
            public_key = bytes.fromhex(params.get("public_key", ""))
            message = bytes.fromhex(params.get("message", ""))
            signature = bytes.fromhex(params.get("signature", ""))
            identity = RNS.Identity(create_keys=False)
            identity.load_public_key(public_key)
            valid = bool(identity.validate(signature, message))
            return {"result": {"valid": valid}}

        elif method == "group_encrypt":
            # Encrypt plaintext with a GROUP destination holding a shared key.
            # Exercises the real Python GROUP branch: Destination.encrypt ->
            # self.prv.encrypt (RNS Token, AES-256-CBC). key/plaintext are hex.
            key = bytes.fromhex(params.get("key", ""))
            plaintext = bytes.fromhex(params.get("plaintext", ""))
            dest = RNS.Destination(
                None, RNS.Destination.IN, RNS.Destination.GROUP, "levgroup", "interop"
            )
            dest.load_private_key(key)
            ciphertext = dest.encrypt(plaintext)
            return {"result": {"ciphertext": ciphertext.hex()}}

        elif method == "group_decrypt":
            # Decrypt a GROUP token with a shared key via a Python GROUP
            # destination. key/ciphertext are hex. Returns plaintext hex, or
            # null if decryption failed (bad HMAC / wrong key).
            key = bytes.fromhex(params.get("key", ""))
            ciphertext = bytes.fromhex(params.get("ciphertext", ""))
            dest = RNS.Destination(
                None, RNS.Destination.IN, RNS.Destination.GROUP, "levgroup", "interop"
            )
            dest.load_private_key(key)
            plaintext = dest.decrypt(ciphertext)
            return {
                "result": {
                    "plaintext": plaintext.hex() if plaintext is not None else None
                }
            }

        elif method == "register_echo_request_handler":
            # Register an echo request handler on a destination
            dest_hash = params.get("dest_hash")
            path = params.get("path", "/echo")
            if dest_hash not in self.destinations:
                return {"error": f"destination {dest_hash} not found"}
            _identity, dest = self.destinations[dest_hash]
            def echo_request_handler(path, data, request_id, link_id, remote_identity, requested_at):
                """Echo handler for request/response interop testing."""
                return data  # Echo back whatever was sent
            dest.register_request_handler(
                path,
                response_generator=echo_request_handler,
                allow=RNS.Destination.ALLOW_ALL,
            )
            return {"result": "ok"}

        elif method == "get_destinations":
            dests = {}
            for h, (identity, dest) in self.destinations.items():
                dests[h] = {
                    "app_name": getattr(dest, 'app_name', None),
                }
            return {"result": dests}

        elif method == "get_links":
            links = {}
            for h, link in self.links.items():
                links[h] = {
                    "status": str(link.status),
                    "activated_at": getattr(link, 'activated_at', None),
                }
            return {"result": links}

        elif method == "get_link_remote_identity":
            link_hash = params.get("link_hash")
            link = self.links.get(link_hash)
            if not link:
                return {"error": f"link {link_hash} not found"}
            remote_id = link.get_remote_identity()
            if remote_id is None:
                return {"result": None}
            return {"result": {"identity_hash": remote_id.hash.hex()}}

        elif method == "get_received_packets":
            # Return packets received over links
            packets = []
            for ts, link, data in self.received_packets:
                packets.append({
                    "timestamp": ts,
                    "link_hash": link.hash.hex() if link.hash else None,
                    "data": data.hex() if isinstance(data, bytes) else str(data),
                })
            return {"result": packets}

        elif method == "set_resource_strategy":
            dest_hash = params.get("dest_hash")
            strategy = params.get("strategy", "accept_none")
            self.resource_strategies[dest_hash] = strategy
            return {"result": "ok"}

        elif method == "send_resource":
            link_hash = params.get("link_hash")
            data_hex = params.get("data")
            metadata_hex = params.get("metadata")

            link = self.links.get(link_hash)
            if not link:
                return {"error": f"link {link_hash} not found"}

            data = bytes.fromhex(data_hex)
            metadata = bytes.fromhex(metadata_hex) if metadata_hex else None

            resource = RNS.Resource(data, link, metadata=metadata, advertise=True)
            return {"result": {"resource_hash": resource.hash.hex()}}

        elif method == "get_received_resources":
            return {"result": self.received_resources}

        elif method == "announce_destination":
            dest_hash = params.get("hash")
            app_data = params.get("app_data", b"")
            if isinstance(app_data, str):
                app_data = bytes.fromhex(app_data)

            if dest_hash not in self.destinations:
                return {"error": f"Destination {dest_hash} not registered"}

            _, dest = self.destinations[dest_hash]
            dest.announce(app_data=app_data)
            return {"result": "announced"}

        elif method == "enable_ratchets":
            # Enable ratchets for a destination (forward secrecy)
            dest_hash = params.get("hash")

            if dest_hash not in self.destinations:
                return {"error": f"Destination {dest_hash} not registered"}

            _, dest = self.destinations[dest_hash]

            # Create ratchet storage path - RNS expects this to be a file path
            # (it writes to <path> and creates <path>.tmp during writes)
            ratchet_dir = os.path.join(self.config_dir, "ratchets")
            os.makedirs(ratchet_dir, exist_ok=True)
            ratchet_path = os.path.join(ratchet_dir, dest_hash)

            try:
                dest.enable_ratchets(ratchet_path)
                return {"result": {
                    "enabled": True,
                    "ratchet_dir": ratchet_path,
                }}
            except Exception as e:
                return {"error": f"Failed to enable ratchets: {str(e)}"}

        elif method == "enforce_ratchets":
            dest_hash = params.get("hash")
            if dest_hash not in self.destinations:
                return {"error": f"Destination {dest_hash} not registered"}
            _, dest = self.destinations[dest_hash]
            try:
                result = dest.enforce_ratchets()
                return {"result": {"enforced": result}}
            except Exception as e:
                return {"error": f"Failed to enforce ratchets: {str(e)}"}

        elif method == "get_ratchet_info":
            # Get ratchet state for a destination
            dest_hash = params.get("hash")

            if dest_hash not in self.destinations:
                return {"error": f"Destination {dest_hash} not registered"}

            _, dest = self.destinations[dest_hash]

            # Check if ratchets are enabled
            ratchets_enabled = hasattr(dest, 'ratchets') and dest.ratchets is not None

            result = {
                "enabled": ratchets_enabled,
            }

            if ratchets_enabled:
                # Get ratchet count and latest ID
                ratchet_count = len(dest.ratchets) if hasattr(dest.ratchets, '__len__') else 0
                latest_id = dest.latest_ratchet_id.hex() if hasattr(dest, 'latest_ratchet_id') and dest.latest_ratchet_id else None
                result["count"] = ratchet_count
                result["latest_id"] = latest_id

            return {"result": result}

        elif method == "create_link":
            # Create a link to an external destination (Python as initiator)
            dest_hash = params.get("dest_hash")
            dest_key = params.get("dest_key")  # 64-byte public key hex
            timeout = params.get("timeout", 10)

            if not dest_hash or not dest_key:
                return {"error": "dest_hash and dest_key required"}

            try:
                dest_hash_bytes = bytes.fromhex(dest_hash)
                dest_key_bytes = bytes.fromhex(dest_key)

                # Create identity from public key (no private key)
                dest_identity = RNS.Identity(create_keys=False)
                dest_identity.load_public_key(dest_key_bytes)

                # Create outgoing destination
                dest = RNS.Destination(
                    dest_identity,
                    RNS.Destination.OUT,
                    RNS.Destination.SINGLE,
                    "rust",
                    "interop"
                )

                # Override the hash (for testing - destination was registered externally)
                dest.hash = dest_hash_bytes

                # Create link with timeout
                link = RNS.Link(dest)

                # Wait for link establishment
                start = time.time()
                while link.status != RNS.Link.ACTIVE and time.time() - start < timeout:
                    time.sleep(0.1)
                    if link.status == RNS.Link.CLOSED:
                        return {"error": "Link closed before establishment"}

                if link.status != RNS.Link.ACTIVE:
                    return {"error": "Link establishment timeout"}

                link_hash = link.hash.hex() if link.hash else "unknown"
                self.links[link_hash] = link

                # Set up callbacks
                link.set_packet_callback(lambda msg, pkt, l=link: self._on_packet(l, msg, pkt))
                link.set_link_closed_callback(lambda l: self._on_link_closed(l))

                # Set up channel handler for Rust channel messages
                try:
                    channel = link.get_channel()
                    channel.register_message_type(RawBytesMessage)
                    channel.add_message_handler(lambda msg, l=link: self._on_channel_message(l, msg))
                except Exception as e:
                    if self.verbose:
                        print(f"Failed to set up channel handler on initiator link: {e}")

                # Resource callbacks for initiator-side links
                if self.resource_strategies.get(dest_hash) == "accept_all":
                    link.set_resource_strategy(RNS.Link.ACCEPT_ALL)
                    link.set_resource_started_callback(
                        lambda resource: self._on_resource_started(resource)
                    )
                    link.set_resource_concluded_callback(
                        lambda resource: self._on_resource_concluded(resource)
                    )

                return {
                    "result": {
                        "link_hash": link_hash,
                        "status": str(link.status),
                    }
                }

            except Exception as e:
                return {"error": f"Failed to create link: {str(e)}"}

        elif method == "identify_link":
            # Identify an identity on an existing link (Python as initiator).
            # Creates a fresh identity with private keys for signing.
            link_hash = params.get("link_hash")
            if not link_hash:
                return {"error": "link_hash required"}
            link = self.links.get(link_hash)
            if not link:
                return {"error": f"link {link_hash} not found"}
            try:
                identity = RNS.Identity()
                link.identify(identity)
                return {"result": {"identity_hash": identity.hash.hex()}}
            except Exception as e:
                return {"error": f"identify failed: {str(e)}"}

        elif method == "send_on_link":
            # Send data on an existing link
            link_hash = params.get("link_hash")
            data = params.get("data")  # hex string

            if not link_hash or not data:
                return {"error": "link_hash and data required"}

            if link_hash not in self.links:
                return {"error": f"Link {link_hash} not found"}

            link = self.links[link_hash]
            data_bytes = bytes.fromhex(data)

            try:
                packet = RNS.Packet(link, data_bytes)
                packet.send()
                return {"result": "sent"}
            except Exception as e:
                return {"error": f"Send failed: {str(e)}"}

        elif method == "add_client_interface":
            # Connect this daemon to another daemon's TCPServerInterface
            target_ip = params.get("target_ip", "127.0.0.1")
            target_port = params.get("target_port")
            name = params.get("name")

            if not target_port:
                return {"error": "target_port is required"}

            if not name:
                name = f"TCPClient_{target_ip}_{target_port}"

            try:
                # Create TCPClientInterface using configuration dict
                # The new RNS API expects a configuration object
                config = {
                    "name": name,
                    "target_host": target_ip,
                    "target_port": target_port,
                }

                client_iface = TCPClientInterface(
                    RNS.Transport,
                    config,
                )

                # Enable inbound and outbound on the interface.
                # TCPClientInterface defaults OUT=False, but Transport.outbound()
                # checks `if interface.OUT:` as the first gate for every outgoing
                # packet. Without this, no announces or data get sent through it.
                client_iface.OUT = True
                client_iface.IN = True
                client_iface.mode = RNS.Interfaces.Interface.Interface.MODE_GATEWAY

                # Set announce rate attributes required by Transport.inbound's
                # announce processing (Transport.py:1692). Without these,
                # incoming announces on this interface cause an AttributeError.
                client_iface.announce_rate_target = None
                client_iface.announce_rate_grace = None
                client_iface.announce_rate_penalty = None

                # Register with Transport
                RNS.Transport.interfaces.append(client_iface)

                # Store reference
                self.client_interfaces[name] = client_iface

                # Wait briefly for connection
                time.sleep(0.5)

                return {
                    "result": {
                        "name": name,
                        "online": getattr(client_iface, 'online', False),
                        "target_ip": target_ip,
                        "target_port": target_port,
                    }
                }

            except Exception as e:
                return {"error": f"Failed to create client interface: {str(e)}"}

        elif method == "add_backbone_client_interface":
            # Connect this daemon to another daemon's TCP listener using the REAL
            # Python BackboneClientInterface (Codeberg #89 drop-in proof). Backbone
            # is wire-identical to TCP, so a BackboneClientInterface connects to
            # our lnsd's TCP server built from a `type = BackboneInterface` config.
            target_ip = params.get("target_ip", "127.0.0.1")
            target_port = params.get("target_port")
            name = params.get("name")

            if not target_port:
                return {"error": "target_port is required"}

            if not name:
                name = f"BackboneClient_{target_ip}_{target_port}"

            try:
                config = {
                    "name": name,
                    "target_host": target_ip,
                    "target_port": target_port,
                }

                client_iface = BackboneClientInterface(
                    RNS.Transport,
                    config,
                )

                # BackboneClientInterface defaults OUT=False; Transport.outbound()
                # gates on interface.OUT, so enable it (mirrors add_client_interface).
                client_iface.OUT = True
                client_iface.IN = True
                client_iface.mode = RNS.Interfaces.Interface.Interface.MODE_GATEWAY

                client_iface.announce_rate_target = None
                client_iface.announce_rate_grace = None
                client_iface.announce_rate_penalty = None

                RNS.Transport.interfaces.append(client_iface)
                self.client_interfaces[name] = client_iface

                # Wait briefly for the connection to establish.
                time.sleep(0.5)

                return {
                    "result": {
                        "name": name,
                        "online": getattr(client_iface, 'online', False),
                        "target_ip": target_ip,
                        "target_port": target_port,
                    }
                }

            except Exception as e:
                return {"error": f"Failed to create backbone client interface: {str(e)}"}

        elif method == "get_transport_status":
            # Get transport/routing status
            try:
                result = {
                    "enabled": RNS.Reticulum.transport_enabled(),
                    "identity_hash": Transport.identity.hash.hex() if Transport.identity else None,
                    "path_table_size": len(Transport.path_table),
                    "link_table_size": len(Transport.link_table) if hasattr(Transport, 'link_table') else 0,
                    "announce_table_size": len(Transport.announce_table),
                    "interface_count": len(Transport.interfaces),
                }
                return {"result": result}
            except Exception as e:
                return {"error": f"Failed to get transport status: {str(e)}"}

        elif method == "get_link_table":
            # Get link table for relay verification
            try:
                link_table = {}
                if hasattr(Transport, 'link_table'):
                    for link_id, entry in Transport.link_table.items():
                        # link_table entry format: [timestamp, next_hop, outbound_interface, remaining_hops, ...]
                        link_table[link_id.hex()] = {
                            "timestamp": entry[0] if len(entry) > 0 else None,
                            "interface": str(entry[2]) if len(entry) > 2 else None,
                            "hops": entry[3] if len(entry) > 3 else None,
                        }
                return {"result": link_table}
            except Exception as e:
                return {"error": f"Failed to get link table: {str(e)}"}

        elif method == "enable_inbound_trace":
            # Install monkey-patch on Transport.inbound to trace all packets
            original_inbound = Transport.inbound.__func__ if hasattr(Transport.inbound, '__func__') else Transport.inbound
            daemon_ref = self

            @staticmethod
            def traced_inbound(raw, interface=None):
                try:
                    if len(raw) > 2:
                        flags = raw[0]
                        packet_type = flags & 0x03
                        header_type = (flags >> 6) & 0x01
                        context_flag = (flags >> 5) & 0x01
                        dest_type = (flags >> 2) & 0x03
                        transport_id_hex = None
                        context_val = None
                        if header_type == 1:  # HEADER_2
                            transport_id_hex = raw[2:18].hex()
                            dest_hash_hex = raw[18:34].hex()
                            if len(raw) > 34:
                                context_val = raw[34]
                        else:  # HEADER_1
                            dest_hash_hex = raw[2:18].hex()
                            if len(raw) > 18:
                                context_val = raw[18]
                        # Compute packet hash (same algorithm as Packet.get_hash)
                        import hashlib
                        if header_type == 1:  # HEADER_2
                            hashable = bytes([flags & 0x0F]) + raw[18:]
                        else:  # HEADER_1
                            hashable = bytes([flags & 0x0F]) + raw[2:]
                        full_hash = hashlib.sha256(hashable).digest()
                        computed_hash_hex = full_hash[:16].hex()
                        daemon_ref.inbound_trace.append({
                            "time": time.time(),
                            "flags": f"0x{flags:02x}",
                            "header_type": header_type,
                            "packet_type": packet_type,
                            "dest_type": dest_type,
                            "context_flag": context_flag,
                            "context_val": context_val,
                            "dest_hash": dest_hash_hex,
                            "transport_id": transport_id_hex,
                            "interface": str(interface),
                            "raw_len": len(raw),
                            "hops": raw[1] if len(raw) > 1 else None,
                            "computed_packet_hash": computed_hash_hex,
                            "hashable_hex": hashable.hex(),
                            "raw_head_hex": raw[:20].hex() if len(raw) >= 20 else raw.hex(),
                        })
                except Exception as e:
                    daemon_ref.inbound_trace.append({"error": str(e)})
                return original_inbound(raw, interface=interface)

            Transport.inbound = traced_inbound
            return {"result": "trace enabled"}

        elif method == "enable_lrproof_trace":
            # Detailed tracing of LRPROOF handling inside Transport
            daemon_ref = self
            daemon_ref.lrproof_trace = []

            # Monkey-patch Transport.transmit to trace outgoing packets
            original_transmit = Transport.transmit.__func__ if hasattr(Transport.transmit, '__func__') else Transport.transmit
            @staticmethod
            def traced_transmit(interface, raw):
                try:
                    if len(raw) > 2:
                        flags = raw[0]
                        pt = flags & 0x03
                        daemon_ref.lrproof_trace.append({
                            "event": "transmit",
                            "interface": str(interface),
                            "flags": f"0x{flags:02x}",
                            "packet_type": pt,
                            "raw_len": len(raw),
                            "dest_hash": raw[2:18].hex() if (flags >> 6) & 0x01 == 0 else raw[18:34].hex(),
                        })
                except Exception as e:
                    daemon_ref.lrproof_trace.append({"event": "transmit_error", "error": str(e)})
                return original_transmit(interface, raw)
            Transport.transmit = traced_transmit

            # Log link table and path table state
            daemon_ref.lrproof_trace.append({
                "event": "tables_snapshot",
                "link_table_keys": [k.hex() if isinstance(k, bytes) else str(k) for k in Transport.link_table.keys()],
                "path_table_keys": [k.hex() if isinstance(k, bytes) else str(k) for k in Transport.path_table.keys()],
                "transport_enabled": RNS.Reticulum.transport_enabled(),
                "transport_identity": Transport.identity.hash.hex() if Transport.identity else None,
            })

            return {"result": "lrproof trace enabled"}

        elif method == "get_lrproof_trace":
            return {"result": getattr(self, 'lrproof_trace', [])}

        elif method == "enable_lrproof_drop":
            # Monkey-patch Transport.transmit to silently drop LRPROOF packets.
            # LRPROOF has context byte 0xFF. Context byte offset depends on
            # header type: HEADER_1 → raw[18], HEADER_2 → raw[34].
            daemon_ref = self
            daemon_ref.lrproof_drops = getattr(daemon_ref, 'lrproof_drops', [])

            # Save original transmit only once (avoid double-patching)
            if not hasattr(daemon_ref, '_original_transmit'):
                daemon_ref._original_transmit = (
                    Transport.transmit.__func__
                    if hasattr(Transport.transmit, '__func__')
                    else Transport.transmit
                )

            original = daemon_ref._original_transmit

            @staticmethod
            def dropping_transmit(interface, raw):
                try:
                    if len(raw) > 2:
                        flags = raw[0]
                        is_header_2 = (flags >> 6) & 0x01
                        context_offset = 34 if is_header_2 else 18
                        if len(raw) > context_offset and raw[context_offset] == 0xFF:
                            daemon_ref.lrproof_drops.append({
                                "time": time.time(),
                                "interface": str(interface),
                                "raw_len": len(raw),
                                "header_type": 2 if is_header_2 else 1,
                            })
                            return  # silently drop
                except Exception:
                    pass  # on error, fall through to original
                return original(interface, raw)

            Transport.transmit = dropping_transmit
            return {"result": "lrproof_drop_enabled"}

        elif method == "disable_lrproof_drop":
            # Restore original Transport.transmit saved during enable.
            if hasattr(self, '_original_transmit'):
                Transport.transmit = self._original_transmit
                del self._original_transmit
                return {"result": "lrproof_drop_disabled"}
            else:
                return {"result": "lrproof_drop_was_not_enabled"}

        elif method == "get_lrproof_drops":
            return {"result": getattr(self, 'lrproof_drops', [])}

        elif method == "get_link_table_detail":
            # Get full link table with all fields
            result = {}
            for k, v in Transport.link_table.items():
                key_hex = k.hex() if isinstance(k, bytes) else str(k)
                result[key_hex] = {
                    "timestamp": v[0],
                    "next_hop": v[1].hex() if isinstance(v[1], bytes) else str(v[1]),
                    "nh_interface": str(v[2]),
                    "remaining_hops": v[3],
                    "rcvd_interface": str(v[4]),
                    "taken_hops": v[5],
                    "dest_hash": v[6].hex() if isinstance(v[6], bytes) else str(v[6]),
                    "validated": v[7],
                    "proof_timeout": v[8],
                }
            return {"result": result}

        elif method == "get_inbound_trace":
            return {"result": self.inbound_trace}

        elif method == "rotate_ratchet":
            # Force ratchet rotation for a destination
            dest_hash = params.get("hash")

            if dest_hash not in self.destinations:
                return {"error": f"Destination {dest_hash} not registered"}

            _, dest = self.destinations[dest_hash]

            # Check if ratchets are enabled
            if not hasattr(dest, 'ratchets') or dest.ratchets is None:
                return {"error": "Ratchets not enabled for this destination"}

            try:
                # Rotate ratchet - this generates a new ratchet key pair
                dest.rotate_ratchets()

                # Get new state
                ratchet_count = len(dest.ratchets) if hasattr(dest.ratchets, '__len__') else 0
                latest_id = dest.latest_ratchet_id.hex() if hasattr(dest, 'latest_ratchet_id') and dest.latest_ratchet_id else None

                return {
                    "result": {
                        "rotated": True,
                        "ratchet_count": ratchet_count,
                        "new_ratchet_id": latest_id,
                    }
                }
            except Exception as e:
                return {"error": f"Failed to rotate ratchet: {str(e)}"}

        elif method == "close_link":
            # Gracefully close a link by its hash
            link_hash = params.get("link_hash")
            if not link_hash:
                return {"error": "link_hash is required"}

            if link_hash not in self.links:
                return {"error": f"Link {link_hash} not found", "status": "not_found"}

            link = self.links[link_hash]
            try:
                link.teardown()
                return {"result": {"status": "closed", "link_hash": link_hash}}
            except Exception as e:
                return {"error": f"Failed to close link: {str(e)}"}

        elif method == "get_link_status":
            # Get detailed status of a link
            link_hash = params.get("link_hash")
            if not link_hash:
                return {"error": "link_hash is required"}

            if link_hash not in self.links:
                return {"result": {"status": "not_found", "link_hash": link_hash}}

            link = self.links[link_hash]
            return {
                "result": {
                    "status": "found",
                    "link_hash": link_hash,
                    "state": str(link.status),
                    "is_initiator": getattr(link, 'initiator', None),
                    "rtt": getattr(link, 'rtt', None),
                    "established_at": getattr(link, 'activated_at', None),
                    "last_inbound": getattr(link, 'last_inbound', None),
                    "last_outbound": getattr(link, 'last_outbound', None),
                    "mtu": link.get_mtu() if hasattr(link, 'get_mtu') else None,
                    "mdu": link.get_mdu() if hasattr(link, 'get_mdu') else None,
                }
            }

        elif method == "wait_for_link_state":
            # Wait for a link to reach a specific state
            link_hash = params.get("link_hash")
            expected_state = params.get("state")
            timeout_secs = params.get("timeout", 10)

            if not link_hash or not expected_state:
                return {"error": "link_hash and state are required"}

            deadline = time.time() + timeout_secs

            while time.time() < deadline:
                if link_hash in self.links:
                    link = self.links[link_hash]
                    current_state = str(link.status)
                    if current_state == expected_state:
                        return {"result": {"status": "reached", "state": expected_state}}
                    # Also check for CLOSED state (link may be removed from dict)
                    if link.status == RNS.Link.CLOSED and expected_state == "CLOSED":
                        return {"result": {"status": "reached", "state": expected_state}}
                else:
                    # Link removed from dict means it's closed
                    if expected_state == "CLOSED":
                        return {"result": {"status": "reached", "state": expected_state}}

                time.sleep(0.1)

            # Timeout - return current state if available
            current = None
            if link_hash in self.links:
                current = str(self.links[link_hash].status)

            return {
                "result": {
                    "status": "timeout",
                    "expected": expected_state,
                    "current": current
                }
            }

        elif method == "set_proof_strategy":
            # Set proof strategy for a destination
            dest_hash = params.get("hash")
            strategy = params.get("strategy")

            if dest_hash not in self.destinations:
                return {"error": f"Destination {dest_hash} not registered"}

            _, dest = self.destinations[dest_hash]

            strategy_map = {
                "PROVE_NONE": RNS.Destination.PROVE_NONE,
                "PROVE_APP": RNS.Destination.PROVE_APP,
                "PROVE_ALL": RNS.Destination.PROVE_ALL,
            }

            if strategy not in strategy_map:
                return {"error": f"Unknown strategy: {strategy}. Use PROVE_NONE, PROVE_APP, or PROVE_ALL"}

            try:
                dest.set_proof_strategy(strategy_map[strategy])
                return {"result": {"strategy": strategy, "dest_hash": dest_hash}}
            except Exception as e:
                return {"error": f"Failed to set proof strategy: {str(e)}"}

        elif method == "get_proof_strategy":
            # Get proof strategy for a destination
            dest_hash = params.get("hash")

            if dest_hash not in self.destinations:
                return {"error": f"Destination {dest_hash} not registered"}

            _, dest = self.destinations[dest_hash]

            strategy_names = {
                RNS.Destination.PROVE_NONE: "PROVE_NONE",
                RNS.Destination.PROVE_APP: "PROVE_APP",
                RNS.Destination.PROVE_ALL: "PROVE_ALL",
            }

            return {"result": {
                "strategy": strategy_names.get(dest.proof_strategy, "unknown"),
                "strategy_value": dest.proof_strategy,
            }}

        elif method == "get_announce_table_detail":
            # Return full announce_table entries with rebroadcast info
            # Python format: announce_table[dest_hash] = [timestamp, retransmit_timeout,
            #   retries, received_from, announce_hops, packet, local_rebroadcasts,
            #   block_rebroadcasts, attached_interface]
            try:
                table = {}
                for h, entry in Transport.announce_table.items():
                    detail = {
                        "timestamp": entry[0] if len(entry) > 0 else None,
                        "retransmit_timeout": entry[1] if len(entry) > 1 else None,
                        "retries": entry[2] if len(entry) > 2 else None,
                        "received_from": entry[3].hex() if len(entry) > 3 and entry[3] is not None else None,
                        "hops": entry[4] if len(entry) > 4 else None,
                        "packet_length": len(entry[5].raw) if len(entry) > 5 and entry[5] is not None and hasattr(entry[5], 'raw') else None,
                        "local_rebroadcasts": entry[6] if len(entry) > 6 else None,
                        "block_rebroadcasts": entry[7] if len(entry) > 7 else None,
                    }
                    table[h.hex()] = detail
                return {"result": table}
            except Exception as e:
                return {"error": f"Failed to get announce table detail: {str(e)}"}

        elif method == "get_reverse_table":
            # Return reverse_table entries for reverse-path routing verification
            # Python format: reverse_table[dest_hash] = [received_from, outbound_interface,
            #   timestamp, ...]
            try:
                table = {}
                if hasattr(Transport, 'reverse_table'):
                    for h, entry in Transport.reverse_table.items():
                        detail = {
                            "received_from": entry[0].hex() if len(entry) > 0 and entry[0] is not None else None,
                            "interface": str(entry[1]) if len(entry) > 1 and entry[1] is not None else None,
                            "timestamp": entry[2] if len(entry) > 2 else None,
                        }
                        table[h.hex()] = detail
                return {"result": table}
            except Exception as e:
                return {"error": f"Failed to get reverse table: {str(e)}"}

        elif method == "get_path_request_info":
            # Return discovery_path_requests state
            try:
                requests = {}
                if hasattr(Transport, 'discovery_path_requests'):
                    for h, entry in Transport.discovery_path_requests.items():
                        detail = {
                            "timeout": entry[0] if len(entry) > 0 else None,
                            "request_tag": entry[1].hex() if len(entry) > 1 and entry[1] is not None else None,
                        }
                        requests[h.hex()] = detail
                return {"result": requests}
            except Exception as e:
                return {"error": f"Failed to get path request info: {str(e)}"}

        elif method == "trigger_path_request":
            # Initiate a path request for a given destination hash
            dest_hash = params.get("hash")
            if not dest_hash:
                return {"error": "hash is required"}
            try:
                dest_hash_bytes = bytes.fromhex(dest_hash)
                Transport.request_path(dest_hash_bytes)
                return {"result": "path_request_sent"}
            except Exception as e:
                return {"error": f"Failed to trigger path request: {str(e)}"}

        elif method == "get_announce_cache":
            # Return cached announces (destination_table) for path response verification
            try:
                cache = {}
                if hasattr(Transport, 'destination_table'):
                    for h, entry in Transport.destination_table.items():
                        detail = {
                            "timestamp": entry[0] if len(entry) > 0 else None,
                            "received_from": entry[1].hex() if len(entry) > 1 and entry[1] is not None else None,
                            "hops": entry[2] if len(entry) > 2 else None,
                            "expires": entry[3] if len(entry) > 3 else None,
                            "random_blobs": [b.hex() for b in entry[4]] if len(entry) > 4 and entry[4] is not None else None,
                        }
                        cache[h.hex()] = detail
                return {"result": cache}
            except Exception as e:
                return {"error": f"Failed to get announce cache: {str(e)}"}

        elif method == "get_received_single_packets":
            # Return single packets received at destinations (not via links)
            packets = []
            for ts, dest_hash, data_hex in self.received_single_packets:
                packets.append({
                    "timestamp": ts,
                    "dest_hash": dest_hash,
                    "data": data_hex,
                })
            return {"result": packets}

        elif method == "send_single_packet":
            # Send a single (non-link) packet to a remote destination
            dest_hash_hex = params.get("dest_hash")
            data_hex = params.get("data")

            if not dest_hash_hex or not data_hex:
                return {"error": "dest_hash and data required"}

            try:
                dest_hash_bytes = bytes.fromhex(dest_hash_hex)

                # Look up identity from RNS identity cache (populated by announces)
                recalled_identity = RNS.Identity.recall(dest_hash_bytes)
                if recalled_identity is None:
                    return {"error": f"No identity known for {dest_hash_hex} (no announce received?)"}

                # Create Destination.OUT with the recalled identity
                # We need app_name + aspects to match the dest hash, but for OUT
                # destinations we can just override the hash directly
                out_dest = RNS.Destination(
                    recalled_identity,
                    RNS.Destination.OUT,
                    RNS.Destination.SINGLE,
                    "unknown",
                    "app"
                )
                out_dest.hash = dest_hash_bytes

                # Build and send the packet
                data_bytes = bytes.fromhex(data_hex)
                packet = RNS.Packet(out_dest, data_bytes)
                packet.send()

                return {"result": "sent"}
            except Exception as e:
                return {"error": f"Failed to send single packet: {str(e)}"}

        elif method == "register_plain_destination":
            # Create a PLAIN destination (no identity, unencrypted broadcast)
            app_name = params.get("app_name", "test")
            aspects = params.get("aspects", [])

            dest = RNS.Destination(
                None,
                RNS.Destination.IN,
                RNS.Destination.PLAIN,
                app_name,
                *aspects
            )

            dest.set_packet_callback(
                lambda data, pkt, d=dest: self._on_plain_packet(d, data, pkt)
            )

            dest_hash = dest.hash.hex()
            self.plain_destinations[dest_hash] = dest

            return {
                "result": {
                    "hash": dest_hash,
                }
            }

        elif method == "send_plain_packet":
            # Send a plain broadcast packet (unencrypted, no identity)
            app_name = params.get("app_name", "test")
            aspects = params.get("aspects", [])
            data_hex = params.get("data", "")

            try:
                dest = RNS.Destination(
                    None,
                    RNS.Destination.OUT,
                    RNS.Destination.PLAIN,
                    app_name,
                    *aspects
                )

                data_bytes = bytes.fromhex(data_hex)
                packet = RNS.Packet(dest, data_bytes)
                packet.send()

                return {"result": {"sent": True, "hash": dest.hash.hex()}}
            except Exception as e:
                return {"error": f"Failed to send plain packet: {str(e)}"}

        elif method == "get_received_plain_packets":
            packets = []
            for ts, dest_hash, data_hex in self.received_plain_packets:
                packets.append({
                    "timestamp": ts,
                    "dest_hash": dest_hash,
                    "data": data_hex,
                })
            return {"result": packets}

        elif method == "shutdown":
            self.running = False
            return {"result": "shutting_down"}

        else:
            return {"error": f"Unknown method: {method}"}

    def _on_single_packet(self, dest, data, packet):
        """Called when a single (non-link) packet is received at a destination."""
        if self.verbose:
            print(f"Single packet received at {dest.hash.hex()}: {len(data)} bytes")

        self.received_single_packets.append((
            time.time(),
            dest.hash.hex(),
            data.hex() if isinstance(data, bytes) else data.encode().hex(),
        ))

    def _on_plain_packet(self, dest, data, packet):
        """Called when a plain broadcast packet is received."""
        if self.verbose:
            print(f"Plain packet received at {dest.hash.hex()}: {len(data)} bytes")

        self.received_plain_packets.append((
            time.time(),
            dest.hash.hex(),
            data.hex() if isinstance(data, bytes) else data.encode().hex(),
        ))

    def _on_link_established(self, link, dest):
        """Called when a link is established to one of our destinations."""
        if self.verbose:
            print(f"Link established: {link.hash.hex() if link.hash else 'unknown'}")

        link_hash = link.hash.hex() if link.hash else "unknown"
        self.links[link_hash] = link

        link.set_packet_callback(lambda msg, pkt: self._on_packet(link, msg, pkt))
        link.set_link_closed_callback(lambda l: self._on_link_closed(l))

        # Set up channel handler for Rust channel messages (RawBytesMessage)
        try:
            channel = link.get_channel()
            channel.register_message_type(RawBytesMessage)
            channel.add_message_handler(lambda msg: self._on_channel_message(link, msg))
        except Exception as e:
            if self.verbose:
                print(f"Failed to set up channel handler: {e}")

        # Resource callbacks: if this destination has accept_all strategy, set up
        # ACCEPT_ALL and concluded callback on the link
        dest_hash = dest.hash.hex()
        if self.resource_strategies.get(dest_hash) == "accept_all":
            link.set_resource_strategy(RNS.Link.ACCEPT_ALL)
            link.set_resource_started_callback(
                lambda resource: self._on_resource_started(resource)
            )
            link.set_resource_concluded_callback(
                lambda resource: self._on_resource_concluded(resource)
            )

    def _on_resource_started(self, resource):
        """Called when a resource transfer starts."""
        if self.verbose:
            print(f"Resource transfer started: {resource.hash.hex()}")

    def _on_resource_concluded(self, resource):
        """Called when a resource transfer concludes (complete or failed)."""
        if self.verbose:
            print(f"Resource concluded: {resource.hash.hex()}, status={resource.status}")
        if resource.status == RNS.Resource.COMPLETE:
            data = resource.data.read() if hasattr(resource.data, 'read') else bytes(resource.data)
            # resource.metadata is the DECODED value (after umsgpack.unpackb).
            # For bytes metadata, .hex() gives hex of the raw bytes.
            meta = None
            if resource.metadata is not None:
                if isinstance(resource.metadata, bytes):
                    meta = resource.metadata.hex()
                else:
                    # Non-bytes metadata (dict, string, etc) — encode back for RPC
                    import umsgpack
                    meta = umsgpack.packb(resource.metadata).hex()
            self.received_resources.append({
                "resource_hash": resource.hash.hex(),
                "data": data.hex(),
                "metadata": meta,
                "status": "complete",
            })
        else:
            self.received_resources.append({
                "resource_hash": resource.hash.hex() if resource.hash else "unknown",
                "data": "",
                "metadata": None,
                "status": str(resource.status),
            })

    def _on_channel_message(self, link, message):
        """Called when a channel message is received over a link."""
        if self.verbose:
            print(f"Channel message received: {message.data}")

        self.received_packets.append((time.time(), link, message.data))

        # Echo back via channel (only when --echo-channel is set)
        if self.echo_channel:
            try:
                echo = RawBytesMessage()
                echo.data = message.data
                link.get_channel().send(echo)
            except Exception as e:
                if self.verbose:
                    print(f"Failed to echo channel message: {e}")

        return True

    def _on_packet(self, link, message, packet):
        """Called when a packet is received over a link."""
        if self.verbose:
            print(f"Packet received: {message}")

        self.received_packets.append((time.time(), link, message))

        # Echo back
        if isinstance(message, str):
            message = message.encode('utf-8')
        RNS.Packet(link, message).send()

    def _on_link_closed(self, link):
        """Called when a link is closed."""
        if self.verbose:
            print(f"Link closed: {link.hash.hex() if link.hash else 'unknown'}")

        link_hash = link.hash.hex() if link.hash else None
        if link_hash and link_hash in self.links:
            del self.links[link_hash]

    def run(self):
        """Run the daemon until shutdown."""
        # Signal readiness by printing to stdout
        extra = f" {self.instance_name}" if self.share_instance else ""
        print(f"READY {self.rns_port} {self.cmd_port}{extra}", flush=True)

        try:
            while self.running:
                time.sleep(0.5)
        except KeyboardInterrupt:
            pass
        finally:
            self._cleanup()

    def _cleanup(self):
        """Clean up resources."""
        self.running = False
        try:
            self.cmd_socket.close()
        except:
            pass

        # Clean up temp directory
        try:
            import shutil
            shutil.rmtree(self.config_dir, ignore_errors=True)
        except:
            pass


def main():
    parser = argparse.ArgumentParser(description="RNS test daemon for Rust interop")
    parser.add_argument("--rns-port", type=int, required=True,
                        help="Port for Reticulum TCP interface")
    parser.add_argument("--cmd-port", type=int, required=True,
                        help="Port for JSON-RPC command interface")
    parser.add_argument("--udp-listen-port", type=int, default=None,
                        help="Port for UDP interface (listen)")
    parser.add_argument("--udp-forward-port", type=int, default=None,
                        help="Port for UDP interface (forward)")
    parser.add_argument("--auto-interface", action="store_true",
                        help="Enable AutoInterface for LAN discovery")
    parser.add_argument("--group-id", type=str, default=None,
                        help="Group ID for AutoInterface (default: reticulum)")
    parser.add_argument("--share-instance", action="store_true",
                        help="Enable shared instance (local Unix socket)")
    parser.add_argument("--instance-name", type=str, default=None,
                        help="Instance name for shared instance (default: default)")
    parser.add_argument("--echo-channel", action="store_true",
                        help="Echo received channel messages back via channel")
    parser.add_argument("--respond-to-probes", action="store_true",
                        help="Enable respond_to_probes (prints PROBE_DEST:<hex> on startup)")
    parser.add_argument("-v", "--verbose", action="store_true",
                        help="Enable verbose output")
    parser.add_argument("--mgmt-announce-interval-seconds", type=int, default=None,
                        help="Override Transport.mgmt_announce_interval for keepalive tests")
    parser.add_argument("--enable-remote-management", action="store_true",
                        help="Enable remote management (rnstatus -R) destination")
    parser.add_argument("--remote-management-allowed", type=str, action="append",
                        default=None,
                        help="Hex identity hash allowed to remote-manage (repeatable)")

    args = parser.parse_args()

    # Apply mgmt_announce_interval override BEFORE Transport.start by setting
    # the class attribute. Transport reads it at check time, so subsequent
    # firings use the reduced interval. Production default stays 2 h.
    if args.mgmt_announce_interval_seconds is not None:
        Transport.mgmt_announce_interval = args.mgmt_announce_interval_seconds

    daemon = TestDaemon(
        rns_port=args.rns_port,
        cmd_port=args.cmd_port,
        verbose=args.verbose,
        udp_listen_port=args.udp_listen_port,
        udp_forward_port=args.udp_forward_port,
        auto_interface=args.auto_interface,
        group_id=args.group_id,
        share_instance=args.share_instance,
        instance_name=args.instance_name,
        echo_channel=args.echo_channel,
        respond_to_probes=args.respond_to_probes,
        enable_remote_management=args.enable_remote_management,
        remote_management_allowed=args.remote_management_allowed,
    )
    daemon.run()


if __name__ == "__main__":
    main()

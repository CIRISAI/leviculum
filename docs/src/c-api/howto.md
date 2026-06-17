# C API: How-To, Building Applications

This chapter shows how the functions combine into working programs. It assumes
the model from the [Overview](overview.md): opaque handles, integer error
codes, read(2) buffers, and the pollable event fd. Each recipe gives the
functions involved and a focused snippet; the complete, compiling programs are
the acceptance tests under `reticulum-ffi/examples/c/`, named per recipe.

Error checks are abbreviated in the snippets for readability. In real code,
check every `int` return against `LEV_OK` and report `lev_last_error()` (see
[Errors and logging](#errors-and-logging)).

## A minimal node

Build a node, attach an interface, start it, and shut it down. The builder is
single-use: `lev_builder_build` consumes its configuration and you still free
the empty handle.

```c
#include <leviculum.h>
#include <stdio.h>

int main(void) {
    lev_init();
    printf("leviculum %s\n", lev_version_string());

    lev_builder_t *b = lev_builder_new();
    lev_builder_storage_path(b, "/var/lib/myapp/reticulum");
    lev_builder_add_tcp_client(b, "127.0.0.1:4242");   /* a Reticulum hub */

    leviculum_t *node = lev_builder_build(b);
    lev_builder_free(b);                               /* build emptied it */
    if (!node) {
        fprintf(stderr, "build failed: %s\n", lev_last_error());
        return 1;
    }

    if (lev_start(node) != LEV_OK) {
        fprintf(stderr, "start failed: %s\n", lev_last_error());
        lev_free(node);
        return 1;
    }

    /* ... run the application ... */

    lev_stop(node);
    lev_free(node);     /* lev_free also stops a still-running node */
    return 0;
}
```

Interfaces are added on the builder: `lev_builder_add_tcp_client`,
`lev_builder_add_tcp_server`, `lev_builder_add_udp`,
`lev_builder_add_auto_interface`. Use `lev_builder_identity` to pin a specific
identity (otherwise one is generated), and `lev_builder_enable_transport(b, 1)`
to act as a relay.

Full program: `reticulum-ffi/examples/c/phase_a.c`.

## Running the event loop

Everything inbound arrives as events. Add `lev_event_fd(node)` to your loop,
and on each wake drain with `lev_next_event` until it yields `NULL`.

```c
#include <poll.h>

int fd = lev_event_fd(node);
for (;;) {
    struct pollfd p = { .fd = fd, .events = POLLIN };
    poll(&p, 1, -1);

    lev_event_t *ev;
    while (lev_next_event(node, &ev) == LEV_OK && ev) {
        switch (lev_event_type(ev)) {
            case LEV_EVENT_ANNOUNCE_RECEIVED: on_announce(ev); break;
            case LEV_EVENT_LINK_REQUEST:      on_link_request(ev); break;
            case LEV_EVENT_LINK_DATA:         on_link_data(ev); break;
            /* ... */
        }
        lev_event_free(ev);
    }
}
```

If you do not want to own a loop, block for one event at a time:

```c
lev_event_t *ev = NULL;
if (lev_wait_event(node, &ev, 1000) == LEV_OK && ev) {   /* up to 1s */
    /* handle ev */
    lev_event_free(ev);
}
```

Rules: the fd is level-triggered (readable while the queue is non-empty); the
two drain functions are single-consumer (one thread at a time); and the
shutdown order is stop reacting to the fd, then `lev_free`. Reading an event's
fields uses the typed accessors shown in the recipes below.

## Identities

An identity is a key pair. Generate one, persist it, and reload it next run.
The on-disk format is the raw 64-byte private key, compatible with Python
Reticulum.

```c
lev_identity_t *id;
id = lev_identity_load_file("/var/lib/myapp/identity");
if (!id) {                                   /* first run: make one */
    id = lev_identity_generate();
    lev_identity_save_file(id, "/var/lib/myapp/identity");
}

uint8_t hash[LEV_ADDR_LEN];
uintptr_t len = sizeof(hash);
lev_identity_hash(id, hash, sizeof(hash), &len);   /* the 16-byte address */
```

A combined key is 64 bytes (`LEV_IDENTITY_KEY_LEN`): the X25519 encryption key
in bytes `0..32` and the Ed25519 signing key in bytes `32..64`. Applications
rarely split it by hand, because `lev_connect` resolves the signing key for
you (see below). Use `lev_builder_identity(b, id)` to give a node a fixed
identity, and `lev_identity_free(id)` when done.

Full program: `reticulum-ffi/examples/c/phase_a.c`.

## Announcing and discovering

To be reachable, a node registers an incoming destination and announces it.
Other nodes learn the destination (its address, identity, and a path) from the
announce, which arrives as `LEV_EVENT_ANNOUNCE_RECEIVED`.

Announcing side:

```c
const char *aspects[] = { "inbox" };
lev_destination_t *dest = lev_destination_new(
    id, LEV_DIRECTION_IN, LEV_DEST_SINGLE, "myapp", aspects, 1);

uint8_t dh[LEV_ADDR_LEN];
uintptr_t dhl = sizeof(dh);
lev_destination_hash(dest, dh, sizeof(dh), &dhl);   /* read before registering */

lev_register_destination(node, dest);   /* consumes dest */
lev_destination_free(dest);             /* free the empty shell */

lev_announce(node, dh, NULL, 0, 2000);  /* optional app_data, here none */
```

Receiving side, in the event loop:

```c
case LEV_EVENT_ANNOUNCE_RECEIVED: {
    uint8_t peer[LEV_ADDR_LEN];
    uintptr_t n = sizeof(peer);
    lev_event_dest_hash(ev, peer, sizeof(peer), &n);   /* who announced */
    /* optional payload via lev_event_data(ev, ...) */
    break;
}
```

After processing the announce, the receiver has a path and the announcer's
cached identity, so `lev_has_path(node, peer)` returns 1 and `lev_connect` will
work.

Full program: `reticulum-ffi/examples/c/phase_b.c`.

## Links and exchanging data

A link is an encrypted session to a destination. `lev_connect` resolves the
peer's signing key from the identity cached by an announce, so you pass only
the destination hash:

```c
lev_link_t *link = NULL;
int rc = lev_connect(node, peer, 5000, &link);
if (rc == LEV_ERR_UNKNOWN_DEST) { /* no announce seen yet */ }
else if (rc == LEV_ERR_NO_PATH) { lev_request_path(node, peer, 3000); }
else if (rc == LEV_OK) { /* link is pending; wait for established */ }
```

The connecting node watches for `LEV_EVENT_LINK_ESTABLISHED`; the destination
node watches for `LEV_EVENT_LINK_REQUEST` and accepts it:

```c
case LEV_EVENT_LINK_REQUEST: {
    uint8_t lid[LEV_ADDR_LEN];
    uintptr_t n = sizeof(lid);
    lev_event_link_id(ev, lid, sizeof(lid), &n);
    lev_link_t *accepted = NULL;
    lev_accept_link(node, lid, 5000, &accepted);
    /* keep `accepted` to send on this link */
    break;
}
```

Send and receive link data. `lev_link_send` blocks up to its deadline,
retrying backpressure; `lev_link_try_send` returns `LEV_ERR_AGAIN` instead of
blocking. Inbound data arrives as `LEV_EVENT_LINK_DATA`:

```c
lev_link_send(link, (const uint8_t *)"hello", 5, 5000);

case LEV_EVENT_LINK_DATA: {
    uint8_t buf[512];
    uintptr_t n = sizeof(buf);
    if (lev_event_data(ev, buf, sizeof(buf), &n) == LEV_OK) {
        /* `n` bytes received */
    }
    break;
}
```

Close with `lev_close_link(link, 2000)` and release with `lev_link_free(link)`
(which also closes an open link). A `LEV_EVENT_LINK_CLOSED` event reports a
link that drops for any reason.

Full program: `reticulum-ffi/examples/c/phase_c.c`.

## Proving identity on a link

By default a link is anonymous. Either side can prove an identity to the peer;
the peer is notified with `LEV_EVENT_LINK_IDENTIFIED` and can read it back.

```c
/* prover */
lev_link_identify(node, my_link_id, my_identity, 3000);

/* peer, in the event loop */
case LEV_EVENT_LINK_IDENTIFIED: {
    lev_identity_t *who = lev_link_remote_identity(node, my_link_id);
    if (who) {
        uint8_t h[LEV_ADDR_LEN];
        uintptr_t n = sizeof(h);
        lev_identity_hash(who, h, sizeof(h), &n);   /* the peer's address */
        lev_identity_free(who);
    }
    break;
}
```

The 16-byte identity hash is also the payload of the
`LEV_EVENT_LINK_IDENTIFIED` event (`lev_event_data`).

Full program: `reticulum-ffi/examples/c/phase_c.c`.

## Request and response

For a request/response service, the responder registers a handler for a path
on its destination; the requester sends a request over a link. Request and
response payloads are msgpack-encoded values.

Responder:

```c
lev_register_request_handler(node, dh, "/echo",
                             LEV_REQUEST_POLICY_ALLOW_ALL, NULL, 0);

case LEV_EVENT_REQUEST_RECEIVED: {
    uint8_t link_id[LEV_ADDR_LEN], req_id[LEV_ADDR_LEN], data[512];
    uintptr_t a = sizeof(link_id), b = sizeof(req_id), c = sizeof(data);
    lev_event_link_id(ev, link_id, sizeof(link_id), &a);
    lev_event_request_id(ev, req_id, sizeof(req_id), &b);
    lev_event_data(ev, data, sizeof(data), &c);          /* the request body */
    /* path is available via lev_event_path(ev, ...) */
    lev_send_response(node, link_id, req_id, data, c, 3000);  /* echo it */
    break;
}
```

Requester (over an established link, whose id comes from `lev_link_id`):

```c
uint8_t req[] = { 0xA4, 'p','i','n','g' };   /* msgpack "ping" */
uint8_t request_id[LEV_ADDR_LEN];
lev_send_request(node, link_id, "/echo", req, sizeof(req), 5000, request_id);

case LEV_EVENT_RESPONSE_RECEIVED: {
    uint8_t rid[LEV_ADDR_LEN], body[512];
    uintptr_t a = sizeof(rid), b = sizeof(body);
    lev_event_request_id(ev, rid, sizeof(rid), &a);   /* match request_id */
    lev_event_data(ev, body, sizeof(body), &b);
    break;
}
```

A request that gets no reply within its deadline surfaces as
`LEV_EVENT_REQUEST_TIMEOUT`. To restrict callers, use
`LEV_REQUEST_POLICY_ALLOW_LIST` with an array of `n_ids` 16-byte identity
hashes.

Full program: `reticulum-ffi/examples/c/phase_d.c`.

## Datagrams

A datagram is a single, unreliable packet to a destination. A path must
already be known. Delivery is best-effort: a `LEV_EVENT_PACKET_RECEIVED` on the
other side, and a delivery confirmation only if the destination returns a
proof.

```c
uint8_t packet_hash[LEV_ADDR_LEN];
int rc = lev_send_datagram(node, dest_hash, (const uint8_t *)"hi", 2,
                           packet_hash, 3000);
if (rc == LEV_ERR_NO_PATH) { lev_request_path(node, dest_hash, 3000); }

/* receiver */
case LEV_EVENT_PACKET_RECEIVED: {
    uint8_t buf[256];
    uintptr_t n = sizeof(buf);
    lev_event_data(ev, buf, sizeof(buf), &n);
    break;
}
```

Full program: `reticulum-ffi/examples/c/phase_d.c`.

## Resource transfer

A resource carries bulk data (a file) over a link, in segments, with optional
compression and msgpack metadata. The receiver chooses a strategy: accept all,
reject all, or be asked per transfer.

Receiver sets a strategy on the link, then accepts when advertised:

```c
lev_set_resource_strategy(node, link_id, LEV_RESOURCE_ACCEPT_APP);

case LEV_EVENT_RESOURCE_ADVERTISED:
    lev_accept_resource(node, link_id, 3000);   /* or lev_reject_resource */
    break;

case LEV_EVENT_RESOURCE_COMPLETED: {
    uint8_t buf[65536];
    uintptr_t n = sizeof(buf);
    lev_event_data(ev, buf, sizeof(buf), &n);    /* the assembled data */
    /* metadata via lev_event_metadata(ev, ...) if present */
    break;
}
```

Sender initiates the transfer and tracks progress:

```c
uint8_t resource_hash[LEV_RESOURCE_HASH_LEN];
lev_send_resource(node, link_id, file_data, file_len,
                  NULL, 0,        /* optional msgpack metadata */
                  1,              /* auto-compress */
                  resource_hash, 5000);

case LEV_EVENT_RESOURCE_PROGRESS: {
    double frac;
    lev_event_progress(ev, &frac);   /* 0.0 .. 1.0 */
    break;
}
```

`LEV_EVENT_RESOURCE_COMPLETED` carries the data only on the receiver;
`LEV_EVENT_RESOURCE_FAILED` reports a transfer that did not finish.

Full program: `reticulum-ffi/examples/c/phase_e.c`.

## Errors and logging

Every fallible call returns `int`. Pair the code with the thread-local detail:

```c
int rc = lev_connect(node, peer, 5000, &link);
if (rc != LEV_OK) {
    fprintf(stderr, "connect: %s (%s)\n", lev_strerror(rc), lev_last_error());
}
```

`LEV_ERR_AGAIN` (from `lev_link_try_send`) and `LEV_ERR_TIMEOUT` are normal,
retryable conditions, not hard failures. Logging from the stack itself is off
by default; turn it on and route it to your own sink:

```c
static void log_sink(int level, const char *msg, void *user) {
    (void)user;
    fprintf(stderr, "[lev %d] %s\n", level, msg);
}

lev_init();
lev_log_set_callback(log_sink, NULL);
lev_log_set_level(LEV_LOG_INFO);
```

The callback may run on an internal thread and must not call back into any
`lev_*` function. For hex display of an address, use `lev_hex_encode` and
`lev_hex_decode`.

## Putting it together

A typical application wires these into one loop: it loads or generates an
identity, builds and starts a node with an interface, registers and announces a
destination, then runs the event loop, reacting to announces by connecting,
to link requests by accepting, and to data, request, and resource events by
serving the application. The `phase_b.c` through `phase_e.c` programs are
complete two-node demonstrations of exactly these flows, runnable via
`cargo test-ffi`.

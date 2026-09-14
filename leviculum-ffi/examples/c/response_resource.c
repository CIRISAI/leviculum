/*
 * Leviculum C API example and acceptance test: answering a request with a
 * response that does not fit in one packet.
 *
 * Codeberg #391. A C program serving pages (a NomadNet node, say) answers
 * most requests with lev_send_response, which is bounded by the link MDU. A
 * page above that bound has to travel as a *response* Resource: a resource
 * that carries the request id, so the requester correlates it to the request
 * it is still waiting on. lev_send_resource cannot do that — it carries no
 * request id, so the requester never matches it and simply waits forever.
 * lev_send_response_resource is the call that closes that gap, and this
 * program is the proof that the failure mode is a completed transfer and not
 * a hang.
 *
 * Node A serves "/page/large.mu" and answers with a payload well above the
 * MDU; node B requests it and must receive the exact bytes as one response
 * event. The single-packet call is tried first and must refuse the payload:
 * if it ever stopped refusing, this test would silently be exercising the
 * packet path and proving nothing.
 *
 * Takes the loopback listen address as argv[1], same contract as the phase
 * examples: `127.0.0.1:0`, node A reports the kernel-assigned port back and
 * node B dials it (Codeberg #221).
 *
 * Returns 0 on success, non-zero on failure. Compiled and run by the Rust
 * harness in tests/ffi_c_tests.rs.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "leviculum.h"

static int failures = 0;

#define CHECK(cond)                                                            \
    do {                                                                       \
        if (!(cond)) {                                                         \
            fprintf(stderr, "  CHECK failed at %s:%d: %s\n", __FILE__,         \
                    __LINE__, #cond);                                          \
            failures++;                                                        \
        }                                                                      \
    } while (0)

/* The link MDU is not a small constant here: over TCP, MTU discovery raises it
 * to the interface HW_MTU (262144 B), so a payload has to clear *that* to
 * genuinely need the resource path. 300000 B does, and stays well under the
 * single-segment resource ceiling. */
#define PAYLOAD_LEN 300000u

/* Wait up to `rounds` * 200ms for an event of `want` type on `n`, returning the
 * matched event (caller frees) or NULL. Non-matching events are drained. */
static lev_event_t *wait_for_ev(leviculum_t *n, int want, int rounds) {
    for (int r = 0; r < rounds; r++) {
        lev_event_t *ev = NULL;
        if (lev_wait_event(n, &ev, 200) != LEV_OK) {
            return NULL;
        }
        if (!ev) {
            continue;
        }
        if (lev_event_type(ev) == want) {
            return ev;
        }
        lev_event_free(ev);
    }
    return NULL;
}

/* One msgpack bin32 value of PAYLOAD_LEN deterministic bytes: the header plus
 * the body, which is exactly what the `data` contract asks for — a single
 * encoded value, with no [request_id, response] wrapper of the caller's own. */
static uint8_t *make_response_value(size_t *out_len) {
    size_t len = 5 + PAYLOAD_LEN;
    uint8_t *v = malloc(len);
    if (!v) {
        return NULL;
    }
    v[0] = 0xc6; /* bin32 */
    v[1] = (uint8_t)((PAYLOAD_LEN >> 24) & 0xff);
    v[2] = (uint8_t)((PAYLOAD_LEN >> 16) & 0xff);
    v[3] = (uint8_t)((PAYLOAD_LEN >> 8) & 0xff);
    v[4] = (uint8_t)(PAYLOAD_LEN & 0xff);
    for (size_t i = 0; i < PAYLOAD_LEN; i++) {
        v[5 + i] = (uint8_t)(i % 251);
    }
    *out_len = len;
    return v;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr,
                "usage: %s <host:port>\n"
                "  Listen address for node A's TCP server; 127.0.0.1:0 is the\n"
                "  intended form. A binds it and B dials the kernel-assigned\n"
                "  port A reports (Codeberg #221). No default on purpose: a\n"
                "  compiled-in literal is what made ports collide across the\n"
                "  suite (Codeberg #206).\n",
                argv[0]);
        return 2;
    }
    const char *addr = argv[1];

    printf("leviculum over-MDU response C acceptance test\n");
    CHECK(lev_init() == LEV_OK);

    size_t value_len = 0;
    uint8_t *value = make_response_value(&value_len);
    if (!value) {
        fprintf(stderr, "out of memory building the response value\n");
        return 1;
    }

    /* Node A: TCP server, identity, incoming destination, request handler. */
    lev_identity_t *ida = lev_identity_generate();
    lev_builder_t *ba = lev_builder_new();
    CHECK(lev_builder_identity(ba, ida) == LEV_OK);
    CHECK(lev_builder_storage_path(ba, "/tmp/leviculum-c-response-resource-a") ==
          LEV_OK);
    CHECK(lev_builder_add_tcp_server(ba, addr) == LEV_OK);
    leviculum_t *a = lev_builder_build(ba);
    lev_builder_free(ba);
    CHECK(a != NULL);
    CHECK(lev_start(a) == LEV_OK);

    char bound[64];
    size_t bound_len = 0;
    CHECK(lev_tcp_listen_addr(a, 0, (uint8_t *)bound, sizeof(bound) - 1,
                              &bound_len) == LEV_OK);
    bound[bound_len] = '\0';

    /* Node B: TCP client. */
    lev_builder_t *bb = lev_builder_new();
    CHECK(lev_builder_storage_path(bb, "/tmp/leviculum-c-response-resource-b") ==
          LEV_OK);
    CHECK(lev_builder_add_tcp_client(bb, bound) == LEV_OK);
    leviculum_t *b = lev_builder_build(bb);
    lev_builder_free(bb);
    CHECK(b != NULL);
    CHECK(lev_start(b) == LEV_OK);

    const char *aspects[] = {"page"};
    lev_destination_t *dest = lev_destination_new(
        ida, LEV_DIRECTION_IN, LEV_DEST_SINGLE, "leviculum-demo", aspects, 1);
    CHECK(dest != NULL);
    uint8_t dh[LEV_ADDR_LEN];
    size_t dhl = sizeof(dh);
    CHECK(lev_destination_hash(dest, dh, sizeof(dh), &dhl) == LEV_OK);
    CHECK(lev_register_destination(a, dest) == LEV_OK);
    lev_destination_free(dest);

    CHECK(lev_register_request_handler(a, dh, "/page/large.mu",
                                       LEV_REQUEST_POLICY_ALLOW_ALL, NULL,
                                       0) == LEV_OK);

    /* B learns A. */
    int ready = 0;
    for (int round = 0; round < 50 && !ready; round++) {
        CHECK(lev_announce(a, dh, NULL, 0, 2000) == LEV_OK);
        lev_event_t *ev = NULL;
        lev_wait_event(b, &ev, 300);
        while (ev) {
            lev_event_free(ev);
            ev = NULL;
            if (lev_next_event(b, &ev) != LEV_OK) {
                break;
            }
        }
        if (lev_has_path(b, dh)) {
            ready = 1;
        }
    }
    CHECK(ready == 1);

    /* Link: B connects, A auto-accepts and mints its handle from the event. */
    lev_link_t *lb = NULL;
    CHECK(lev_connect(b, dh, 5000, &lb) == LEV_OK);
    CHECK(lb != NULL);
    lev_event_t *lr = wait_for_ev(a, LEV_EVENT_LINK_ESTABLISHED, 50);
    CHECK(lr != NULL);
    lev_link_t *la = NULL;
    if (lr) {
        uint8_t lid[LEV_ADDR_LEN];
        size_t lidl = sizeof(lid);
        CHECK(lev_event_link_id(lr, lid, sizeof(lid), &lidl) == LEV_OK);
        lev_event_free(lr);
        CHECK(lev_accept_link(a, lid, 5000, &la) == LEV_OK);
        CHECK(la != NULL);
    }
    lev_event_t *est = wait_for_ev(b, LEV_EVENT_LINK_ESTABLISHED, 50);
    CHECK(est != NULL);
    if (est) {
        lev_event_free(est);
    }

    uint8_t lb_id[LEV_ADDR_LEN];
    size_t lb_idl = sizeof(lb_id);
    CHECK(lev_link_id(lb, lb_id, sizeof(lb_id), &lb_idl) == LEV_OK);

    /* B asks for the page. The deadline has to outlast a real multi-part
     * transfer, not just a packet round trip. */
    uint8_t req_id[LEV_ADDR_LEN];
    CHECK(lev_send_request(b, lb_id, "/page/large.mu", NULL, 0, 60000,
                           req_id) == LEV_OK);

    lev_event_t *rr = wait_for_ev(a, LEV_EVENT_REQUEST_RECEIVED, 50);
    CHECK(rr != NULL);
    if (rr) {
        uint8_t path[32];
        size_t pl = sizeof(path);
        CHECK(lev_event_path(rr, path, sizeof(path), &pl) == LEV_OK);
        CHECK(pl == 14 && memcmp(path, "/page/large.mu", 14) == 0);
        uint8_t a_link[LEV_ADDR_LEN];
        size_t all = sizeof(a_link);
        CHECK(lev_event_link_id(rr, a_link, sizeof(a_link), &all) == LEV_OK);
        uint8_t got_id[LEV_ADDR_LEN];
        size_t gil = sizeof(got_id);
        CHECK(lev_event_request_id(rr, got_id, sizeof(got_id), &gil) == LEV_OK);
        lev_event_free(rr);

        /* The single-packet call must refuse this payload. If it ever accepts
         * it, the payload no longer exceeds the MDU and everything below
         * would pass without testing the resource path at all. */
        int packet_rc =
            lev_send_response(a, a_link, got_id, value, value_len, 3000);
        CHECK(packet_rc == LEV_ERR_REQUEST);
        if (packet_rc == LEV_OK) {
            fprintf(stderr,
                    "  lev_send_response accepted a %zu-byte payload: it no "
                    "longer exceeds the link MDU, so this test proves "
                    "nothing. Raise PAYLOAD_LEN.\n",
                    value_len);
        }

        /* The call this test exists for. */
        CHECK(lev_send_response_resource(a, a_link, got_id, value, value_len,
                                         10000) == LEV_OK);
    }

    /* B must actually complete: the whole point is that the old failure mode
     * was a requester waiting forever, not an error code. */
    lev_event_t *re = wait_for_ev(b, LEV_EVENT_RESPONSE_RECEIVED, 250);
    CHECK(re != NULL);
    if (re) {
        uint8_t got_id[LEV_ADDR_LEN];
        size_t gil = sizeof(got_id);
        CHECK(lev_event_request_id(re, got_id, sizeof(got_id), &gil) == LEV_OK);
        CHECK(memcmp(got_id, req_id, LEV_ADDR_LEN) == 0);

        /* read(2) style: ask for the length, then for the bytes. */
        size_t need = 0;
        CHECK(lev_event_data(re, NULL, 0, &need) == LEV_ERR_BUFFER_TOO_SMALL);
        CHECK(need == value_len);
        uint8_t *got = malloc(need ? need : 1);
        if (got) {
            size_t got_len = 0;
            CHECK(lev_event_data(re, got, need, &got_len) == LEV_OK);
            CHECK(got_len == value_len);
            CHECK(got_len == value_len && memcmp(got, value, value_len) == 0);
            free(got);
        } else {
            CHECK(0);
        }
        lev_event_free(re);
    }

    lev_link_free(lb);
    lev_link_free(la);
    CHECK(lev_stop(a) == LEV_OK);
    CHECK(lev_stop(b) == LEV_OK);
    lev_free(a);
    lev_free(b);
    lev_identity_free(ida);
    free(value);

    if (failures == 0) {
        printf("OK\n");
        return 0;
    }
    fprintf(stderr, "%d check(s) failed\n", failures);
    return 1;
}

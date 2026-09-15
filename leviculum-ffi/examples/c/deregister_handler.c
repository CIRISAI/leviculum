/*
 * Leviculum C API example and acceptance test: retiring a request handler.
 *
 * Codeberg #400. A C node that serves paths could register a handler and never
 * take it back: the C surface had no export for the core's
 * deregister_request_handler, so lev_register_request_handler's doc said in as
 * many words that there is no unregister. A node that stops serving a path had
 * to be restarted, and a node that wanted to close a path down for a
 * maintenance window could not. lev_deregister_request_handler is the missing
 * half, and it answers the question a caller actually has: was there a handler
 * to retire? LEV_OK says one was removed, LEV_ERR_NO_HANDLER says there was
 * none, so a caller never has to keep its own book to know which happened.
 *
 * Node A serves "/page/hello.mu"; node B asks and is answered. A retires the
 * handler; B asks again on the SAME link and must now fail the way any
 * unserved path fails — the request dropped without an answer, the requester's
 * deadline expiring as LEV_EVENT_REQUEST_TIMEOUT, and no request ever
 * surfacing on A. A second retire of the same path must report
 * LEV_ERR_NO_HANDLER rather than claiming success a second time.
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

#define PATH "/page/hello.mu"

/* The response: one msgpack value (fixstr "hello"), as lev_send_response's
 * contract demands. */
static const uint8_t RESPONSE[] = {0xa5, 'h', 'e', 'l', 'l', 'o'};

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

/* Drain whatever `n` has queued right now, discarding it. */
static void drain(leviculum_t *n) {
    for (;;) {
        lev_event_t *ev = NULL;
        if (lev_next_event(n, &ev) != LEV_OK || !ev) {
            return;
        }
        lev_event_free(ev);
    }
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

    printf("leviculum deregister request handler C acceptance test\n");
    CHECK(lev_init() == LEV_OK);

    /* Node A: TCP server, identity, incoming destination. */
    lev_identity_t *ida = lev_identity_generate();
    lev_builder_t *ba = lev_builder_new();
    CHECK(lev_builder_identity(ba, ida) == LEV_OK);
    CHECK(lev_builder_storage_path(ba, "/tmp/leviculum-c-deregister-a") ==
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
    CHECK(lev_builder_storage_path(bb, "/tmp/leviculum-c-deregister-b") ==
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

    /* Nothing is registered yet, so the retire call must say so rather than
     * report a success it did not have. */
    CHECK(lev_deregister_request_handler(a, dh, PATH) == LEV_ERR_NO_HANDLER);

    CHECK(lev_register_request_handler(a, dh, PATH,
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

    /* Round one: the path is served, so B is answered. Without this the
     * silence in round two would prove nothing — an unserved path and a
     * broken link look the same from the requester's side. */
    uint8_t req1[LEV_ADDR_LEN];
    CHECK(lev_send_request(b, lb_id, PATH, NULL, 0, 15000, req1) == LEV_OK);

    lev_event_t *rr = wait_for_ev(a, LEV_EVENT_REQUEST_RECEIVED, 100);
    CHECK(rr != NULL);
    if (rr) {
        uint8_t a_link[LEV_ADDR_LEN];
        size_t all = sizeof(a_link);
        CHECK(lev_event_link_id(rr, a_link, sizeof(a_link), &all) == LEV_OK);
        uint8_t got_id[LEV_ADDR_LEN];
        size_t gil = sizeof(got_id);
        CHECK(lev_event_request_id(rr, got_id, sizeof(got_id), &gil) == LEV_OK);
        lev_event_free(rr);
        CHECK(lev_send_response(a, a_link, got_id, RESPONSE, sizeof(RESPONSE),
                                5000) == LEV_OK);
    }

    lev_event_t *re = wait_for_ev(b, LEV_EVENT_RESPONSE_RECEIVED, 100);
    CHECK(re != NULL);
    if (re) {
        uint8_t got_id[LEV_ADDR_LEN];
        size_t gil = sizeof(got_id);
        CHECK(lev_event_request_id(re, got_id, sizeof(got_id), &gil) == LEV_OK);
        CHECK(memcmp(got_id, req1, LEV_ADDR_LEN) == 0);
        lev_event_free(re);
    }

    /* The call this test exists for. */
    CHECK(lev_deregister_request_handler(a, dh, PATH) == LEV_OK);

    /* And it is gone for good: a second retire has nothing to retire. */
    CHECK(lev_deregister_request_handler(a, dh, PATH) == LEV_ERR_NO_HANDLER);

    /* Round two on the same link. The request must now be dropped unanswered:
     * B's deadline expires as LEV_EVENT_REQUEST_TIMEOUT and A never surfaces
     * the request at all. Both halves matter — a REQUEST_RECEIVED on A with a
     * timeout on B would be a responder that forgot to answer, not a retired
     * handler. */
    drain(a);
    drain(b);
    uint8_t req2[LEV_ADDR_LEN];
    CHECK(lev_send_request(b, lb_id, PATH, NULL, 0, 3000, req2) == LEV_OK);

    int saw_request_on_a = 0;
    int timed_out = 0;
    for (int r = 0; r < 100 && !timed_out; r++) {
        lev_event_t *ev = NULL;
        if (lev_wait_event(b, &ev, 200) != LEV_OK) {
            break;
        }
        if (ev) {
            if (lev_event_type(ev) == LEV_EVENT_REQUEST_TIMEOUT) {
                uint8_t got_id[LEV_ADDR_LEN];
                size_t gil = sizeof(got_id);
                CHECK(lev_event_request_id(ev, got_id, sizeof(got_id), &gil) ==
                      LEV_OK);
                CHECK(memcmp(got_id, req2, LEV_ADDR_LEN) == 0);
                timed_out = 1;
            }
            lev_event_free(ev);
        }
        for (;;) {
            lev_event_t *aev = NULL;
            if (lev_next_event(a, &aev) != LEV_OK || !aev) {
                break;
            }
            if (lev_event_type(aev) == LEV_EVENT_REQUEST_RECEIVED) {
                saw_request_on_a = 1;
            }
            lev_event_free(aev);
        }
    }
    CHECK(timed_out == 1);
    CHECK(saw_request_on_a == 0);

    lev_link_free(lb);
    lev_link_free(la);
    CHECK(lev_stop(a) == LEV_OK);
    CHECK(lev_stop(b) == LEV_OK);
    lev_free(a);
    lev_free(b);
    lev_identity_free(ida);

    if (failures == 0) {
        printf("OK\n");
        return 0;
    }
    fprintf(stderr, "%d check(s) failed\n", failures);
    return 1;
}

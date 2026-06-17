/*
 * Leviculum C API example and acceptance test, phase c.
 *
 * Two nodes over TCP loopback establish a link and exchange data: node B
 * learns node A from an announce, connects, A accepts, and B sends a message
 * that A receives as a link-data event.
 *
 * Returns 0 on success, non-zero on failure. Compiled and run by the Rust
 * harness in tests/ffi_c_tests.rs.
 */

#include <stdio.h>
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

/* Wait up to `rounds` * 200ms for an event of `want` type on `n`. On a match,
 * optionally copy the link id and data out. Returns 1 if seen. */
static int wait_for(leviculum_t *n, int want, uint8_t *link_id, uint8_t *data,
                    size_t *data_len, int rounds) {
    for (int r = 0; r < rounds; r++) {
        lev_event_t *ev = NULL;
        if (lev_wait_event(n, &ev, 200) != LEV_OK) {
            return 0;
        }
        if (!ev) {
            continue;
        }
        int matched = 0;
        if (lev_event_type(ev) == want) {
            if (link_id) {
                size_t l = LEV_ADDR_LEN;
                lev_event_link_id(ev, link_id, LEV_ADDR_LEN, &l);
            }
            if (data && data_len) {
                lev_event_data(ev, data, *data_len, data_len);
            }
            matched = 1;
        }
        lev_event_free(ev);
        if (matched) {
            return 1;
        }
    }
    return 0;
}

int main(void) {
    printf("leviculum phase c C acceptance test\n");
    CHECK(lev_init() == LEV_OK);

    const char *addr = "127.0.0.1:45873";

    /* Node A: TCP server with an identity and an incoming destination. */
    lev_identity_t *ida = lev_identity_generate();
    lev_builder_t *ba = lev_builder_new();
    CHECK(lev_builder_identity(ba, ida) == LEV_OK);
    CHECK(lev_builder_storage_path(ba, "/tmp/leviculum-c-phase-c-a") == LEV_OK);
    CHECK(lev_builder_add_tcp_server(ba, addr) == LEV_OK);
    leviculum_t *a = lev_builder_build(ba);
    lev_builder_free(ba);
    CHECK(a != NULL);
    CHECK(lev_start(a) == LEV_OK);

    /* Node B: TCP client to A. */
    lev_builder_t *bb = lev_builder_new();
    CHECK(lev_builder_storage_path(bb, "/tmp/leviculum-c-phase-c-b") == LEV_OK);
    CHECK(lev_builder_add_tcp_client(bb, addr) == LEV_OK);
    leviculum_t *b = lev_builder_build(bb);
    lev_builder_free(bb);
    CHECK(b != NULL);
    CHECK(lev_start(b) == LEV_OK);

    /* A registers an incoming SINGLE destination. */
    const char *aspects[] = {"link"};
    lev_destination_t *dest = lev_destination_new(
        ida, LEV_DIRECTION_IN, LEV_DEST_SINGLE, "leviculum-demo", aspects, 1);
    CHECK(dest != NULL);
    uint8_t dh[LEV_ADDR_LEN];
    size_t dhl = sizeof(dh);
    CHECK(lev_destination_hash(dest, dh, sizeof(dh), &dhl) == LEV_OK);
    CHECK(lev_register_destination(a, dest) == LEV_OK);
    lev_destination_free(dest);

    /* B learns A: re-announce until B has a path (and the cached identity). */
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

    /* B connects; the signing key is resolved from the cached identity. */
    lev_link_t *lb = NULL;
    CHECK(lev_connect(b, dh, 5000, &lb) == LEV_OK);
    CHECK(lb != NULL);

    /* A accepts the incoming link request. */
    uint8_t lid[LEV_ADDR_LEN];
    CHECK(wait_for(a, LEV_EVENT_LINK_REQUEST, lid, NULL, NULL, 50));
    lev_link_t *la = NULL;
    CHECK(lev_accept_link(a, lid, 5000, &la) == LEV_OK);
    CHECK(la != NULL);

    /* B sees the link come up, then sends a message A receives as link data. */
    CHECK(wait_for(b, LEV_EVENT_LINK_ESTABLISHED, NULL, NULL, NULL, 50));

    const char *msg = "ping";
    CHECK(lev_link_send(lb, (const uint8_t *)msg, 4, 5000) == LEV_OK);

    uint8_t rx[64];
    size_t rxl = sizeof(rx);
    CHECK(wait_for(a, LEV_EVENT_LINK_DATA, NULL, rx, &rxl, 50));
    CHECK(rxl == 4 && memcmp(rx, msg, 4) == 0);

    /* Tear down. */
    CHECK(lev_close_link(lb, 2000) == LEV_OK);
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

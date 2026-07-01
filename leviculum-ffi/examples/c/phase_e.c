/*
 * Leviculum C API example and acceptance test, phase e.
 *
 * Resource transfer over a link. Node B opens a link to node A and sends a
 * resource; A uses the AcceptApp strategy, is advertised the transfer,
 * accepts it, and receives the assembled data as a completion event.
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

int main(void) {
    printf("leviculum phase e C acceptance test\n");
    CHECK(lev_init() == LEV_OK);

    const char *addr = "127.0.0.1:45875";

    lev_identity_t *ida = lev_identity_generate();
    lev_builder_t *ba = lev_builder_new();
    CHECK(lev_builder_identity(ba, ida) == LEV_OK);
    CHECK(lev_builder_storage_path(ba, "/tmp/leviculum-c-phase-e-a") == LEV_OK);
    CHECK(lev_builder_add_tcp_server(ba, addr) == LEV_OK);
    leviculum_t *a = lev_builder_build(ba);
    lev_builder_free(ba);
    CHECK(a != NULL);
    CHECK(lev_start(a) == LEV_OK);

    lev_builder_t *bb = lev_builder_new();
    CHECK(lev_builder_storage_path(bb, "/tmp/leviculum-c-phase-e-b") == LEV_OK);
    CHECK(lev_builder_add_tcp_client(bb, addr) == LEV_OK);
    leviculum_t *b = lev_builder_build(bb);
    lev_builder_free(bb);
    CHECK(b != NULL);
    CHECK(lev_start(b) == LEV_OK);

    const char *aspects[] = {"res"};
    lev_destination_t *dest = lev_destination_new(
        ida, LEV_DIRECTION_IN, LEV_DEST_SINGLE, "leviculum-demo", aspects, 1);
    CHECK(dest != NULL);
    uint8_t dh[LEV_ADDR_LEN];
    size_t dhl = sizeof(dh);
    CHECK(lev_destination_hash(dest, dh, sizeof(dh), &dhl) == LEV_OK);
    CHECK(lev_register_destination(a, dest) == LEV_OK);
    lev_destination_free(dest);

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

    /* Establish a link B -> A. Auto-accept model (Python-RNS parity): the core
     * accepts and proves the inbound link on A, which surfaces as an inbound
     * LINK_ESTABLISHED; mint its handle. */
    lev_link_t *lb = NULL;
    CHECK(lev_connect(b, dh, 5000, &lb) == LEV_OK);
    CHECK(lb != NULL);
    lev_event_t *lr = wait_for_ev(a, LEV_EVENT_LINK_ESTABLISHED, 50);
    CHECK(lr != NULL);
    lev_link_t *la = NULL;
    uint8_t la_id[LEV_ADDR_LEN];
    if (lr) {
        size_t lidl = sizeof(la_id);
        CHECK(lev_event_link_id(lr, la_id, sizeof(la_id), &lidl) == LEV_OK);
        lev_event_free(lr);
        CHECK(lev_accept_link(a, la_id, 5000, &la) == LEV_OK);
        CHECK(la != NULL);
    }
    lev_event_t *est = wait_for_ev(b, LEV_EVENT_LINK_ESTABLISHED, 50);
    CHECK(est != NULL);
    if (est) {
        lev_event_free(est);
    }

    /* A asks the app about incoming resources. */
    CHECK(lev_set_resource_strategy(a, la_id, LEV_RESOURCE_ACCEPT_APP) == LEV_OK);

    /* B sends a resource. */
    uint8_t payload[300];
    for (size_t i = 0; i < sizeof(payload); i++) {
        payload[i] = (uint8_t)(i * 7 + 1);
    }
    uint8_t lb_id[LEV_ADDR_LEN];
    size_t lb_idl = sizeof(lb_id);
    CHECK(lev_link_id(lb, lb_id, sizeof(lb_id), &lb_idl) == LEV_OK);
    uint8_t rhash[32];
    CHECK(lev_send_resource(b, lb_id, payload, sizeof(payload), NULL, 0, 1, rhash,
                            5000) == LEV_OK);

    /* A is advertised the resource and accepts it. */
    lev_event_t *adv = wait_for_ev(a, LEV_EVENT_RESOURCE_ADVERTISED, 50);
    CHECK(adv != NULL);
    if (adv) {
        lev_event_free(adv);
        CHECK(lev_accept_resource(a, la_id, 3000) == LEV_OK);
    }

    /* A receives the completed resource and its data matches. */
    lev_event_t *done = wait_for_ev(a, LEV_EVENT_RESOURCE_COMPLETED, 100);
    CHECK(done != NULL);
    if (done) {
        uint8_t rx[512];
        size_t rxl = sizeof(rx);
        CHECK(lev_event_data(done, rx, sizeof(rx), &rxl) == LEV_OK);
        CHECK(rxl == sizeof(payload) &&
              memcmp(rx, payload, sizeof(payload)) == 0);
        uint8_t rh[32];
        size_t rhl = sizeof(rh);
        CHECK(lev_event_resource_hash(done, rh, sizeof(rh), &rhl) == LEV_OK);
        CHECK(rhl == 32);
        lev_event_free(done);
    }

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

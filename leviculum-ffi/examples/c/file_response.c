/*
 * Leviculum C API example and acceptance test: answering a request with a
 * file, the way a NomadNet node answers a `/file/` download.
 *
 * Codeberg #400. A C node that serves pages has three response calls whose
 * names do not tell them apart. lev_send_response and
 * lev_send_response_resource both take ONE msgpack value and let the library
 * wrap it as [request_id, response]; that is what a page is. A file is not:
 * Python's Link.handle_request sends a file handle as a response Resource of
 * the RAW bytes plus a metadata map, with no wrapper, and the requester's
 * has_metadata branch delivers those raw bytes. A C author who reaches for
 * lev_send_response_resource for a download hands the client a msgpack blob
 * where it expected a file, and loses the {"name": ...} it needed to save it.
 * lev_send_file_response is the call that sends the file form, and this
 * program is the proof that BOTH halves arrive: the bytes verbatim and the
 * metadata beside them.
 *
 * Node A serves "/file/data.bin" and answers with lev_send_file_response;
 * node B requests it and must receive the exact raw bytes plus the exact
 * metadata blob. The raw bytes are deliberately not a valid msgpack value, so
 * a wrapped answer cannot pass this test by accident.
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

/* Big enough to be a real multi-part resource transfer rather than something
 * that could ride one packet, small enough to stay quick. */
#define FILE_LEN 40000u

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

/* The file's contents: raw bytes, no msgpack framing of any kind. Byte 0 is
 * 0xc1, the one byte msgpack never assigns, so if this payload ever reached
 * the requester through a wrapping call the decode would be unambiguous
 * nonsense rather than something that might happen to parse. */
static uint8_t *make_file_bytes(void) {
    uint8_t *f = malloc(FILE_LEN);
    if (!f) {
        return NULL;
    }
    f[0] = 0xc1;
    for (size_t i = 1; i < FILE_LEN; i++) {
        f[i] = (uint8_t)((i * 31u + 7u) % 251u);
    }
    return f;
}

/* The {"name": "data.bin"} map NomadNet's serve_file sends as metadata,
 * hand-encoded as msgpack: fixmap(1), fixstr "name", fixstr "data.bin". */
static const uint8_t FILE_METADATA[] = {0x81, 0xa4, 'n',  'a',  'm', 'e',  0xa8,
                                        'd',  'a',  't',  'a',  '.', 'b',  'i',
                                        'n'};

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

    printf("leviculum file response C acceptance test\n");
    CHECK(lev_init() == LEV_OK);

    uint8_t *file = make_file_bytes();
    if (!file) {
        fprintf(stderr, "out of memory building the file\n");
        return 1;
    }

    /* Node A: TCP server, identity, incoming destination, request handler. */
    lev_identity_t *ida = lev_identity_generate();
    lev_builder_t *ba = lev_builder_new();
    CHECK(lev_builder_identity(ba, ida) == LEV_OK);
    CHECK(lev_builder_storage_path(ba, "/tmp/leviculum-c-file-response-a") ==
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
    CHECK(lev_builder_storage_path(bb, "/tmp/leviculum-c-file-response-b") ==
          LEV_OK);
    CHECK(lev_builder_add_tcp_client(bb, bound) == LEV_OK);
    leviculum_t *b = lev_builder_build(bb);
    lev_builder_free(bb);
    CHECK(b != NULL);
    CHECK(lev_start(b) == LEV_OK);

    const char *aspects[] = {"file"};
    lev_destination_t *dest = lev_destination_new(
        ida, LEV_DIRECTION_IN, LEV_DEST_SINGLE, "leviculum-demo", aspects, 1);
    CHECK(dest != NULL);
    uint8_t dh[LEV_ADDR_LEN];
    size_t dhl = sizeof(dh);
    CHECK(lev_destination_hash(dest, dh, sizeof(dh), &dhl) == LEV_OK);
    CHECK(lev_register_destination(a, dest) == LEV_OK);
    lev_destination_free(dest);

    CHECK(lev_register_request_handler(a, dh, "/file/data.bin",
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

    /* B asks for the download. The deadline has to outlast a real transfer,
     * not just a packet round trip. */
    uint8_t req_id[LEV_ADDR_LEN];
    CHECK(lev_send_request(b, lb_id, "/file/data.bin", NULL, 0, 60000,
                           req_id) == LEV_OK);

    lev_event_t *rr = wait_for_ev(a, LEV_EVENT_REQUEST_RECEIVED, 50);
    CHECK(rr != NULL);
    if (rr) {
        uint8_t path[32];
        size_t pl = sizeof(path);
        CHECK(lev_event_path(rr, path, sizeof(path), &pl) == LEV_OK);
        CHECK(pl == 14 && memcmp(path, "/file/data.bin", 14) == 0);
        uint8_t a_link[LEV_ADDR_LEN];
        size_t all = sizeof(a_link);
        CHECK(lev_event_link_id(rr, a_link, sizeof(a_link), &all) == LEV_OK);
        uint8_t got_id[LEV_ADDR_LEN];
        size_t gil = sizeof(got_id);
        CHECK(lev_event_request_id(rr, got_id, sizeof(got_id), &gil) == LEV_OK);
        lev_event_free(rr);

        /* The call this test exists for: raw bytes, metadata beside them. */
        CHECK(lev_send_file_response(a, a_link, got_id, file, FILE_LEN,
                                     FILE_METADATA, sizeof(FILE_METADATA),
                                     10000) == LEV_OK);
    }

    lev_event_t *re = wait_for_ev(b, LEV_EVENT_RESPONSE_RECEIVED, 250);
    CHECK(re != NULL);
    if (re) {
        uint8_t got_id[LEV_ADDR_LEN];
        size_t gil = sizeof(got_id);
        CHECK(lev_event_request_id(re, got_id, sizeof(got_id), &gil) == LEV_OK);
        CHECK(memcmp(got_id, req_id, LEV_ADDR_LEN) == 0);

        /* Half one: the bytes, verbatim and unwrapped. read(2) style — ask
         * for the length, then for the bytes. A wrapped answer would arrive
         * with a different length here and a different first byte. */
        size_t need = 0;
        CHECK(lev_event_data(re, NULL, 0, &need) == LEV_ERR_BUFFER_TOO_SMALL);
        CHECK(need == FILE_LEN);
        uint8_t *got = malloc(need ? need : 1);
        if (got) {
            size_t got_len = 0;
            CHECK(lev_event_data(re, got, need, &got_len) == LEV_OK);
            CHECK(got_len == FILE_LEN);
            CHECK(got_len == FILE_LEN && memcmp(got, file, FILE_LEN) == 0);
            free(got);
        } else {
            CHECK(0);
        }

        /* Half two: the metadata. This is what a client needs to name the
         * file it just downloaded, and it is the half a wrapped response
         * cannot carry at all. */
        size_t mneed = 0;
        CHECK(lev_event_metadata(re, NULL, 0, &mneed) ==
              LEV_ERR_BUFFER_TOO_SMALL);
        CHECK(mneed == sizeof(FILE_METADATA));
        uint8_t mgot[sizeof(FILE_METADATA)];
        size_t mgot_len = 0;
        CHECK(lev_event_metadata(re, mgot, sizeof(mgot), &mgot_len) == LEV_OK);
        CHECK(mgot_len == sizeof(FILE_METADATA));
        CHECK(mgot_len == sizeof(FILE_METADATA) &&
              memcmp(mgot, FILE_METADATA, sizeof(FILE_METADATA)) == 0);

        lev_event_free(re);
    }

    lev_link_free(lb);
    lev_link_free(la);
    CHECK(lev_stop(a) == LEV_OK);
    CHECK(lev_stop(b) == LEV_OK);
    lev_free(a);
    lev_free(b);
    lev_identity_free(ida);
    free(file);

    if (failures == 0) {
        printf("OK\n");
        return 0;
    }
    fprintf(stderr, "%d check(s) failed\n", failures);
    return 1;
}

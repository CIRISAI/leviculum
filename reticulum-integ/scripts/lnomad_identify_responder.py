#!/usr/bin/env python3
# Minimal Python RNS node responder for the lnomad identify (fingerprint)
# acceptance. It is its OWN shared instance and serves a single request path
# /page/whoami.mu whose response reports the remote_identity the node observed
# for the request: the same mechanism NomadNet uses (Destination
# register_request_handler with the six-argument generator receiving
# remote_identity), without NomadNet's executable-page env-var layer, so the
# identify handshake is observed directly and unambiguously.
#
# Usage: lnomad_identify_responder.py <config_dir>
# Prints "DEST <hex>" on stdout once ready; logs each request's observed
# remote_identity to stderr as "HANDLER ... remote_identity=<hex|None>".
import RNS, sys, time, os

configdir = sys.argv[1]
RNS.Reticulum(configdir)

idpath = os.path.join(configdir, "responder_identity")
if os.path.isfile(idpath):
    identity = RNS.Identity.from_file(idpath)
else:
    identity = RNS.Identity()
    identity.to_file(idpath)

dest = RNS.Destination(identity, RNS.Destination.IN, RNS.Destination.SINGLE,
                       "nomadnetwork", "node")
sys.stdout.write("DEST " + dest.hash.hex() + "\n")
sys.stdout.flush()


def whoami(path, data, request_id, link_id, remote_identity, requested_at):
    ri = None
    if remote_identity is not None and hasattr(remote_identity, "hash"):
        ri = RNS.hexrep(remote_identity.hash, delimit=False)
    sys.stderr.write("HANDLER path=%s remote_identity=%s\n" % (path, ri))
    sys.stderr.flush()
    if ri:
        return ("identified as " + ri).encode("utf-8")
    return b"anonymous"


dest.register_request_handler("/page/whoami.mu", response_generator=whoami,
                              allow=RNS.Destination.ALLOW_ALL)
dest.announce()
sys.stderr.write("responder up\n")
sys.stderr.flush()
while True:
    time.sleep(1)

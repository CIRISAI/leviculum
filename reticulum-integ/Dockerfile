FROM debian:trixie-slim

# System dependencies
RUN apt-get update && apt-get install -y \
    python3 python3-pip python3-venv \
    iproute2 iputils-ping net-tools iptables \
    && rm -rf /var/lib/apt/lists/*

# Install Python RNS at build time so rnstatus/rnpath/rnprobe/rnsd are
# available in all containers without a runtime pip install (eliminates
# startup I/O contention).
#
# Installed from PyPI rather than the former `vendor/Reticulum` submodule,
# which was removed in a7e1102. Release 1.3.5 upstreamed the shared-instance
# behaviour the vendored patch provided: the CLI tools now default
# `require_shared_instance=True`, so they still fail loudly when they cannot
# reach the daemon under test (the `RNS_REQUIRE_SHARED` env the harness sets is
# now a harmless no-op). Pinned for reproducible image builds.
RUN pip3 install --break-system-packages rns==1.3.5

# Create reticulum config directory
RUN mkdir -p /root/.reticulum/storage

# Copy entrypoint
COPY reticulum-integ/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENTRYPOINT ["/entrypoint.sh"]

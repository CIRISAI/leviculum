# lblogd

A dev-blog server. Posts are Markdown files with a TOML frontmatter block; the
server renders them to HTML for HTTP/HTTPS clients and to micron for NomadNet
clients over Reticulum. One binary runs both sides concurrently: a NomadNet
page node served through a running `lnsd` shared instance, and a clearnet web
server with automatic HTTPS (Let's Encrypt via rustls-acme).

## Post format

```
+++
title = "Hello"
date = "2026-07-12"
slug = "hello"        # optional, defaults to slugify(title)
+++

Markdown body...
```

Slugs are plain lowercase ASCII: alphanumerics kept, everything else collapsed
to single hyphens (matching micron heading-anchor slugs).

## Renderers

`markdown_to_html` uses pulldown-cmark; `markdown_to_micron` emits micron as
defined by the `leviculum-micron` parser. Constructs without a micron
equivalent degrade gracefully (images to `[image: alt]`, tables to plaintext
rows, blockquotes to indented text); see the mapping table in
`src/render.rs`. Round-trip tests parse the generated micron with
`leviculum-micron` and assert the document structure.

## Configuration

One TOML file drives everything:

```toml
data_dir  = "/var/lib/lblogd"          # identity, node storage, ACME cache
posts_dir = "/var/lib/lblogd/posts"    # the *.md blog posts

[node]
instance_name          = "leviculum"   # must match the running lnsd's instance_name
display_name           = "leviculum.network dev blog"
announce_interval_secs = 21600         # optional, default 21600 (6 hours)

[web]
domains            = ["leviculum.network"]
acme_contact_email = "you@example.org"
acme_staging       = true              # true: LE staging (test), false: production
http_bind          = "0.0.0.0:80"      # optional, this is the default
https_bind         = "0.0.0.0:443"     # optional, this is the default
```

The node's persistent identity lives at `data_dir/identities/lblogd`, node
storage at `data_dir/storage`, the ACME account and certificates at
`data_dir/acme`. Losing the identity file changes the NomadNet destination
hash; losing the ACME cache forces certificate re-issuance. Back up `data_dir`.

Run it:

```
lblogd --config /etc/lblogd.toml
```

`lblogd --config /etc/lblogd.toml --print-hash` resolves the node's persistent
identity locally (no running lnsd needed, the identity is generated on first
use), prints the destination hash on the first line and the served page paths
after it, and exits without starting any server.

## Deployment

### Prerequisites

A running `lnsd` shared instance on the same machine. Give it at least one
public TCP interface so the box doubles as a useful public transport node.
Its `instance_name` must match the config's `[node] instance_name`.

Point DNS A/AAAA records for every name in `domains` at the machine. The
TLS-ALPN-01 challenge means Let's Encrypt must reach port 443 on those names;
no port 80 challenge plumbing exists, port 80 only serves redirects.

### Ports 80 and 443

Binding ports below 1024 as a non-root user needs `CAP_NET_BIND_SERVICE`.
Under systemd that is one line in the unit:

```
AmbientCapabilities=CAP_NET_BIND_SERVICE
```

### systemd units

A dedicated user owning the data directory:

```
useradd --system --home /var/lib/lblogd --create-home lblogd
mkdir -p /var/lib/lblogd/posts
chown -R lblogd:lblogd /var/lib/lblogd
```

`/etc/systemd/system/lnsd.service`:

```ini
[Unit]
Description=Reticulum network daemon
After=network-online.target
Wants=network-online.target

[Service]
User=lblogd
ExecStart=/usr/local/bin/lnsd
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

`/etc/systemd/system/lblogd.service`:

```ini
[Unit]
Description=lblogd dev blog server
After=lnsd.service
Wants=lnsd.service

[Service]
User=lblogd
ExecStart=/usr/local/bin/lblogd --config /etc/lblogd.toml
AmbientCapabilities=CAP_NET_BIND_SERVICE
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

Enable both with `systemctl enable --now lnsd lblogd`.

### ACME staging first

Start with `acme_staging = true` and check the logs for a successful
certificate order (the browser will warn, staging certificates are
untrusted; that is expected). Only then set `acme_staging = false` and
restart. Let's Encrypt production rate-limits failed and repeated orders per
domain, so debugging DNS or firewall problems against production can lock
you out for days.

### Adding a post

Drop a Markdown file into `posts_dir` and restart the service:

```
cat > /var/lib/lblogd/posts/hello.md <<'EOF'
+++
title = "Hello"
date = "2026-07-12"
+++

First post.
EOF
systemctl restart lblogd
```

Posts are loaded once at startup by both the node and the web server; live
reload (SIGHUP or file watching) is a possible future enhancement.

### Publishing the NomadNet address

```
lblogd --config /etc/lblogd.toml --print-hash
```

prints the destination hash readers dial in `lnomad` or NomadNet. Put it on
the web page so clearnet visitors can find the Reticulum side.

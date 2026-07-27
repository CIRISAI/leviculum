# lblogd

A dev-blog server. Posts are Markdown files with a TOML frontmatter block; the
server renders them to HTML for HTTP/HTTPS clients and to micron for NomadNet
clients over Reticulum. One binary runs both sides concurrently: a NomadNet
page node served through a running `lnsd` shared instance, and a clearnet web
server with automatic HTTPS (Let's Encrypt via rustls-acme).

## Post format

A post is a Markdown file. Nothing else is required:

```
Just write. This whole file is one post.
```

It is titled after its file name without the extension, and dated by its
modification time. To override either, open the file with a TOML frontmatter
block:

```
+++
title = "Hello"       # optional, defaults to the file name without .md
date = "2026-07-12"   # optional, defaults to the file's mtime
slug = "hello"        # optional, defaults to slugify(title)
+++

Markdown body...
```

The mtime fallback is the **UTC** calendar day. Without a timezone database
there is no honest way to render a local one, so a file saved shortly after
local midnight dates to the previous day; set `date` explicitly when the date
matters.

Slugs are plain lowercase ASCII: alphanumerics kept, everything else collapsed
to single hyphens (matching micron heading-anchor slugs). Non-ASCII characters
count as separators, so `Größe` slugifies to `gr-e`. Set `slug` explicitly for
titles that are not plain ASCII.

What is still an error: a frontmatter block that opens with `+++` and never
closes, a `date` that is not a valid `YYYY-MM-DD` calendar day, and a title
that slugifies to nothing.

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
data_dir    = "/var/lib/lblogd"        # identity, node storage, ACME cache
posts_dir   = "/var/lib/lblogd/posts"  # the *.md blog posts
watch_posts = false                    # optional, default false; see below

[blog]                                 # optional, but see below
title       = "leviculum.network"      # the heading of every page
author      = "Lew Palm"               # optional
description = "Notes on Reticulum"     # optional, one sentence
language    = "en"                     # optional, BCP 47, default "en"
css         = "/etc/lblogd/style.css"  # optional, replaces the built-in one

[node]
instance_name          = "leviculum"   # must match the running lnsd's instance_name
display_name           = "leviculum.network dev blog"  # optional, defaults to blog.title
announce_interval_secs = 21600         # optional, default 21600 (6 hours)

[web]
acme               = true              # optional, default true; false = plain HTTP
domains            = ["leviculum.network"]
acme_contact_email = "you@example.org"
acme_staging       = true              # true: LE staging (test), false: production
http_bind          = "0.0.0.0:80"      # optional, this is the default
https_bind         = "0.0.0.0:443"     # optional, this is the default
```

With `acme = true` (the default) `domains`, `acme_contact_email` and
`acme_staging` are all required; leaving one out is a config error naming the
field. With `acme = false` they are ignored and may be omitted.

### What the pages say

`[blog]` is what a reader sees; `[node]` and `[web]` are the machinery.

The blog needs a name, so either `blog.title` or `node.display_name` must be
set; each defaults to the other. Set both only when the name on the page and
the name in the announce stream should differ.

`author` is shown under the title and on every post. A post can override it in
its frontmatter, which is what a guest post wants; the index only names an
author where it differs from the blog's, so the usual case stays quiet.

`description` appears under the title and as the HTML meta description, which
is what a search result or a link preview shows.

`language` sets the HTML `lang` attribute. It matters for screen readers,
hyphenation and translation offers, so set it if the blog is not in English.

Each side names the other: HTML pages carry the NomadNet destination hash,
Micron pages the clearnet URL, taken from the first entry in `web.domains`. A
plaintext development run advertises no URL, since there is no public name to
give out.

`acme_contact_email` is **not** used for any of this. It is the operator's
contact for Let's Encrypt, and publishing it on the blog would expose an
address that was given for certificate warnings.

### Feed

An Atom feed is served at `/feed.xml`, with a `<link rel="alternate">` in
every page so readers find it on their own. Entries carry the post's full
text, with relative links and images resolved against the blog's URL so they
still work when read elsewhere.

There is no feed without `web.domains`: absolute links need a domain, and a
plaintext development run has none. The route answers 404 there, and the
pages advertise no feed.

Two things worth knowing:

Posts are dated to the day, so entry timestamps are midnight UTC. Two posts
published on the same day carry identical timestamps and a reader may order
them either way; the index breaks that tie by title, a reader cannot.

An entry is identified by its URL, so a post that changes slug appears in
readers as a new entry. Pin `slug` in the frontmatter for anything already
published, and retitling is then free.

### Styling

Without `blog.css` the pages use a small built-in stylesheet. With it, that
file's contents are inlined into every page instead. It is read at startup and
on every reload, so editing it takes effect the same way editing a post does;
with `watch_posts` on it is watched too.

A stylesheet is inlined rather than linked, so there is no second request and
no way for a page and its stylesheet to disagree.

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

## Local development run

Certificate acquisition needs a publicly reachable domain, so a developer
machine cannot run the deployment config at all. `acme = false` selects
plaintext mode: no HTTPS listener, no ACME traffic, and the HTTP listener
serves the blog itself instead of redirecting.

Point it at an `lnsd` of its own so it neither collides with nor disturbs a
running production daemon — the shared instance is keyed by `instance_name`,
and `default` is usually taken. A config directory holding just

```
[reticulum]
enable_transport = No
share_instance   = Yes
instance_name    = lblogd-dev
```

with no interface sections gives an isolated, network-free daemon that only
carries the local IPC. Then:

```
lnsd --config /tmp/lblogd-dev/reticulum &
lblogd --config /tmp/lblogd-dev/lblogd.toml &
```

with `/tmp/lblogd-dev/lblogd.toml`:

```toml
data_dir    = "/tmp/lblogd-dev/data"
posts_dir   = "/tmp/lblogd-dev/posts"
watch_posts = true

[node]
instance_name = "lblogd-dev"
display_name  = "local dev blog"

[web]
acme      = false
http_bind = "127.0.0.1:8080"
```

The web side is then `curl http://127.0.0.1:8080/`, the NomadNet side

```
lblogd --config /tmp/lblogd-dev/lblogd.toml --print-hash   # the hash to dial
lnomad --instance lblogd-dev --print <hash>:/page/index.mu
lnomad --instance lblogd-dev --discover 20                 # see the announce
```

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
ExecReload=/bin/kill -HUP $MAINPID
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

Drop a Markdown file into `posts_dir` and send SIGHUP:

```
echo "First post." > /var/lib/lblogd/posts/hello.md
systemctl reload lblogd
```

The reload swaps the served content in place: the HTTP listener keeps its
socket and the NomadNet node keeps its links and its destination hash. New
pages get a request handler, removed ones lose theirs, and the destination is
deliberately not re-announced, since it has not changed.

A reload that fails changes nothing. If a post has a malformed date, the
error is logged with its file name and the previous content keeps being
served, so a typo cannot take the blog offline. Startup is the exception:
there the same error is fatal, because there is no previous good state.

To check a post before publishing it, use the dry run — it performs the same
parse without touching the running server:

```
lblogd --config /etc/lblogd.toml --print-hash
```

For the systemd unit to accept `systemctl reload`, add:

```ini
ExecReload=/bin/kill -HUP $MAINPID
```

### Reloading automatically

`watch_posts = true` reloads whenever `posts_dir` changes, with no signal at
all: create a file, and it is live about a second later.

It is a second trigger for the same reload, not a second mechanism, so the
guarantee is unchanged — a failed load leaves the previous content serving.
SIGHUP keeps working alongside it, which is the way out if the filesystem
ever fails to report something.

Off by default. A deployment publishes deliberately, and watching means
reacting to every write, including the half-finished ones an editor produces
on the way to a saved file. The watcher waits for the directory to be quiet
for 500 ms and then reloads once, so a burst of writes collapses into a
single reload of the final state; still, on a server the explicit signal is
the better default. While writing, it is the setting you want.

Changes are only picked up while the process runs — a file added while
`lblogd` is stopped is loaded at the next start like any other.

### Publishing the NomadNet address

```
lblogd --config /etc/lblogd.toml --print-hash
```

prints the destination hash readers dial in `lnomad` or NomadNet. Put it on
the web page so clearnet visitors can find the Reticulum side.

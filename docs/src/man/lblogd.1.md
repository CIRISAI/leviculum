# lblogd(1)

## NAME

lblogd -- dev blog server, on the web and on NomadNet

## SYNOPSIS

**lblogd** **--config** *file*
**lblogd** **--config** *file* **--print-hash**

## DESCRIPTION

**lblogd** serves a directory of Markdown posts on two sides at once: as a NomadNet page node over Reticulum, and as a web server on the clearnet. Posts are plain Markdown files with an optional TOML frontmatter block; adding a file and reloading the service publishes it to both sides.

The NomadNet side is a shared-instance client, so a Reticulum daemon must already be running — either **lnsd**(1) or Python's **rnsd** — under the instance name named in the configuration file. **lblogd** exits if no daemon answers, and the packaged service restarts it until one does.

A post may use the whole standard Markdown feature set, basic and extended: tables, footnotes, strikethrough, task lists, definition lists, heading identifiers, highlights, sub- and superscript, and bare URLs and e-mail addresses, which become links without being written as such. The web side renders all of it as HTML. Micron, the NomadNet page format, has fewer constructs than HTML, so a few degrade on the mesh: struck text is dimmed, a highlight becomes a background colour, and `H~2~O` and `X^2^` use the Unicode subscripts and superscripts where they exist and keep their markers where they do not. Footnotes lose only the jump, not their content: the reference stays as `[1]` and the definitions are collected behind a divider at the end of the page. A heading identifier has nothing to attach itself to on the mesh and is dropped there. Emoji shortcodes such as `:joy:` are not translated on either side and stay as written.

Images travel as files. Micron, the NomadNet page format, has no image construct at all, so a picture referenced from a post is published as a file and linked from the page: NomadNet saves it to the reader's download directory, and **lnomad**(1) draws it inline. On the web the same reference becomes an ordinary `<img>`. Write `![Mast](mast.jpg)` in a post and put `mast.jpg` in the file area; the same name is then served as `/files/mast.jpg` over HTTP and `/file/mast.jpg` over Reticulum. `./mast.jpg`, `files/mast.jpg` and `/files/mast.jpg` all name that file. A reference with a scheme is left alone: nothing on the mesh can fetch an `https://` image, so it degrades to its alt text there and stays an external image on the web.

The area is flat, and a requested name can carry no path separator, so no request can reach outside it. `max_file_bytes` bounds a single file, 10 MiB by default; anything larger is skipped with a line on standard error rather than served, because over a LoRa interface an unbounded transfer denies service to every other reader of the node for as long as it runs.

A domain usually says more than "here are my posts". `pages_dir` holds Markdown pages that are not entries — a landing page, a page about the code — parsed in the post format and rendered like the about page: no date, no byline, never in the post index, never in the feed. Each is served at `/<name>` on the web and `/page/<name>.mu` on the mesh. The name `index` is special: with a `pages_dir/index.md` the site gets a landing page at `/` and `/page/index.mu`, and the post index moves down to `/blog` and `/page/blog.mu`. Without one nothing moves, so a configuration that names no `pages_dir` serves exactly what it served before any of this existed. Posts keep `/posts/<slug>` and the feed keeps `/feed.xml` on purpose: a feed entry is identified by its URL, so moving posts would show every entry again as new in every reader.

The `[links]` section is the other half — names that point at something off this server:

    [links]
    code   = "https://codeberg.org/Lew_Palm/leviculum"
    issues = "https://codeberg.org/Lew_Palm/leviculum/issues"

Each becomes `/<name>` on the web and `/page/<name>.mu` on the mesh, and the two answer differently because they must. The web answers **302**, not 301: a forge changes host, and a browser that cached a 301 would keep going to the old one long after the configuration said otherwise. The mesh answers with a short page naming the URL as text, because a NomadNet client cannot follow a web link at all. A name that is both a link and a page in `pages_dir` shows that page's text above the URL on the mesh, while the web still redirects. A link target must be an absolute `http://` or `https://` URL.

Every page, on both sides, carries a small nav line: the landing page, the blog, then each page by name and each link in the order the configuration lists them. With neither pages nor links there is nothing to put in it and none is emitted.

The web's top level and the mesh's `/page/<name>.mu` are shared namespaces, so page and link names are checked against one list: the routes `blog`, `posts`, `files` and `feed.xml`; `about`, when an about page is configured; every post's slug, which owns `/page/<slug>.mu`; and `index` for a link, since as a page name that *is* the landing page. A collision is a startup error naming both the offender and what already answers there; on a reload it is refused and the previous content keeps serving. Names follow the same slug rules as a post: plain lowercase ASCII letters, digits and hyphens. Unlike the file area, `pages_dir` is not optional by existence — the operator named it, so a directory that is not there is a startup error rather than a site that quietly lost its landing page.

Every page, on both sides, also carries the source offer AGPL section 13 requires: the running version, the licence, and where the source is — a link on the web, the bare URL as text on the mesh. It needs no configuration and cannot be switched off, because a reader of a served page is a user interacting with AGPL software over a network and is owed the Corresponding Source. The `[source] url` key exists for the one case the compiled-in default gets wrong: an operator running a *modified* **lblogd** owes their readers their own tree, not this project's repository. An empty value is a startup error rather than a footer that offers nothing.

The web side either obtains its own certificate from Let's Encrypt, or runs plain behind a reverse proxy that terminates TLS. Note that the canonical page URL and the Atom feed are derived from the configured `domains` list even when certificate handling is switched off, so a deployment behind a proxy still has to set that list.

## COUNTING

**lblogd** appends one record per day to a counts file — `counts.log` under `data_dir` unless `[counter] path` says otherwise, and not at all if `[counter] enabled` is false. There is no UI and no HTTP endpoint; the file is the interface:

    DAY date=2026-08-07 tz=UTC mesh_requests=41 mesh_sessions=12 mesh_identified_requests=0 web_requests=308 web_not_found=57 clock_behind=0 written=2026-08-07T23:59:12Z

The counts are requests and links, and are named as such. **They are not visitors.** On the mesh a request arrives on a Reticulum link, and a link is a session, not a person: one reader browsing five pages over one link is one session and five requests, and the same reader tomorrow is a different session. Reticulum discloses who a peer is only when the peer chooses to identify, which fetching a public page never asks for — `mesh_identified_requests` exists so that its zero is measured rather than assumed. On the web side there is a peer address, and **lblogd** never reads it, on disk or in memory: an address would buy a "unique visitors" figure that CGNAT, rotating IPv6 privacy addresses and crawlers make wrong anyway, at the price of making a blog server hold personal data. `web_not_found` is separate so that scans for pages that do not exist can be subtracted instead of inflating the total.

Dates are UTC calendar days, the same midnight a post's mtime fallback uses, and each record says `tz=UTC` so a bare date is still readable a year later.

The file is append-only and each record carries that day's whole running total, so the last record for a date wins and a `kill -9` mid-write can lose at most the line being written — never an earlier day. A clock that steps backwards never reopens a day already written; its counts land on the open day and `clock_behind` records that it happened. The open day is written every five minutes, at every rollover, and once more on `SIGTERM`, and a restart resumes the day from the file rather than starting it again at zero. Each start compacts the file to one record per date.

`awk '$1=="DAY" {print $2, $4}' /var/lib/lblogd/counts.log` is the intended reader.

## OPTIONS

**--config** *file*
:   Path to the TOML configuration file. Required.

**--print-hash**
:   Resolve the node's destination hash and the request paths it would serve — the pages first, including the static pages and the links, then the files — print them, and exit without starting any server. Needs no running daemon, so it doubles as a dry run for publishing: the posts and the file area are read exactly as serve mode reads them, with the same errors.

## FILES

*/etc/lblogd/config.toml*
:   Configuration file installed by the Debian package. Registered as a conffile, so local edits survive upgrades.

*/var/lib/lblogd/posts/*
:   Where the packaged service reads posts from: one Markdown file per post.

*pages_dir*
:   Static pages: one Markdown file per page, named by its file stem. Not configured by default. `index.md` in it becomes the landing page and moves the post index to `/blog`. Reloaded with the posts.

*/var/lib/lblogd/files/*
:   The file area: pictures and other files a post references. Set with `files_dir`, which defaults to a `files` directory beside `posts_dir`. The directory need not exist; without it the blog simply serves no files. Reloaded with the posts.

*/var/lib/lblogd/counts.log*
:   The per-day request counts. See COUNTING. Moved with `[counter] path`, suppressed entirely with `[counter] enabled = false`.

*/var/lib/lblogd/*
:   Node identity and, when certificate handling is enabled, the ACME certificate cache.

## SIGNALS

**SIGHUP**
:   Re-read the posts directory, the static pages and the file area. The packaged service maps `systemctl reload lblogd` onto this.

**SIGTERM**, **SIGINT**
:   Stop. The open day's counts are written out first, so `systemctl stop` and `systemctl restart` do not lose the day so far.

## EXIT STATUS

**lblogd** exits non-zero when the configuration cannot be loaded, when a post cannot be parsed at startup, and when no Reticulum daemon is reachable on the configured shared instance. Once running it is more forgiving: a reload that fails leaves the previous content serving.

## EXAMPLES

Print the mesh address the blog would announce, without starting it:

    lblogd --config /etc/lblogd/config.toml --print-hash

Publish a post to the packaged service:

    sudo cp my-post.md /var/lib/lblogd/posts/
    sudo systemctl reload lblogd

Publish a picture the post refers to as `![Mast](mast.jpg)`:

    sudo install -o lblogd -g lblogd -m 640 mast.jpg /var/lib/lblogd/files/
    sudo systemctl reload lblogd

## SEE ALSO

**lnsd**(1), **lnomad**(1)

#!/usr/bin/env python3
"""Check the external links of the leviculum.network pages (site/pages/*.md)
and of the READMEs (README.md, lblogd/README.md).

The lblogd test `site_content` covers everything that can be decided from the
tree: the pages load, render on both sides, and their site links land. What it
cannot decide is whether a URL pointing off the server still answers, and the
download page is almost nothing but such URLs. This script is that half.
The READMEs hand out the same download URLs, mostly as bare URLs in shell
blocks, so every http(s) URL in them is collected, not only link targets;
local and example.org-style hosts are left out.

Two checks, in this order:

1. Offline, always. Every link into the nightly release, on the forge or on
   leviculum.network/releases/nightly/latest/, must name a file the nightly
   actually produces. The names come from scripts/collect-nightly-debs.sh,
   which stages them under dist/ and is the one place they are decided; a
   page that links `lnsd-nightly-amd64.deb` fails here without a network.
   Likewise every link into the book, leviculum.network/docs/<path>.html,
   must name a page whose source docs/src/<path>.md exists.

2. Online, when the network is there. The forge release's asset list is
   fetched once, and every nightly link must name an asset in it; every other
   external link must answer below 400. Links under
   leviculum.network/releases/ are checked by name only, against the same
   asset list: the site target mirrors the forge's names, and until it is live
   (see scripts/publish-site.sh) fetching them would only report that.

A URL listed in PENDING_FIRST_PUBLISH may answer 404 without failing, for a
target that is only there once something has run on the server. Online, its
status is still fetched and printed, and a 404 there is reported as "not
published yet" rather than as a finding; any other status of 400 or above
still fails. The set holds that exemption and nothing else, and an entry
comes out once the URL has answered 200. It is empty since the book at
https://leviculum.network/docs/ answered 200 on 2026-10-08 (Codeberg #260).

When the forge cannot be reached at all (no route, no DNS, a timeout), the
online half is skipped with a message and the script exits 0: an offline
machine has nothing to say about URLs, and the offline half has already run.
An HTTP error from a reachable host is a finding, not an absence of network.

Exit 0 = every check that could run passed. Exit 1 = a link is wrong.

Usage:
  python3 scripts/site-check.py [--selftest]
"""

import json
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PAGES = ROOT / "site" / "pages"
READMES = (ROOT / "README.md", ROOT / "lblogd" / "README.md")
BOOK_SRC = ROOT / "docs" / "src"
COLLECT = ROOT / "scripts" / "collect-nightly-debs.sh"

FORGE_PREFIX = "https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/"
SITE_PREFIX = "https://leviculum.network/releases/nightly/latest/"
BOOK_PREFIX = "https://leviculum.network/docs/"
RELEASE_API = "https://codeberg.org/api/v1/repos/Lew_Palm/leviculum/releases/tags/nightly"
TIMEOUT = 20

# URLs whose 404 means "not published yet", not "wrong link", with the reason.
PENDING_FIRST_PUBLISH: dict = {}

LINK = re.compile(r"\]\((https?://[^)\s]+)\)")
BARE_URL = re.compile(r"https?://[^\s)<>\"'`]+")
# Hosts a README names as an example or as the reader's own machine.
NOT_EXTERNAL = re.compile(r"^https?://(127\.0\.0\.1|localhost|([^/]*\.)?example\.(org|com|net))([:/]|$)")


class Offline(Exception):
    """The forge could not be reached at all."""


def nightly_names(collect_script: str) -> set:
    """The file names collect-nightly-debs.sh stages under dist/, each with
    its .sha256 beside it."""
    names = set()
    for line in collect_script.splitlines():
        line = line.strip()
        m = re.match(r"^collect_deb (\S+) (\S+)(?: (\S+))?$", line)
        if m:
            names.add(m.group(3) or f"{m.group(1)}-nightly-{m.group(2)}.deb")
            continue
        m = re.match(r"^pack_bin_tarball (\S+) (\S+) ", line)
        if m:
            names.add(f"{m.group(1)}-nightly-{m.group(2)}.tar.gz")
            continue
        for literal in re.findall(r'"\$DIST/([A-Za-z0-9][A-Za-z0-9._+-]*)"', line):
            names.add(literal)
    return names | {f"{n}.sha256" for n in names if not n.endswith(".sha256")}


def page_links(pages: dict) -> list:
    """(page, url) for every external link destination in the pages."""
    return [(name, url) for name, text in sorted(pages.items()) for url in LINK.findall(text)]


def readme_links(readmes: dict) -> list:
    """(readme, url) for every external http(s) URL in the READMEs, bare or
    linked, trailing sentence punctuation stripped."""
    links = []
    for name, text in sorted(readmes.items()):
        for url in BARE_URL.findall(text):
            url = url.rstrip(".,;:")
            if not NOT_EXTERNAL.match(url):
                links.append((name, url))
    return links


def book_source(url: str):
    """The docs/src path a book URL renders from, or None for any other URL.
    mdBook turns docs/src/foo/bar.md into /docs/foo/bar.html; the bare
    directory is the book's index."""
    if not url.startswith(BOOK_PREFIX):
        return None
    path = url[len(BOOK_PREFIX):].split("#", 1)[0]
    if path in ("", "index.html"):
        return None
    if not path.endswith(".html"):
        return path
    return path[: -len(".html")] + ".md"


def release_file(url: str):
    """The file name a nightly-release URL names, or None for any other URL,
    the release directory itself included (that one is fetched like a page)."""
    for prefix in (FORGE_PREFIX, SITE_PREFIX):
        if url.startswith(prefix):
            return url[len(prefix):] or None
    return None


def offline_findings(links: list, produced: set, book_pages: set) -> list:
    findings = []
    for page, url in links:
        name = release_file(url)
        if name is not None and name not in produced:
            findings.append(
                f"{page}: {url} names {name!r}, which collect-nightly-debs.sh does not produce"
            )
        source = book_source(url)
        if source is not None and source not in book_pages:
            findings.append(f"{page}: {url} names a book page with no source docs/src/{source}")
    return findings


def fetch(url: str, method: str = "GET") -> tuple:
    """(status, body) for url. Raises Offline when no answer came back."""
    request = urllib.request.Request(url, method=method, headers={"User-Agent": "leviculum-site-check"})
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
            return response.status, response.read() if method == "GET" else b""
    except urllib.error.HTTPError as e:
        return e.code, b""
    except (urllib.error.URLError, TimeoutError, OSError) as e:
        raise Offline(f"{url}: {e}") from e


def online_findings(links: list) -> list:
    status, body = fetch(RELEASE_API)
    if status != 200:
        return [f"{RELEASE_API} answered {status}; cannot read the forge's asset list"]
    assets = {a["name"] for a in json.loads(body).get("assets", [])}
    findings = []
    checked = set()
    for page, url in links:
        name = release_file(url)
        if name is not None:
            if name not in assets:
                findings.append(f"{page}: {url} names {name!r}, not an asset of the forge's nightly release")
            continue
        if url in checked:
            continue
        checked.add(url)
        status, _ = fetch(url, method="HEAD")
        # Some forges refuse HEAD on pages they serve to GET.
        if status in (405, 501):
            status, _ = fetch(url)
        finding, note = judge(page, url, status)
        if note:
            print(f"site-check: {note}")
        if finding:
            findings.append(finding)
    return findings


def judge(page: str, url: str, status: int, pending: dict = PENDING_FIRST_PUBLISH) -> tuple:
    """(finding or None, note or None) for an external link's HTTP status."""
    if url in pending:
        if status == 404:
            return None, (f"{page}: {url} answered 404, not published yet and not counted as a "
                          f"failure: {pending[url]}")
        if status < 400:
            return None, f"{page}: {url} answered {status}; it is published, drop it from PENDING_FIRST_PUBLISH"
    if status >= 400:
        return f"{page}: {url} answered {status}", None
    return None, None


def selftest() -> int:
    collect = (
        "collect_deb leviculum amd64\n"
        "collect_deb leviculum-heapacct arm64 leviculum-nightly-arm64-heapacct.deb\n"
        "pack_bin_tarball lnomad arm64 aarch64-unknown-linux-musl lnomad/README.md lnomad\n"
        'cp "$lnflash_src" "$DIST/lnflash-nightly-amd64.tar.gz"\n'
    )
    produced = nightly_names(collect)
    expected = {
        "leviculum-nightly-amd64.deb",
        "leviculum-nightly-arm64-heapacct.deb",
        "lnomad-nightly-arm64.tar.gz",
        "lnflash-nightly-amd64.tar.gz",
    }
    expected |= {f"{n}.sha256" for n in expected}
    ok = produced == expected
    if not ok:
        print(f"selftest: names parsed {sorted(produced)}, expected {sorted(expected)}")
    pages = {
        "good.md": f"[a]({FORGE_PREFIX}leviculum-nightly-amd64.deb) [b]({SITE_PREFIX}lnomad-nightly-arm64.tar.gz.sha256) [c](https://example.org/x)",
        "bad.md": f"[a]({SITE_PREFIX}lnsd-nightly-amd64.deb)",
    }
    findings = offline_findings(page_links(pages), produced, set())
    if len(findings) != 1 or "bad.md" not in findings[0] or "lnsd-nightly-amd64.deb" not in findings[0]:
        print(f"selftest: expected exactly the bad.md finding, got {findings}")
        ok = False
    readme = (
        f"wget {SITE_PREFIX}leviculum-nightly-amd64.deb\n"
        f"# mirror: {FORGE_PREFIX}lnflash-nightly-amd64.tar.gz\n"
        f"wget {SITE_PREFIX}lnsd-nightly-amd64.deb\n"
        f"The [directory]({SITE_PREFIX}). See the [guide]({BOOK_PREFIX}guide/installation.html), "
        f"<{BOOK_PREFIX}guide/gone.html#top>, <https://example.org/x>.\n"
        "Serves on `http://127.0.0.1:8180/` and at https://blog.example.com/.\n"
    )
    links = readme_links({"README.md": readme})
    want_links = [
        f"{SITE_PREFIX}leviculum-nightly-amd64.deb",
        f"{FORGE_PREFIX}lnflash-nightly-amd64.tar.gz",
        f"{SITE_PREFIX}lnsd-nightly-amd64.deb",
        SITE_PREFIX,
        f"{BOOK_PREFIX}guide/installation.html",
        f"{BOOK_PREFIX}guide/gone.html#top",
    ]
    if [url for _, url in links] != want_links:
        print(f"selftest: README links parsed {links}, expected {want_links}")
        ok = False
    findings = offline_findings(links, produced, {"guide/installation.md"})
    if (len(findings) != 2 or "lnsd-nightly-amd64.deb" not in findings[0]
            or "docs/src/guide/gone.md" not in findings[1]):
        print(f"selftest: expected the lnsd and gone.md README findings, got {findings}")
        ok = False
    docs = "https://leviculum.network/docs/"
    pending = {docs: "a test reason"}
    cases = [
        (docs, 404, False, True),
        (docs, 500, True, False),
        (docs, 200, False, True),
        ("https://example.org/x", 404, True, False),
        ("https://example.org/x", 200, False, False),
    ]
    for url, status, want_finding, want_note in cases:
        finding, note = judge("p.md", url, status, pending)
        if bool(finding) != want_finding or bool(note) != want_note:
            print(f"selftest: {url} {status} gave finding={finding!r} note={note!r}")
            ok = False
    print("site-check selftest:", "ok" if ok else "FAILED")
    return 0 if ok else 1


def main() -> int:
    if sys.argv[1:] == ["--selftest"]:
        return selftest()
    if sys.argv[1:]:
        print(__doc__)
        return 2
    pages = {p.name: p.read_text(encoding="utf-8") for p in sorted(PAGES.glob("*.md"))}
    if not pages:
        print(f"site-check: no pages under {PAGES}")
        return 1
    readmes = {str(p.relative_to(ROOT)): p.read_text(encoding="utf-8") for p in READMES}
    links = page_links(pages) + readme_links(readmes)
    produced = nightly_names(COLLECT.read_text(encoding="utf-8"))
    book_pages = {str(p.relative_to(BOOK_SRC)) for p in BOOK_SRC.rglob("*.md")}
    findings = offline_findings(links, produced, book_pages)
    print(f"site-check: {len(links)} external link(s) in {len(pages)} page(s) "
          f"and {len(readmes)} README(s); "
          f"{len(produced)} nightly file name(s) from {COLLECT.relative_to(ROOT)}")
    try:
        findings += online_findings(links)
        online = "checked"
    except Offline as e:
        online = None
        print(f"site-check: SKIPPED the online check, the forge is not reachable ({e})")
    for finding in findings:
        print(f"site-check: {finding}")
    if findings:
        print(f"site-check: FAILED, {len(findings)} finding(s)")
        return 1
    print(f"site-check: ok (offline names checked, online links {online or 'skipped'})")
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Check the external links of the leviculum.network pages (site/pages/*.md).

The lblogd test `site_content` covers everything that can be decided from the
tree: the pages load, render on both sides, and their site links land. What it
cannot decide is whether a URL pointing off the server still answers, and the
download page is almost nothing but such URLs. This script is that half.

Two checks, in this order:

1. Offline, always. Every link into the nightly release, on the forge or on
   leviculum.network/releases/nightly/latest/, must name a file the nightly
   actually produces. The names come from scripts/collect-nightly-debs.sh,
   which stages them under dist/ and is the one place they are decided; a
   page that links `lnsd-nightly-amd64.deb` fails here without a network.

2. Online, when the network is there. The forge release's asset list is
   fetched once, and every nightly link must name an asset in it; every other
   external link must answer below 400. Links under
   leviculum.network/releases/ are checked by name only, against the same
   asset list: the site target mirrors the forge's names, and until it is live
   (see scripts/publish-site.sh) fetching them would only report that.

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
COLLECT = ROOT / "scripts" / "collect-nightly-debs.sh"

FORGE_PREFIX = "https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/"
SITE_PREFIX = "https://leviculum.network/releases/nightly/latest/"
RELEASE_API = "https://codeberg.org/api/v1/repos/Lew_Palm/leviculum/releases/tags/nightly"
TIMEOUT = 20

LINK = re.compile(r"\]\((https?://[^)\s]+)\)")


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


def release_file(url: str):
    """The file name a nightly-release URL names, or None for any other URL."""
    for prefix in (FORGE_PREFIX, SITE_PREFIX):
        if url.startswith(prefix):
            return url[len(prefix):]
    return None


def offline_findings(links: list, produced: set) -> list:
    findings = []
    for page, url in links:
        name = release_file(url)
        if name is not None and name not in produced:
            findings.append(
                f"{page}: {url} names {name!r}, which collect-nightly-debs.sh does not produce"
            )
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
        if status >= 400:
            findings.append(f"{page}: {url} answered {status}")
    return findings


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
    findings = offline_findings(page_links(pages), produced)
    if len(findings) != 1 or "bad.md" not in findings[0] or "lnsd-nightly-amd64.deb" not in findings[0]:
        print(f"selftest: expected exactly the bad.md finding, got {findings}")
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
    links = page_links(pages)
    produced = nightly_names(COLLECT.read_text(encoding="utf-8"))
    findings = offline_findings(links, produced)
    print(f"site-check: {len(links)} external link(s) in {len(pages)} page(s); "
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

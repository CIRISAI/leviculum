#!/usr/bin/env python3
"""Bake the travel facts of every country and territory, emergency numbers,
driving side and currency, from Wikidata into the table leviculum-map reads
(leviculum-map/src/countries.rs, provenance in leviculum-map/fixtures/README.md).

Wikidata's structured data is CC0 1.0
(https://www.wikidata.org/wiki/Wikidata:Licensing). One SPARQL query over
every item with an ISO 3166-1 alpha-2 code (P297) that is not dissolved
(P576) asks for its English label, its emergency phone numbers (P2852), its
driving side (P1622) and its currencies (P38, with the currency's ISO 4217
code P498), each statement with its rank and qualifiers. `--fetch` runs it
against https://query.wikidata.org/sparql and saves the raw response, so a
re-bake reads the saved file and needs no network.

Rules, all of them on the data, none invented:

- A statement of deprecated rank, or with an end time (P582), is dropped.
  Where a property has a statement of preferred rank, only those count.
- An emergency number without a use (P366) is a general one; with uses it
  is kept for police, ambulance and fire (and the few national names for
  those services), and a use of "emergency" counts as general. A number
  with only other uses (coast guard, roadside assistance, poison centre
  ...) or for some callers only (P1535, teleprinter users) is dropped.
  Order: general numbers first, preferred rank first, a number without
  qualifier before one qualified "emergency", a national number before the
  GSM-wide 112, then shorter and smaller; then police, ambulance, fire.
- A driving side is left out where the statements still disagree.
- A currency counts only with exactly one ISO 4217 code (the old
  Zimbabwean dollar carries four and none of them is current). Order:
  preferred rank first, one for the whole country before one that applies
  to a part (P518, the CFP franc of France's Pacific territories), then
  the currency more codes of the table hold, then the code.
- Where two items carry one code (Cyprus the state and the island), the one
  with the most facts wins, then the lower item number.
- Where Wikidata has no value the field stays empty.

Output, UTF-8 text, one row per line, sorted by code:

    LCF1 <row count>
    <code>\t<English name>\t<numbers>\t<side>\t<currencies>

numbers: space separated, each the digits and, for a service number, a
colon and the letters of its services, p police, a ambulance, f fire
("112 110:p"); side: L, R or empty; currencies: ISO 4217 codes, space
separated, the main one first. Baking the same response again gives the
same bytes.

Usage: scripts/bake-country-facts.py [--fetch] RESPONSE.json OUT.lcf
"""

import json
import sys
import urllib.parse
import urllib.request

ENDPOINT = "https://query.wikidata.org/sparql"

QUERY = """SELECT ?code ?item ?kind ?st ?v ?vLabel ?rank ?iso ?irank ?iend ?own ?qp ?q ?qLabel WHERE {
  ?item wdt:P297 ?code .
  FILTER NOT EXISTS { ?item wdt:P576 ?dissolved . }
  {
    ?item rdfs:label ?vLabel . FILTER(LANG(?vLabel) = "en")
    BIND("name" AS ?kind)
  } UNION {
    ?item p:P2852 ?st . ?st ps:P2852 ?v ; wikibase:rank ?rank .
    OPTIONAL { ?st ?pq ?q . ?qp wikibase:qualifier ?pq . }
    BIND("emergency" AS ?kind)
  } UNION {
    ?item p:P1622 ?st . ?st ps:P1622 ?v ; wikibase:rank ?rank .
    OPTIONAL { ?st ?pq ?q . ?qp wikibase:qualifier ?pq . }
    BIND("driving" AS ?kind)
  } UNION {
    ?item p:P38 ?st . ?st ps:P38 ?v ; wikibase:rank ?rank .
    OPTIONAL { ?st ?pq ?q . ?qp wikibase:qualifier ?pq . }
    OPTIONAL {
      ?v p:P498 ?ist . ?ist ps:P498 ?iso ; wikibase:rank ?irank .
      OPTIONAL { ?ist pq:P582 ?iend . }
    }
    BIND(EXISTS { ?v wdt:P17 ?item . } AS ?own)
    BIND("currency" AS ?kind)
  }
  SERVICE wikibase:label { bd:serviceParam wikibase:language "en". }
}
ORDER BY ?code ?kind ?st ?qp ?q ?iso ?irank ?iend
"""

# Uses (P366) of an emergency number, by item: general, or the service.
GENERAL = {"Q5070802", "Q814610"}  # emergency, emergency service
SERVICE = {
    "Q35535": "p",  # police
    "Q204310": "p",  # gendarmerie
    "Q594444": "p",  # Israel Police
    "Q2317752": "p",  # Carabineros de Chile
    "Q7374210": "p",  # Royal Gendarmerie
    "Q860447": "a",  # emergency medical services
    "Q577902": "a",  # Magen David Adom
    "Q6498663": "f",  # fire department
    "Q107711": "f",  # firefighter
    "Q1409931": "f",  # Israel Fire and Rescue Services
}
GSM = "112"
LEFT = "Q11920728"
RIGHT = "Q14565199"
PREFERRED = "http://wikiba.se/ontology#PreferredRank"
DEPRECATED = "http://wikiba.se/ontology#DeprecatedRank"


def fetch(path):
    url = ENDPOINT + "?" + urllib.parse.urlencode({"query": QUERY})
    req = urllib.request.Request(
        url,
        headers={
            "Accept": "application/sparql-results+json",
            "User-Agent": "leviculum-bake/1.0 (https://leviculum.network)",
        },
    )
    with urllib.request.urlopen(req, timeout=120) as r:
        body = r.read()
    with open(path, "wb") as f:
        f.write(body)


def tail(uri):
    return uri.rsplit("/", 1)[-1]


def live(statements):
    """Statements that count: not deprecated, not ended, preferred if any."""
    out = [s for s in statements.values() if s["rank"] != DEPRECATED and "P582" not in s["q"]]
    if any(s["rank"] == PREFERRED for s in out):
        out = [s for s in out if s["rank"] == PREFERRED]
    return out


def gather(rows):
    """code -> item -> {name, emergency, driving, currency}, each statement
    keyed by its node with its value, rank, qualifiers and ISO codes."""
    items = {}
    for r in rows:
        def g(k):
            return r.get(k, {}).get("value", "")

        code, item, kind = g("code"), tail(g("item")), g("kind")
        if len(code) != 2 or not code.isascii() or not code.isupper():
            continue
        it = items.setdefault(code, {}).setdefault(
            item, {"name": "", "emergency": {}, "driving": {}, "currency": {}}
        )
        if kind == "name":
            it["name"] = g("vLabel")
            continue
        st = it[kind].setdefault(
            g("st"),
            {"v": tail(g("v")), "label": g("vLabel"), "rank": g("rank"), "q": {}, "iso": set(),
             "own": g("own") == "true"},
        )
        if g("qp"):
            st["q"].setdefault(tail(g("qp")), set()).add(tail(g("q")))
        if g("iso"):
            st["iso"].add((g("iso"), g("irank"), bool(g("iend"))))
    return items


def numbers(it):
    found = {}
    for s in live(it["emergency"]):
        digits = s["label"].strip()
        if not digits.isascii() or not digits.isdigit() or "P1535" in s["q"]:
            continue
        uses = s["q"].get("P366", set())
        if not uses or uses & GENERAL:
            role = ""
        else:
            role = "".join(c for c in "paf" if c in {SERVICE.get(u, "") for u in uses})
            if not role:
                continue
        # Sort key of a general number: preferred, unqualified, national.
        key = (s["rank"] != PREFERRED, bool(uses), digits == GSM)
        old = found.get(digits)
        if old is not None:
            # Seen twice: general wins, else the services join.
            r0, k0 = old
            role = "" if "" in (r0, role) else "".join(c for c in "paf" if c in r0 + role)
            key = min(key, k0)
        found[digits] = (role, key)

    def order(kv):
        digits, (role, key) = kv
        if role:
            return (1, "paf".index(role[0]), len(digits), digits)
        return (0, key, len(digits), digits)

    return " ".join(d + (":" + r if r else "") for d, (r, _) in sorted(found.items(), key=order))


def side(it):
    sides = {s["v"] for s in live(it["driving"])}
    return {frozenset([LEFT]): "L", frozenset([RIGHT]): "R"}.get(frozenset(sides), "")


def iso(statement):
    """The currency's one current alphabetic ISO 4217 code, or None."""
    codes = [
        (c, rank)
        for c, rank, ended in statement["iso"]
        if len(c) == 3 and c.isascii() and c.isalpha() and c.isupper()
        and rank != DEPRECATED and not ended
    ]
    if any(r == PREFERRED for _, r in codes):
        codes = [(c, r) for c, r in codes if r == PREFERRED]
    codes = {c for c, _ in codes}
    return next(iter(codes)) if len(codes) == 1 else None


def current(statements):
    """Currency statements that count: live ones with a code, preferred if
    any; a currency without an ISO code (the Manx pound) does not hide the
    one beside it that has one."""
    out = [
        s for s in statements.values()
        if s["rank"] != DEPRECATED and "P582" not in s["q"] and iso(s) is not None
    ]
    if any(s["rank"] == PREFERRED for s in out):
        out = [s for s in out if s["rank"] == PREFERRED]
    return out


def currencies(it, held):
    best = {}
    for s in current(it["currency"]):
        code = iso(s)
        if code is None:
            continue
        key = (s["rank"] != PREFERRED, "P518" in s["q"], not s["own"])
        best[code] = min(key, best.get(code, key))
    return sorted(best, key=lambda c: (best[c], -held.get(c, 0), c))


def main():
    args = sys.argv[1:]
    if args and args[0] == "--fetch":
        args = args[1:]
        fetch(args[0])
    src, dst = args
    with open(src, encoding="utf-8") as f:
        rows = json.load(f)["results"]["bindings"]
    items = gather(rows)

    chosen = {}
    for code, cands in items.items():
        def weight(item):
            it = cands[item]
            n = sum(len(it[k]) for k in ("emergency", "driving", "currency"))
            return (-n, int(item[1:]) if item[1:].isdigit() else 0)

        chosen[code] = cands[min(cands, key=weight)]

    # How many codes hold each currency, for the order among equals.
    held = {}
    for it in chosen.values():
        for s in current(it["currency"]):
            code = iso(s)
            if code is not None:
                held[code] = held.get(code, 0) + 1

    out = []
    gaps = {"numbers": [], "side": [], "currency": []}
    for code in sorted(chosen):
        it = chosen[code]
        nums, drive, cur = numbers(it), side(it), currencies(it, held)
        name = " ".join(it["name"].split()) or code
        if not nums:
            gaps["numbers"].append(code)
        if not drive:
            gaps["side"].append(code)
        if not cur:
            gaps["currency"].append(code)
        out.append("\t".join([code, name, nums, drive, " ".join(cur)]))

    data = (f"LCF1 {len(out)}\n" + "".join(line + "\n" for line in out)).encode("utf-8")
    with open(dst, "wb") as f:
        f.write(data)
    print(f"FACTS rows={len(out)} bytes={len(data)}")
    for k, v in gaps.items():
        print(f"GAP {k} {len(v)}: {' '.join(v)}")


if __name__ == "__main__":
    main()

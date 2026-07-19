#!/usr/bin/env python3
"""Render the Leviculum benchmark page from the measured ``bench_results.json``.

Mirrors CIRISServer's publish-only bench discipline: every number here is
measured on a CI runner (the ``transport_fanout_bench`` fan-out sweep), never
modeled or extrapolated. Shared runners are noisy, so this is a *published
trend*, not a pass/fail gate.

    python3 scripts/bench_report.py --bench-results bench_results.json \
        --out bench-site --commit "$GITHUB_SHA" --date "$BENCH_DATE"

Emits ``<out>/index.html`` and copies ``bench_results.json`` into ``<out>/`` so
the raw data is downloadable straight from the page.
"""

import argparse
import html
import json
import os
import shutil


def H(s) -> str:
    return html.escape(str(s))


def fmt(n, places=0) -> str:
    if n is None:
        return "—"
    if places == 0:
        return f"{round(n):,}"
    return f"{n:,.{places}f}"


CSS = """
:root{--bg:#0b0e14;--panel:#121724;--ink:#e8edf6;--dim:#9aa7bd;--line:#222a3a;
--accent:#6ea8ff;--good:#36d399;--warn:#f5c451;--bar:#6ea8ff;--ceil:#f5716f}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--ink);
font:16px/1.6 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Helvetica,Arial,sans-serif}
.wrap{max-width:920px;margin:0 auto;padding:0 20px}
a{color:var(--accent)}
.hero{padding:60px 0 32px;border-bottom:1px solid var(--line)}
.kicker{color:var(--accent);font-weight:600;letter-spacing:.08em;text-transform:uppercase;font-size:13px;margin:0 0 14px}
.hero h1{font-size:38px;line-height:1.15;margin:0 0 16px;letter-spacing:-.02em}
.hero .sub{color:var(--dim);font-size:18px;max-width:700px;margin:0 0 34px}
.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(190px,1fr));gap:16px}
.card{background:var(--panel);border:1px solid var(--line);border-radius:14px;padding:20px}
.card .big{font-size:34px;font-weight:800;letter-spacing:-.02em;color:#fff;line-height:1}
.card .lbl{color:var(--accent);font-weight:600;font-size:12px;text-transform:uppercase;letter-spacing:.05em;margin:6px 0 8px}
.card .cap{color:var(--dim);font-size:13.5px}
section{padding:36px 0;border-bottom:1px solid var(--line)}
h2{font-size:23px;margin:0 0 8px;letter-spacing:-.01em}
.note{color:var(--dim);max-width:760px;margin:0 0 20px}
table.data{width:100%;border-collapse:collapse;font-size:15px}
table.data th{text-align:left;color:var(--dim);font-weight:600;font-size:12px;text-transform:uppercase;
letter-spacing:.05em;padding:8px 12px;border-bottom:1px solid var(--line)}
table.data td{padding:11px 12px;border-bottom:1px solid var(--line);vertical-align:top}
td.num{text-align:right;font-variant-numeric:tabular-nums;white-space:nowrap;font-weight:600}
td.num.good{color:var(--good)}
.dim{color:var(--dim);font-weight:400}
code{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:12.5px;color:#bcd}
.chart{background:var(--panel);border:1px solid var(--line);border-radius:14px;padding:20px;overflow-x:auto}
.foot{padding:28px 0 60px;color:var(--dim);font-size:13.5px}
.pill{display:inline-block;font-size:11px;font-weight:700;text-transform:uppercase;letter-spacing:.06em;
color:#9af5cf;background:#1c5b3a;border-radius:6px;padding:3px 8px;margin-left:8px}
"""


def render_chart(sweep, ceiling):
    """Inline SVG: throughput bars per N, with a dashed ceiling line."""
    if not sweep:
        return ""
    W, Hgt = 820, 300
    padL, padR, padB, padT = 56, 16, 44, 20
    plot_w = W - padL - padR
    plot_h = Hgt - padT - padB
    top = max(ceiling, max(r["throughput_pkts_s"] for r in sweep)) * 1.12 or 1
    n = len(sweep)
    slot = plot_w / n
    bw = min(64, slot * 0.6)

    parts = [f'<svg viewBox="0 0 {W} {Hgt}" width="100%" role="img" '
             f'aria-label="throughput vs active links">']
    # y gridlines + labels
    for i in range(5):
        yv = top * i / 4
        y = padT + plot_h - (yv / top) * plot_h
        parts.append(f'<line x1="{padL}" y1="{y:.1f}" x2="{W-padR}" y2="{y:.1f}" '
                     f'stroke="#222a3a" stroke-width="1"/>')
        parts.append(f'<text x="{padL-8}" y="{y+4:.1f}" fill="#9aa7bd" font-size="11" '
                     f'text-anchor="end">{fmt(yv)}</text>')
    # ceiling line
    yc = padT + plot_h - (ceiling / top) * plot_h
    parts.append(f'<line x1="{padL}" y1="{yc:.1f}" x2="{W-padR}" y2="{yc:.1f}" '
                 f'stroke="#f5716f" stroke-width="1.5" stroke-dasharray="5 4"/>')
    parts.append(f'<text x="{W-padR}" y="{yc-6:.1f}" fill="#f5716f" font-size="11" '
                 f'text-anchor="end">ceiling ≈ {fmt(ceiling)} pkts/s</text>')
    # bars
    for i, r in enumerate(sweep):
        cx = padL + slot * (i + 0.5)
        h = (r["throughput_pkts_s"] / top) * plot_h
        y = padT + plot_h - h
        parts.append(f'<rect x="{cx-bw/2:.1f}" y="{y:.1f}" width="{bw:.1f}" height="{h:.1f}" '
                     f'rx="3" fill="#6ea8ff"/>')
        parts.append(f'<text x="{cx:.1f}" y="{y-6:.1f}" fill="#e8edf6" font-size="11" '
                     f'text-anchor="middle" font-weight="600">{fmt(r["throughput_pkts_s"])}</text>')
        parts.append(f'<text x="{cx:.1f}" y="{Hgt-padB+18:.1f}" fill="#9aa7bd" font-size="12" '
                     f'text-anchor="middle">N={r["n"]}</text>')
    parts.append(f'<text x="{padL}" y="{Hgt-6}" fill="#9aa7bd" font-size="11">active links (N)</text>')
    parts.append('</svg>')
    return '<div class="chart">' + "".join(parts) + '</div>'


def render(data) -> str:
    sweep = sorted(data.get("sweep", []), key=lambda r: r["n"])
    params = data.get("params", {})
    # The ceiling is the plateau: the best sustained throughput, which the sweep
    # approaches and then flattens against.
    ceiling = max((r["throughput_pkts_s"] for r in sweep), default=0)
    peak = next((r for r in sweep if r["throughput_pkts_s"] == ceiling), None)
    base = sweep[0] if sweep else None
    # Scaling efficiency at the top of the sweep: if throughput scaled linearly
    # with N it would keep climbing; the plateau ratio shows it does not.
    tail = sweep[-1] if sweep else None
    scales = (tail["throughput_pkts_s"] / ceiling) if (tail and ceiling) else 0

    rows = ""
    for r in sweep:
        good = "good" if r["throughput_pkts_s"] >= 0.9 * ceiling else ""
        rows += (
            f'<tr><td class="num">{r["n"]}</td>'
            f'<td class="num dim">{r["established"]}</td>'
            f'<td class="num dim">{fmt(r["elapsed_s"],2)}</td>'
            f'<td class="num {good}">{fmt(r["throughput_pkts_s"])}</td></tr>'
        )

    return f"""<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Leviculum transport — measured concurrency ceiling</title>
<style>{CSS}</style>
</head><body>
<div class="wrap">
  <div class="hero">
    <p class="kicker">Leviculum · measured performance</p>
    <h1>Transport concurrency ceiling <span class="pill">measured</span></h1>
    <p class="sub">One serve node, N client links, each pumping link messages. The serve
    node decrypts and routes all of it in a single event-loop task behind one
    <code>Mutex&lt;StdNodeCore&gt;</code>, so aggregate throughput <b>plateaus</b> rather
    than scaling with N. This is the ceiling <a href="https://github.com/CIRISAI/leviculum/issues/29">leviculum#29</a>
    tracks — measured with the native <code>transport_fanout_bench</code>, no downstream stack.</p>
    <div class="cards">
      <div class="card"><div class="lbl">Ceiling</div><div class="big">{fmt(ceiling)}</div><div class="cap">link msgs / sec, sustained — the plateau the single event loop tops out at.</div></div>
      <div class="card"><div class="lbl">Plateau at</div><div class="big">N={peak["n"] if peak else "—"}</div><div class="cap">more active links past this add ~no throughput.</div></div>
      <div class="card"><div class="lbl">Top vs peak</div><div class="big">{fmt(scales*100)}%</div><div class="cap">throughput at N={tail["n"] if tail else "—"} relative to the peak — flat, not climbing.</div></div>
    </div>
  </div>

  <section>
    <h2>Throughput vs active links</h2>
    <p class="note">If the node scaled with peer count, the bars would keep climbing. They
    flatten against the dashed ceiling instead — the signature of a serialization wall,
    not a per-peer latency cost.</p>
    {render_chart(sweep, ceiling)}
  </section>

  <section>
    <h2>Sweep</h2>
    <p class="note">Load: {fmt(params.get("packets_per_client"))} link messages/client,
    {fmt(params.get("payload_bytes"))} B payload. Link data is the cheapest packet class
    (HMAC + AES-CBC); single-destination ECDH decrypt and bulk <code>send_resource</code>
    sit under the same lock and plateau lower.</p>
    <table class="data">
      <thead><tr><th>N (links)</th><th>established</th><th>elapsed (s)</th><th>throughput (pkts/s)</th></tr></thead>
      <tbody>{rows}</tbody>
    </table>
  </section>

  <div class="foot">
    <p>Measured on <code>{H(data.get("runner","?"))}</code> at commit
    <code>{H(str(data.get("commit","?"))[:12])}</code> on {H(data.get("date","?"))}.
    Publish-only trend (shared CI runners are noisy) — not a pass/fail gate.
    Raw data: <a href="bench_results.json">bench_results.json</a>.</p>
  </div>
</div>
</body></html>
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bench-results", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--commit", default=None)
    ap.add_argument("--date", default=None)
    args = ap.parse_args()

    with open(args.bench_results) as f:
        data = json.load(f)
    if args.commit:
        data["commit"] = args.commit
    if args.date:
        data["date"] = args.date

    os.makedirs(args.out, exist_ok=True)
    with open(os.path.join(args.out, "index.html"), "w") as f:
        f.write(render(data))
    shutil.copyfile(args.bench_results, os.path.join(args.out, "bench_results.json"))
    print(f"wrote {args.out}/index.html + bench_results.json")


if __name__ == "__main__":
    main()

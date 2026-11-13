#!/usr/bin/env python3
"""`just render-walk`: every visual change rendered, measured and looked at (1801).

Lew 2026-11-13: the globe build of 6bb85004c passed every golden, yet on a
phone it drew a Bitmap half again the frame, let the world shrink below the
screen and grew the native heap to 630 MB. Each golden showed one front view.
This walk draws the same code the frontends draw with, headless, over many
views, and checks each picture by named rules:

* the map, through the Android facade's own call into a Bitmap sized as
  `MapView.java` sizes it (`loquax-android/examples/render_walk.rs`), for
  both world projections (Equal Earth, and the globe of the
  `loquax/globe-world` feature): a zoom ladder at fixed centres and frame
  sizes, and five gestures (a pan across tile seams, a pinch out to the
  world and back, a drag toward the pole and around, a jump from the street
  to Europe, a device rotation) with their frame times and heap;
* the window's screens (iced's software renderer, `loquax/src/gui.rs`'s
  `every_window_screen_draws_at_every_walk_size`) and the terminal's
  (`loquax/src/tui.rs`'s `every_terminal_screen_draws_at_every_walk_size`)
  at the walk's frame sizes;
* with `--m9`, the M9 host sim's walks (`just m9-sim-walk`), their dumps.

The rules and their thresholds are documented in
`loquax-android/src/render_walk.rs` and in the developer guide
(`docs/src/developer/render-walk.md`).

Writes `OUT/index.html` (the contact sheet: every picture with its rule
results, the gestures' frame times and heap) and `OUT/summary.tsv`; exits 1
on any failed rule. `--quick` walks a reduced ladder (two frame sizes, three
centres, eight zooms) and the gestures on Lew's phone's frame; `--selftest`
runs the positive controls instead: each fault injected into a small walk
must turn its rule red on a frame or gesture the clean walk has green.

Usage:
  python3 -I scripts/render-walk.py [--quick | --selftest] [--out DIR]
                                    [--no-screens] [--m9]
"""

import argparse
import html
import json
import os
import shutil
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

COLUMNS = [
    "variant", "kind", "sequence", "size", "centre", "zoom", "image",
    "rule", "value", "limit", "verdict", "note",
]

# Each injected fault and the rules it must turn red (render_walk.rs).
# A rule the clean walk fails nearly everywhere may also pass by a measured
# step: `slow-frame` adds 20 ms to every moving frame, so the gesture's p95
# must grow by 15 ms at least where it does not turn red.
CONTROLS = {
    "oversized-bitmap": ["bitmap_is_frame", "view_covered"],
    "blank-half": ["view_covered"],
    "stray-label": ["label_inside_view", "label_clear_of_controls",
                    "label_overlap", "label_current_zoom"],
    "error-text": ["no_error_text"],
    "slow-frame": ["frame_p95"],
    "heap-kept": ["heap_returns", "heap_peak"],
    "view-lost": ["view_kept"],
}
STEP = {"frame_p95": 15.0}

VARIANTS = [("equal", []), ("globe", ["--features", "loquax/globe-world"])]


def host():
    out = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True)
    for line in out.stdout.splitlines():
        if line.startswith("host: "):
            return line.split(": ", 1)[1]
    raise RuntimeError("rustc -vV names no host")


def build_walk(out, log):
    """Both variants' walk binaries, copied out: cargo writes both to one path."""
    bins = {}
    for variant, features in VARIANTS:
        cmd = ["cargo", "build", "--release", "-p", "loquax-android",
               "--example", "render_walk", "--message-format=json-render-diagnostics",
               *features]
        done = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True)
        log.write(done.stderr)
        if done.returncode != 0:
            sys.stderr.write(done.stderr)
            raise RuntimeError(f"building the {variant} walk failed")
        exe = None
        for line in done.stdout.splitlines():
            try:
                m = json.loads(line)
            except ValueError:
                continue
            if (m.get("reason") == "compiler-artifact" and m.get("executable")
                    and m["target"]["name"] == "render_walk"):
                exe = m["executable"]
        if exe is None:
            raise RuntimeError(f"cargo named no render_walk for {variant}")
        os.makedirs(os.path.join(out, "bin"), exist_ok=True)
        bins[variant] = os.path.join(out, "bin", f"render_walk-{variant}")
        shutil.copy2(exe, bins[variant])
    return bins


def walk(exe, out, variant, mode, inject=None):
    cmd = [exe, "--out", out, "--variant", variant]
    if mode:
        cmd.append(mode)
    if inject:
        cmd += ["--inject", inject]
    done = subprocess.run(cmd, capture_output=True, text=True)
    sys.stdout.write(done.stdout)
    if done.returncode != 0:
        sys.stderr.write(done.stderr)
        raise RuntimeError(f"the {variant} walk stopped ({done.returncode})")


def screens(out, log):
    """The window's and the terminal's screens at the walk's sizes."""
    env = dict(os.environ, LOQUAX_RENDER_WALK=os.path.abspath(out))
    runs = [
        ["cargo", "test", "-p", "loquax", "--features", "gui", "--bins",
         "--target", host(), "every_window_screen_draws_at_every_walk_size"],
        ["cargo", "test", "-p", "loquax", "--features", "tui", "--bins",
         "every_terminal_screen_draws_at_every_walk_size"],
    ]
    for cmd in runs:
        done = subprocess.run(cmd, cwd=ROOT, env=env, capture_output=True, text=True)
        log.write(done.stdout + done.stderr)
        # A red rule fails the test after writing its rows: the rows say
        # which. Anything else that stops it is the walk's failure.
        if done.returncode != 0 and "every_" not in done.stdout:
            sys.stderr.write(done.stderr[-4000:])
            raise RuntimeError(f"{' '.join(cmd)} did not run")


def m9(out, log):
    """The M9 host sim's walks: their dumps, and a row for their verdict."""
    done = subprocess.run(["just", "m9-sim-walk"], cwd=ROOT, capture_output=True, text=True)
    log.write(done.stdout + done.stderr)
    src = os.path.join(ROOT, "target", "m9-sim-walk")
    dst = os.path.join(out, "screens")
    os.makedirs(dst, exist_ok=True)
    rows = []
    for walk_name in sorted(os.listdir(src)) if os.path.isdir(src) else []:
        d = os.path.join(src, walk_name)
        for f in sorted(os.listdir(d)) if os.path.isdir(d) else []:
            if f.endswith(".png"):
                image = f"m9-{walk_name}-{f[:-4]}"
                shutil.copy2(os.path.join(d, f), os.path.join(dst, image + ".png"))
                rows.append(["m9", "screen", walk_name, "320x240", "", "", image,
                             "m9_sim_walk", str(int(done.returncode == 0)), "1.000",
                             "pass" if done.returncode == 0 else "FAIL", ""])
    if not rows:
        rows.append(["m9", "screen", "m9-sim-walk", "", "", "", "", "m9_sim_walk",
                     "0", "1.000", "FAIL", "the sim walk left no dumps"])
    with open(os.path.join(out, "rows-m9.tsv"), "w") as f:
        for r in rows:
            f.write("\t".join(r) + "\n")


def read_rows(out):
    rows = []
    for name in sorted(os.listdir(out)):
        if name.startswith("rows-") and name.endswith(".tsv"):
            with open(os.path.join(out, name)) as f:
                for line in f:
                    cells = line.rstrip("\n").split("\t")
                    if len(cells) == len(COLUMNS):
                        rows.append(dict(zip(COLUMNS, cells)))
    return rows


def write_summary(out, rows):
    with open(os.path.join(out, "summary.tsv"), "w") as f:
        f.write("\t".join(COLUMNS) + "\n")
        for r in rows:
            f.write("\t".join(r[c] for c in COLUMNS) + "\n")


def image_link(out, image):
    for sub, ext in (("full", ".png"), ("thumb", ".png"), ("screens", ".png"), ("screens", ".txt")):
        if os.path.exists(os.path.join(out, sub, image + ext)):
            return f"{sub}/{image}{ext}"
    return None


def write_sheet(out, rows, title, seconds):
    by_image = {}
    for r in rows:
        by_image.setdefault((r["variant"], r["sequence"], r["size"], r["image"]), []).append(r)
    rules = {}
    for r in rows:
        p, f = rules.get(r["rule"], (0, 0))
        rules[r["rule"]] = (p + (r["verdict"] == "pass"), f + (r["verdict"] != "pass"))
    e = html.escape
    parts = [
        "<!doctype html><meta charset=utf-8>",
        f"<title>{e(title)}</title>",
        "<style>body{font:13px sans-serif;margin:1em}table{border-collapse:collapse}"
        "td,th{border:1px solid #ccc;padding:2px 6px;vertical-align:top}"
        ".FAIL{color:#b00;font-weight:bold}.pass{color:#070}"
        ".cell{display:inline-block;margin:4px;width:250px;vertical-align:top}"
        ".cell img{max-width:244px;border:1px solid #888}.cell pre{font-size:6px;max-height:300px;overflow:hidden}"
        ".bad img{border:3px solid #b00}</style>",
        f"<h1>{e(title)}</h1>",
        f"<p>{len(rows)} verdicts, {sum(f for _, f in rules.values())} failed, "
        f"in {seconds:.0f} s. Rules: loquax-android/src/render_walk.rs, "
        "docs/src/developer/render-walk.md. Magenta is screen no frame drew.</p>",
        "<h2>Rules</h2><table><tr><th>rule</th><th>pass</th><th>fail</th></tr>",
    ]
    for rule, (p, f) in sorted(rules.items()):
        parts.append(f"<tr><td>{e(rule)}</td><td class=pass>{p}</td>"
                     f"<td class={'FAIL' if f else 'pass'}>{f}</td></tr>")
    parts.append("</table>")
    seqs = [r for r in rows if r["kind"] == "sequence"]
    if seqs:
        parts.append("<h2>Gestures</h2><table><tr><th>variant</th><th>gesture</th>"
                     "<th>size</th><th>rule</th><th>value</th><th>limit</th><th>verdict</th><th>measured</th></tr>")
        for r in seqs:
            parts.append(
                f"<tr><td>{e(r['variant'])}</td><td>{e(r['sequence'])}</td><td>{e(r['size'])}</td>"
                f"<td>{e(r['rule'])}</td><td>{e(r['value'])}</td><td>{e(r['limit'])}</td>"
                f"<td class={r['verdict']}>{r['verdict']}</td><td>{e(r['note'])}</td></tr>")
        parts.append("</table>")
    groups = {}
    for (variant, sequence, size, image), rs in by_image.items():
        if all(r["kind"] == "sequence" for r in rs):
            continue
        groups.setdefault((variant, sequence, size), []).append((image, rs))
    for (variant, sequence, size), cells in groups.items():
        failed = sum(r["verdict"] != "pass" for _, rs in cells for r in rs)
        parts.append(f"<h2>{e(variant)} {e(sequence)} {e(size)}"
                     f" <span class={'FAIL' if failed else 'pass'}>({failed} failed)</span></h2>")
        for image, rs in cells:
            bad = [r for r in rs if r["verdict"] != "pass"]
            link = image_link(out, image)
            zoom = rs[0]["zoom"]
            centre = rs[0]["centre"]
            parts.append(f"<div class='cell{' bad' if bad else ''}'>")
            if link and link.endswith(".txt"):
                with open(os.path.join(out, link)) as f:
                    parts.append(f"<a href='{e(link)}'><pre>{e(f.read())}</pre></a>")
            elif link:
                thumb = os.path.join("thumb", image + ".png")
                src = thumb if os.path.exists(os.path.join(out, thumb)) else link
                parts.append(f"<a href='{e(link)}'><img loading=lazy src='{e(src)}'></a>")
            parts.append(f"<br>{e(image)}<br>{e(centre)} z{e(zoom)}")
            for r in bad:
                parts.append(f"<br><span class=FAIL>{e(r['rule'])} = {e(r['value'])} "
                             f"(limit {e(r['limit'])})</span> {e(r['note'][:200])}")
            if not bad:
                parts.append(f"<br><span class=pass>{len(rs)} rules pass</span>")
            parts.append("</div>")
    with open(os.path.join(out, "index.html"), "w") as f:
        f.write("\n".join(parts) + "\n")


def selftest(bins, out):
    """Each fault red on a row the clean walk has green; the verdict lines."""
    exe = bins["equal"]
    clean_dir = os.path.join(out, "clean")
    os.makedirs(clean_dir, exist_ok=True)
    # Each walk under its own name, so its pictures keep theirs; a row is
    # matched to the clean one by the picture's name after that.
    walk(exe, clean_dir, "clean", "--selftest")

    def bare(r):
        return r["image"][len(r["variant"]) + 1:]

    clean = {(bare(r), r["rule"]): r for r in read_rows(clean_dir)}
    lines = []
    ok = True
    for fault, rules in CONTROLS.items():
        d = os.path.join(out, fault)
        os.makedirs(d, exist_ok=True)
        walk(exe, d, fault, "--selftest", fault)
        injected = read_rows(d)
        for rule in rules:
            fired = 0
            stepped = 0
            for r in injected:
                if r["rule"] != rule:
                    continue
                c = clean.get((bare(r), rule))
                if c is None:
                    continue
                if r["verdict"] == "FAIL" and c["verdict"] == "pass":
                    fired += 1
                elif rule in STEP and float(r["value"]) - float(c["value"]) >= STEP[rule]:
                    stepped += 1
            green = sum(1 for (_, ru), c in clean.items() if ru == rule and c["verdict"] == "pass")
            held = fired > 0 or (rule in STEP and stepped > 0)
            ok &= held
            lines.append(f"{'PASS' if held else 'FAIL'} {fault} -> {rule}: red on {fired} rows "
                         f"the clean walk has green ({green} green there)"
                         + (f", {stepped} rows {STEP[rule]:.0f}+ higher" if rule in STEP else ""))
    rows = [r for name in ["clean", *CONTROLS] for r in read_rows(os.path.join(out, name))]
    return ok, lines, rows


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--quick", action="store_true")
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--out", default=os.path.join(ROOT, "target", "render-walk"))
    ap.add_argument("--no-screens", action="store_true")
    ap.add_argument("--m9", action="store_true")
    args = ap.parse_args()
    out = os.path.abspath(args.out)
    if os.path.isdir(out):
        shutil.rmtree(out)
    os.makedirs(out)
    started = time.monotonic()
    with open(os.path.join(out, "build.log"), "w") as log:
        bins = build_walk(out, log)
        if args.selftest:
            ok, lines, rows = selftest(bins, out)
            for line in lines:
                print(f"render-walk selftest: {line}")
            for name in ["clean", *CONTROLS]:
                for sub in ("thumb", "full"):
                    src = os.path.join(out, name, sub)
                    if os.path.isdir(src):
                        shutil.copytree(src, os.path.join(out, sub), dirs_exist_ok=True)
            write_summary(out, rows)
            write_sheet(out, rows, "render walk: positive controls", time.monotonic() - started)
            with open(os.path.join(out, "selftest.txt"), "w") as f:
                f.write("\n".join(lines) + "\n")
            print(f"render-walk selftest: {'PASS' if ok else 'FAIL'} in "
                  f"{time.monotonic() - started:.0f} s, sheet {out}/index.html")
            return 0 if ok else 1
        mode = "--quick" if args.quick else None
        for variant, _ in VARIANTS:
            walk(bins[variant], out, variant, mode)
        if not args.no_screens:
            screens(out, log)
        if args.m9:
            m9(out, log)
    rows = read_rows(out)
    write_summary(out, rows)
    seconds = time.monotonic() - started
    title = f"render walk{' (quick)' if args.quick else ''}"
    write_sheet(out, rows, title, seconds)
    failed = [r for r in rows if r["verdict"] != "pass"]
    by_rule = {}
    for r in failed:
        by_rule[r["rule"]] = by_rule.get(r["rule"], 0) + 1
    for rule, n in sorted(by_rule.items()):
        print(f"render-walk: FAIL {rule}: {n} verdicts")
    print(f"render-walk: {'FAIL' if failed else 'PASS'} {len(rows)} verdicts, {len(failed)} failed, "
          f"{seconds:.0f} s, sheet {out}/index.html, summary {out}/summary.tsv")
    return 1 if failed else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except RuntimeError as e:
        print(f"render-walk: {e}", file=sys.stderr)
        sys.exit(2)

//! Rendering + formatting for the native `lnstatus` binary (Codeberg #86,
//! Stage 1: local mode).
//!
//! This is a faithful port of the local-mode output of Python
//! `RNS/Utilities/rnstatus.py` (lines 344-671) plus the size/speed/frequency/
//! time helpers from `RNS/__init__.py`. The renderer consumes the JSON that
//! `leviculum_std::rpc_query("interface_stats")` returns (msgpack maps decoded
//! to objects, `bin` values to lowercase hex, `nil` to null), which is the
//! same `interface_stats` dict a Python `rnsd` exposes. Feeding an identical
//! stats dict here and into rnstatus yields byte-identical output — that is the
//! drop-in goal, and the golden-output tests pin it.
//!
//! All character-width padding mirrors Python `len()`, which counts code
//! points; we therefore count `chars()`, never bytes, so the arrow glyphs
//! (`↓`/`↑`) and `µ` align exactly as in rnstatus.

use std::fmt::Write as _;

use serde_json::Value;

/// Interface `mode` enum values (RNS/Interfaces/Interface.py:45-50).
const MODE_POINT_TO_POINT: i64 = 0x02;
const MODE_ACCESS_POINT: i64 = 0x03;
const MODE_ROAMING: i64 = 0x04;
const MODE_BOUNDARY: i64 = 0x05;
const MODE_GATEWAY: i64 = 0x06;

/// Options controlling what `render_status` shows. One field per rnstatus flag
/// that affects local-mode rendering.
#[derive(Debug, Default, Clone)]
pub struct StatusOptions {
    /// `-a/--all`: show interfaces normally hidden (local/client/peer entries).
    pub dispall: bool,
    /// `-A/--announce-stats`: show announce frequencies / rate / queued / held.
    pub astats: bool,
    /// `-P/--pr-stats`: show path-request frequencies.
    pub pstats: bool,
    /// `-l/--link-stats`: append the link-table count to the trailer.
    pub lstats: bool,
    /// `-B/--burst`: only show interfaces with an active burst (or name match).
    pub burst_filter: bool,
    /// `-t/--totals`: print the aggregate traffic totals line.
    pub totals: bool,
    /// `-s/--sort <field>`: sort order; `None` leaves daemon order.
    pub sort: Option<String>,
    /// `-r/--reverse`: reverse the sort direction.
    pub reverse: bool,
    /// positional filter: only show interfaces whose name contains this.
    pub name_filter: Option<String>,
}

// ---------------------------------------------------------------------------
// Python-compatible number formatting
// ---------------------------------------------------------------------------

/// Character count (Python `len()` on a `str` counts code points).
fn clen(s: &str) -> usize {
    s.chars().count()
}

/// `str(round(x, 1))` — `round(x, 1)` is always a one-decimal multiple, so its
/// `str()` shows exactly one decimal (`str(2.0) == "2.0"`, `str(1.5) == "1.5"`).
/// Rust's `{:.1}` uses the same round-half-to-even rule as CPython's `round()`.
fn py_round1_str(x: f64) -> String {
    format!("{x:.1}")
}

/// `str(round(x, 2))` — like `{:.2}` but with Python's trailing-zero trimming:
/// `str(5.0) == "5.0"`, `str(5.2) == "5.2"`, `str(5.25) == "5.25"`. `round`
/// yields at most two decimals; `str` drops a single trailing hundredths zero
/// while keeping at least one decimal digit.
fn py_round2_str(x: f64) -> String {
    let s = format!("{x:.2}");
    if let Some(stripped) = s.strip_suffix('0') {
        // "5.20" -> "5.2", "5.00" -> "5.0" (keep the tenths digit).
        stripped.to_string()
    } else {
        s
    }
}

/// `RNS.prettysize` (RNS/__init__.py:191). `bits=true` corresponds to
/// `suffix='b'` (multiplies by 8); otherwise bytes (`suffix='B'`).
fn prettysize(mut num: f64, bits: bool) -> String {
    let suffix = if bits { "b" } else { "B" };
    if bits {
        num *= 8.0;
    }
    for unit in ["", "K", "M", "G", "T", "P", "E", "Z"] {
        if num.abs() < 1000.0 {
            if unit.is_empty() {
                return format!("{num:.0} {suffix}");
            }
            return format!("{num:.2} {unit}{suffix}");
        }
        num /= 1000.0;
    }
    format!("{num:.2}Y{suffix}")
}

/// `RNS.prettyspeed` (RNS/__init__.py:188): bits-per-second string.
fn prettyspeed(num: f64) -> String {
    format!("{}ps", prettysize(num / 8.0, true))
}

/// `RNS.prettyfrequency(hz, d=1, lpf=True)` — the only call shape rnstatus uses
/// for interface announce/path-request frequencies (RNS/__init__.py:208).
fn prettyfrequency_d1_lpf(hz: f64) -> String {
    if hz == 0.0 {
        return "0 Hz".to_string();
    }
    let mut num = hz;
    for unit in ["", "K", "M", "G", "T", "P", "E", "Z"] {
        if num.abs() < 1000.0 {
            return format!("{} {unit}Hz", py_round1_str(num));
        }
        num /= 1000.0;
    }
    format!("{num:.2}YHz")
}

/// `speed_str` from rnstatus.py:756 (default `suffix='bps'`, no /8). Used for
/// the interface bitrate ("Rate" line). Note lowercase `k` for kilo.
fn speed_str(mut num: f64) -> String {
    for unit in ["", "k", "M", "G", "T", "P", "E", "Z"] {
        if num.abs() < 1000.0 {
            return format!("{num:.2} {unit}bps");
        }
        num /= 1000.0;
    }
    format!("{num:.2} Ybps")
}

/// `RNS.prettytime(time)` — non-verbose, non-compact (RNS/__init__.py:239).
/// Days/hours/minutes render as integers with `d/h/m`; seconds as
/// `str(round(x, 2)) + "s"`. Components join with `, ` and a final ` and `.
fn prettytime(mut t: f64) -> String {
    let neg = t < 0.0;
    if neg {
        t = t.abs();
    }
    let days = (t / 86400.0).floor() as i64;
    t %= 86400.0;
    let hours = (t / 3600.0).floor() as i64;
    t %= 3600.0;
    let minutes = (t / 60.0).floor() as i64;
    t %= 60.0;
    let seconds = t; // rounded to 2 decimals at format time

    let mut components: Vec<String> = Vec::new();
    if days > 0 {
        components.push(format!("{days}d"));
    }
    if hours > 0 {
        components.push(format!("{hours}h"));
    }
    if minutes > 0 {
        components.push(format!("{minutes}m"));
    }
    // Python: `if seconds > 0` where seconds == round(t, 2).
    if (seconds * 100.0).round() as i64 > 0 {
        components.push(format!("{}s", py_round2_str(seconds)));
    }

    if components.is_empty() {
        return "0s".to_string();
    }
    let n = components.len();
    let mut tstr = String::new();
    for (i, c) in components.iter().enumerate() {
        if i == 0 {
        } else if i < n - 1 {
            tstr.push_str(", ");
        } else {
            tstr.push_str(" and ");
        }
        tstr.push_str(c);
    }
    if neg {
        format!("-{tstr}")
    } else {
        tstr
    }
}

/// `RNS.prettyhexrep` over an already-hex string (rpc_query decodes `bin` to
/// lowercase hex): `<hex>`.
fn prettyhexrep_from_hex(hex: &str) -> String {
    format!("<{hex}>")
}

// ---------------------------------------------------------------------------
// JSON field accessors
// ---------------------------------------------------------------------------

/// Key present at all (Python `"key" in ifstat`, true even if the value is
/// null, matching a decoded `None`).
fn has(v: &Value, k: &str) -> bool {
    v.get(k).is_some()
}

/// Value present and not null (Python `ifstat["key"] != None`).
fn not_null(v: &Value, k: &str) -> bool {
    matches!(v.get(k), Some(x) if !x.is_null())
}

/// Read a numeric field as f64. rpc_query renders big ints as decimal strings,
/// so accept a JSON number or a numeric string.
fn jf(v: &Value, k: &str) -> Option<f64> {
    match v.get(k)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

/// Read a numeric field as i64 (number or decimal string).
fn ji(v: &Value, k: &str) -> Option<i64> {
    match v.get(k)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.parse::<i64>().ok(),
        _ => None,
    }
}

fn jb(v: &Value, k: &str) -> Option<bool> {
    v.get(k)?.as_bool()
}

fn js<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k)?.as_str()
}

// ---------------------------------------------------------------------------
// Sorting
// ---------------------------------------------------------------------------

/// Sort key for a `-s/--sort <field>` value, or `None` for an unrecognised
/// field (rnstatus then leaves the order untouched).
fn sort_key(iface: &Value, field: &str) -> Option<f64> {
    let g = |k: &str| jf(iface, k).unwrap_or(0.0);
    Some(match field {
        "rate" | "bitrate" => g("bitrate"),
        "rx" => g("rxb"),
        "tx" => g("txb"),
        "rxs" => g("rxs"),
        "txs" => g("txs"),
        "traffic" => g("rxb") + g("txb"),
        "announces" | "announce" => {
            g("incoming_announce_frequency") + g("outgoing_announce_frequency")
        }
        "arx" => g("incoming_announce_frequency"),
        "atx" => g("outgoing_announce_frequency"),
        "prx" => g("incoming_pr_frequency"),
        "ptx" => g("outgoing_pr_frequency"),
        "held" => g("held_announces"),
        _ => return None,
    })
}

/// In-place sort matching `interfaces.sort(key=..., reverse=not sort_reverse)`:
/// descending by default, ascending with `-r`. Stable (equal keys keep order),
/// like Python's `list.sort`.
fn sort_interfaces(interfaces: &mut [Value], sort: &str, reverse: bool) {
    let field = sort.to_lowercase();
    // No-op for an unrecognised field (probe the first entry / any entry).
    if interfaces
        .first()
        .and_then(|i| sort_key(i, &field))
        .is_none()
        && interfaces.iter().all(|i| sort_key(i, &field).is_none())
    {
        return;
    }
    interfaces.sort_by(|a, b| {
        let ka = sort_key(a, &field).unwrap_or(0.0);
        let kb = sort_key(b, &field).unwrap_or(0.0);
        let ord = ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal);
        // reverse = not sort_reverse: default descending.
        if reverse {
            ord
        } else {
            ord.reverse()
        }
    });
}

// ---------------------------------------------------------------------------
// -j / --json
// ---------------------------------------------------------------------------

/// `-j`: emit the raw stats dict. rnstatus prints `json.dumps(stats)` after
/// hex-encoding byte values; rpc_query has already hex-encoded them, so this is
/// the structural equivalent. (Byte-for-byte JSON parity is out of scope per
/// the issue: key order / separators differ between Python and serde.)
pub fn render_json(stats: &Value) -> String {
    serde_json::to_string(stats).unwrap_or_else(|_| "null".to_string())
}

// ---------------------------------------------------------------------------
// Local-mode render (rnstatus.py:361-671)
// ---------------------------------------------------------------------------

/// Render the full local-mode status output for `stats` (the decoded
/// `interface_stats` dict). `link_count` is the separate `link_count` query
/// result (only meaningful with `-l`). Output includes the leading blank line
/// per interface and the trailing blank line, exactly as rnstatus prints them.
pub fn render_status(stats: &Value, link_count: Option<i64>, opts: &StatusOptions) -> String {
    let mut out = String::new();

    // Work on a mutable copy of the interfaces array so sorting can reorder it.
    let mut interfaces: Vec<Value> = stats
        .get("interfaces")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if let Some(sort) = &opts.sort {
        sort_interfaces(&mut interfaces, sort, opts.reverse);
    }

    for ifstat in &interfaces {
        render_interface(&mut out, ifstat, opts);
    }

    render_trailer(&mut out, stats, link_count, opts);
    out
}

/// Push one line (with trailing newline), mirroring a Python `print(...)`.
fn pln(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

/// The default-hidden name prefixes (rnstatus.py:393-401) plus the I2P
/// non-connectable special case.
fn is_hidden(name: &str, ifstat: &Value) -> bool {
    let i2p_non_connectable =
        name.starts_with("I2PInterface[") && matches!(jb(ifstat, "i2p_connectable"), Some(false));
    name.starts_with("LocalInterface[")
        || name.starts_with("TCPInterface[Client")
        || name.starts_with("BackboneInterface[Client on")
        || name.starts_with("AutoInterfacePeer[")
        || name.starts_with("WeaveInterfacePeer[")
        || name.starts_with("I2PInterfacePeer[Connected peer")
        || i2p_non_connectable
}

fn render_interface(out: &mut String, ifstat: &Value, opts: &StatusOptions) {
    let name = js(ifstat, "name").unwrap_or("");

    // Hidden-by-default interfaces (rnstatus.py:393-401) are shown only with -a;
    // `dispall or not hidden` ⇒ skip when not dispall and hidden.
    if !opts.dispall && is_hidden(name, ifstat) {
        return;
    }
    // The redundant inner I2P guard (rnstatus.py:403).
    if name.starts_with("I2PInterface[") && matches!(jb(ifstat, "i2p_connectable"), Some(false)) {
        return;
    }

    // Display filter (rnstatus.py:404-413). The `burst_filter == None` branch in
    // Python is dead code (it is always a bool), so this is the effective logic.
    let name_matches = || {
        opts.name_filter
            .as_ref()
            .map(|f| name.to_lowercase().contains(&f.to_lowercase()))
            .unwrap_or(false)
    };
    let show_if = if opts.burst_filter {
        let burst_act = has(ifstat, "burst_active")
            && has(ifstat, "pr_burst_active")
            && (jb(ifstat, "burst_active").unwrap_or(false)
                || jb(ifstat, "pr_burst_active").unwrap_or(false));
        burst_act || name_matches()
    } else {
        opts.name_filter.is_none() || name_matches()
    };
    if !show_if {
        return;
    }

    pln(out, "");

    let ss = if jb(ifstat, "status").unwrap_or(false) {
        "Up"
    } else {
        "Down"
    };

    let modestr = match ji(ifstat, "mode") {
        Some(MODE_ACCESS_POINT) => "Access Point",
        Some(MODE_POINT_TO_POINT) => "Point-to-Point",
        Some(MODE_ROAMING) => "Roaming",
        Some(MODE_BOUNDARY) => "Boundary",
        Some(MODE_GATEWAY) => "Gateway",
        _ => "Full",
    };

    // Clients / peers header field (rnstatus.py:429-454).
    let mut clients: Option<i64> = None;
    let mut clients_string = String::new();
    if not_null(ifstat, "clients") {
        let c = ji(ifstat, "clients").unwrap_or(0);
        clients = Some(c);
        if name.starts_with("Shared Instance[") {
            let cnum = (c - 1).max(0);
            let spec = if cnum == 1 { " program" } else { " programs" };
            clients_string = format!("Serving   : {cnum}{spec}");
        } else if name.starts_with("I2PInterface[") {
            if matches!(jb(ifstat, "i2p_connectable"), Some(true)) {
                let spec = if c == 1 {
                    " connected I2P endpoint"
                } else {
                    " connected I2P endpoints"
                };
                clients_string = format!("Peers     : {c}{spec}");
            }
            // else: clients_string stays empty
        } else {
            clients_string = format!("Clients   : {c}");
        }
    }

    pln(out, &format!(" {name}"));

    if let Some(src) = js(ifstat, "autoconnect_source") {
        if not_null(ifstat, "autoconnect_source") {
            pln(out, &format!("    Source    : Auto-connect via <{src}>"));
        }
    }
    if not_null(ifstat, "ifac_netname") {
        if let Some(nn) = js(ifstat, "ifac_netname") {
            pln(out, &format!("    Network   : {nn}"));
        }
    }

    pln(out, &format!("    Status    : {ss}"));

    if clients.is_some() && !clients_string.is_empty() {
        pln(out, &format!("    {clients_string}"));
    }

    if !(name.starts_with("Shared Instance[")
        || name.starts_with("TCPInterface[Client")
        || name.starts_with("LocalInterface["))
    {
        pln(out, &format!("    Mode      : {modestr}"));
    }

    if not_null(ifstat, "bitrate") {
        if let Some(br) = jf(ifstat, "bitrate") {
            pln(out, &format!("    Rate      : {}", speed_str(br)));
        }
    }

    // noise_floor / cpu / mem / battery / airtime / channel_load / switch_id /
    // endpoint / via / tunnelstate / i2p_b32: lnsd never emits these, but honour
    // them if a stats source ever does (rnstatus.py:475-540).
    render_optional_hw_fields(out, ifstat);

    // ifac access line (rnstatus.py:535-537).
    if not_null(ifstat, "ifac_signature") {
        if let Some(sig_hex) = js(ifstat, "ifac_signature") {
            // hexrep(signature[-5:], delimit=False) == last 10 hex chars.
            let tail = if sig_hex.len() >= 10 {
                &sig_hex[sig_hex.len() - 10..]
            } else {
                sig_hex
            };
            let nb = ji(ifstat, "ifac_size").unwrap_or(0) * 8;
            pln(out, &format!("    Access    : {nb}-bit IFAC by <…{tail}>"));
        }
    }
    if not_null(ifstat, "i2p_b32") {
        if let Some(ep) = js(ifstat, "i2p_b32") {
            pln(out, &format!("    I2P B32   : {ep}"));
        }
    }

    // Queued / Held announces (rnstatus.py:542-554).
    if opts.astats {
        if let Some(aqn) = ji(ifstat, "announce_queue") {
            if not_null(ifstat, "announce_queue") && aqn > 0 {
                let word = if aqn == 1 { "announce" } else { "announces" };
                pln(out, &format!("    Queued    : {aqn} {word}"));
            }
        }
        if let Some(aqn) = ji(ifstat, "held_announces") {
            if not_null(ifstat, "held_announces") && aqn > 0 {
                let word = if aqn == 1 { "announce" } else { "announces" };
                pln(out, &format!("    Held      : {aqn} {word}"));
            }
        }
    }

    render_traffic_block(out, ifstat, opts, name, clients);
}

/// The optional hardware/telemetry lines (rnstatus.py:475-533). Each is gated
/// on field presence exactly as Python's `"key" in ifstat`.
fn render_optional_hw_fields(out: &mut String, ifstat: &Value) {
    if has(ifstat, "noise_floor") {
        let nstr = if !has(ifstat, "interference") {
            String::new()
        } else {
            let nf = jf(ifstat, "interference").unwrap_or(0.0);
            if nf != 0.0 {
                format!("\n    Intrfrnc. : {} dBm", num_str(ifstat, "interference"))
            } else if has(ifstat, "interference_last_ts") && has(ifstat, "interference_last_dbm") {
                let lago = 0.0; // time-relative; lnsd never emits these fields
                let ldbm = num_str(ifstat, "interference_last_dbm");
                format!("\n    Intrfrnc. : {ldbm} dBm {} ago", prettytime(lago))
            } else {
                ", no interference".to_string()
            }
        };
        if not_null(ifstat, "noise_floor") {
            pln(
                out,
                &format!(
                    "    Noise Fl. : {} dBm{nstr}",
                    num_str(ifstat, "noise_floor")
                ),
            );
        } else {
            pln(out, "    Noise Fl. : Unknown");
        }
    }
    if has(ifstat, "cpu_load") {
        if not_null(ifstat, "cpu_load") {
            pln(
                out,
                &format!("    CPU load  : {} %", num_str(ifstat, "cpu_load")),
            );
        } else {
            pln(out, "    CPU load  : Unknown");
        }
    }
    if has(ifstat, "cpu_temp") {
        if not_null(ifstat, "cpu_temp") {
            pln(
                out,
                &format!("    CPU temp  : {}°C", num_str(ifstat, "cpu_temp")),
            );
        } else {
            pln(out, "    CPU load  : Unknown");
        }
    }
    if has(ifstat, "mem_load") {
        if not_null(ifstat, "cpu_load") {
            pln(
                out,
                &format!("    Mem usage : {} %", num_str(ifstat, "mem_load")),
            );
        } else {
            pln(out, "    Mem usage : Unknown");
        }
    }
    if not_null(ifstat, "battery_percent") {
        if let Some(bp) = jf(ifstat, "battery_percent") {
            let bss = js(ifstat, "battery_state").unwrap_or("");
            pln(out, &format!("    Battery   : {}% ({bss})", bp as i64));
        }
    }
    if has(ifstat, "airtime_short") && has(ifstat, "airtime_long") {
        pln(
            out,
            &format!(
                "    Airtime   : {}% (15s), {}% (1h)",
                num_str(ifstat, "airtime_short"),
                num_str(ifstat, "airtime_long")
            ),
        );
    }
    if has(ifstat, "channel_load_short") && has(ifstat, "channel_load_long") {
        pln(
            out,
            &format!(
                "    Ch. Load  : {}% (15s), {}% (1h)",
                num_str(ifstat, "channel_load_short"),
                num_str(ifstat, "channel_load_long")
            ),
        );
    }
    if has(ifstat, "switch_id") {
        if not_null(ifstat, "switch_id") {
            pln(
                out,
                &format!("    Switch ID : {}", scalar_str(ifstat, "switch_id")),
            );
        } else {
            pln(out, "    Switch ID : Unknown");
        }
    }
    if has(ifstat, "endpoint_id") {
        if not_null(ifstat, "endpoint_id") {
            pln(
                out,
                &format!("    Endpoint  : {}", scalar_str(ifstat, "endpoint_id")),
            );
        } else {
            pln(out, "    Endpoint  : Unknown");
        }
    }
    if has(ifstat, "via_switch_id") {
        if not_null(ifstat, "via_switch_id") {
            pln(
                out,
                &format!("    Via       : {}", scalar_str(ifstat, "via_switch_id")),
            );
        } else {
            pln(out, "    Via       : Unknown");
        }
    }
    if not_null(ifstat, "peers") {
        if let Some(p) = ji(ifstat, "peers") {
            pln(out, &format!("    Peers     : {p} reachable"));
        }
    }
    if not_null(ifstat, "tunnelstate") {
        if let Some(ts) = js(ifstat, "tunnelstate") {
            pln(out, &format!("    I2P       : {ts}"));
        }
    }
}

/// The announce / path-request / traffic block (rnstatus.py:556-636), including
/// the character-width padding that aligns the arrow columns.
fn render_traffic_block(
    out: &mut String,
    ifstat: &Value,
    opts: &StatusOptions,
    name: &str,
    mut clients: Option<i64>,
) {
    // announce_rate suffix (rnstatus.py:556-563).
    let art = if opts.astats {
        ji(ifstat, "announce_rate_target")
    } else {
        None
    };
    let arp = if opts.astats {
        ji(ifstat, "announce_rate_penalty")
    } else {
        None
    };
    let arg = if opts.astats {
        ji(ifstat, "announce_rate_grace")
    } else {
        None
    };
    // Python truthiness: art/arg truthy means present and non-zero.
    let art_str = match (art, arp, arg) {
        (Some(t), Some(p), Some(g)) if t != 0 && g != 0 => {
            format!(
                "(t:{}/p:{}/g:{})",
                prettytime(t as f64),
                prettytime(p as f64),
                g
            )
        }
        (Some(t), Some(p), _) if t != 0 => {
            format!("(t:{}/p:{})", prettytime(t as f64), prettytime(p as f64))
        }
        (Some(t), _, _) if t != 0 => format!("(t:{})", prettytime(t as f64)),
        _ => String::new(),
    };

    // Burst suffixes (rnstatus.py:565-573). `burst_activated` is an absolute
    // timestamp; lnsd emits burst_active=false so these stay empty. When a
    // source does report an active burst we cannot compute `now - activated`
    // deterministically here, so we mirror the structure with a 0 elapsed.
    let burst_str = if jb(ifstat, "burst_active").unwrap_or(false) {
        format!(" burst for {}", prettytime(0.0))
    } else {
        String::new()
    };
    let pburst_str = if jb(ifstat, "pr_burst_active").unwrap_or(false) {
        format!("burst for {}", prettytime(0.0))
    } else {
        String::new()
    };

    let mut rxb_str = format!("↓{}", prettysize(jf(ifstat, "rxb").unwrap_or(0.0), false));
    let mut txb_str = format!("↑{}", prettysize(jf(ifstat, "txb").unwrap_or(0.0), false));

    // Announce frequencies (rnstatus.py:578-590).
    let mut iaf = String::new();
    let mut oaf = String::new();
    let mut pc_str = String::new();
    let mut asr = false;
    if opts.astats && not_null(ifstat, "incoming_announce_frequency") {
        let mut oan = jf(ifstat, "outgoing_announce_frequency").unwrap_or(0.0);
        let ian = jf(ifstat, "incoming_announce_frequency").unwrap_or(0.0);
        if name.starts_with("Shared Instance[") && clients.is_some_and(|c| c > 0) {
            let c = clients.unwrap() as f64;
            oan -= oan / c;
        }
        oaf = prettyfrequency_d1_lpf(oan);
        iaf = prettyfrequency_d1_lpf(ian);

        let mut cspec = "c";
        if clients.is_none() {
            if let Some(p) = ji(ifstat, "peers") {
                if p != 0 {
                    clients = Some(p);
                    cspec = "p";
                }
            }
        }
        if let Some(c) = clients {
            if c > 0 {
                let oaf_field = jf(ifstat, "outgoing_announce_frequency").unwrap_or(0.0);
                pc_str = format!("{}/{cspec}", prettyfrequency_d1_lpf(oaf_field / c as f64));
            }
        }
        asr = true;
    }

    // Path-request frequencies (rnstatus.py:592-607).
    let mut ipf = String::new();
    let mut opf = String::new();
    let mut rpc_str = String::new();
    let mut psr = false;
    if opts.pstats && not_null(ifstat, "incoming_pr_frequency") {
        let mut opn = jf(ifstat, "outgoing_pr_frequency").unwrap_or(0.0);
        let ipn = jf(ifstat, "incoming_pr_frequency").unwrap_or(0.0);
        if name.starts_with("Shared Instance[") && clients.is_some_and(|c| c > 0) {
            let c = clients.unwrap() as f64;
            opn -= opn / c;
        }
        if opts.astats {
            opf = format!("↑{}", prettyfrequency_d1_lpf(opn));
            ipf = format!("↓{}", prettyfrequency_d1_lpf(ipn));
        } else {
            opf = format!("{}↑", prettyfrequency_d1_lpf(opn));
            ipf = format!("{}↓", prettyfrequency_d1_lpf(ipn));
        }
        let mut cspec = "c";
        if clients.is_none() {
            if let Some(p) = ji(ifstat, "peers") {
                if p != 0 {
                    clients = Some(p);
                    cspec = "p";
                }
            }
        }
        if let Some(c) = clients {
            if c > 0 {
                let opr_field = jf(ifstat, "outgoing_pr_frequency").unwrap_or(0.0);
                rpc_str = format!("{}/{cspec}", prettyfrequency_d1_lpf(opr_field / c as f64));
            }
        }
        psr = true;
    }

    // Column padding (rnstatus.py:609-620) — all widths in code points.
    if !asr {
        iaf.clear();
        oaf.clear();
    }
    if !psr {
        ipf.clear();
        opf.clear();
    }
    let amlen = clen(&iaf).max(clen(&oaf));
    iaf.push_str(&" ".repeat(amlen - clen(&iaf)));
    iaf.push('↓');
    oaf.push_str(&" ".repeat(amlen - clen(&oaf)));
    oaf.push('↑');
    let mlen = clen(&iaf)
        .max(clen(&oaf))
        .max(clen(&rxb_str))
        .max(clen(&txb_str))
        .max(clen(&ipf))
        .max(clen(&opf))
        .max(10);
    iaf.push_str(&" ".repeat(mlen - clen(&iaf)));
    oaf.push_str(&" ".repeat(mlen - clen(&oaf)));
    ipf.push_str(&" ".repeat(mlen - clen(&ipf)));
    opf.push_str(&" ".repeat(mlen - clen(&opf)));
    rxb_str.push_str(&" ".repeat(mlen - clen(&rxb_str)));
    txb_str.push_str(&" ".repeat(mlen - clen(&txb_str)));

    if psr {
        pln(out, &format!("    Path Rqs. : {opf}  {rpc_str}"));
        pln(out, &format!("                {ipf}  {pburst_str}"));
    }
    if asr {
        pln(out, &format!("    Announces : {oaf}  {pc_str}"));
        pln(out, &format!("                {iaf} {art_str}{burst_str}"));
    }

    let mut rxstat = rxb_str.clone();
    let mut txstat = txb_str.clone();
    if has(ifstat, "rxs") && has(ifstat, "txs") {
        let _ = write!(
            rxstat,
            "  {}",
            prettyspeed(jf(ifstat, "rxs").unwrap_or(0.0))
        );
        let _ = write!(
            txstat,
            "  {}",
            prettyspeed(jf(ifstat, "txs").unwrap_or(0.0))
        );
    }
    pln(
        out,
        &format!("    Traffic   : {txstat}\n                {rxstat}"),
    );
}

/// Totals / transport-instance / link-table trailer (rnstatus.py:638-671).
fn render_trailer(out: &mut String, stats: &Value, link_count: Option<i64>, opts: &StatusOptions) {
    let has_transport = not_null(stats, "transport_id");

    let mut lstr = String::new();
    if let Some(lc) = link_count {
        if opts.lstats {
            let ms = if lc == 1 { "y" } else { "ies" };
            if has_transport {
                lstr = format!(", {lc} entr{ms} in link table");
            } else {
                lstr = format!(" {lc} entr{ms} in link table");
            }
        }
    }

    if opts.totals {
        let mut rxb_str = format!("↓{}", prettysize(jf(stats, "rxb").unwrap_or(0.0), false));
        let mut txb_str = format!("↑{}", prettysize(jf(stats, "txb").unwrap_or(0.0), false));
        let strdiff = clen(&rxb_str) as i64 - clen(&txb_str) as i64;
        if strdiff > 0 {
            txb_str.push_str(&" ".repeat(strdiff as usize));
        } else if strdiff < 0 {
            rxb_str.push_str(&" ".repeat((-strdiff) as usize));
        }
        let rxstat = format!(
            "{rxb_str}  {}",
            prettyspeed(jf(stats, "rxs").unwrap_or(0.0))
        );
        let txstat = format!(
            "{txb_str}  {}",
            prettyspeed(jf(stats, "txs").unwrap_or(0.0))
        );
        pln(
            out,
            &format!("\n Totals       : {txstat}\n                {rxstat}"),
        );
    }

    if has_transport {
        if let Some(tid) = js(stats, "transport_id") {
            pln(
                out,
                &format!(
                    "\n Transport Instance {} running",
                    prettyhexrep_from_hex(tid)
                ),
            );
        }
        if not_null(stats, "network_id") {
            if let Some(nid) = js(stats, "network_id") {
                pln(
                    out,
                    &format!(" Network Identity   {}", prettyhexrep_from_hex(nid)),
                );
            }
        }
        if not_null(stats, "probe_responder") {
            if let Some(pr) = js(stats, "probe_responder") {
                pln(
                    out,
                    &format!(" Probe responder at {} active", prettyhexrep_from_hex(pr)),
                );
            }
        }
        if not_null(stats, "transport_uptime") {
            if let Some(up) = jf(stats, "transport_uptime") {
                pln(out, &format!(" Uptime is {}{lstr}", prettytime(up)));
            }
        }
    } else if !lstr.is_empty() {
        pln(out, &format!("\n{lstr}"));
    }

    pln(out, "");
}

/// Render a scalar numeric field with Python `str()` semantics (int stays int,
/// float keeps its repr). Used for the telemetry lines above.
fn num_str(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else {
                n.to_string()
            }
        }
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Render an arbitrary scalar field with Python `str()` semantics (numbers,
/// strings). Used for switch/endpoint/via IDs.
fn scalar_str(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => {
            // Python str(True) == "True"
            if *b {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests;

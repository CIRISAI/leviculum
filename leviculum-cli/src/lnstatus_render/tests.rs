//! Tests for the `lnstatus` renderer.
//!
//! The centrepiece is a GOLDEN-OUTPUT suite: `tests_data/lnstatus_golden.json`
//! is produced by `tests_gen/gen_lnstatus_golden.py`, which drives the REAL
//! vendored Python `rnstatus.program_setup` over fixed `interface_stats` dicts
//! and captures its exact stdout. Each case carries the rpc-style stats JSON
//! (bytes -> lowercase hex, as `rpc_query` yields) and the Python ground-truth
//! string; `render_status` must reproduce it byte-for-byte. That is the drop-in
//! parity the issue requires, pinned without a live daemon.
//!
//! The remaining tests exercise the pretty-formatters against known Python
//! outputs, plus sorting / filtering / `-j` structure directly.

use super::*;
use serde_json::Value;

const GOLDEN: &str = include_str!("../../tests_data/lnstatus_golden.json");

fn opts_from_json(o: &Value) -> StatusOptions {
    StatusOptions {
        dispall: o["dispall"].as_bool().unwrap(),
        astats: o["astats"].as_bool().unwrap(),
        pstats: o["pstats"].as_bool().unwrap(),
        lstats: o["lstats"].as_bool().unwrap(),
        burst_filter: o["burst_filter"].as_bool().unwrap(),
        totals: o["totals"].as_bool().unwrap(),
        sort: o["sort"].as_str().map(String::from),
        reverse: o["reverse"].as_bool().unwrap(),
        name_filter: o["name_filter"].as_str().map(String::from),
    }
}

#[test]
fn golden_output_matches_rnstatus() {
    let cases: Vec<Value> = serde_json::from_str(GOLDEN).expect("parse golden json");
    assert!(cases.len() >= 20, "expected a broad golden suite");
    let mut failures = Vec::new();
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let opts = opts_from_json(&case["opts"]);
        let stats = &case["stats"];
        let link_count = case["link_count"].as_i64();
        let expected = case["expected"].as_str().unwrap();
        let got = render_status(stats, link_count, &opts);
        if got != expected {
            failures.push(format!(
                "case `{name}` mismatch:\n--- expected ---\n{expected:?}\n--- got ---\n{got:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn golden_json_structure_roundtrips() {
    // `-j` must emit valid JSON that parses back to the same structure the
    // daemon returned (byte-exact JSON parity is out of scope; structure is).
    let cases: Vec<Value> = serde_json::from_str(GOLDEN).unwrap();
    for case in &cases {
        let stats = &case["stats"];
        let emitted = render_json(stats);
        let reparsed: Value = serde_json::from_str(&emitted).expect("emitted json parses");
        assert_eq!(&reparsed, stats, "case `{}` json roundtrip", case["name"]);
    }
}

// ---------------------------------------------------------------------------
// Pretty-formatter unit tests (ground truth = Python RNS)
// ---------------------------------------------------------------------------

#[test]
fn prettysize_bytes() {
    // RNS.prettysize: no-unit uses %.0f, scaled uses %.2f.
    assert_eq!(prettysize(0.0, false), "0 B");
    assert_eq!(prettysize(999.0, false), "999 B");
    assert_eq!(prettysize(1024.0, false), "1.02 KB");
    assert_eq!(prettysize(123456.0, false), "123.46 KB");
    assert_eq!(prettysize(1_000_000.0, false), "1.00 MB");
}

#[test]
fn prettyspeed_bits() {
    // RNS.prettyspeed(num) = prettysize(num/8, suffix="b")+"ps"; input is bytes/s.
    assert_eq!(prettyspeed(0.0), "0 bps");
    assert_eq!(prettyspeed(42.0), "42 bps");
    assert_eq!(prettyspeed(1500.0), "1.50 Kbps");
    assert_eq!(prettyspeed(125.0), "125 bps");
    assert_eq!(prettyspeed(1000.0), "1.00 Kbps");
}

#[test]
fn speed_str_bitrate() {
    // rnstatus.speed_str default suffix "bps", lowercase k.
    assert_eq!(speed_str(10_000_000.0), "10.00 Mbps");
    assert_eq!(speed_str(1_000_000_000.0), "1.00 Gbps");
    assert_eq!(speed_str(9600.0), "9.60 kbps");
}

#[test]
fn prettyfrequency_d1_lpf_cases() {
    assert_eq!(prettyfrequency_d1_lpf(0.0), "0 Hz");
    assert_eq!(prettyfrequency_d1_lpf(0.5), "0.5 Hz");
    assert_eq!(prettyfrequency_d1_lpf(1500.0), "1.5 KHz");
    assert_eq!(prettyfrequency_d1_lpf(375.0), "375.0 Hz");
}

#[test]
fn prettytime_cases() {
    assert_eq!(prettytime(0.0), "0s");
    assert_eq!(prettytime(3600.0), "1h");
    assert_eq!(prettytime(3661.0), "1h, 1m and 1.0s");
    assert_eq!(prettytime(90.0), "1m and 30.0s");
    assert_eq!(prettytime(86400.0 + 3600.0), "1d and 1h");
}

#[test]
fn py_round2_trims_like_python() {
    assert_eq!(py_round2_str(5.0), "5.0");
    assert_eq!(py_round2_str(5.2), "5.2");
    assert_eq!(py_round2_str(5.25), "5.25");
    assert_eq!(py_round2_str(1.0), "1.0");
}

// ---------------------------------------------------------------------------
// Sorting
// ---------------------------------------------------------------------------

fn iface(name: &str, bitrate: i64, rxb: i64, txb: i64) -> Value {
    serde_json::json!({
        "name": name, "type": "AutoInterface", "status": true, "mode": 1,
        "bitrate": bitrate, "rxb": rxb, "txb": txb, "rxs": 0.0, "txs": 0.0,
        "clients": null, "peers": null,
        "incoming_announce_frequency": 0.0, "outgoing_announce_frequency": 0.0,
        "incoming_pr_frequency": 0.0, "outgoing_pr_frequency": 0.0,
        "held_announces": 0, "announce_queue": null,
        "burst_active": false, "pr_burst_active": false,
        "ifac_signature": null, "ifac_size": null, "ifac_netname": null
    })
}

fn names_after_sort(mut ifaces: Vec<Value>, sort: &str, reverse: bool) -> Vec<String> {
    sort_interfaces(&mut ifaces, sort, reverse);
    ifaces
        .iter()
        .map(|i| i["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn sort_rate_descending_by_default() {
    let v = vec![
        iface("a", 10, 0, 0),
        iface("b", 1000, 0, 0),
        iface("c", 100, 0, 0),
    ];
    // reverse = not sort_reverse: default descending.
    assert_eq!(
        names_after_sort(v.clone(), "rate", false),
        vec!["b", "c", "a"]
    );
    // -r => ascending.
    assert_eq!(names_after_sort(v, "rate", true), vec!["a", "c", "b"]);
}

#[test]
fn sort_traffic_sums_rx_tx() {
    let v = vec![
        iface("a", 0, 1, 1),   // 2
        iface("b", 0, 10, 10), // 20
        iface("c", 0, 5, 0),   // 5
    ];
    assert_eq!(names_after_sort(v, "traffic", false), vec!["b", "c", "a"]);
}

#[test]
fn sort_unknown_field_is_noop() {
    let v = vec![iface("a", 10, 0, 0), iface("b", 1000, 0, 0)];
    assert_eq!(names_after_sort(v, "bogus", false), vec!["a", "b"]);
}

#[test]
fn sort_is_stable_on_equal_keys() {
    let v = vec![
        iface("first", 5, 0, 0),
        iface("second", 5, 0, 0),
        iface("third", 5, 0, 0),
    ];
    assert_eq!(
        names_after_sort(v, "rate", false),
        vec!["first", "second", "third"]
    );
}

// ---------------------------------------------------------------------------
// Filtering
// ---------------------------------------------------------------------------

#[test]
fn positional_filter_is_case_insensitive_substring() {
    let stats = serde_json::json!({
        "interfaces": [ iface("AutoInterface[Alpha]", 10, 0, 0),
                        iface("AutoInterface[Beta]", 10, 0, 0) ],
        "rxb": 0, "txb": 0, "rxs": 0.0, "txs": 0.0, "rss": null
    });
    let opts = StatusOptions {
        name_filter: Some("beta".to_string()),
        ..Default::default()
    };
    let out = render_status(&stats, None, &opts);
    assert!(out.contains("AutoInterface[Beta]"));
    assert!(!out.contains("AutoInterface[Alpha]"));
}

#[test]
fn burst_filter_shows_only_active_burst_interfaces() {
    // -B: keep only interfaces with an active burst (both burst flags present)
    // or a name match. This is the filter the golden suite pins deterministically
    // (the "burst for <elapsed>" duration is wall-clock dependent and only
    // renders under -A/-P, so it is not asserted here).
    let mut active = iface("AutoInterface[Hot]", 10, 0, 0);
    active["burst_active"] = Value::Bool(true);
    let stats = serde_json::json!({
        "interfaces": [ active, iface("AutoInterface[Cold]", 10, 0, 0) ],
        "rxb": 0, "txb": 0, "rxs": 0.0, "txs": 0.0, "rss": null
    });
    let out = render_status(
        &stats,
        None,
        &StatusOptions {
            burst_filter: true,
            ..Default::default()
        },
    );
    assert!(out.contains("AutoInterface[Hot]"));
    assert!(!out.contains("AutoInterface[Cold]"));
}

#[test]
fn active_burst_renders_duration_line_under_astats() {
    // The burst suffix ("burst for ...") appears on the announce line only with
    // -A. We can't pin the exact elapsed (wall-clock) so we assert the marker.
    let mut active = iface("AutoInterface[Hot]", 10, 0, 0);
    active["burst_active"] = Value::Bool(true);
    active["pr_burst_active"] = Value::Bool(true);
    let stats = serde_json::json!({
        "interfaces": [ active ],
        "rxb": 0, "txb": 0, "rxs": 0.0, "txs": 0.0, "rss": null
    });
    let out = render_status(
        &stats,
        None,
        &StatusOptions {
            astats: true,
            pstats: true,
            ..Default::default()
        },
    );
    assert!(
        out.contains("burst for"),
        "expected burst suffix, got:\n{out}"
    );
}

#[test]
fn default_hides_local_and_client_interfaces() {
    let stats = serde_json::json!({
        "interfaces": [ iface("LocalInterface[shared]", 10, 0, 0),
                        iface("AutoInterface[X]", 10, 0, 0) ],
        "rxb": 0, "txb": 0, "rxs": 0.0, "txs": 0.0, "rss": null
    });
    let hidden = render_status(&stats, None, &StatusOptions::default());
    assert!(!hidden.contains("LocalInterface[shared]"));
    let shown = render_status(
        &stats,
        None,
        &StatusOptions {
            dispall: true,
            ..Default::default()
        },
    );
    assert!(shown.contains("LocalInterface[shared]"));
}

// ---------------------------------------------------------------------------
// -d / -D discovered interfaces (Codeberg #32)
// ---------------------------------------------------------------------------

/// One RNode discovered-interface record as `rpc_query` yields it (bytes are
/// already hex strings; ids as hex; timestamps as epoch seconds).
fn rnode_record() -> Value {
    serde_json::json!({
        "type": "RNodeInterface",
        "transport": true,
        "name": "Node A",
        "received": 1000.0,
        "value": 15,
        "transport_id": "abababababababababababababababab",
        "network_id": "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
        "hops": 2,
        "latitude": 52.5,
        "longitude": 13.4,
        "height": null,
        "frequency": 867200000,
        "bandwidth": 125000,
        "sf": 8,
        "cr": 5,
        "config_entry": "[[Node A]]\n  type = RNodeInterface\n  enabled = yes\n  port = \n  frequency = 867200000\n  bandwidth = 125000\n  spreadingfactor = 8\n  codingrate = 5\n  txpower = ",
        "discovered": 1000.0,
        "last_heard": 1000.0,
        "heard_count": 0,
        "status": "available",
        "status_code": 1000
    })
}

#[test]
fn discovered_detail_renders_python_lines() {
    let list = Value::Array(vec![rnode_record()]);
    let out = render_discovered(&list, true, None, 1000.0);
    // transport_id != network_id -> both shown.
    assert!(out.contains("Network   ID : cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd\n"));
    assert!(out.contains("Transport ID : abababababababababababababababab\n"));
    assert!(out.contains("Name         : Node A\n"));
    assert!(out.contains("Type         : RNodeInterface\n"));
    assert!(out.contains("Status       : Available\n"));
    assert!(out.contains("Transport    : Enabled\n"));
    assert!(out.contains("Distance     : 2 hops\n"));
    assert!(out.contains("Discovered   : 0s ago\n"));
    assert!(out.contains("Last Heard   : 0s ago\n"));
    assert!(out.contains("Location     : 52.5, 13.4\n"));
    // {:,} thousands grouping.
    assert!(out.contains("Frequency    : 867,200,000 Hz\n"));
    assert!(out.contains("Bandwidth    : 125,000 Hz\n"));
    assert!(out.contains("Sprd. Factor : 8\n"));
    assert!(out.contains("Coding Rate  : 5\n"));
    assert!(out.contains("Stamp Value  : 15\n"));
    // Config entry indented by two spaces per line.
    assert!(out.contains("\nConfiguration Entry:\n"));
    assert!(out.contains("  [[Node A]]\n"));
    assert!(out.contains("  spreadingfactor = 8\n"));
}

#[test]
fn discovered_list_renders_python_columns() {
    let list = Value::Array(vec![rnode_record()]);
    let out = render_discovered(&list, false, None, 1000.0);
    assert!(out.contains(
        "Name                      Type         Status       Last Heard   Value    Location       "
    ));
    assert!(out.contains(&"-".repeat(89)));
    // Type drops the "Interface" suffix; status carries the ✓ glyph; last heard
    // "Just now" for a sub-minute delta.
    assert!(out.contains("Node A"));
    assert!(out.contains("RNode"));
    assert!(out.contains("✓ Available"));
    assert!(out.contains("Just now"));
}

#[test]
fn discovered_list_last_heard_buckets() {
    let mut r = rnode_record();
    r["last_heard"] = serde_json::json!(0.0);
    let list = Value::Array(vec![r]);
    // 2h since last_heard -> "2h ago".
    let out = render_discovered(&list, false, None, 7200.0);
    assert!(out.contains("2h ago"), "got: {out}");
    // 3 days -> "3d ago".
    let mut r2 = rnode_record();
    r2["last_heard"] = serde_json::json!(0.0);
    let out2 = render_discovered(&Value::Array(vec![r2]), false, None, 3.0 * 86400.0);
    assert!(out2.contains("3d ago"));
}

#[test]
fn discovered_name_filter_applies() {
    let mut other = rnode_record();
    other["name"] = serde_json::json!("Other");
    let list = Value::Array(vec![rnode_record(), other]);
    let out = render_discovered(&list, true, Some("node"), 1000.0);
    assert!(out.contains("Name         : Node A\n"));
    assert!(!out.contains("Name         : Other\n"));
}

#[test]
fn discovered_json_passthrough() {
    let list = Value::Array(vec![rnode_record()]);
    let s = render_json(&list);
    let back: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(back, list);
}

#[test]
fn thousands_and_compact_time_match_python() {
    assert_eq!(fmt_thousands(867200000), "867,200,000");
    assert_eq!(fmt_thousands(125000), "125,000");
    assert_eq!(fmt_thousands(5), "5");
    assert_eq!(prettytime_compact(0.0), "0s");
    assert_eq!(prettytime_compact(90.0), "1m and 30s");
    assert_eq!(prettytime_compact(3661.0), "1h and 1m");
    // Compact keeps only the two most-significant components.
    assert_eq!(prettytime_compact(90061.0), "1d and 1h");
}

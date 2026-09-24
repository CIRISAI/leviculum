//! mvr: `lora_pn_board_sync` step 21 lost the sync round's third and last
//! offer frame (179 B) in the night run of 2026-09-23 on 1599dc72, because
//! lnode_b's carrier-detect tore down the window that was receiving it.
//!
//! lnode_b's own periodic announce put its loop on the CSMA path while the
//! frame was arriving. From the board's capture (board time in ms):
//!
//! ```text
//! 114580  [SX_RX_TEARDOWN] site=cad preamble=1
//! ```
//!
//! 41 ms after the frame's preamble had latched, the standing RX came down to
//! run the CAD. The CAD then read the very frame it had just discarded as busy,
//! twice; the backoff RX re-armed mid-frame, where header sync is
//! unrecoverable; the third CAD found the ended frame gone and the announce
//! keyed up. The round never recovered inside the step's window — lnode_a
//! re-sent only the 83 B window packet, and the round's own deadline is 180 s
//! (`OUTBOUND_DEADLINE_MS`, `leviculum-nrf/src/pn.rs:370`). Codeberg #426.
//!
//! The firmware already had one teardown that waits: the idle select's
//! (`disarm_rx_for_tx`), which holds the key-up for one maximum-size frame's
//! airtime at the live modulation and hands the frame up through the loop's own
//! sink. The CAD site was, by that accounting, a teardown that could lose a
//! frame which would otherwise have completed.
//!
//! What is host-testable and what is not. The sequence itself — read the latch,
//! wait, take the frame, spend one standby — is
//! `leviculum_rx_arming::stand_down_for_tx`, and its CAD-site behaviour is
//! asserted against a modelled radio in that crate's own suite
//! (`a_cad_waits_for_the_frame_its_window_had_latched`,
//! `a_cad_releases_a_carrier_that_never_becomes_a_frame`). What cannot be
//! linked here is the driver: `Sx1262` is thumbv7em-only. So the part this file
//! holds is which SITE reaches which sequence, as a source invariant, the way
//! `radio_config_sleeps_through_the_peer_yield_window` holds the LoRa loop's
//! windows — and it enumerates the sites, because the failure mode is exactly a
//! teardown that nobody counted.

use std::path::{Path, PathBuf};

fn nrf_source(rel: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("leviculum-nrf")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The three forms a caller can leave a standing receive window by: the plain
/// one, the one that waits for a transmit and hands the frame to a sink, and
/// the one that waits for the receive path and hands the frame back. The
/// pairing below takes the LAST of these before the site name rather than the
/// first match in this list, so a form that is a prefix of another cannot
/// report a waiting site as a plain one.
///
/// Written with the receiver's dot, because a definition is not a teardown.
/// `disarm_rx_for_rx` takes no type parameter, so its own `fn` line would
/// otherwise match the pattern and be counted as a site that stands nothing
/// down.
const TEARDOWN_CALLS: [&str; 3] = [".disarm_rx_for_tx(", ".disarm_rx_for_rx(", ".disarm_rx("];

/// Every `RxTeardownBy::` site in `src`, paired with the call it is an argument
/// of, in source order.
///
/// The pairing is "the nearest teardown call before the site name", which is
/// what the argument position means in all seven cases and is why the window is
/// deliberately narrow: a site whose call cannot be found within it is reported
/// as `<none>` rather than skipped, because a teardown this scan cannot name is
/// still a teardown.
fn teardown_sites(src: &str) -> Vec<(String, String)> {
    const SITE: &str = "RxTeardownBy::";
    let mut sites = Vec::new();
    for (at, _) in src.match_indices(SITE) {
        let tail = &src[at + SITE.len()..];
        let name = tail
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .next()
            .unwrap_or("");
        // `by: leviculum_core::sx126x::RxTeardownBy` in a signature names no
        // variant; only a `RxTeardownBy::Variant` is a site.
        if name.is_empty() || !name.starts_with(|c: char| c.is_ascii_uppercase()) {
            continue;
        }
        let head = &src[at.saturating_sub(600)..at];
        let call = TEARDOWN_CALLS
            .iter()
            .filter_map(|c| head.rfind(c).map(|i| (i, *c)))
            .max_by_key(|(i, _)| *i)
            .map(|(_, c)| c)
            .unwrap_or("<none>");
        sites.push((name.to_string(), call.to_string()));
    }
    sites
}

/// What every site that leaves a standing window does about a frame that may
/// still be arriving on it, and why that is right for that site.
///
/// The table IS the invariant. Three sites wait, and the other four each have a
/// stated reason why the window they end cannot be holding a live frame, or has
/// already been given the wait:
///
/// | site | call | why |
/// |---|---|---|
/// | `Config` (`configure_lora`) | plain | reached only after `rx_window`'s config arm has already stood the window down with the waiting form, so nothing is standing |
/// | `Tx` (`transmit`) | plain | reached only after a CAD, which took the standby; on the CSMA path this finds nothing standing |
/// | `Arm` (`arm_rx`) | plain | the arming's own head, and the latched window is kept instead by `ensure_armed`'s adoption before it ever gets here |
/// | `RxWait` (`finish_rx`) | for-rx | the RX extension one branch above it is not the frame's own end — the re-read that follows it can be reading a preamble that latched DURING the extension, which is what cut a LINKCLOSE in `lora_pn_board_offer_past_the_link` round 3 on 2026-09-24; it defers on the caller's pre-clear latch and returns the frame instead of delivering it |
/// | `Cad` | for-tx | Codeberg #426, this batch |
/// | `Config` (`rx_window`) | for-tx | one host config push |
/// | `Select` (idle select) | for-tx | one key-up |
const SITE_TABLE: &str = "every teardown site's form is stated in SITE_TABLE's \
    doc comment beside the reason it is right for that site. A row that moved \
    has to move there too, with its reason re-argued: the four plain sites are \
    plain because the window they end cannot be holding a live frame, and that \
    is a claim about the path, not about the call.";

/// The invariant, and the assertion that was red before the fix: every site
/// that stands down a window which may be holding a frame waits for that frame
/// the way `disarm_rx_for_tx` does, and the sites are counted rather than
/// sampled.
///
/// Counting is the point. The CAD site was not overlooked in the abstract — it
/// was one of six teardowns of which three had been reasoned about, and the
/// reasoning never enumerated the rest. A new site added without a form fails
/// here by arithmetic, with nobody having to notice it.
#[test]
fn every_teardown_site_that_can_hold_a_frame_waits_for_it() {
    let driver = teardown_sites(&nrf_source("src/sx1262.rs"));
    assert_eq!(
        driver,
        [
            ("Config".to_string(), ".disarm_rx(".to_string()),
            ("Tx".to_string(), ".disarm_rx(".to_string()),
            ("Arm".to_string(), ".disarm_rx(".to_string()),
            ("RxWait".to_string(), ".disarm_rx_for_rx(".to_string()),
            ("Cad".to_string(), ".disarm_rx_for_tx(".to_string()),
        ],
        "the teardown sites in leviculum-nrf/src/sx1262.rs changed. {SITE_TABLE}"
    );
    let loop_sites = teardown_sites(&nrf_source("src/lora.rs"));
    assert_eq!(
        loop_sites,
        [
            ("Config".to_string(), ".disarm_rx_for_tx(".to_string()),
            ("Select".to_string(), ".disarm_rx_for_tx(".to_string()),
        ],
        "the teardown sites in leviculum-nrf/src/lora.rs changed. {SITE_TABLE}"
    );

    // And the vocabulary is complete: every `RxTeardownBy` variant core defines
    // appears in the table above. A seventh site cannot be added without a row
    // here, which is what makes the table exhaustive rather than a sample.
    let core_src = {
        let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("leviculum-core/src/sx126x.rs");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    };
    let enum_at = core_src
        .find("pub enum RxTeardownBy {")
        .expect("leviculum-core/src/sx126x.rs no longer defines RxTeardownBy");
    let body_end = enum_at
        + core_src[enum_at..]
            .find("\n}\n")
            .expect("unterminated RxTeardownBy");
    let mut defined: Vec<String> = core_src[enum_at..body_end]
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            l.strip_suffix(',')
                .filter(|v| v.chars().all(|c| c.is_alphanumeric() || c == '_'))
                .filter(|v| v.starts_with(|c: char| c.is_ascii_uppercase()))
                .map(|v| v.to_string())
        })
        .collect();
    defined.sort();
    let mut seen: Vec<String> = driver
        .iter()
        .chain(loop_sites.iter())
        .map(|(site, _)| site.clone())
        .collect();
    seen.sort();
    seen.dedup();
    assert_eq!(
        seen, defined,
        "leviculum-core's RxTeardownBy variants and the sites the firmware \
         actually reaches have diverged. {SITE_TABLE}"
    );
}

/// Which sites release a bare carrier early, and which keep the full frame
/// bound.
///
/// The policy is the whole difference between the four deferring sites, and it
/// follows from how often each is reached, not from taste: the select and
/// config arms are spent once per externally paced event and can afford one
/// maximum-size frame; the CAD is reached once per CSMA retry and cannot; and
/// the rxwait site is reached once per receive window whose software wait
/// expires, which is the loop's own cadence and not an external one, so it
/// releases too.
#[test]
fn the_release_policy_follows_how_often_a_site_is_reached() {
    for (rel, expected) in [
        (
            "src/sx1262.rs",
            ["ReleaseFalsePreamble", "ReleaseFalsePreamble"].as_slice(),
        ),
        ("src/lora.rs", ["OneFrame", "OneFrame"].as_slice()),
    ] {
        let src = nrf_source(rel);
        let mut sites: Vec<usize> = src
            .match_indices(".disarm_rx_for_tx(")
            .chain(src.match_indices(".disarm_rx_for_rx("))
            .map(|(at, _)| at)
            .collect();
        sites.sort_unstable();
        let policies: Vec<&str> = sites
            .into_iter()
            .map(|at| {
                let call = &src[at..(at + 600).min(src.len())];
                if call.contains("DeferPolicy::ReleaseFalsePreamble") {
                    "ReleaseFalsePreamble"
                } else if call.contains("DeferPolicy::OneFrame") {
                    "OneFrame"
                } else {
                    "<none>"
                }
            })
            .collect();
        assert_eq!(
            policies, expected,
            "the deferral policies in leviculum-nrf/{rel} changed; a site that \
             passes no policy at all would not compile, so this is a site that \
             swapped one for the other, and the bound it may spend follows from \
             how often it is reached, not from taste"
        );
    }
}

/// A frame the CAD waited for reaches the node core, and by the loop's own
/// route.
///
/// Waiting without handing the frame up would have fixed nothing: the window
/// would complete the reception, the `ClearIrqStatus` two commands later would
/// drop it, and the offer would be just as absent from the round. So the CAD
/// takes a buffer and a sink, and the sink is the same `CoreHandoff` every
/// other window's frames go through.
#[test]
fn a_frame_the_cad_waited_for_reaches_the_core() {
    let driver = nrf_source("src/sx1262.rs");
    let cad_at = driver.find("pub async fn cad<S>(").expect(
        "leviculum-nrf/src/sx1262.rs: cad no longer takes a sink, so a \
                 frame it waits for has nowhere to go",
    );
    let head = &driver[cad_at..cad_at + 400];
    assert!(
        head.contains("S: leviculum_rx_arming::FrameSink<Meta = RxStatus>"),
        "cad's sink is not the driver's frame sink any more"
    );

    let lora = nrf_source("src/lora.rs");
    let call_at = lora
        .find("radio.cad(")
        .expect("leviculum-nrf/src/lora.rs no longer calls radio.cad");
    let before = &lora[call_at.saturating_sub(600)..call_at];
    assert!(
        before.contains("let mut cad_sink = CoreHandoff {"),
        "the CSMA path hands the CAD something other than the loop's own \
         CoreHandoff, so a frame the wait catches would not reach the core the \
         way every other reception does"
    );
}

/// The affordability of the CAD site's wait, as a number.
///
/// The select site spends its bound once per key-up. The CAD is reached up to
/// `CAD_MAX_RETRIES` times for one packet, so the same bound would be eight of
/// them; what it spends instead is the preamble-plus-header time, and eight of
/// those stay under two frames at both PHYs the rig runs. That is the whole
/// argument for admitting a third deferring site, and it is arithmetic.
#[test]
fn eight_cad_retries_cost_less_than_two_frames() {
    let access = nrf_source("channel-access/src/lib.rs");
    let retries: u64 = access
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("pub const CAD_MAX_RETRIES: u8 = ")?
                .strip_suffix(';')?
                .parse()
                .ok()
        })
        .expect("leviculum-nrf/channel-access no longer states CAD_MAX_RETRIES");
    assert_eq!(
        retries, 8,
        "the retry ladder changed; re-run the numbers below"
    );

    for (bw, sf, cr, preamble) in [(125_000u32, 8u8, 5u8, 18u16), (62_500, 10, 5, 18)] {
        let frame = leviculum_core::sx126x::tx_defer_ms(
            leviculum_core::sx126x::IRQ_PREAMBLE_DETECTED,
            bw,
            sf,
            cr,
            preamble,
        )
        .expect("a configured radio defers");
        let carrier = leviculum_core::sx126x::false_preamble_ms(bw, sf, preamble)
            .expect("a configured radio has a false-preamble bound");
        assert!(
            retries * carrier < 2 * frame,
            "eight CAD retries at bw={bw} sf={sf} cost {} ms against one frame's \
             {frame} ms: the releasing policy is no longer what makes this site \
             affordable",
            retries * carrier
        );
        // And the same bound spent per retry at the select site's policy would
        // be the hold this avoids: worth stating as the counterfactual, because
        // it is what the CAD site would cost if a later batch simplified the two
        // policies into one.
        assert!(
            retries * frame > 4 * frame,
            "the counterfactual stopped being a hold; check the ladder"
        );
    }
}

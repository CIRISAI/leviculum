//! Measurement helper, not a gate: how long the client-side propagation
//! stamp takes to mine at a given cost on this machine.
//!
//! ```sh
//! cargo test -p lnmsg --release --test stamp_timing -- --ignored --nocapture
//! ```
//!
//! Ignored by default because a proof-of-work sample is a measurement, not
//! an assertion — its runtime is a coin-flip distribution and a debug-build
//! run says nothing about the shipped binary. Cost 13 is what default
//! propagation nodes announce (`PropagationNodeConfig::default`,
//! `leviculum-lxmf/src/propagation_node.rs`); the workblock rounds are the
//! propagation-stamp rounds, not the delivery-stamp ones.

use leviculum_lxmf::constants::WORKBLOCK_EXPAND_ROUNDS_PN;
use leviculum_lxmf::CooperativeStamper;

#[tokio::test]
#[ignore = "measurement helper; run explicitly with --release and --nocapture"]
async fn measure_propagation_stamp_at_cost_13() {
    const COST: u8 = 13;
    const SAMPLES: usize = 8;
    let mut times = Vec::with_capacity(SAMPLES);
    for n in 0..SAMPLES {
        let mut transient_id = [0u8; 32];
        transient_id[0] = n as u8;
        let mut executor = CooperativeStamper::cooperative(rand_core::OsRng);
        let started = std::time::Instant::now();
        let stamp = executor
            .generate(&transient_id, COST, WORKBLOCK_EXPAND_ROUNDS_PN)
            .await
            .expect("cost 13 is mineable");
        let elapsed = started.elapsed();
        assert_ne!(stamp, [0u8; 32]);
        println!("sample {n}: {:.3}s", elapsed.as_secs_f64());
        times.push(elapsed.as_secs_f64());
    }
    times.sort_by(|a, b| a.total_cmp(b));
    let total: f64 = times.iter().sum();
    println!(
        "cost {COST}, {SAMPLES} samples: min {:.3}s median {:.3}s mean {:.3}s max {:.3}s",
        times[0],
        times[SAMPLES / 2],
        total / SAMPLES as f64,
        times[SAMPLES - 1]
    );
}

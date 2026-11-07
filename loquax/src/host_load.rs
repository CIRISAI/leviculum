//! Whether the host was quiet enough over a measurement for a time to
//! measure the code and not the host (1473). The window's timing tests
//! asserted wall-clock times and went red when the host was loaded or the
//! suite ran beside them (590 ms frames at load 18); the facade's first
//! pull went red the same way (359 ms against a 250 ms bar at load 17.5,
//! 63 ms alone, 1486). A measurement counts when the 1-minute load average
//! at its start is within the bound (`WINDOW_BENCH_MAX_LOAD`, as
//! `scripts/window-bench.sh` reads it, else a fifth of the CPUs) and the
//! tasks running or waiting for a CPU, sampled while it runs, never exceed
//! that bound plus the measuring thread itself. The load average trails a
//! burst by a minute (the tests beside one in the same run start
//! together); the runnable count does not. Outside the bound a test prints
//! `NOT MEASURED` and checks only what does not depend on time, never a
//! guess.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// The host's load over one measurement. Sampled from any thread: the
/// probes of a measurement sample it where they wait.
#[derive(Debug)]
pub struct HostLoad {
    load: Option<f64>,
    bound: f64,
    most: AtomicU32,
    unread: AtomicBool,
}

impl HostLoad {
    /// The measurement starts: the load average now, the bound, the first
    /// runnable sample.
    pub fn start() -> Self {
        let load = std::fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|s| s.split_whitespace().next()?.parse().ok());
        let bound = std::env::var("WINDOW_BENCH_MAX_LOAD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| {
                std::thread::available_parallelism().map_or(1, |n| n.get()) as f64 / 5.0
            });
        let host = Self {
            load,
            bound,
            most: AtomicU32::new(0),
            unread: AtomicBool::new(false),
        };
        host.sample();
        host
    }

    /// The tasks running or waiting for a CPU now, the measuring thread
    /// among them (`/proc/loadavg`'s fourth field).
    pub fn sample(&self) {
        let runnable = std::fs::read_to_string("/proc/loadavg").ok().and_then(|s| {
            let field = s.split_whitespace().nth(3)?;
            field.split('/').next()?.parse::<u32>().ok()
        });
        match runnable {
            Some(n) => {
                self.most.fetch_max(n, Ordering::Relaxed);
            }
            None => self.unread.store(true, Ordering::Relaxed),
        }
    }

    /// Whether the measurement counts; printed when it does not, so a run
    /// says what it did not measure.
    pub fn measured(&self, what: &str) -> bool {
        self.sample();
        let unread = self.unread.load(Ordering::Relaxed);
        let most = self.most.load(Ordering::Relaxed);
        let quiet = !unread
            && self.load.is_some_and(|l| l <= self.bound)
            && f64::from(most) <= self.bound + 1.0;
        if !quiet {
            println!(
                "NOT MEASURED: {what}: load {:?} against {:.1}, at most {most} runnable{}",
                self.load,
                self.bound,
                if unread { ", unread" } else { "" }
            );
        }
        quiet
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate's two arms, as 1473's controls: a bound above any load
    /// measures, a bound below the measuring thread itself does not.
    #[test]
    fn the_gate_measures_only_within_its_bound() {
        let host = HostLoad {
            load: Some(0.5),
            bound: 1_000.0,
            most: AtomicU32::new(0),
            unread: AtomicBool::new(false),
        };
        host.sample();
        assert!(host.measured("a quiet host"));
        let busy = HostLoad {
            load: Some(17.5),
            bound: 2.0,
            most: AtomicU32::new(0),
            unread: AtomicBool::new(false),
        };
        assert!(!busy.measured("load 17.5"));
        let burst = HostLoad {
            load: Some(0.5),
            bound: 2.0,
            most: AtomicU32::new(9),
            unread: AtomicBool::new(false),
        };
        assert!(!burst.measured("nine runnable"));
        let blind = HostLoad {
            load: Some(0.5),
            bound: 1_000.0,
            most: AtomicU32::new(0),
            unread: AtomicBool::new(true),
        };
        assert!(!blind.measured("no /proc/loadavg"));
    }
}

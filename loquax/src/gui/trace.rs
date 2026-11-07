//! What the window's map did per event and per frame (1431): the pointer
//! events it was handed, the frames it asked the worker for, and the
//! frames that came back with what they cost. Counted always, for the
//! tests; written as lines to the file `LOQUAX_MAP_TRACE` names, for a
//! look at a window a person drags:
//! `LOQUAX_MAP_TRACE=/tmp/map.trace loquax gui`, then
//! `scripts/map-trace-summary.py /tmp/map.trace`.
//!
//! A line is `<ms since the window opened> <what> <detail>`: `event down|move|up|wheel`,
//! `ask <zoom in thousandths> moving|rest`, `painted <worker ms> <missing tiles>`.

use std::io::Write;
use std::time::Instant;

/// The map's counts, and the file the lines go to.
pub struct Trace {
    /// Pointer events handed to the map.
    pub events: u64,
    /// Frames asked of the worker, and of them those while the view
    /// moved.
    pub asked: u64,
    pub asked_moving: u64,
    /// Frames the worker drew and the window took.
    pub painted: u64,
    out: Option<std::io::BufWriter<std::fs::File>>,
    origin: Instant,
}

impl Trace {
    /// Counting, and writing where `LOQUAX_MAP_TRACE` says; a file that
    /// cannot be made is said once and not written.
    pub fn from_env() -> Self {
        let out =
            std::env::var_os("LOQUAX_MAP_TRACE").and_then(|path| {
                match std::fs::File::create(&path) {
                    Ok(file) => Some(std::io::BufWriter::new(file)),
                    Err(error) => {
                        tracing::warn!("no map trace at {}: {error}", path.to_string_lossy());
                        None
                    }
                }
            });
        Self {
            events: 0,
            asked: 0,
            asked_moving: 0,
            painted: 0,
            out,
            origin: Instant::now(),
        }
    }

    fn line(&mut self, what: std::fmt::Arguments<'_>) {
        let ms = self.origin.elapsed().as_secs_f64() * 1e3;
        if let Some(out) = &mut self.out {
            let _ = writeln!(out, "{ms:.3} {what}");
            let _ = out.flush();
        }
    }

    pub fn event(&mut self, pointer: &super::Pointer) {
        self.events += 1;
        let what = match pointer {
            super::Pointer::Down(_) => "down",
            super::Pointer::Move(_) => "move",
            super::Pointer::Up => "up",
            super::Pointer::Wheel(..) => "wheel",
        };
        self.line(format_args!("event {what}"));
    }

    pub fn asked(&mut self, order: &super::Order) {
        self.asked += 1;
        self.asked_moving += u64::from(order.moving);
        let how = match order.moving {
            true => "moving",
            false => "rest",
        };
        self.line(format_args!("ask {} {how}", order.view.zoom_milli()));
    }

    pub fn painted(&mut self, shown: &super::Shown) {
        self.painted += 1;
        self.line(format_args!("painted {:.2} {}", shown.ms, shown.missing));
    }
}

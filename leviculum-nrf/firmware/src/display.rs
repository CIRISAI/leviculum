//! The status display's page and power (Codeberg #296; moved here in #469
//! 980).
//!
//! Once a second the task composes the status page from what the board
//! knows, paints it into the [`Panel`]'s frame and pushes it, skipping the
//! push when nothing on the page changed ([`FrameKey`]). On a panel with
//! power control it also owns the one-minute timeout: the panel boots lit,
//! goes dark [`DISPLAY_WAKE_MS`](leviculum_screen::power::DISPLAY_WAKE_MS)
//! after boot or after the last press, and any press, raised by
//! [`crate::button`] on `board_state::DISPLAY_WAKE`, lights it again
//! ([`DisplayWake`]). A panel without power control (the T114's ST7789)
//! stays lit. The drivers, the bus and the chip probe stay with the bind
//! crate (`leviculum_nrf::display`, `leviculum_nrf::st7789`).
//!
//! # What the page reads
//!
//! | Line | Source |
//! |---|---|
//! | title, `ID:` | the bind crate, once at start: the board's name and its identity hash |
//! | `R`/`T`, `B`, `S` | [`StatusFacts`], filled by the bind crate's backends once per lit frame tick |
//! | `Bat:`, `GPS:` | [`SensorLines`], read off `board_state` ([`sensor_lines`]) |
//!
//! # One registered instance
//!
//! The task is [`Display`], over a panel, the two readings and the wake
//! signal, so a host test holds one over `fakes::RecordingPanel` and a
//! signal of its own. The board's one instance is the one [`run`] builds.

use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use leviculum_screen::power::{apply_power, DisplayWake};
use leviculum_screen::{ident_short, FrameKey, StatusModel};

pub use leviculum_screen::{BatteryStatus, GnssStatus};

use crate::seams::{Clock, Panel};

/// How often the page is composed, and how often a dark panel looks for a
/// press it has not been woken by.
const FRAME_TICK_MS: u64 = 1_000;

/// What the page shows that only the bind crate's backends know: the LoRa
/// frame counters, the BLE links and the propagation store. Filled once
/// per lit frame tick by the function the bind crate hands to [`run`]; a
/// value handed down, so this crate names none of those backends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusFacts {
    /// LoRa frames received and sent since boot.
    pub rx: u32,
    pub tx: u32,
    /// BLE links up, `None` on a board without BLE.
    pub ble_peers: Option<u8>,
    /// Message records in the propagation store, `None` when the board is
    /// not a propagation node.
    pub pn_fill: Option<u16>,
}

/// The battery and GNSS lines, as the page renders them.
#[derive(Clone, Copy, Debug)]
pub struct SensorLines {
    pub battery: BatteryStatus,
    pub gnss: GnssStatus,
}

/// The battery and GNSS lines off `board_state`, or their "no feature"
/// forms on a board without the capability. Read through the watches'
/// own `try_get`, not a receiver: the page wants the latest value, and a
/// receiver would take a slot of the watch's capacity.
pub fn sensor_lines() -> SensorLines {
    #[cfg(feature = "battery")]
    let battery = match crate::board_state::BATTERY_STATE.try_get() {
        Some(b) if b.voltage_mv > 0 => BatteryStatus::Data {
            percent: b.percent,
            voltage_mv: b.voltage_mv,
        },
        _ => BatteryStatus::NoData,
    };
    #[cfg(not(feature = "battery"))]
    let battery = BatteryStatus::FeatureOff;

    // The presence tri-state (#240) drives the GPS line: NoHardware
    // renders "check wiring", and the fix/search label follows the
    // hysteresis-held policy state rather than the raw per-sentence
    // receiver flag, so the panel agrees with what every consumer of
    // GNSS_PRESENCE acts on. Sats and coordinates still come from the
    // GNSS_FIX snapshot. Before the first settle (watch empty) the line
    // stays in the "init" state.
    #[cfg(feature = "gnss")]
    let gnss = {
        use crate::board_state::{GnssFix, GnssPresence, GNSS_FIX, GNSS_PRESENCE};
        match GNSS_PRESENCE.try_get() {
            Some(p) if p.state == GnssPresence::NoHardware => GnssStatus::NoHardware,
            Some(p) => {
                let f = GNSS_FIX.try_get().unwrap_or(GnssFix::empty());
                GnssStatus::Data {
                    sats: f.sat_in_use,
                    valid: p.state == GnssPresence::Fix,
                    coords: f.latitude.zip(f.longitude),
                }
            }
            None => GnssStatus::NoData,
        }
    };
    #[cfg(not(feature = "gnss"))]
    let gnss = GnssStatus::FeatureOff;

    SensorLines { battery, gnss }
}

/// The board's display, forever: the page over `panel`, its title line
/// `title`, the identity `identity_hash`, the bind crate's `facts`, the
/// sensor lines off `board_state` and the press signal
/// `board_state::DISPLAY_WAKE`.
pub async fn run<P: Panel, C: Clock>(
    panel: P,
    clock: C,
    title: &str,
    identity_hash: &[u8; 16],
    facts: fn() -> StatusFacts,
) -> ! {
    Display::new(
        panel,
        title,
        identity_hash,
        facts,
        sensor_lines,
        &crate::board_state::DISPLAY_WAKE,
    )
    .run(&clock)
    .await
}

/// The page and the power policy over one panel.
pub struct Display<'a, P> {
    panel: P,
    title: &'a str,
    id_short: heapless::String<10>,
    facts: fn() -> StatusFacts,
    sensors: fn() -> SensorLines,
    wake: &'a Signal<CriticalSectionRawMutex, ()>,
}

impl<'a, P: Panel> Display<'a, P> {
    /// A display over `panel`, lit from the moment [`run`](Self::run)
    /// starts.
    pub fn new(
        panel: P,
        title: &'a str,
        identity_hash: &[u8; 16],
        facts: fn() -> StatusFacts,
        sensors: fn() -> SensorLines,
        wake: &'a Signal<CriticalSectionRawMutex, ()>,
    ) -> Self {
        Self {
            panel,
            title,
            id_short: ident_short(identity_hash),
            facts,
            sensors,
            wake,
        }
    }

    /// The panel, for a test to read what it was given.
    #[cfg(test)]
    pub fn panel(&self) -> &P {
        &self.panel
    }

    /// Compose, paint and push against `clock`, forever.
    pub async fn run<C: Clock>(&mut self, clock: &C) -> ! {
        let power_control = self.panel.has_power_control();
        let mut wake = DisplayWake::new(clock.now_ms());
        let mut display_on = true;
        // A press the dark-display wait below took off the signal, handed
        // to the next iteration.
        let mut pressed = false;
        // When nothing on the page changed since the last push, the push
        // is skipped.
        let mut last_key: Option<FrameKey> = None;
        let mut tick: u32 = 0;

        loop {
            tick = tick.wrapping_add(1);
            let heartbeat = (tick / 5) & 1 != 0; // toggles every 5 seconds

            // A press restarts the minute and lights a dark display;
            // without one, a lit display goes dark once the minute is up.
            // An unchanged request is not sent and a failed one keeps the
            // old state: `leviculum_screen::power::apply_power`.
            pressed |= self.wake.try_take().is_some();
            if power_control {
                let now_ms = clock.now_ms();
                let request = if core::mem::take(&mut pressed) {
                    wake.press(now_ms, display_on)
                } else {
                    wake.timeout(now_ms, display_on)
                };
                if let Some(req) = request {
                    let panel = &mut self.panel;
                    let watch = &crate::thread_watch::THREAD;
                    match apply_power(req, &mut display_on, |on| {
                        // A synchronous bus write on some panels: named, so
                        // a stall inside it is reported by where it is.
                        watch.enter(crate::thread_watch::Site::PanelPower, now_ms as u32);
                        let done = panel.set_power(on);
                        watch.leave();
                        done
                    }) {
                        None => {}
                        Some(Err(e)) => crate::log::log_fmt(
                            "[DISP] ",
                            format_args!("set_display_on({}) failed: {:?}", req, e),
                        ),
                        Some(Ok(())) => crate::log::log_fmt(
                            "[DISP] ",
                            format_args!("display power → {}", if req { "on" } else { "off" }),
                        ),
                    }
                }
            } else {
                pressed = false;
            }

            // While the screen is off there is no point composing or
            // pushing a frame. Wait for a press, so a dark display lights
            // as soon as the button reports one, or a tick, whichever is
            // first.
            if !display_on {
                let woke = select(
                    clock.wait_until(clock.now_ms() + FRAME_TICK_MS),
                    self.wake.wait(),
                )
                .await;
                pressed = matches!(woke, Either::Second(()));
                continue;
            }

            let facts = (self.facts)();
            let sensors = (self.sensors)();
            let model = StatusModel {
                title: self.title,
                id_short: self.id_short.as_str(),
                rx: facts.rx,
                tx: facts.tx,
                battery: sensors.battery,
                gnss: sensors.gnss,
                ble_peers: facts.ble_peers,
                pn_fill: facts.pn_fill,
                heartbeat,
            };

            let key = model.key();
            if last_key.as_ref() != Some(&key) {
                // The painter clears, draws the six lines and sets the
                // heartbeat dot three pixels in from the panel's width.
                let _ = model.paint(self.panel.frame(), P::WIDTH);
                let watch = &crate::thread_watch::THREAD;
                watch.enter(crate::thread_watch::Site::PanelFlush, clock.now_ms() as u32);
                let flushed = self.panel.flush().await;
                watch.leave();
                if let Err(e) = flushed {
                    crate::log::log_fmt("[DISP] ", format_args!("flush failed: {:?}", e));
                }
                last_key = Some(key);
            }

            clock.wait_until(clock.now_ms() + FRAME_TICK_MS).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::{drive, FakeClock, RecordingPanel, RecordingPanelFb};
    use alloc::vec;
    use alloc::vec::Vec;
    use leviculum_screen::power::DISPLAY_WAKE_MS;

    const IDENTITY: [u8; 16] = [
        0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0, 0, 0, 0, 0, 0, 0, 0,
    ];

    fn facts() -> StatusFacts {
        StatusFacts {
            rx: 12,
            tx: 7,
            ble_peers: Some(2),
            pn_fill: Some(5),
        }
    }

    fn sensors() -> SensorLines {
        SensorLines {
            battery: BatteryStatus::Data {
                percent: Some(81),
                voltage_mv: 4_012,
            },
            gnss: GnssStatus::NoHardware,
        }
    }

    /// What one run left on the panel: its power writes and the times of
    /// its pushes.
    struct Outcome {
        power: Vec<(u64, bool)>,
        pushes: Vec<u64>,
    }

    /// Run a display over a panel with or without power control until
    /// `until_ms`, raising the wake signal at each of `presses`.
    fn show(power_control: bool, presses: &[u64], until_ms: u64) -> Outcome {
        let clock = FakeClock::new();
        let wake = Signal::new();
        let panel = RecordingPanel::new(&clock, power_control);
        let mut display = Display::new(panel, "test", &IDENTITY, facts, sensors, &wake);
        let mut presses: Vec<u64> = presses.to_vec();
        drive(display.run(&clock), &clock, until_ms, |now| {
            while presses.first().is_some_and(|at| *at <= now) {
                presses.remove(0);
                wake.signal(());
            }
            presses.first().copied()
        });
        Outcome {
            power: display.panel().power(),
            pushes: display.panel().pushes().iter().map(|(t, _)| *t).collect(),
        }
    }

    // Lit at 0 (`DisplayWake::new(clock.now_ms())`), dark at the first
    // tick at or past `DISPLAY_WAKE_MS` (`timeout`): 60 s, the ticks
    // falling on whole seconds. No push while dark.
    #[test]
    fn a_lit_display_goes_dark_after_a_minute_without_a_press() {
        let out = show(true, &[], 120_000);
        assert_eq!(out.power, vec![(DISPLAY_WAKE_MS, false)]);
        assert!(out.pushes.iter().all(|t| *t < DISPLAY_WAKE_MS));
    }

    // Dark at 60 s; a press at 100.5 s ends the dark wait at once (the
    // `select` on the signal), the next pass asks `press` with the
    // display off: on at 100.5 s. Dark again a minute later: the ticks
    // now fall on 100.5 + n s, so 160.5 s.
    #[test]
    fn a_press_lights_a_dark_display_at_once_and_for_a_minute() {
        let out = show(true, &[100_500], 200_000);
        assert_eq!(
            out.power,
            vec![
                (DISPLAY_WAKE_MS, false),
                (100_500, true),
                (100_500 + DISPLAY_WAKE_MS, false),
            ]
        );
    }

    // A press at 30 s on a lit display asks for no change (`press_target`)
    // but moves the dark moment to 90 s; a second at 80 s moves it to
    // 140 s. Without the restart the display would go dark at 60 s.
    #[test]
    fn a_second_press_restarts_the_minute() {
        let out = show(true, &[30_000, 80_000], 200_000);
        assert_eq!(out.power, vec![(80_000 + DISPLAY_WAKE_MS, false)]);
    }

    // The T114's ST7789: no power control, so neither the minute nor a
    // press ever reaches the panel, and the page keeps being pushed.
    #[test]
    fn a_panel_without_power_control_never_powers_off() {
        let out = show(false, &[30_000], 300_000);
        assert_eq!(out.power, vec![]);
        assert!(out.pushes.iter().any(|t| *t > 4 * DISPLAY_WAKE_MS));
    }

    // The first push is the page of the facts given, at 0: the six lines
    // `StatusModel::lines` writes for them, quoted, and no heartbeat dot
    // (`tick / 5` is 0 on the first pass). The frame is compared pixel
    // for pixel with those lines painted at the panel's width.
    #[test]
    fn the_page_shows_the_facts_it_was_given() {
        let clock = FakeClock::new();
        let wake = Signal::new();
        let panel = RecordingPanel::new(&clock, true);
        let mut display = Display::new(panel, "test", &IDENTITY, facts, sensors, &wake);
        drive(display.run(&clock), &clock, 0, |_| None);

        let expected = StatusModel {
            title: "test",
            id_short: "abcdef0123",
            rx: 12,
            tx: 7,
            battery: sensors().battery,
            gnss: sensors().gnss,
            ble_peers: Some(2),
            pn_fill: Some(5),
            heartbeat: false,
        };
        assert_eq!(
            expected
                .lines()
                .map(|l| alloc::string::String::from(l.as_str())),
            [
                "test",
                "ID: abcdef0123",
                "R12   T7    B2 S5",
                "Bat:  81% 4.01V",
                "GPS: no receiver",
                "(check wiring)",
            ]
        );
        let mut painted = RecordingPanelFb::new();
        let _ = expected.paint(&mut painted, RecordingPanel::WIDTH);
        let pushes = display.panel().pushes();
        assert_eq!(pushes.len(), 1);
        assert_eq!(pushes[0].0, 0);
        assert_eq!(pushes[0].1.diff(&painted), None, "the pushed frame");
    }
}

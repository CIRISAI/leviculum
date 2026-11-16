//! The position announce's rules (order 1301, Refs #232): who may read it,
//! when it moves ahead of the cadence, and when it thins out.
//!
//! A board that shares its position announces a third destination,
//! `lev.position` (`leviculum_lxmf::lev_position`), on the one cadence the
//! other two ride ([`crate::AnnounceCadence`], slot
//! [`crate::AnnounceSlot::Position`]): every five minutes while it moves,
//! once an hour while it stands still, at once on the triggers of rules 3
//! and 4. Three rules are its own, and they are here:
//!
//! 1. **Privacy** ([`PositionShare::resolve`]). An announce is plaintext.
//!    By default a board shares only where it already sends its position
//!    away by LXMF telemetry, the tracker role, and then only to the same
//!    recipient, sealed to the owner's identity. A plain mesh node shares
//!    nothing unless configured, and "everyone nearby" is an explicit
//!    opt-in.
//! 2. **The 25 m advance** ([`POSITION_ADVANCE_DISTANCE_M`]). A fix that
//!    moved that far since the last position announce brings the next one
//!    forward to the earliest moment the duty budget allows, never earlier.
//! 3. **Thinning** ([`thinned_interval_ms`]). When the channel fills, a
//!    position announce is the first thing a mesh can do without, so its
//!    interval stretches with the channel load the CHTM measures, above
//!    the share of the channel the announce cap gives announces.

use crate::{MOVEMENT_SAMPLE_INTERVAL_MS, STILL_ANNOUNCE_INTERVAL_MS};

/// How soon after boot the first position announce may fire: ten seconds
/// after the board's own (`PERIODIC_ANNOUNCE_INITIAL_DELAY_MS`, 40 s), as
/// that one is ten after the propagation role's pair, so each of the boot
/// announces has its airtime window to itself.
pub const POSITION_ANNOUNCE_INITIAL_DELAY_MS: u64 = 50 * 1_000;

/// How far a fix has to move from the last position announced before the
/// next one is brought forward: 25 m (order 1301).
///
/// Half the movement proof's 50 m (`PolicyParams::TRACKER.min_distance_m`),
/// because the two answer different questions: the proof asks whether the
/// board moves at all and must not be fooled by a wandering receiver, this
/// asks whether the map's pin is still where the board is. It only ever
/// acts on a board the proof already let out of the hourly floor, or on a
/// still board whose receiver jumped, and both are bounded by the budget.
pub const POSITION_ADVANCE_DISTANCE_M: u32 = 25;

/// How soon a position announce that found no fix looks again: the
/// movement sample's own period, one minute. A board that loses the sky
/// is not silent on the other two announces, and the position comes back
/// within a minute of the fix.
pub const POSITION_NO_FIX_RETRY_MS: u64 = MOVEMENT_SAMPLE_INTERVAL_MS;

/// The share of an interface's capacity the announce cap gives announces,
/// percent: core's `DEFAULT_ANNOUNCE_CAP_PERCENT`, the reference's 2 %
/// (`ANNOUNCE_CAP = 2`, `reference/Reticulum/RNS/Reticulum.py:114`). Restated
/// because this crate does not depend on the core; the firmware asserts
/// the two equal at compile time.
pub const ANNOUNCE_CAP_PERCENT: u64 = 2;

/// The channel load, in the CHTM's hundredths of a percent, above which
/// position announces thin out: the announce cap itself.
///
/// Below it the whole channel carries less than the cap allows announces
/// alone, so nothing a position announce adds can push the announces past
/// the cap. Above it they might, and the interval stretches in proportion,
/// which holds the mesh's position traffic constant as boards are added
/// instead of growing with their number.
pub const POSITION_THIN_LOAD_E4: u64 = ANNOUNCE_CAP_PERCENT * 100;

/// The position interval at a measured channel load: `base_ms` while the
/// load is at or below [`POSITION_THIN_LOAD_E4`], stretched by
/// `load / POSITION_THIN_LOAD_E4` above it, and never beyond the hourly
/// floor ([`STILL_ANNOUNCE_INTERVAL_MS`]), which is what a still board
/// sends anyway: slow, never silent.
///
/// `channel_load_long_e4` is the CHTM's `channel_load_long`, the hour's
/// mean, not the short window: a burst of traffic must not thin the map
/// for an hour, and a full hour must.
#[must_use]
pub const fn thinned_interval_ms(base_ms: u64, channel_load_long_e4: u16) -> u64 {
    let load = channel_load_long_e4 as u64;
    if load <= POSITION_THIN_LOAD_E4 {
        return base_ms;
    }
    let stretched = base_ms.saturating_mul(load) / POSITION_THIN_LOAD_E4;
    let ceiling = if base_ms > STILL_ANNOUNCE_INTERVAL_MS {
        base_ms
    } else {
        STILL_ANNOUNCE_INTERVAL_MS
    };
    if stretched > ceiling {
        ceiling
    } else {
        stretched
    }
}

/// What a board was told about sharing its position (`lnflash
/// --share-position`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShareSetting {
    /// Nobody said: [`PositionShare::resolve`] decides by role.
    #[default]
    Default,
    /// Never.
    Off,
    /// Sealed to the owner, the telemetry target's identity.
    Owner,
    /// In the clear, to everyone in range.
    Everyone,
}

impl ShareSetting {
    /// The wire byte of the setting (`TYPE_POSITION_SHARE`'s payload and
    /// the flash record).
    #[must_use]
    pub const fn wire(self) -> u8 {
        match self {
            Self::Default => 0,
            Self::Off => 1,
            Self::Owner => 2,
            Self::Everyone => 3,
        }
    }

    /// The setting a wire byte names, `None` for one that names none.
    #[must_use]
    pub const fn from_wire(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Default),
            1 => Some(Self::Off),
            2 => Some(Self::Owner),
            3 => Some(Self::Everyone),
            _ => None,
        }
    }

    /// The token in the `[POSITION]` lines and in `lnflash`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Off => "off",
            Self::Owner => "owner",
            Self::Everyone => "everyone",
        }
    }
}

/// Who reads the position announce, as the board runs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionShare {
    /// No position announce.
    Off,
    /// Sealed to the owner's identity.
    Owner,
    /// In the clear.
    Everyone,
}

impl PositionShare {
    /// The privacy rule.
    ///
    /// `tracker_reports` is whether the board already sends its position
    /// away by LXMF telemetry as a tracker: a telemetry target set with
    /// the tracker profile. Only there is the default on, and sealed to
    /// that same recipient, so the default tells nobody anything the
    /// board's telemetry does not already tell them. A station's
    /// telemetry carries a position too, but a station is a node that does
    /// not move, and a map of the mesh's fixed nodes is the owner's choice
    /// to make, not the default's.
    #[must_use]
    pub const fn resolve(setting: ShareSetting, tracker_reports: bool) -> Self {
        match setting {
            ShareSetting::Default if tracker_reports => Self::Owner,
            ShareSetting::Default | ShareSetting::Off => Self::Off,
            ShareSetting::Owner => Self::Owner,
            ShareSetting::Everyone => Self::Everyone,
        }
    }

    /// How many announce-sized frames one position announce costs, for the
    /// duty budget's set (`DutyBudget::frames_per_set`): the sealed form is
    /// 297 bytes on the air at our defaults and LoRa carries 254 a frame,
    /// so it goes out as two; the plain form is 209 bytes and goes as one
    /// (`leviculum-lxmf/tests/lev_position_airtime.rs` pins both).
    #[must_use]
    pub const fn frames(self) -> u32 {
        match self {
            Self::Off => 0,
            Self::Owner => 2,
            Self::Everyone => 1,
        }
    }

    /// The token in the `[POSITION]` lines.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Owner => "owner",
            Self::Everyone => "everyone",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_shares_only_where_a_tracker_already_reports() {
        assert_eq!(
            PositionShare::resolve(ShareSetting::Default, true),
            PositionShare::Owner
        );
        assert_eq!(
            PositionShare::resolve(ShareSetting::Default, false),
            PositionShare::Off
        );
        // A configured setting wins over the role, both ways.
        assert_eq!(
            PositionShare::resolve(ShareSetting::Off, true),
            PositionShare::Off
        );
        assert_eq!(
            PositionShare::resolve(ShareSetting::Owner, false),
            PositionShare::Owner
        );
        // The clear form is never a default.
        for tracker in [false, true] {
            assert_ne!(
                PositionShare::resolve(ShareSetting::Default, tracker),
                PositionShare::Everyone
            );
            assert_eq!(
                PositionShare::resolve(ShareSetting::Everyone, tracker),
                PositionShare::Everyone
            );
        }
    }

    #[test]
    fn the_setting_round_trips_its_wire_byte_and_refuses_others() {
        for setting in [
            ShareSetting::Default,
            ShareSetting::Off,
            ShareSetting::Owner,
            ShareSetting::Everyone,
        ] {
            assert_eq!(ShareSetting::from_wire(setting.wire()), Some(setting));
        }
        assert_eq!(ShareSetting::from_wire(4), None);
        assert_eq!(ShareSetting::from_wire(0xFF), None);
    }

    #[test]
    fn the_interval_thins_above_the_cap_in_proportion_and_stops_at_the_floor() {
        let five = 5 * 60 * 1_000;
        assert_eq!(thinned_interval_ms(five, 0), five);
        assert_eq!(thinned_interval_ms(five, 200), five);
        // Twice the cap's load: twice the interval.
        assert_eq!(thinned_interval_ms(five, 400), 2 * five);
        assert_eq!(thinned_interval_ms(five, 500), five * 5 / 2);
        // Never beyond the hourly floor.
        assert_eq!(
            thinned_interval_ms(five, 10_000),
            STILL_ANNOUNCE_INTERVAL_MS
        );
        // An interval already past the floor (a stretched carrier) is
        // never shortened by thinning.
        let stretched = 2 * STILL_ANNOUNCE_INTERVAL_MS;
        assert_eq!(thinned_interval_ms(stretched, 10_000), stretched);
    }
}

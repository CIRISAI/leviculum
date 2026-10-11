use crate::coords::{Course, Latitude, Longitude, MagneticCourse, Speed};
use crate::datetime::{Date, DateTime, Time};
use crate::modes::{Mode, Status};
use crate::Source;

/// Recommended Minimum Sentence for any GNSS source.
#[derive(Debug, PartialEq, Clone)]
pub struct RMC {
    /// Navigational system.
    pub source: Source,
    /// Date and time of fix in UTC. Always present when the status is
    /// valid; None on a void RMC that leaves it blank (Leviculum change,
    /// see README.md).
    pub datetime: Option<DateTime>,
    /// Latitude in reference datum, mostly WGS-84. Same as datetime.
    pub latitude: Option<Latitude>,
    /// Logitude in reference datum, mostly WGS-84. Same as datetime.
    pub longitude: Option<Longitude>,
    /// Speed over ground. Same as datetime.
    pub speed: Option<Speed>,
    /// Course over ground. Some receivers do not report it when no movement.
    pub course: Option<Course>,
    /// Magnetic course over ground (angle to magnetic North pole). Receiver may not report it.
    pub magnetic: Option<MagneticCourse>,
    /// Receiver's mode of operation.
    pub mode: Mode,
}

impl RMC {
    pub(crate) fn parse<'a>(
        source: Source,
        fields: &mut core::str::Split<'a, char>,
    ) -> Result<Option<Self>, &'static str> {
        let time = Time::parse_from_hhmmss(fields.next())?;
        let status = if let Some(f_status) = fields.next() {
            Status::from_str(f_status)?
        } else {
            return Err("Status field is mandatory for RMC sentence!");
        };
        let latitude = Latitude::parse(fields.next(), fields.next())?;
        let longitude = Longitude::parse(fields.next(), fields.next())?;
        let speed = Speed::parse(fields.next())?;
        let course = Course::parse(fields.next())?;
        let date = Date::parse_from_ddmmyy(fields.next())?;
        let magnetic = MagneticCourse::parse_from_mvar_mdir(&course, fields.next(), fields.next())?;
        let mode = Mode::from_some_str_or_status(fields.next(), &status)?;

        let datetime = DateTime::from_date_and_time(date, time)?;
        // A receiver that has lost its fix sends a void RMC with position,
        // speed and often the date blank; upstream returned nothing for it,
        // so the caller kept the time of the last valid sentence as if the
        // receiver still vouched for it (leviculum#633). A void RMC now
        // parses with whatever it carries; a valid one still needs all four.
        let complete =
            datetime.is_some() && latitude.is_some() && longitude.is_some() && speed.is_some();
        if complete || status == Status::NotValid {
            Ok(Some(RMC {
                source,
                datetime,
                latitude,
                longitude,
                speed,
                course,
                magnetic: magnetic,
                mode,
            }))
        } else {
            Ok(None)
        }
    }
}

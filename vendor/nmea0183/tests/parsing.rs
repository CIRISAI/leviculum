use core::convert::TryFrom;
use nmea0183::coords;
use nmea0183::coords::Hemisphere;
use nmea0183::coords::Latitude;
use nmea0183::coords::Longitude;
use nmea0183::datetime;
use nmea0183::satellite;
use nmea0183::FixType;
use nmea0183::GPSQuality;
#[cfg(feature = "mtk")]
use nmea0183::JammingStatus;
use nmea0183::Mode;
use nmea0183::GGA;
use nmea0183::GLL;
#[cfg(feature = "mtk")]
use nmea0183::PMTKSPF;
use nmea0183::RMC;
use nmea0183::VTG;
use nmea0183::{ParseResult, Parser, Source};

#[test]
#[cfg(feature = "strict")]
fn test_too_long_sentence() {
    let line = "$01234567890123456789012345678901234567890123456789012345678901234567890123456789";
    let mut caught_error = false;
    for result in Parser::new().parse_from_bytes(line.as_bytes()) {
        match result {
            Ok(_) => continue,
            Err("NMEA sentence is too long!") => {
                caught_error = true;
                break;
            }
            Err(_) => panic!("Unexpected error caught in test!"),
        }
    }
    assert!(caught_error);
}

#[test]
#[cfg(not(feature = "strict"))]
fn test_too_long_sentence_non_strict() {
    let line = "$01234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890012345678901234567890";
    let mut caught_error = false;
    for result in Parser::new().parse_from_bytes(line.as_bytes()) {
        match result {
            Ok(_) => continue,
            Err("NMEA sentence is too long!") => {
                caught_error = true;
                break;
            }
            Err(_) => panic!("Unexpected error caught in test!"),
        }
    }
    assert!(caught_error);
}

#[test]
fn test_correct_but_unsupported_source() {
    let mut p = Parser::new();
    let sentence = b"$LCVTG,089.0,T,,,15.2,N,,*67\r\n";
    let mut parsed = false;
    for b in sentence.iter() {
        let r = p.parse_from_byte(*b);
        if r.is_some() {
            assert_eq!(r.unwrap(), Err("Source is not supported!"));
            parsed = true;
            break;
        }
    }
    assert!(parsed);
}

#[test]
fn test_correct_but_unsupported_nmea_block() {
    let mut p = Parser::new();
    let sentence = b"$GPZZZ,,,,,,,,,*61\r\n";
    let mut parsed = false;
    for b in sentence.iter() {
        let r = p.parse_from_byte(*b);
        if r.is_some() {
            assert_eq!(r.unwrap(), Err("Unsupported sentence type."));
            parsed = true;
            break;
        }
    }
    assert!(parsed);
}

#[test]
fn test_stream_slice() {
    let mut p = Parser::new();
    let sentence = b"0,T,,,15.2,N,,,A*12\r\n$GPVTG,089.0,T,,,15.2,N,,,A*12\r\n$GPVTG,089.0,T,,,15.2,N,,,A*12\r\n$GPVTG,089.0,T,";
    let mut parse_count = 0;
    for b in sentence.iter() {
        let r = p.parse_from_byte(*b);
        if r.is_some() {
            assert_eq!(
                r.unwrap(),
                Ok(ParseResult::VTG(Some(VTG {
                    source: Source::GPS,
                    course: Some(From::from(89.0)),
                    magnetic: None,
                    speed: coords::Speed::from_knots(15.2),
                    mode: Mode::Autonomous
                })))
            );
            parse_count += 1;
        }
    }
    assert_eq!(parse_count, 2);
}

#[test]
fn test_correct_vtg() {
    let mut p = Parser::new();
    let sentence = b"$GPVTG,089.0,T,,,15.2,N,,,A*12\r\n";
    let mut parsed = false;
    for b in sentence.iter() {
        let r = p.parse_from_byte(*b);
        if r.is_some() {
            assert_eq!(
                r.unwrap(),
                Ok(ParseResult::VTG(Some(VTG {
                    source: Source::GPS,
                    course: Some(From::from(89.0)),
                    magnetic: None,
                    speed: coords::Speed::from_knots(15.2),
                    mode: Mode::Autonomous
                })))
            );
            parsed = true;
            break;
        }
    }
    assert!(parsed);
}

#[test]
fn test_correct_rmc() {
    let mut p = Parser::new();
    let sentence = b"$GPRMC,125504.049,A,5542.2389,N,03741.6063,E,0.06,25.82,200906,,,A*56\r\n";
    let mut parsed = false;
    for b in sentence.iter() {
        let r = p.parse_from_byte(*b);
        if r.is_some() {
            assert_eq!(
                r.unwrap(),
                Ok(ParseResult::RMC(Some(RMC {
                    source: Source::GPS,
                    datetime: datetime::DateTime {
                        date: datetime::Date {
                            day: 20,
                            month: 9,
                            year: 2006
                        },
                        time: datetime::Time {
                            hours: 12,
                            minutes: 55,
                            seconds: 4.049
                        }
                    },
                    latitude: TryFrom::try_from(55.703981666666664).unwrap(),
                    longitude: TryFrom::try_from(37.69343833333333).unwrap(),
                    speed: coords::Speed::from_knots(0.06),
                    course: Some(From::from(25.82)),
                    magnetic: None,
                    mode: Mode::Autonomous
                })))
            );
            parsed = true;
            break;
        }
    }
    assert!(parsed);
}

#[test]
fn test_correct_gga() {
    let mut p = Parser::new();
    let sentence = b"$GPGGA,145659.00,5956.695396,N,03022.454999,E,2,07,0.6,9.0,M,18.0,M,,*62\r\n";
    let mut parsed = false;
    for b in sentence.iter() {
        let r = p.parse_from_byte(*b);
        if r.is_some() {
            assert_eq!(
                r.unwrap(),
                Ok(ParseResult::GGA(Some(GGA {
                    source: Source::GPS,
                    time: datetime::Time {
                        hours: 14,
                        minutes: 56,
                        seconds: 59.0
                    },
                    latitude: TryFrom::try_from(59.944923266667).unwrap(),
                    longitude: TryFrom::try_from(30.3742499833).unwrap(),
                    gps_quality: GPSQuality::DGPS,
                    sat_in_use: 7,
                    hdop: 0.6,
                    altitude: Some(coords::Altitude { meters: 9.0 }),
                    geoidal_separation: Some(18.0),
                    age_dgps: None,
                    dgps_station_id: None
                })))
            );
            parsed = true;
            break;
        }
    }
    assert!(parsed);
}

#[test]
fn test_correct_gga_without_altitude() {
    let mut p = Parser::new();
    let sentence = b"$GPGGA,160545,5008.6263,N,01422.4224,E,1,03,3.6,,M,45.0,M,,*61\r\n";
    let mut parsed = false;
    for b in sentence.iter() {
        let r = p.parse_from_byte(*b);
        if r.is_some() {
            assert_eq!(
                r.unwrap(),
                Ok(ParseResult::GGA(Some(GGA {
                    source: Source::GPS,
                    time: datetime::Time {
                        hours: 16,
                        minutes: 5,
                        seconds: 45.0
                    },
                    latitude: Latitude {
                        degrees: 50,
                        minutes: 8,
                        seconds: 37.578,
                        hemisphere: Hemisphere::North
                    },
                    longitude: Longitude {
                        degrees: 14,
                        minutes: 22,
                        seconds: 25.344,
                        hemisphere: Hemisphere::East
                    },
                    gps_quality: GPSQuality::GPS,
                    sat_in_use: 3,
                    hdop: 3.6,
                    altitude: None,
                    geoidal_separation: Some(45.0),
                    age_dgps: None,
                    dgps_station_id: None
                })))
            );
            parsed = true;
            break;
        }
    }
    assert!(parsed);
}

#[test]
fn test_correct_rmc2() {
    let mut p = Parser::new();
    let sentence = b"$GPRMC,113650.0,A,5548.607,S,03739.387,W,000.01,255.6,210403,08.7,E*66\r\n";
    let mut parsed = false;
    for b in sentence.iter() {
        let r = p.parse_from_byte(*b);
        if r.is_some() {
            assert_eq!(
                r.unwrap(),
                Ok(ParseResult::RMC(Some(RMC {
                    source: Source::GPS,
                    datetime: datetime::DateTime {
                        date: datetime::Date {
                            day: 21,
                            month: 4,
                            year: 2003
                        },
                        time: datetime::Time {
                            hours: 11,
                            minutes: 36,
                            seconds: 50.0
                        }
                    },
                    latitude: TryFrom::try_from(-55.810116666666666).unwrap(),
                    longitude: TryFrom::try_from(-37.65645).unwrap(),
                    speed: coords::Speed::from_knots(0.01),
                    course: Some(From::from(255.6)),
                    magnetic: Some(From::from(246.90001)),
                    mode: Mode::Autonomous
                })))
            );
            parsed = true;
            break;
        }
    }
    assert!(parsed);
}

#[test]
fn test_correct_gll() {
    let mut p = Parser::new();
    let b = b"$GPGLL,4916.45,N,12311.12,W,225444,A*31\r\n";
    {
        let mut iter = p.parse_from_bytes(&b[..]);
        assert_eq!(
            iter.next().unwrap(),
            Ok(ParseResult::GLL(Some(GLL {
                source: Source::GPS,
                time: datetime::Time {
                    hours: 22,
                    minutes: 54,
                    seconds: 44.0
                },
                latitude: TryFrom::try_from(49.2741666667).unwrap(),
                longitude: TryFrom::try_from(-123.18533333334).unwrap(),
                mode: Mode::Autonomous
            })))
        );
    }
}

#[test]
fn test_correct_gsv() {
    let mut p = Parser::new();
    let b = b"$GPGSV,8,1,25,21,44,141,47,15,14,049,44,6,31,255,46,3,25,280,44*75\r\n";
    {
        let mut iter = p.parse_from_bytes(&b[..]);
        let gsv = match iter.next().unwrap().unwrap() {
            ParseResult::GSV(Some(gsv)) => gsv,
            _ => {
                panic!("Unexpected ParseResult variant while parsing GSV data.");
            }
        };
        assert_eq!(gsv.source, Source::GPS);
        assert_eq!(gsv.total_messages_number, 8);
        assert_eq!(gsv.message_number, 1);
        assert_eq!(gsv.sat_in_view, 25);

        assert_eq!(
            gsv.get_in_view_satellites(),
            [
                satellite::Satellite {
                    prn: 21,
                    elevation: 44,
                    azimuth: 141,
                    snr: Some(47)
                },
                satellite::Satellite {
                    prn: 15,
                    elevation: 14,
                    azimuth: 49,
                    snr: Some(44)
                },
                satellite::Satellite {
                    prn: 6,
                    elevation: 31,
                    azimuth: 255,
                    snr: Some(46)
                },
                satellite::Satellite {
                    prn: 3,
                    elevation: 25,
                    azimuth: 280,
                    snr: Some(44)
                }
            ],
        )
    }
}

#[test]
fn test_correct_gsv2() {
    let mut p = Parser::new();
    let b = b"$GLGSV,8,7,25,68,37,284,50*5C\r\n";
    {
        let mut iter = p.parse_from_bytes(&b[..]);
        let gsv = match iter.next().unwrap().unwrap() {
            ParseResult::GSV(Some(gsv)) => gsv,
            _ => {
                panic!("Unexpected ParseResult variant while parsing GSV data.");
            }
        };
        assert_eq!(gsv.source, Source::GLONASS);
        assert_eq!(gsv.total_messages_number, 8);
        assert_eq!(gsv.message_number, 7);
        assert_eq!(gsv.sat_in_view, 25);

        assert_eq!(
            gsv.get_in_view_satellites(),
            [satellite::Satellite {
                prn: 68,
                elevation: 37,
                azimuth: 284,
                snr: Some(50)
            },],
        )
    }
}

#[test]
#[cfg(feature = "mtk")]
fn test_correct_pmtk() {
    let mut p = Parser::new();
    let b = b"$PMTKSPF,2*59\r\n";
    {
        let mut iter = p.parse_from_bytes(&b[..]);
        assert_eq!(
            iter.next().unwrap(),
            Ok(ParseResult::PMTK(Some(PMTKSPF {
                source: Source::MTK,
                jamming_status: JammingStatus::Warning
            })))
        );
    }
}

#[test]
fn test_correct_gsa() {
    let mut p = Parser::new();
    let b = b"$GNGSA,A,3,21,5,29,25,12,10,26,2,,,,,1.2,0.7,1.0*27\r\n";
    {
        let mut iter = p.parse_from_bytes(&b[..]);
        let gsa = match iter.next().unwrap().unwrap() {
            ParseResult::GSA(Some(gsa)) => gsa,
            _ => {
                panic!("Unexpected ParseResult variant while parsing GSA data.");
            }
        };
        assert_eq!(gsa.source, Source::GNSS);
        assert_eq!(gsa.mode, Mode::Autonomous);
        assert_eq!(gsa.fix_type, FixType::Fix3D);
        assert_eq!(gsa.get_fix_satellites_prn(), [21, 5, 29, 25, 12, 10, 26, 2]);
        assert_eq!(gsa.pdop, 1.2);
        assert_eq!(gsa.hdop, 0.7);
        assert_eq!(gsa.vdop, 1.0);
    }
}

#[test]
fn test_correct_zda() {
    let mut p = Parser::new();
    let b = b"$GNZDA,181604.456,12,09,2018,-01,15*6C\r\n";
    {
        let mut iter = p.parse_from_bytes(&b[..]);
        let zda = match iter.next().unwrap().unwrap() {
            ParseResult::ZDA(Some(zda)) => zda,
            _ => {
                panic!("Unexpected ParseResult variant while parsing GSA data.");
            }
        };
        let expected_datetime = datetime::DateTime {
            date: datetime::Date {
                day: 12,
                month: 9,
                year: 2018,
            },
            time: datetime::Time {
                hours: 18,
                minutes: 16,
                seconds: 04.456,
            },
        };
        assert_eq!(zda.source, Source::GNSS);
        assert_eq!(zda.datetime, expected_datetime);
        assert_eq!(zda.offset_hours, Some(-1));
        assert_eq!(zda.offset_minutes, Some(15));
    }
}

#[test]
fn test_correct_zda_2() {
    let mut p = Parser::new();
    let b = b"$GNZDA,181604.456,12,09,2018,,*44\r\n";
    {
        let mut iter = p.parse_from_bytes(&b[..]);
        let zda = match iter.next().unwrap().unwrap() {
            ParseResult::ZDA(Some(zda)) => zda,
            _ => {
                panic!("Unexpected ParseResult variant while parsing GSA data.");
            }
        };
        let expected_datetime = datetime::DateTime {
            date: datetime::Date {
                day: 12,
                month: 9,
                year: 2018,
            },
            time: datetime::Time {
                hours: 18,
                minutes: 16,
                seconds: 04.456,
            },
        };
        assert_eq!(zda.source, Source::GNSS);
        assert_eq!(zda.datetime, expected_datetime);
        assert_eq!(zda.offset_hours, None);
        assert_eq!(zda.offset_minutes, None);
    }
}

#[test]
fn test_parser_iterator() {
    let mut p = Parser::new();
    let b = b"$GPRMC,125504.049,A,5542.2389,N,03741.6063,E,0.06,25.82,200906,,,A*56\r\n";
    {
        let mut iter = p.parse_from_bytes(&b[..]);
        assert_eq!(
            iter.next().unwrap(),
            Ok(ParseResult::RMC(Some(RMC {
                source: Source::GPS,
                datetime: datetime::DateTime {
                    date: datetime::Date {
                        day: 20,
                        month: 9,
                        year: 2006
                    },
                    time: datetime::Time {
                        hours: 12,
                        minutes: 55,
                        seconds: 4.049
                    }
                },
                latitude: TryFrom::try_from(55.703981666666664).unwrap(),
                longitude: TryFrom::try_from(37.69343833333333).unwrap(),
                speed: coords::Speed::from_knots(0.06),
                course: Some(From::from(25.82)),
                magnetic: None,
                mode: Mode::Autonomous
            })))
        );
    }
    let b1 = b"$GPRMC,125504.049,A,5542.2389,N";
    {
        let mut iter = p.parse_from_bytes(&b1[..]);
        assert!(iter.next().is_none());
    }
    let b2 = b",03741.6063,E,0.06,25.82,200906,,,";
    {
        let mut iter = p.parse_from_bytes(&b2[..]);
        assert!(iter.next().is_none());
    }
    let b3 = b"A*56\r\n";
    {
        let mut iter = p.parse_from_bytes(&b3[..]);
        assert_eq!(
            iter.next().unwrap(),
            Ok(ParseResult::RMC(Some(RMC {
                source: Source::GPS,
                datetime: datetime::DateTime {
                    date: datetime::Date {
                        day: 20,
                        month: 9,
                        year: 2006
                    },
                    time: datetime::Time {
                        hours: 12,
                        minutes: 55,
                        seconds: 4.049
                    }
                },
                latitude: TryFrom::try_from(55.703981666666664).unwrap(),
                longitude: TryFrom::try_from(37.69343833333333).unwrap(),
                speed: coords::Speed::from_knots(0.06),
                course: Some(From::from(25.82)),
                magnetic: None,
                mode: Mode::Autonomous
            })))
        );
        assert!(iter.next().is_none());
    }
}

/// Fork-only (Codeberg #386). Every decimal field in this crate used to go
/// through `str::parse::<f32>()` / `::<f64>()`, which links
/// `core::num::dec2flt` — 14.8 KiB of flash in the nRF firmware. `src/fixed`
/// replaced those seven call sites with an integer parse and one division.
///
/// This test is the acceptance for that swap: it parses a corpus of real
/// receiver sentences through the crate, then recomputes what every affected
/// field would have been by running `str::parse` over the same field text
/// and the same downstream arithmetic, and asserts the two agree *bit for
/// bit*. `assert_eq!` on `f32`/`f64` is exact, so a one-ulp drift fails here.
///
/// `str::parse` is only reachable from the test binary; the firmware links
/// neither it nor the table behind it.
///
/// One limit, found by injecting a one-ulp error into each conversion and
/// watching what went red: this test does NOT pin the `f64` path. The
/// minutes field is parsed as `f64` but the crate immediately folds it into
/// an `f32` seconds field, and `Latitude::as_f64` rebuilds the degrees from
/// that `f32`, so a one-ulp `f64` difference is absorbed before it reaches
/// any observable field. Injecting it here left all 17 tests green. What
/// caught it was `fixed::tests::minutes_fields_match_float_parse`, which
/// compares the `f64` at the conversion itself — that is where the `f64`
/// bound is pinned, not here.
mod fixed_point_matches_float_parse {
    // As in `src/fixed.rs`: the crate-wide allowances are for upstream's
    // code, not ours.
    #![warn(clippy::all)]

    use super::*;

    /// GGA and RMC from u-blox, MediaTek and SiRF receivers, plus the four
    /// sentences the tests above already pin. Between them they cover four,
    /// five and six decimals of minutes, a negative geoidal separation, an
    /// absent altitude, a zero speed, and a magnetic variation given as
    /// value plus direction.
    const CORPUS: &[&[u8]] = &[
        b"$GNGGA,083559.00,4717.11437,N,00833.91522,E,1,08,1.01,499.6,M,48.0,M,,*46\r\n",
        b"$GNRMC,083559.00,A,4717.11437,N,00833.91522,E,0.004,77.52,091202,,,A*49\r\n",
        b"$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76\r\n",
        b"$GPRMC,092750.000,A,5321.6802,N,00630.3372,W,0.02,31.66,280511,,,A*43\r\n",
        b"$GPGGA,134658.00,5106.9792,N,11402.3003,W,2,09,1.0,1048.47,M,-16.27,M,08,0429*6F\r\n",
        b"$GPRMC,134658.00,A,5106.9792,N,11402.3003,W,0.0,0.0,130694,003.1,W,A*32\r\n",
        b"$GPGGA,145659.00,5956.695396,N,03022.454999,E,2,07,0.6,9.0,M,18.0,M,,*62\r\n",
        b"$GPRMC,125504.049,A,5542.2389,N,03741.6063,E,0.06,25.82,200906,,,A*56\r\n",
        b"$GPRMC,113650.0,A,5548.607,S,03739.387,W,000.01,255.6,210403,08.7,E*66\r\n",
        b"$GPGGA,160545,5008.6263,N,01422.4224,E,1,03,3.6,,M,45.0,M,,*61\r\n",
    ];

    /// The body of a sentence split into its comma-separated fields, with
    /// the leading `$` and the trailing `*CS\r\n` stripped.
    fn fields(sentence: &[u8]) -> Vec<&str> {
        let text = core::str::from_utf8(sentence).expect("corpus is ASCII");
        let body = text
            .trim_start_matches('$')
            .split('*')
            .next()
            .expect("split always yields one element");
        body.split(',').collect()
    }

    /// What `Latitude::parse` computed before the swap: an `f64` parse of
    /// the minutes, then the degrees/minutes/seconds split on top of it.
    fn oracle_latitude(field: &str) -> (u8, u8, f32) {
        let degrees: u8 = field[..2].parse().expect("degrees");
        let min_sec: f64 = field[2..].parse().expect("minutes");
        let minutes = min_sec as u8;
        (
            degrees,
            minutes,
            ((min_sec - minutes as f64) * 60f64) as f32,
        )
    }

    fn oracle_longitude(field: &str) -> (u8, u8, f32) {
        let degrees: u8 = field[..3].parse().expect("degrees");
        let min_sec: f64 = field[3..].parse().expect("minutes");
        let minutes = min_sec as u8;
        (
            degrees,
            minutes,
            ((min_sec - minutes as f64) * 60f64) as f32,
        )
    }

    fn assert_latitude(actual: &Latitude, field: &str) {
        let (degrees, minutes, seconds) = oracle_latitude(field);
        assert_eq!(actual.degrees, degrees, "latitude degrees from {field}");
        assert_eq!(actual.minutes, minutes, "latitude minutes from {field}");
        assert_eq!(
            actual.seconds.to_bits(),
            seconds.to_bits(),
            "latitude seconds from {field}: {} vs {seconds}",
            actual.seconds
        );
    }

    fn assert_longitude(actual: &Longitude, field: &str) {
        let (degrees, minutes, seconds) = oracle_longitude(field);
        assert_eq!(actual.degrees, degrees, "longitude degrees from {field}");
        assert_eq!(actual.minutes, minutes, "longitude minutes from {field}");
        assert_eq!(
            actual.seconds.to_bits(),
            seconds.to_bits(),
            "longitude seconds from {field}: {} vs {seconds}",
            actual.seconds
        );
    }

    fn assert_f32_field(actual: f32, field: &str, what: &str) {
        let expected: f32 = field.parse().expect("oracle field is a float");
        assert_eq!(
            actual.to_bits(),
            expected.to_bits(),
            "{what} from {field}: {actual} vs {expected}"
        );
    }

    fn check_gga(gga: &GGA, f: &[&str]) {
        assert_latitude(&gga.latitude, f[2]);
        assert_longitude(&gga.longitude, f[4]);
        assert_f32_field(gga.hdop, f[8], "hdop");
        assert_f32_field(gga.time.seconds, &f[1][4..], "time seconds");
        match &gga.altitude {
            Some(altitude) => assert_f32_field(altitude.meters, f[9], "altitude"),
            None => assert!(f[9].is_empty(), "altitude absent but field is {}", f[9]),
        }
        match gga.geoidal_separation {
            Some(separation) => assert_f32_field(separation, f[11], "geoidal separation"),
            None => assert!(f[11].is_empty()),
        }
    }

    fn check_rmc(rmc: &RMC, f: &[&str]) {
        assert_latitude(&rmc.latitude, f[3]);
        assert_longitude(&rmc.longitude, f[5]);
        assert_f32_field(rmc.speed.as_knots(), f[7], "speed");
        assert_f32_field(rmc.datetime.time.seconds, &f[1][4..], "time seconds");
        let course = rmc.course.as_ref().expect("corpus RMC all report course");
        assert_f32_field(course.degrees, f[8], "course");
        // Magnetic course is derived, not parsed: true course minus (east)
        // or plus (west) the parsed variation. Both the variation parse and
        // the arithmetic on top of it have to land on the same bits.
        match &rmc.magnetic {
            Some(magnetic) => {
                let variation: f32 = f[10].parse().expect("variation");
                let expected = match f[11] {
                    "E" => course.degrees - variation,
                    "W" => course.degrees + variation,
                    other => panic!("unexpected magnetic direction {other}"),
                };
                assert_eq!(*magnetic, coords::MagneticCourse::from(expected));
            }
            None => assert!(
                f[10].is_empty() && f[11].is_empty(),
                "magnetic absent but fields are {} {}",
                f[10],
                f[11]
            ),
        }
    }

    #[test]
    fn every_parsed_field_has_the_bits_str_parse_would_have_given() {
        let mut checked = 0usize;
        for sentence in CORPUS {
            let f = fields(sentence);
            let mut parser = Parser::new();
            let results: Vec<_> = parser.parse_from_bytes(sentence).collect();
            assert_eq!(results.len(), 1, "one sentence, one result");
            match results.into_iter().next().expect("one result") {
                Ok(ParseResult::GGA(Some(gga))) => check_gga(&gga, &f),
                Ok(ParseResult::RMC(Some(rmc))) => check_rmc(&rmc, &f),
                other => panic!("corpus sentence did not parse: {other:?}"),
            }
            checked += 1;
        }
        assert_eq!(checked, CORPUS.len(), "the whole corpus was walked");
    }
}

// The crate-wide clippy allowances in Cargo.toml exist for upstream's 2019
// code. This module is ours; hold it to the workspace standard.
#![warn(clippy::all)]

//! Decimal field parsing that does not pull in `core::num::dec2flt`.
//!
//! This module is the reason this crate is vendored. Upstream parses every
//! decimal field with `str::parse::<f32>()` / `::<f64>()`, and those two
//! monomorphisations drag `core::num::dec2flt` into the firmware image: 14.8
//! KiB on thumbv7em, of which `POWER_OF_FIVE_128` alone is 10 416 B. The
//! Eisel-Lemire algorithm behind that table is correct for the full 17
//! significant digits an IEEE double can hold; an NMEA field carries at most
//! ten. So we parse the digits into an integer and divide once.
//!
//! # When this is bit-exact with `str::parse`
//!
//! `parse` returns the float nearest the exact decimal value. So does a
//! `mantissa / 10^scale` division, *provided both operands are exactly
//! representable* — IEEE 754 division is correctly rounded, so one rounding
//! happens, on the same exact quotient, in the same direction. That holds
//! when:
//!
//! * `scale == 0`: no division at all, and `u64 as f32`/`as f64` is itself
//!   correctly rounded for any magnitude; or
//! * `mantissa <= 2^24` and `scale <= 10` for [`Decimal::to_f32`]
//!   (10^10 = 2^10 * 5^10 and 5^10 = 9 765 625 < 2^24, so the divisor is
//!   exact), or `mantissa <= 2^53` and `scale <= 18` for
//!   [`Decimal::to_f64`].
//!
//! Every NMEA field this crate parses is inside those bounds: the widest is a
//! longitude minute field `mmm.mmmmmm`, nine digits into the f64 path. The
//! `fixed_point_matches_float_parse` module in `tests/parsing.rs` asserts the
//! bit equality over a corpus of real sentences, and `fixed::tests` below
//! does the same per field shape.
//!
//! Outside those bounds (a field with more digits than any receiver emits) we
//! shed low digits with round-half-up until the mantissa fits, so the result
//! is within one ulp instead of exact. It never silently changes a fix: the
//! shed digits are below the float's own precision.
//!
//! # What this rejects that `str::parse` accepts
//!
//! Exponent notation (`1e3`), `inf`, `NaN`. No NMEA sentence contains them,
//! and on a receiver they are a corrupt field, which the caller already has
//! to handle — every call site turns `None` into the same "wrong field
//! format" error it returned before.

/// 10^n for n in 0..=10, the range in which the divisor is exact in `f32`.
const POW10_F32: [f32; 11] = [1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10];

/// 10^n for n in 0..=18, the range in which the divisor is exact in `f64`.
/// Stops at 18 because a `u64` mantissa cannot carry more fraction digits
/// than that and still be a number rather than a rounding artefact.
const POW10_F64: [f64; 19] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18,
];

/// Largest mantissa `f32` represents without rounding.
const F32_EXACT: u64 = 1 << 24;

/// Largest mantissa `f64` represents without rounding.
const F64_EXACT: u64 = 1 << 53;

/// A decimal field split into digits and a decimal-point position: the value
/// is `mantissa / 10^scale`, negated if `negative`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) struct Decimal {
    mantissa: u64,
    scale: u32,
    negative: bool,
}

impl Decimal {
    /// Parse `[+-]?digits[.digits]`. Returns `None` for anything else,
    /// including the empty string and a bare sign.
    pub(crate) fn parse(input: &str) -> Option<Decimal> {
        let bytes = input.as_bytes();
        let (negative, digits) = match bytes.first() {
            Some(b'-') => (true, &bytes[1..]),
            Some(b'+') => (false, &bytes[1..]),
            _ => (false, bytes),
        };
        let mut mantissa: u64 = 0u64;
        let mut scale: u32 = 0u32;
        let mut seen_digit = false;
        let mut seen_point = false;
        // Set once the mantissa is full (19 digits). Further fraction digits
        // are dropped: they are 10^-19 of the value, far below the 2^-53 the
        // widest destination can hold.
        let mut saturated = false;
        for &b in digits {
            match b {
                b'0'..=b'9' => {
                    seen_digit = true;
                    if saturated {
                        continue;
                    }
                    let digit = u64::from(b - b'0');
                    match mantissa.checked_mul(10).and_then(|m| m.checked_add(digit)) {
                        Some(next) => {
                            mantissa = next;
                            if seen_point {
                                scale += 1;
                            }
                        }
                        // An integer part wider than a u64 is not a field
                        // this crate can represent in any case.
                        None if !seen_point => return None,
                        None => saturated = true,
                    }
                }
                b'.' if !seen_point => seen_point = true,
                _ => return None,
            }
        }
        if !seen_digit {
            return None;
        }
        Some(Decimal {
            mantissa,
            scale,
            negative,
        })
    }

    /// Shed low digits with round-half-up until the mantissa is at most
    /// `limit`, or until there is no fraction left to shed.
    fn reduce_to(mut self, limit: u64, max_scale: u32) -> Decimal {
        while self.scale > 0 && (self.mantissa > limit || self.scale > max_scale) {
            let dropped = self.mantissa % 10;
            self.mantissa /= 10;
            self.scale -= 1;
            if dropped >= 5 {
                self.mantissa = self.mantissa.saturating_add(1);
            }
        }
        self
    }

    /// The value as `f32`, bit-identical to `str::parse::<f32>()` for every
    /// field shape this crate meets. See the module docs for the bound.
    pub(crate) fn to_f32(self) -> f32 {
        let reduced = self.reduce_to(F32_EXACT, POW10_F32.len() as u32 - 1);
        let magnitude = reduced.mantissa as f32 / POW10_F32[reduced.scale as usize];
        if reduced.negative {
            -magnitude
        } else {
            magnitude
        }
    }

    /// The value as `f64`, bit-identical to `str::parse::<f64>()` for every
    /// field shape this crate meets. See the module docs for the bound.
    pub(crate) fn to_f64(self) -> f64 {
        let reduced = self.reduce_to(F64_EXACT, POW10_F64.len() as u32 - 1);
        let magnitude = reduced.mantissa as f64 / POW10_F64[reduced.scale as usize];
        if reduced.negative {
            -magnitude
        } else {
            magnitude
        }
    }
}

/// Parse a decimal field into `f32`, or `None` if it is not a decimal.
pub(crate) fn f32_from_str(input: &str) -> Option<f32> {
    Decimal::parse(input).map(Decimal::to_f32)
}

/// Parse a decimal field into `f64`, or `None` if it is not a decimal.
pub(crate) fn f64_from_str(input: &str) -> Option<f64> {
    Decimal::parse(input).map(Decimal::to_f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `str::parse` is the oracle. It is only reachable in test builds; the
    /// firmware links neither it nor the table behind it.
    fn assert_same_f32(input: &str) {
        let expected: f32 = input.parse().expect("oracle input must be a float");
        let actual = f32_from_str(input).expect("fixed parse must accept the oracle input");
        assert_eq!(
            actual.to_bits(),
            expected.to_bits(),
            "{input}: fixed {actual} != parse {expected}"
        );
    }

    fn assert_same_f64(input: &str) {
        let expected: f64 = input.parse().expect("oracle input must be a float");
        let actual = f64_from_str(input).expect("fixed parse must accept the oracle input");
        assert_eq!(
            actual.to_bits(),
            expected.to_bits(),
            "{input}: fixed {actual} != parse {expected}"
        );
    }

    #[test]
    fn decimal_splits_digits_from_point() {
        assert_eq!(
            Decimal::parse("56.695396"),
            Some(Decimal {
                mantissa: 56695396,
                scale: 6,
                negative: false
            })
        );
        assert_eq!(
            Decimal::parse("-08.7"),
            Some(Decimal {
                mantissa: 87,
                scale: 1,
                negative: true
            })
        );
        assert_eq!(
            Decimal::parse("15"),
            Some(Decimal {
                mantissa: 15,
                scale: 0,
                negative: false
            })
        );
        assert_eq!(
            Decimal::parse("+.5"),
            Some(Decimal {
                mantissa: 5,
                scale: 1,
                negative: false
            })
        );
    }

    #[test]
    fn decimal_rejects_what_is_not_a_plain_decimal() {
        assert_eq!(Decimal::parse(""), None);
        assert_eq!(Decimal::parse("-"), None);
        assert_eq!(Decimal::parse("."), None);
        assert_eq!(Decimal::parse("a123.0"), None);
        assert_eq!(Decimal::parse("12.3.4"), None);
        assert_eq!(Decimal::parse("1e3"), None);
        assert_eq!(Decimal::parse("inf"), None);
        assert_eq!(Decimal::parse("NaN"), None);
        assert_eq!(Decimal::parse("12 "), None);
    }

    /// Latitude and longitude minutes `mm.mmmm`, the only f64 path, over the
    /// widths receivers actually emit (4 to 6 decimals) and the extremes of
    /// the field range.
    #[test]
    fn minutes_fields_match_float_parse() {
        for input in [
            "42.2389",
            "41.6063",
            "48.607",
            "39.387",
            "56.695396",
            "22.454999",
            "16.45",
            "11.12",
            "00.0000",
            "59.9999",
            "59.999999",
            "0.0",
            "08.7",
        ] {
            assert_same_f64(input);
        }
    }

    /// The f32 fields: altitude, speed, course, magnetic variation, HDOP and
    /// the seconds of a timestamp.
    #[test]
    fn scalar_fields_match_float_parse() {
        for input in [
            "9.0",
            "18.0",
            "45.0",
            "0.06",
            "25.82",
            "000.01",
            "255.6",
            "15.2",
            "089.0",
            "3.6",
            "0.6",
            "1.2",
            "0.7",
            "1.0",
            "04.049",
            "04.456",
            "44.00",
            "59.999",
            "-45.3",
            "0",
            "123.0",
            "1234.5678",
            "9999.9",
        ] {
            assert_same_f32(input);
        }
    }

    /// Every mantissa 0..=999 against every scale the exact range covers,
    /// on both widths and both signs. This is the bound the module claims,
    /// walked rather than argued.
    #[test]
    fn every_short_field_shape_matches_float_parse() {
        let mut buf = [0u8; 24];
        for mantissa in 0..=999u32 {
            for scale in 0..=6usize {
                for negative in [false, true] {
                    let rendered = render(&mut buf, mantissa, scale, negative);
                    assert_same_f32(rendered);
                    assert_same_f64(rendered);
                }
            }
        }
    }

    /// Render `mantissa * 10^-scale` as the digit string a receiver would
    /// emit, so the oracle and the fixed parse see the same characters.
    /// Hand-rolled because the crate is `no_std` in every build, tests
    /// included, so there is no `format!` here.
    fn render(buf: &mut [u8; 24], mantissa: u32, scale: usize, negative: bool) -> &str {
        let mut pos = buf.len();
        let mut rest = mantissa;
        let mut emitted = 0usize;
        loop {
            pos -= 1;
            buf[pos] = b'0' + (rest % 10) as u8;
            rest /= 10;
            emitted += 1;
            if emitted == scale {
                pos -= 1;
                buf[pos] = b'.';
            }
            if rest == 0 && emitted > scale {
                break;
            }
        }
        if negative {
            pos -= 1;
            buf[pos] = b'-';
        }
        core::str::from_utf8(&buf[pos..]).expect("rendered digits are ASCII")
    }

    /// Seven significant digits is the last width `f32` holds exactly; eight
    /// crosses into the reduce path, where the result is within one ulp but
    /// no longer promised bit-identical.
    #[test]
    fn f32_reduce_path_stays_within_one_ulp() {
        let input = "1234.56789";
        let expected: f32 = input.parse().expect("oracle");
        let actual = f32_from_str(input).expect("fixed");
        let ulp = (expected.to_bits() as i64 - actual.to_bits() as i64).abs();
        assert!(ulp <= 1, "{input}: fixed {actual} vs parse {expected}");
    }

    /// A mantissa past `u64` drops its tail rather than failing: the dropped
    /// digits are 10^-19 of the value.
    #[test]
    fn overlong_fraction_saturates_instead_of_failing() {
        let input = "1.2345678901234567890123";
        let expected: f64 = input.parse().expect("oracle");
        let actual = f64_from_str(input).expect("fixed");
        assert!(
            (actual - expected).abs() < 1e-15,
            "fixed {actual} vs parse {expected}"
        );
    }

    /// An integer part wider than a `u64` is a corrupt field, not a number.
    #[test]
    fn overlong_integer_part_is_rejected() {
        assert_eq!(Decimal::parse("123456789012345678901"), None);
    }
}

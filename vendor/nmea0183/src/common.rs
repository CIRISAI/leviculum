/// `&s[range]` without the panic path (order 1326, leviculum's patch).
///
/// Every caller has checked the length first and every input is ASCII
/// (`from_ascii` in lib.rs), so the result is the same slice. The indexing
/// form drags `str::slice_error_fail`, `<char as Debug>` and the unicode
/// grapheme tables into a firmware image, 5392 B of the Pocket's, for a panic that
/// cannot happen; a range that does not fit yields the empty field, which
/// every caller rejects as a malformed one.
pub(crate) fn field<R: core::slice::SliceIndex<str, Output = str>>(s: &str, range: R) -> &str {
    s.get(range).unwrap_or("")
}

pub(crate) fn parse_i8(input: Option<&str>) -> Result<Option<i8>, &'static str> {
    match input {
        Some(s) if s.len() == 0 => Ok(None),
        Some(s) => s
            .parse::<i8>()
            .map_err(|_| "Wrong signed int field format")
            .and_then(|u| Ok(Some(u))),
        None => Ok(None),
    }
}

pub(crate) fn parse_u8(input: Option<&str>) -> Result<Option<u8>, &'static str> {
    match input {
        Some(s) if s.len() == 0 => Ok(None),
        Some(s) => s
            .parse::<u8>()
            .map_err(|_| "Wrong unsigned int field format")
            .and_then(|u| Ok(Some(u))),
        None => Ok(None),
    }
}

pub(crate) fn parse_u16(input: Option<&str>) -> Result<Option<u16>, &'static str> {
    match input {
        Some(s) if s.len() == 0 => Ok(None),
        Some(s) => s
            .parse::<u16>()
            .map_err(|_| "Wrong unsigned int field format")
            .and_then(|u| Ok(Some(u))),
        None => Ok(None),
    }
}

pub(crate) fn parse_f32(input: Option<&str>) -> Result<Option<f32>, &'static str> {
    match input {
        Some(s) if s.len() == 0 => Ok(None),
        Some(s) => crate::fixed::f32_from_str(s)
            .ok_or("Wrong float field format")
            .and_then(|u| Ok(Some(u))),
        None => Ok(None),
    }
}

#[test]
fn test_parse_u8() {
    assert_eq!(parse_u8(Some("")), Ok(None));
    assert_eq!(parse_u8(Some("123")), Ok(Some(123u8)));
    assert_eq!(
        parse_u8(Some("a123")),
        Err("Wrong unsigned int field format")
    );
    assert_eq!(
        parse_u8(Some("-123")),
        Err("Wrong unsigned int field format")
    );
    assert_eq!(
        parse_u8(Some("256")),
        Err("Wrong unsigned int field format")
    );
    assert_eq!(parse_u8(None), Ok(None));
}

#[test]
fn test_parse_u16() {
    assert_eq!(parse_u16(Some("")), Ok(None));
    assert_eq!(parse_u16(Some("123")), Ok(Some(123u16)));
    assert_eq!(
        parse_u16(Some("a123")),
        Err("Wrong unsigned int field format")
    );
    assert_eq!(
        parse_u16(Some("-123")),
        Err("Wrong unsigned int field format")
    );
    assert_eq!(
        parse_u16(Some("70000")),
        Err("Wrong unsigned int field format")
    );
    assert_eq!(parse_u16(None), Ok(None));
}

#[test]
fn test_parse_f32() {
    assert_eq!(parse_f32(Some("")), Ok(None));
    assert_eq!(parse_f32(Some("123.0")), Ok(Some(123.0f32)));
    assert_eq!(parse_f32(Some("a123.0")), Err("Wrong float field format"));
    assert_eq!(parse_f32(None), Ok(None));
}

#[cfg(test)]
mod field_tests {
    use super::field;
    use crate::Source;
    use core::convert::TryFrom;

    #[test]
    fn a_fitting_range_is_the_slice() {
        assert_eq!(field("4807.038", ..2), "48");
        assert_eq!(field("4807.038", 2..), "07.038");
        assert_eq!(field("GPRMC", 2..5), "RMC");
    }

    /// The indexing form panicked here; the field form is the empty field,
    /// which the callers reject.
    #[test]
    fn a_range_past_the_end_is_the_empty_field() {
        assert_eq!(field("G", 0..2), "");
        assert!(Source::try_from("G").is_err());
    }
}

//! The poi section format (LMR5, 1685): the writer's bytes read back,
//! and corrupt bytes an error, never a panic.

use leviculum_map::pois::{encode_section, Poi, PoiError, PoiRecord, Pois, WayRecord};

fn section() -> Vec<u8> {
    encode_section(
        &[
            PoiRecord {
                kind: 3,
                name: Some(7),
                min_zoom: 14,
                flags: 0,
                x: 100,
                y: 200,
                attrs: vec![(1, 2), (4, 5)],
            },
            PoiRecord {
                kind: 0,
                name: None,
                min_zoom: 17,
                flags: 0,
                x: 4095,
                y: 0,
                attrs: vec![],
            },
        ],
        &[
            WayRecord {
                feature: 9,
                attrs: vec![(6, 2)],
            },
            WayRecord {
                feature: 0,
                attrs: vec![],
            },
        ],
    )
    .expect("encode")
}

#[test]
fn a_section_reads_back_what_was_written() {
    let b = section();
    let s = Pois::parse(&b, 10).expect("parse");
    assert_eq!((s.poi_count(), s.way_count()), (2, 2));
    let pois: Vec<Poi> = s.pois().collect();
    assert_eq!((pois[0].kind, pois[0].name), (3, Some(7)));
    assert_eq!((pois[0].min_zoom, pois[0].x, pois[0].y), (14, 100, 200));
    let attrs: Vec<(u32, u32)> = s.attrs_of(&pois[0]).collect();
    assert_eq!(attrs, [(1, 2), (4, 5)]);
    assert_eq!(pois[1].name, None);
    assert_eq!(s.attrs_of(&pois[1]).count(), 0);
    let ways: Vec<_> = s.ways().collect();
    assert_eq!(ways[0].feature, 9);
    let attrs: Vec<(u32, u32)> = s.way_attrs_of(&ways[0]).collect();
    assert_eq!(attrs, [(6, 2)]);
    assert_eq!(s.way_attrs_of(&ways[1]).count(), 0);
}

#[test]
fn corrupt_bytes_are_an_error_not_a_panic() {
    let b = section();
    // Truncated, and one byte long.
    assert_eq!(
        Pois::parse(&b[..b.len() - 1], 10).err(),
        Some(PoiError::Length)
    );
    assert_eq!(Pois::parse(&b[..1], 10).err(), Some(PoiError::Length));
    // A string past the table: the first poi's kind is 3.
    assert!(matches!(
        Pois::parse(&b, 3).err(),
        Some(PoiError::String(_))
    ));
    // An anchor outside the tile.
    let mut bad = section();
    bad[8 + 16..8 + 18].copy_from_slice(&4096i16.to_le_bytes());
    assert_eq!(Pois::parse(&bad, 10).err(), Some(PoiError::Anchor(0)));
    // An attribute run past the attribute array.
    let mut bad = section();
    bad[8 + 12..8 + 14].copy_from_slice(&9u16.to_le_bytes());
    assert_eq!(Pois::parse(&bad, 10).err(), Some(PoiError::Attributes(0)));
}

#[test]
fn an_oversized_record_is_refused_at_the_writer() {
    let too_many = vec![
        PoiRecord {
            kind: 0,
            ..PoiRecord::default()
        };
        usize::from(u16::MAX) + 1
    ];
    assert_eq!(encode_section(&too_many, &[]).err(), Some(PoiError::Count));
}

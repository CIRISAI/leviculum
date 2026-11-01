//! The pixels of a photo (1220): what the shell does with a picture
//! before it goes and after it came, in Rust on every target (the phone
//! hands over the picker's bytes and draws what this returns).
//!
//! Before it goes ([`prepare`]): decoded, turned upright by its EXIF
//! orientation, scaled to the reader's [`PhotoSize`] and encoded as JPEG,
//! which carries no metadata, within [`SHOWN_BY_COLUMBA`]. "Original" keeps the picture as taken and
//! only takes its metadata out: a JPEG loses every APP segment but JFIF,
//! the colour profile and Adobe's colour transform ([`strip_jpeg`]), a
//! PNG its text, time and EXIF chunks ([`strip_png`]). An original that
//! is not upright as stored is drawn upright and encoded at quality 95,
//! since Columba shows the bytes through Android's `BitmapFactory`, which
//! ignores the EXIF orientation
//! (`columba/app/src/main/java/network/columba/app/ui/model/MessageMapper.kt:572-605`),
//! and a stripped tag would turn it on every screen. A WebP original becomes
//! such a JPEG too; a GIF goes as it is (it has no EXIF).
//!
//! After it came ([`fit`]): decoded upright and scaled into a box, for a
//! bubble's thumbnail or the full view. A received picture is the
//! sender's: [`limits`] bounds what decoding it may allocate.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};

pub use crate::ui::photos::PhotoSize;

/// The most picture bytes a photo may carry. A receiver refuses a
/// delivery resource above `DELIVERY_LIMIT` (1000 KB,
/// `reference/LXMF/LXMF/LXMRouter.py:60`, applied at `:1979`), and the
/// message around the picture costs its header, signature, title, text
/// and the field's framing: 990 000 leaves 10 000 for them.
pub const LARGEST: usize = 990_000;

/// The most picture bytes a scaled photo carries, so that Columba shows
/// it (1288). Columba stores a received message's fields as JSON, the
/// picture as `["jpg","<hex>"]`, and once that field is past 512 000
/// characters it writes the field's JSON text to a file
/// (`columba/data/src/main/java/network/columba/app/data/repository/ConversationRepository.kt:957-983`,
/// threshold `columba/data/src/main/java/network/columba/app/data/storage/AttachmentStorageManager.kt:38`);
/// reading it back it takes that text for the hex of the picture
/// (`columba/app/src/main/java/network/columba/app/ui/model/MessageMapper.kt:532-543`),
/// which fails at the `[`, and the bubble says "Not available"
/// (`columba/app/src/main/java/network/columba/app/ui/screens/MessagingScreen.kt:2356-2376`).
/// `2 * bytes + 10 > 512 000` is a picture past 255 995 bytes; 250 000
/// leaves room for a longer format word.
pub const SHOWN_BY_COLUMBA: usize = 250_000;

/// The JPEG quality an original is encoded at when it must be turned.
const UPRIGHT_QUALITY: u8 = 95;

/// The lowest quality a scaled photo steps down to; past it the photo
/// gets smaller instead, which looks better on a phone than blocks.
const LOWEST_QUALITY: u8 = 60;

/// What decoding one picture may take: 12 000 px a side and 400 MiB,
/// a 100 MP camera's photo and not a decompression bomb.
pub fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(12_000);
    limits.max_image_height = Some(12_000);
    limits.max_alloc = Some(400 * 1024 * 1024);
    limits
}

/// A picture ready to go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    /// The image field's format word: `jpg`, `png`, `gif`.
    pub format: &'static str,
    pub data: Vec<u8>,
    /// As it shows, upright.
    pub width: u32,
    pub height: u32,
}

/// Why a picture does not go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PhotoError {
    /// Not a picture this build reads (a HEIC, a video, a document).
    NotAPicture(String),
    /// Larger than a receiver takes, even prepared: an original.
    TooLarge { bytes: usize },
    /// The encoder failed.
    Encode(String),
}

impl core::fmt::Display for PhotoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PhotoError::NotAPicture(detail) => write!(f, "not a picture loquax can send: {detail}"),
            PhotoError::TooLarge { bytes } => write!(
                f,
                "the photo is {} and others take at most {}: choose a smaller photo size",
                crate::ui::photos::size_words(*bytes as u64),
                crate::ui::photos::size_words(LARGEST as u64)
            ),
            PhotoError::Encode(detail) => write!(f, "the photo did not encode: {detail}"),
        }
    }
}

impl std::error::Error for PhotoError {}

/// A decoded picture: RGBA rows, no padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// The picture's own size, upright, before it was scaled: what the
    /// viewer zooms to at most (1269).
    pub full: (u32, u32),
}

fn not_a_picture(error: impl core::fmt::Display) -> PhotoError {
    PhotoError::NotAPicture(error.to_string())
}

/// The format `bytes` are in, by their first bytes, among those read.
fn format_of(bytes: &[u8]) -> Result<ImageFormat, PhotoError> {
    match image::guess_format(bytes) {
        Ok(
            format @ (ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP | ImageFormat::Gif),
        ) => Ok(format),
        Ok(other) => Err(not_a_picture(format!("{other:?}"))),
        Err(error) => Err(not_a_picture(error)),
    }
}

/// `bytes` decoded and turned upright, within [`limits`].
pub fn decode(bytes: &[u8]) -> Result<DynamicImage, PhotoError> {
    let format = format_of(bytes)?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits());
    let mut decoder = reader.into_decoder().map_err(not_a_picture)?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).map_err(not_a_picture)?;
    image.apply_orientation(orientation);
    Ok(image)
}

/// The stored orientation of `bytes`, from its EXIF.
fn orientation(bytes: &[u8]) -> Result<Orientation, PhotoError> {
    let format = format_of(bytes)?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits());
    let mut decoder = reader.into_decoder().map_err(not_a_picture)?;
    Ok(decoder.orientation().unwrap_or(Orientation::NoTransforms))
}

/// Whether `orientation` swaps width and height.
fn turns(orientation: Orientation) -> bool {
    matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    )
}

/// The size `bytes` show at, upright, from the header alone.
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let format = format_of(bytes).ok()?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits());
    let mut decoder = reader.into_decoder().ok()?;
    let (w, h) = decoder.dimensions();
    match turns(decoder.orientation().unwrap_or(Orientation::NoTransforms)) {
        true => Some((h, w)),
        false => Some((w, h)),
    }
}

/// `image` as JPEG at `quality`, a transparent picture on white.
fn jpeg(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, PhotoError> {
    let rgb = match image.color().has_alpha() {
        false => image.to_rgb8(),
        true => {
            let rgba = image.to_rgba8();
            let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
            for (to, from) in rgb.pixels_mut().zip(rgba.pixels()) {
                let a = u16::from(from[3]);
                for c in 0..3 {
                    to[c] = ((u16::from(from[c]) * a + 255 * (255 - a)) / 255) as u8;
                }
            }
            rgb
        }
    };
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&rgb)
        .map_err(|e| PhotoError::Encode(e.to_string()))?;
    Ok(out)
}

/// `image` scaled into an `edge` box (never up) and encoded as JPEG at
/// `quality`, within [`SHOWN_BY_COLUMBA`]. A 12 MP phone photo at 1600 px
/// and quality 80 is 180 to 610 KB (1288's measure), so a picture of fine
/// detail steps its quality down by 10 to [`LOWEST_QUALITY`], as
/// Columba's compressor steps
/// (`columba/app/src/main/java/network/columba/app/util/ImageUtils.kt:186-191`),
/// and then its edge, by the root of how far it is over, since a JPEG's
/// bytes go with its pixels. A smaller edge is scaled from the first
/// scaled picture, not from the camera's, which costs far less.
fn scaled(image: &DynamicImage, edge: u32, quality: u8) -> Result<Prepared, PhotoError> {
    let mut image = match image.width().max(image.height()) > edge {
        true => image.resize(edge, edge, FilterType::Triangle),
        false => image.clone(),
    };
    let mut quality = quality;
    loop {
        let data = jpeg(&image, quality)?;
        if data.len() <= SHOWN_BY_COLUMBA {
            return Ok(Prepared {
                format: "jpg",
                data,
                width: image.width(),
                height: image.height(),
            });
        }
        if quality > LOWEST_QUALITY {
            quality = quality.saturating_sub(10).max(LOWEST_QUALITY);
            continue;
        }
        let shown = image.width().max(image.height());
        // 0.95 of the root so the next try lands under, not on, the limit.
        let factor = (SHOWN_BY_COLUMBA as f64 / data.len() as f64).sqrt() * 0.95;
        let next = (f64::from(shown) * factor) as u32;
        if next < 16 {
            return Err(PhotoError::TooLarge { bytes: data.len() });
        }
        image = image.resize(next, next, FilterType::Triangle);
    }
}

/// `bytes` made ready to go at `size`: see the module's rules.
pub fn prepare(bytes: &[u8], size: PhotoSize) -> Result<Prepared, PhotoError> {
    let format = format_of(bytes)?;
    let prepared = match (size.target(), format) {
        (Some((edge, quality)), _) => scaled(&decode(bytes)?, edge, quality)?,
        (None, ImageFormat::Jpeg) if orientation(bytes)? == Orientation::NoTransforms => {
            let data = strip_jpeg(bytes).ok_or_else(|| not_a_picture("a broken JPEG"))?;
            let (width, height) = dimensions(&data).ok_or_else(|| not_a_picture("no size"))?;
            Prepared {
                format: "jpg",
                data,
                width,
                height,
            }
        }
        (None, ImageFormat::Png) => {
            let data = strip_png(bytes).ok_or_else(|| not_a_picture("a broken PNG"))?;
            let (width, height) = dimensions(&data).ok_or_else(|| not_a_picture("no size"))?;
            Prepared {
                format: "png",
                data,
                width,
                height,
            }
        }
        (None, ImageFormat::Gif) => {
            let (width, height) = dimensions(bytes).ok_or_else(|| not_a_picture("no size"))?;
            Prepared {
                format: "gif",
                data: bytes.to_vec(),
                width,
                height,
            }
        }
        // A turned JPEG, a WebP: upright, as JPEG, at full size.
        (None, _) => {
            let image = decode(bytes)?;
            Prepared {
                format: "jpg",
                data: jpeg(&image, UPRIGHT_QUALITY)?,
                width: image.width(),
                height: image.height(),
            }
        }
    };
    match prepared.data.len() > LARGEST {
        true => Err(PhotoError::TooLarge {
            bytes: prepared.data.len(),
        }),
        false => Ok(prepared),
    }
}

/// `bytes` decoded upright and scaled to fit a `edge` by `edge` box,
/// never up: a bubble's thumbnail, the full view.
pub fn fit(bytes: &[u8], edge: u32) -> Result<Pixels, PhotoError> {
    let image = decode(bytes)?;
    let full = (image.width(), image.height());
    let image = match image.width().max(image.height()) > edge {
        true => image.thumbnail(edge, edge),
        false => image,
    };
    let rgba = image.to_rgba8();
    Ok(Pixels {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
        full,
    })
}

/// A JPEG without the APP segments that carry metadata (EXIF and XMP in
/// APP1, IPTC in APP13, a camera's own in the others) and without
/// comments: SOI, then each segment up to the scan kept or dropped, then
/// the scan and the rest as they are. Kept: APP0 (JFIF), APP2 (the ICC
/// colour profile) and APP14 (Adobe's colour transform, which a CMYK
/// picture needs to decode). `None` for bytes that do not parse.
pub fn strip_jpeg(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.get(..2)? != [0xff, 0xd8] {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..2]);
    let mut at = 2;
    loop {
        // Fill bytes before a marker are allowed.
        while bytes.get(at)? == &0xff && bytes.get(at + 1)? == &0xff {
            at += 1;
        }
        if bytes.get(at)? != &0xff {
            return None;
        }
        let marker = *bytes.get(at + 1)?;
        match marker {
            // Standalone markers: TEM, RST0..7.
            0x01 | 0xd0..=0xd7 => {
                out.extend_from_slice(&bytes[at..at + 2]);
                at += 2;
                continue;
            }
            0xd9 => {
                out.extend_from_slice(&bytes[at..at + 2]);
                return Some(out);
            }
            _ => {}
        }
        let len = usize::from(u16::from_be_bytes([
            *bytes.get(at + 2)?,
            *bytes.get(at + 3)?,
        ]));
        let end = at.checked_add(2)?.checked_add(len)?;
        let segment = bytes.get(at..end)?;
        if marker == 0xda {
            out.extend_from_slice(&bytes[at..]);
            return Some(out);
        }
        let metadata = matches!(marker, 0xe1 | 0xe3..=0xed | 0xef | 0xfe);
        if !metadata {
            out.extend_from_slice(segment);
        }
        at = end;
    }
}

/// A PNG without its ancillary chunks but those that change how it looks
/// (transparency, gamma, chromaticities, sRGB, the ICC profile,
/// significant bits, the pixel size, the background, APNG's frames): its
/// text, time and EXIF go. `None` for bytes that do not parse.
pub fn strip_png(bytes: &[u8]) -> Option<Vec<u8>> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    const KEPT: [&[u8; 4]; 11] = [
        b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"iCCP", b"sBIT", b"pHYs", b"bKGD", b"acTL", b"fcTL",
        b"fdAT",
    ];
    if bytes.get(..8)? != SIGNATURE {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&SIGNATURE);
    let mut at = 8;
    while at < bytes.len() {
        let len = u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize;
        let kind: [u8; 4] = bytes.get(at + 4..at + 8)?.try_into().ok()?;
        let end = at.checked_add(12)?.checked_add(len)?;
        let chunk = bytes.get(at..end)?;
        let critical = kind[0].is_ascii_uppercase();
        if critical || KEPT.contains(&&kind) {
            out.extend_from_slice(chunk);
        }
        at = end;
        if &kind == b"IEND" {
            return Some(out);
        }
    }
    None
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A test picture: a gradient with a block in the top left corner, so
    /// a turn shows where the corner went.
    pub fn picture(width: u32, height: u32) -> DynamicImage {
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            match x < width / 4 && y < height / 4 {
                true => image::Rgb([250, 10, 10]),
                false => image::Rgb([(x * 255 / width) as u8, (y * 255 / height) as u8, 128]),
            }
        });
        DynamicImage::ImageRgb8(image)
    }

    /// An APP1 EXIF segment with the orientation tag and, after the IFD,
    /// a GPS-looking string a stripped picture must not carry.
    pub fn exif_app1(orientation: u16) -> Vec<u8> {
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II*\0");
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0x0112u16.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&orientation.to_le_bytes());
        tiff.extend_from_slice(&[0, 0]);
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(b"GPS 52.5163N 13.3777E secret");
        let mut body = b"Exif\0\0".to_vec();
        body.extend_from_slice(&tiff);
        let mut app1 = vec![0xff, 0xe1];
        app1.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        app1.extend_from_slice(&body);
        app1
    }

    /// `image` as a camera writes it: a JPEG at quality 90 with an EXIF
    /// segment saying `orientation`, a comment, and the pixels stored
    /// turned back by it.
    pub fn camera_jpeg(image: &DynamicImage, orientation: u16) -> Vec<u8> {
        let stored = match orientation {
            6 => image.rotate270(),
            8 => image.rotate90(),
            3 => image.rotate180(),
            _ => image.clone(),
        };
        let plain = jpeg(&stored, 90).expect("encodes");
        let mut out = plain[..2].to_vec();
        out.extend_from_slice(&exif_app1(orientation));
        out.extend_from_slice(&[0xff, 0xfe, 0x00, 0x0a]);
        out.extend_from_slice(b"Pixel 8\0");
        out.extend_from_slice(&plain[2..]);
        out
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// The colour at the top left of `bytes` upright: the red block.
    fn corner_is_red(bytes: &[u8]) -> bool {
        let rgb = decode(bytes).expect("decodes").to_rgb8();
        let p = rgb.get_pixel(rgb.width() / 16, rgb.height() / 16);
        p[0] > 200 && p[1] < 80
    }

    #[test]
    fn phone_scales_a_turned_photo_upright_to_1600_and_strips_it() {
        // Portrait as shown, stored landscape with orientation 6.
        let shown = picture(900, 1200);
        let bytes = camera_jpeg(&shown, 6);
        assert_eq!(dimensions(&bytes), Some((900, 1200)), "the header, upright");
        assert!(contains(&bytes, b"secret") && contains(&bytes, b"Pixel 8"));
        let small = prepare(&bytes, PhotoSize::Small).expect("small");
        assert_eq!((small.width, small.height), (600, 800));
        let phone = prepare(&bytes, PhotoSize::Phone).expect("phone");
        assert_eq!((phone.width, phone.height), (900, 1200), "never scaled up");
        for p in [&small, &phone] {
            assert_eq!(p.format, "jpg");
            assert!(!contains(&p.data, b"Exif"), "no EXIF");
            assert!(!contains(&p.data, b"secret") && !contains(&p.data, b"Pixel 8"));
            assert!(corner_is_red(&p.data), "upright: the block is top left");
            assert_eq!(dimensions(&p.data), Some((p.width, p.height)));
        }
        let big = camera_jpeg(&picture(4000, 3000), 1);
        let phone = prepare(&big, PhotoSize::Phone).expect("phone");
        assert_eq!((phone.width, phone.height), (1600, 1200));
        let small = prepare(&big, PhotoSize::Small).expect("small");
        assert_eq!((small.width, small.height), (800, 600));
        assert!(small.data.len() < phone.data.len());
    }

    #[test]
    fn an_upright_original_keeps_its_scan_and_loses_its_metadata() {
        let bytes = camera_jpeg(&picture(640, 480), 1);
        let original = prepare(&bytes, PhotoSize::Original).expect("original");
        assert_eq!(original.format, "jpg");
        assert_eq!((original.width, original.height), (640, 480));
        assert!(!contains(&original.data, b"Exif") && !contains(&original.data, b"Pixel 8"));
        // The scan is byte for byte the camera's: only segments went.
        let scan = |b: &[u8]| {
            b.windows(2)
                .position(|w| w == [0xff, 0xda])
                .map(|i| b[i..].to_vec())
        };
        assert_eq!(scan(&original.data), scan(&bytes));
        assert_eq!(
            decode(&original.data).expect("decodes").to_rgb8(),
            decode(&bytes).expect("decodes").to_rgb8()
        );
    }

    #[test]
    fn a_turned_original_is_drawn_upright_since_columba_ignores_the_tag() {
        let bytes = camera_jpeg(&picture(400, 600), 6);
        let original = prepare(&bytes, PhotoSize::Original).expect("original");
        assert_eq!((original.width, original.height), (400, 600));
        assert!(!contains(&original.data, b"Exif"));
        assert!(corner_is_red(&original.data));
        assert_eq!(orientation(&original.data), Ok(Orientation::NoTransforms));
    }

    #[test]
    fn a_png_original_loses_its_text_and_exif_chunks_not_its_pixels() {
        let mut png = Vec::new();
        picture(64, 48)
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .expect("png");
        // A tEXt and an eXIf chunk after IHDR (8 + 25 bytes); their CRCs
        // are not checked by the stripper, which drops them whole.
        let chunk = |kind: &[u8; 4], data: &[u8]| {
            let mut c = (data.len() as u32).to_be_bytes().to_vec();
            c.extend_from_slice(kind);
            c.extend_from_slice(data);
            c.extend_from_slice(&[0; 4]);
            c
        };
        let mut tagged = png[..33].to_vec();
        tagged.extend(chunk(b"tEXt", b"Comment\0secret"));
        tagged.extend(chunk(b"eXIf", &exif_app1(1)[10..]));
        tagged.extend_from_slice(&png[33..]);
        let original = prepare(&tagged, PhotoSize::Original).expect("original");
        assert_eq!(original.format, "png");
        assert_eq!(original.data, png, "exactly the PNG without the two chunks");
    }

    #[test]
    fn what_is_no_picture_or_too_large_does_not_go() {
        assert!(matches!(
            prepare(b"%PDF-1.7 not a photo", PhotoSize::Phone),
            Err(PhotoError::NotAPicture(_))
        ));
        assert!(matches!(
            prepare(&[0xff, 0xd8, 0xff], PhotoSize::Phone),
            Err(PhotoError::NotAPicture(_))
        ));
        // Noise does not compress: 1000 x 800 at quality 98 is past the
        // limit, which the original keeps; the phone's size steps its
        // quality down until it fits.
        let mut seed = 0x2545_f491_u32;
        let noise = image::RgbImage::from_fn(1000, 800, |_, _| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            image::Rgb([seed as u8, (seed >> 8) as u8, (seed >> 16) as u8])
        });
        let bytes = jpeg(&DynamicImage::ImageRgb8(noise), 98).expect("encodes");
        assert!(bytes.len() > LARGEST, "{}", bytes.len());
        assert!(matches!(
            prepare(&bytes, PhotoSize::Original),
            Err(PhotoError::TooLarge { .. })
        ));
        let phone = prepare(&bytes, PhotoSize::Phone).expect("steps down");
        assert!(phone.data.len() <= SHOWN_BY_COLUMBA, "{}", phone.data.len());
    }

    /// The bytes Columba hands its bitmap decoder for a received
    /// `fields[6] = [format, data]`, or none where it shows "Not
    /// available": the kt backend writes bytes as lower hex
    /// (`columba/rns-api/src/main/java/network/columba/app/rns/api/util/AppDataParser.kt:204-232`),
    /// a fields JSON of 512 000 characters or more has each field past
    /// 512 000 written to disk as its JSON text and replaced by a file
    /// reference (`columba/data/src/main/java/network/columba/app/data/repository/ConversationRepository.kt:957-983`),
    /// and the reader takes the file's text for hex
    /// (`columba/app/src/main/java/network/columba/app/ui/model/MessageMapper.kt:532-543`,
    /// `:1076-1089`), whose `require` throws on the `[`.
    pub fn columba_reads(format: &str, data: &[u8]) -> Option<Vec<u8>> {
        const THRESHOLD: usize = 500 * 1024;
        let hex: String = data.iter().map(|b| format!("{b:02x}")).collect();
        let field = format!("[\"{format}\",\"{hex}\"]");
        let fields_json = format!("{{\"6\":{field}}}");
        let text = match fields_json.len() >= THRESHOLD && field.len() > THRESHOLD {
            true => field,
            false => hex,
        };
        if text.len() % 2 != 0 {
            return None;
        }
        text.as_bytes()
            .chunks(2)
            .map(|pair| u8::from_str_radix(core::str::from_utf8(pair).ok()?, 16).ok())
            .collect()
    }

    /// The model above, against Columba's own numbers: a picture of
    /// 255 995 bytes arrives whole, one of 255 996 does not.
    #[test]
    fn columba_reads_a_picture_inline_and_loses_one_past_its_threshold() {
        let fits = vec![0xab; 255_995];
        assert_eq!(columba_reads("jpg", &fits).as_deref(), Some(&fits[..]));
        assert_eq!(columba_reads("jpg", &vec![0xab; 255_996]), None);
        const { assert!(SHOWN_BY_COLUMBA < 255_996) };
    }

    /// A portrait photo of fine detail, as a phone takes one: a gradient
    /// under grain and fine stripes, 1500 x 2000 shown, stored turned
    /// with orientation 6. At 1600 px and quality 80 it is past what
    /// Columba shows, the case of Lew's photo of 2026-11-01 (1288).
    pub fn detailed_portrait() -> Vec<u8> {
        let mut seed = 0x9e37_79b9_u32;
        let shown = image::RgbImage::from_fn(1500, 2000, |x, y| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let grain = seed % 48;
            let stripe = if (x / 3 + y / 5) % 2 == 0 { 30 } else { 0 };
            image::Rgb([
                ((x * 180 / 1500) + grain + stripe).min(255) as u8,
                ((y * 180 / 2000) + grain).min(255) as u8,
                (100 + grain + stripe).min(255) as u8,
            ])
        });
        camera_jpeg(&DynamicImage::ImageRgb8(shown), 6)
    }

    #[test]
    fn a_detailed_phone_photo_goes_at_a_size_columba_shows() {
        let bytes = detailed_portrait();
        let upright = decode(&bytes).expect("decodes");
        assert_eq!((upright.width(), upright.height()), (1500, 2000));
        // Without the limit: 1600 px at quality 80, as 1220 sent it.
        let unlimited =
            jpeg(&upright.resize(1600, 1600, FilterType::Triangle), 80).expect("encodes");
        assert!(unlimited.len() > 255_995, "{}", unlimited.len());
        assert_eq!(columba_reads("jpg", &unlimited), None, "the failure");
        for size in [PhotoSize::Phone, PhotoSize::Small] {
            let p = prepare(&bytes, size).expect("prepares");
            assert!(
                p.data.len() <= SHOWN_BY_COLUMBA,
                "{size:?} {}",
                p.data.len()
            );
            let read = columba_reads(p.format, &p.data).expect("Columba reads it");
            assert_eq!(dimensions(&read), Some((p.width, p.height)));
            assert!(p.height > p.width, "still a portrait");
            assert!(p.height >= 800, "{size:?} {}x{}", p.width, p.height);
        }
    }

    /// What goes is a baseline JPEG (SOF0), which every Android decoder
    /// reads, and never a progressive or extended one, a WebP or an EXIF.
    #[test]
    fn what_goes_is_a_baseline_jpeg() {
        let p = prepare(&detailed_portrait(), PhotoSize::Phone).expect("prepares");
        assert_eq!(&p.data[..2], &[0xff, 0xd8]);
        let sof: Vec<u8> = p
            .data
            .windows(2)
            .take_while(|w| w != &[0xff, 0xda])
            .filter(|w| w[0] == 0xff && (0xc0..=0xcf).contains(&w[1]) && w[1] != 0xc4)
            .map(|w| w[1])
            .collect();
        assert_eq!(sof, vec![0xc0], "one baseline frame header");
        assert!(!contains(&p.data, b"Exif") && !contains(&p.data, b"RIFF"));
    }

    /// An Ultra HDR photo, as a Pixel takes one: the picture with an MPF
    /// APP2 segment naming a gain map, a second JPEG after its end. What
    /// goes is the picture alone, one JPEG.
    #[test]
    fn an_ultra_hdr_photo_goes_as_its_picture_alone() {
        let primary = camera_jpeg(&picture(1200, 900), 1);
        let gain_map = jpeg(&picture(300, 225), 70).expect("encodes");
        let mut mpf = vec![0xff, 0xe2];
        let body = b"MPF\0II*\0\x08\0\0\0gain map follows";
        mpf.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        mpf.extend_from_slice(body);
        let mut ultra = primary[..2].to_vec();
        ultra.extend_from_slice(&mpf);
        ultra.extend_from_slice(&primary[2..]);
        ultra.extend_from_slice(&gain_map);
        let p = prepare(&ultra, PhotoSize::Phone).expect("prepares");
        assert_eq!((p.width, p.height), (1200, 900));
        let eoi = p.data.windows(2).filter(|w| w == &[0xff, 0xd9]).count();
        assert_eq!(eoi, 1, "one picture");
        assert!(!contains(&p.data, b"MPF"));
        assert!(corner_is_red(&p.data));
        assert!(columba_reads(p.format, &p.data).is_some());
    }

    /// A 16 bit PNG, as an editor exports one, goes as an 8 bit JPEG.
    #[test]
    fn a_sixteen_bit_png_goes_as_an_eight_bit_jpeg() {
        let wide = image::ImageBuffer::<image::Rgb<u16>, _>::from_fn(640, 480, |x, y| {
            image::Rgb([(x * 100) as u16, (y * 130) as u16, 30_000])
        });
        let mut png = Vec::new();
        DynamicImage::ImageRgb16(wide)
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .expect("png");
        let p = prepare(&png, PhotoSize::Phone).expect("prepares");
        assert_eq!((p.format, p.width, p.height), ("jpg", 640, 480));
        assert_eq!(
            decode(&p.data).expect("decodes").color(),
            image::ColorType::Rgb8
        );
    }

    #[test]
    fn a_thumbnail_fits_its_box_upright() {
        let bytes = camera_jpeg(&picture(900, 1200), 6);
        let thumb = fit(&bytes, 320).expect("fits");
        assert_eq!((thumb.width, thumb.height), (240, 320));
        assert_eq!(thumb.rgba.len(), 240 * 320 * 4);
        let corner = &thumb.rgba[(10 * 240 + 10) * 4..][..4];
        assert!(corner[0] > 200 && corner[1] < 80, "{corner:?}");
        let tiny = fit(&bytes, 4000).expect("fits");
        assert_eq!((tiny.width, tiny.height), (900, 1200), "never up");
    }
}

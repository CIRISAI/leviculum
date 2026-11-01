//! The pixels of a photo (1220): what the shell does with a picture
//! before it goes and after it came, in Rust on every target (the phone
//! hands over the picker's bytes and draws what this returns).
//!
//! Before it goes ([`prepare`]): decoded, turned upright by its EXIF
//! orientation, scaled to the reader's [`PhotoSize`] and encoded as JPEG,
//! which carries no metadata. "Original" keeps the picture as taken and
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

/// The JPEG quality an original is encoded at when it must be turned.
const UPRIGHT_QUALITY: u8 = 95;

/// The lowest quality a scaled photo steps down to before it is refused.
const LOWEST_QUALITY: u8 = 40;

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
                crate::ui::photos::size_words(*bytes),
                crate::ui::photos::size_words(LARGEST)
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

/// `bytes` made ready to go at `size`: see the module's rules.
pub fn prepare(bytes: &[u8], size: PhotoSize) -> Result<Prepared, PhotoError> {
    let format = format_of(bytes)?;
    let prepared = match (size.target(), format) {
        (Some((edge, quality)), _) => {
            let image = decode(bytes)?;
            let image = match image.width().max(image.height()) > edge {
                true => image.resize(edge, edge, FilterType::Triangle),
                false => image,
            };
            // A picture of fine detail can be past the limit even scaled:
            // the quality steps down by 10 to 40 before it is refused,
            // as Columba's compressor steps
            // (`columba/app/src/main/java/network/columba/app/util/ImageUtils.kt:186-191`).
            let mut quality = quality;
            let mut data = jpeg(&image, quality)?;
            while data.len() > LARGEST && quality > LOWEST_QUALITY {
                quality = quality.saturating_sub(10).max(LOWEST_QUALITY);
                data = jpeg(&image, quality)?;
            }
            Prepared {
                format: "jpg",
                data,
                width: image.width(),
                height: image.height(),
            }
        }
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
        assert!(phone.data.len() <= LARGEST, "{}", phone.data.len());
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

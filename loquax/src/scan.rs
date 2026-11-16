//! The scanner's decoder (1152): the QR codes in one camera frame. Java's
//! Camera2 preview hands over the frame's luminance plane, the Y of
//! `YUV_420_888`, which is all a QR code needs; rqrr, a pure-Rust port of
//! quirc, finds and reads the codes in it. No Google service, no AndroidX:
//! the codes this app scans are its own, a mailbox's join code
//! (`lqxh1...@host:port`), a backup line (`lqxb1...`) and a contact code.
//!
//! The desktop has no camera, so it reads a picture of a code instead
//! (1341): a screenshot or a photo of a screen, decoded by [`image_codes`]
//! into the same luminance plane. One decoder for the phone and the PC.

use alloc::string::String;
use alloc::vec::Vec;

/// The text of every QR code `luma` shows, in the order found. `luma` is
/// `height` rows of `stride` bytes, of which the first `width` are the
/// row's pixels; a plane too short for that reads as no code.
pub fn decode(width: usize, height: usize, stride: usize, luma: &[u8]) -> Vec<String> {
    let needed = height
        .checked_sub(1)
        .and_then(|rows| rows.checked_mul(stride))
        .and_then(|bytes| bytes.checked_add(width));
    if width == 0 || stride < width || needed.is_none_or(|needed| luma.len() < needed) {
        return Vec::new();
    }
    let mut image =
        rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| luma[y * stride + x]);
    image
        .detect_grids()
        .into_iter()
        .filter_map(|grid| grid.decode().ok().map(|(_, text)| text))
        .collect()
}

/// The text of every QR code in the picture file `bytes` (PNG, JPEG,
/// WebP, GIF, as [`crate::photo`] decodes them), in the order found; a
/// picture that does not decode is said.
#[cfg(feature = "photos")]
pub fn image_codes(bytes: &[u8]) -> Result<Vec<String>, String> {
    let picture = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let luma = picture.to_luma8();
    let (width, height) = (luma.width() as usize, luma.height() as usize);
    Ok(decode(width, height, width, luma.as_raw()))
}

#[cfg(all(test, feature = "qr"))]
mod tests {
    use super::*;
    use alloc::vec;

    /// A code as a camera might see it: `scale` pixels a module, the quiet
    /// zone around it, dark modules black on a light grey ground, rows
    /// padded to `stride`.
    fn frame(text: &str, scale: usize, stride_pad: usize) -> (usize, usize, Vec<u8>) {
        let (size, modules) = crate::home::qr::modules(text).expect("a code");
        let size = size as usize;
        let side = (size + 8) * scale;
        let stride = side + stride_pad;
        let mut luma = vec![200u8; stride * side];
        for y in 0..size {
            for x in 0..size {
                if modules[y * size + x] {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            luma[((y + 4) * scale + dy) * stride + (x + 4) * scale + dx] = 20;
                        }
                    }
                }
            }
        }
        (side, stride, luma)
    }

    /// The join code of 1121, as `loquax mailbox admit --server` prints it:
    /// the code goes upper case into the alphanumeric segment, the address
    /// behind it as bytes, and both come back.
    #[test]
    fn a_join_code_with_its_address_reads_back() {
        let code = crate::home::code::HomeCode {
            public_key: [0x5a; 64],
            token: [0x11; 16],
        };
        let text = code.encode_with_server("192.168.1.20:4242");
        let (side, stride, luma) = frame(&text, 4, 24);
        let found = decode(side, side, stride, &luma);
        assert_eq!(found.len(), 1, "{found:?}");
        let (scanned, server) =
            crate::home::code::HomeCode::parse_with_server(&found[0]).expect("a code");
        assert_eq!(scanned, code);
        assert_eq!(server.as_deref(), Some("192.168.1.20:4242"));
    }

    #[test]
    fn a_paper_backup_reads_back() {
        let line =
            crate::home::backup::seal_with(&[7; 64], "Kastanienallee 7", [1; 16], [2; 16], 10)
                .expect("sealed");
        let (side, stride, luma) = frame(&line, 3, 0);
        let found = decode(side, side, stride, &luma);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            crate::home::backup::open(&found[0], "Kastanienallee 7"),
            Ok([7; 64])
        );
    }

    #[test]
    fn a_frame_without_a_code_or_too_short_reads_nothing() {
        assert!(decode(64, 64, 64, &[200; 64 * 64]).is_empty());
        assert!(decode(64, 64, 64, &[200; 100]).is_empty());
        assert!(decode(64, 64, 32, &[200; 64 * 64]).is_empty());
        assert!(decode(0, 0, 0, &[]).is_empty());
    }

    /// A screenshot of a contact code as the contact screen shows it (in
    /// byte mode, case for case), saved as PNG, reads back from the file's
    /// bytes to the same code; a file that is no picture is said, not read
    /// as none.
    #[cfg(feature = "photos")]
    #[test]
    fn a_picture_file_of_a_code_reads_back() {
        let private: [u8; 64] = core::array::from_fn(|i| i as u8 + 1);
        let key = leviculum_core::Identity::from_private_key_bytes(&private)
            .expect("a key")
            .public_key_bytes();
        let text = crate::lxma::encode(&key).expect("a code");
        let (size, modules) = crate::home::qr::verbatim_modules(&text).expect("modules");
        let (size, scale) = (size as usize, 4);
        let side = (size + 8) * scale;
        let picture = image::GrayImage::from_fn(side as u32, side as u32, |x, y| {
            let (mx, my) = (x as usize / scale, y as usize / scale);
            let dark = (4..size + 4).contains(&mx)
                && (4..size + 4).contains(&my)
                && modules[(my - 4) * size + mx - 4];
            image::Luma([if dark { 20 } else { 230 }])
        });
        let mut png = Vec::new();
        picture
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("png");
        assert_eq!(image_codes(&png), Ok(vec![text]));
        assert!(image_codes(b"not a picture").is_err());
    }
}

//! The scanner's decoder (1152): the QR codes in one camera frame. Java's
//! Camera2 preview hands over the frame's luminance plane, the Y of
//! `YUV_420_888`, which is all a QR code needs; rqrr, a pure-Rust port of
//! quirc, finds and reads the codes in it. No Google service, no AndroidX:
//! the codes this app scans are its own, a mailbox's join code
//! (`lqxh1...@host:port`) and a backup line (`lqxb1...`).

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A code as a camera might see it: `scale` pixels a module, the quiet
    /// zone around it, dark modules black on a light grey ground, rows
    /// padded to `stride`.
    fn frame(text: &str, scale: usize, stride_pad: usize) -> (usize, usize, Vec<u8>) {
        let (size, modules) = loquax::home::qr::modules(text).expect("a code");
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
        let code = loquax::home::code::HomeCode {
            public_key: [0x5a; 64],
            token: [0x11; 16],
        };
        let text = code.encode_with_server("192.168.1.20:4242");
        let (side, stride, luma) = frame(&text, 4, 24);
        let found = decode(side, side, stride, &luma);
        assert_eq!(found.len(), 1, "{found:?}");
        let (scanned, server) =
            loquax::home::code::HomeCode::parse_with_server(&found[0]).expect("a code");
        assert_eq!(scanned, code);
        assert_eq!(server.as_deref(), Some("192.168.1.20:4242"));
    }

    #[test]
    fn a_paper_backup_reads_back() {
        let line =
            loquax::home::backup::seal_with(&[7; 64], "Kastanienallee 7", [1; 16], [2; 16], 10)
                .expect("sealed");
        let (side, stride, luma) = frame(&line, 3, 0);
        let found = decode(side, side, stride, &luma);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            loquax::home::backup::open(&found[0], "Kastanienallee 7"),
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
}

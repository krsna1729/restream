//! Posters for stored media: one still frame from a little way into a file,
//! scaled down and written as a PNG, so a library reads as pictures. The
//! static FFmpeg build carries no image encoders, so FFmpeg decodes and
//! scales and the PNG is written here (RGB, zlib from `flate2`).
//!
//! Blocking FFI: call [`extract_poster`] on the blocking pool, never on a
//! Tokio worker or an Owner thread.

use ffmpeg_next::format::Pixel;
use ffmpeg_next::software::scaling::{Context as Scaler, Flags};
use ffmpeg_next::{codec, format, frame, media};
use std::io::Write;
use std::path::Path;

/// Poster width in pixels; the height keeps the picture's shape.
pub const POSTER_WIDTH: u32 = 320;
/// How far in to look: past a fade-in or a slate, never past the middle.
const POSTER_AT_SEC: f64 = 5.0;

/// A PNG of one frame of the file's video, `POSTER_WIDTH` wide.
pub fn extract_poster(path: &Path) -> Result<Vec<u8>, String> {
    let mut ictx = format::input(path).map_err(|error| format!("open: {error}"))?;
    let stream = ictx
        .streams()
        .best(media::Type::Video)
        .ok_or_else(|| "no video stream".to_string())?;
    let index = stream.index();
    let context = codec::context::Context::from_parameters(stream.parameters())
        .map_err(|error| format!("decoder: {error}"))?;
    let mut decoder = context
        .decoder()
        .video()
        .map_err(|error| format!("decoder: {error}"))?;

    let duration_sec = (ictx.duration() > 0).then(|| ictx.duration() as f64 / 1_000_000.0);
    let at_sec = duration_sec.map_or(0.0, |total| POSTER_AT_SEC.min(total / 2.0));
    if at_sec > 0.0 {
        let target = (at_sec * 1_000_000.0) as i64;
        // Lands on the keyframe at or before the target; a failed seek
        // leaves the file at its start, which still has a picture.
        let _ = ictx.seek(target, ..target);
    }

    let mut decoded = frame::Video::empty();
    let mut found = false;
    for (packet_stream, packet) in ictx.packets() {
        if packet_stream.index() != index {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        if decoder.receive_frame(&mut decoded).is_ok() {
            found = true;
            break;
        }
    }
    if !found {
        let _ = decoder.send_eof();
        found = decoder.receive_frame(&mut decoded).is_ok();
    }
    if !found || decoded.width() == 0 || decoded.height() == 0 {
        return Err("no frame decoded".to_string());
    }

    let (width, height) = poster_size(decoded.width(), decoded.height());
    let mut scaler = Scaler::get(
        decoded.format(),
        decoded.width(),
        decoded.height(),
        Pixel::RGB24,
        width,
        height,
        Flags::BILINEAR,
    )
    .map_err(|error| format!("scale: {error}"))?;
    let mut rgb = frame::Video::empty();
    scaler
        .run(&decoded, &mut rgb)
        .map_err(|error| format!("scale: {error}"))?;
    let stride = rgb.stride(0);
    let row = width as usize * 3;
    let plane = rgb.data(0);
    let mut pixels = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        pixels.extend_from_slice(&plane[y * stride..y * stride + row]);
    }
    encode_png_rgb(width, height, &pixels)
}

/// `POSTER_WIDTH` wide (or the source's width, if narrower), the height
/// following the source's shape and kept even.
fn poster_size(source_width: u32, source_height: u32) -> (u32, u32) {
    let width = POSTER_WIDTH.min(source_width).max(2);
    let height = ((u64::from(source_height) * u64::from(width) / u64::from(source_width.max(1)))
        as u32)
        .max(2);
    (width, height & !1)
}

/// An 8-bit RGB PNG: signature, IHDR, one zlib IDAT (filter 0 rows), IEND.
pub fn encode_png_rgb(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>, String> {
    let row = width as usize * 3;
    if pixels.len() != row * height as usize {
        return Err("pixel buffer does not match the size".to_string());
    }
    let mut raw = Vec::with_capacity((row + 1) * height as usize);
    for line in pixels.chunks_exact(row) {
        raw.push(0);
        raw.extend_from_slice(line);
    }
    let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    zlib.write_all(&raw).map_err(|error| error.to_string())?;
    let idat = zlib.finish().map_err(|error| error.to_string())?;

    let mut png = Vec::with_capacity(idat.len() + 64);
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    // Bit depth 8, colour type 2 (RGB), deflate, filter 0, no interlace.
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &idat);
    chunk(&mut png, b"IEND", &[]);
    Ok(png)
}

fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let mut crc = crc32fast::Hasher::new();
    crc.update(kind);
    crc.update(data);
    png.extend_from_slice(&crc.finalize().to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn a_png_carries_its_size_valid_crcs_and_the_rows_it_was_given() {
        let pixels: Vec<u8> = (0..(4 * 2 * 3)).map(|value| value as u8).collect();
        let png = encode_png_rgb(4, 2, &pixels).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        // Walk the chunks: each CRC covers its type and data.
        let mut at = 8;
        let mut kinds = Vec::new();
        let mut idat = Vec::new();
        while at < png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let kind = &png[at + 4..at + 8];
            let data = &png[at + 8..at + 8 + len];
            let crc = u32::from_be_bytes(png[at + 8 + len..at + 12 + len].try_into().unwrap());
            let mut hasher = crc32fast::Hasher::new();
            hasher.update(kind);
            hasher.update(data);
            assert_eq!(
                crc,
                hasher.finalize(),
                "{} CRC",
                String::from_utf8_lossy(kind)
            );
            if kind == b"IHDR" {
                assert_eq!(&data[..8], &[0, 0, 0, 4, 0, 0, 0, 2]);
                assert_eq!(&data[8..], &[8, 2, 0, 0, 0]);
            }
            if kind == b"IDAT" {
                idat.extend_from_slice(data);
            }
            kinds.push(String::from_utf8_lossy(kind).into_owned());
            at += 12 + len;
        }
        assert_eq!(kinds, ["IHDR", "IDAT", "IEND"]);
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(&idat[..])
            .read_to_end(&mut raw)
            .unwrap();
        let mut expected = vec![0];
        expected.extend_from_slice(&pixels[..12]);
        expected.push(0);
        expected.extend_from_slice(&pixels[12..]);
        assert_eq!(raw, expected, "filter-0 rows of the given pixels");
        assert!(encode_png_rgb(4, 2, &pixels[1..]).is_err());
    }

    #[test]
    fn a_poster_keeps_the_picture_shape_at_an_even_height() {
        assert_eq!(poster_size(1920, 1080), (320, 180));
        assert_eq!(poster_size(1280, 720), (320, 180));
        assert_eq!(poster_size(720, 576), (320, 256));
        assert_eq!(poster_size(160, 90), (160, 90));
        assert_eq!(poster_size(1080, 1920), (320, 568));
    }

    #[test]
    fn a_fixture_file_yields_a_poster() {
        let path = crate::test_fixtures::checked_in_fixture(
            "test/fixtures/media-library/colorbar-timer-2v16a.mp4",
        )
        .unwrap();
        let png = extract_poster(&path).expect("poster");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(
            u32::from_be_bytes(png[16..20].try_into().unwrap()),
            POSTER_WIDTH
        );
    }
}

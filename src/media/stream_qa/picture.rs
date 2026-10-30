//! What a picture shows, from a small grey thumbnail of a decoded frame:
//! whether it is black, whether it differs from the last one, and whether
//! it looks like a slate (a drawn card) rather than a camera picture.

/// Thumbnail size: enough to tell a picture changed, small enough to
/// compare for nothing.
pub const THUMB_WIDTH: u32 = 64;
pub const THUMB_HEIGHT: u32 = 36;

/// A thumbnail pixel at or below this (full-range grey, 0..255) is dark.
const DARK_LEVEL: u8 = 32;
/// Share of dark pixels that makes a picture black.
const BLACK_SHARE: f64 = 0.98;
/// Mean absolute difference (0..255) above which two thumbnails are two
/// pictures; below it, coding noise on the same picture.
const CHANGE_LEVEL: f64 = 0.5;

/// Neighbouring pixels this close (0..255) are one flat area.
const FLAT_STEP: u8 = 1;
/// Share of flat neighbour pairs at or above which a picture is drawn, not
/// shot. Measured on 64x36 thumbnails: camera photos 0.10-0.44 (texture
/// and sensor noise survive the downscale), bars, test cards, flat colour
/// and gradients 0.71 and up.
const SLATE_FLAT_SHARE: f64 = 0.6;

/// Nearly every pixel dark.
pub fn is_black(thumb: &[u8]) -> bool {
    if thumb.is_empty() {
        return false;
    }
    let dark = thumb.iter().filter(|&&pixel| pixel <= DARK_LEVEL).count();
    dark as f64 >= BLACK_SHARE * thumb.len() as f64
}

/// A drawn card (bars, a "be right back" slate, a test card): most
/// neighbouring pixels, across and down, are equal. A thumbnail of another
/// size is not judged.
pub fn looks_like_slate(thumb: &[u8]) -> bool {
    let (width, height) = (THUMB_WIDTH as usize, THUMB_HEIGHT as usize);
    if thumb.len() != width * height {
        return false;
    }
    let flat = |a: u8, b: u8| a.abs_diff(b) <= FLAT_STEP;
    let across = thumb
        .chunks_exact(width)
        .flat_map(|row| row.windows(2))
        .filter(|pair| flat(pair[0], pair[1]))
        .count();
    let down = thumb
        .chunks_exact(width)
        .zip(thumb.chunks_exact(width).skip(1))
        .flat_map(|(upper, lower)| upper.iter().zip(lower))
        .filter(|(a, b)| flat(**a, **b))
        .count();
    let pairs = height * (width - 1) + (height - 1) * width;
    (across + down) as f64 >= SLATE_FLAT_SHARE * pairs as f64
}

/// Two thumbnails of different pictures; one of another size always is.
pub fn differs(before: &[u8], after: &[u8]) -> bool {
    if before.len() != after.len() || before.is_empty() {
        return true;
    }
    let total: u64 = before
        .iter()
        .zip(after)
        .map(|(a, b)| u64::from(a.abs_diff(*b)))
        .sum();
    total as f64 / before.len() as f64 > CHANGE_LEVEL
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(level: u8) -> Vec<u8> {
        vec![level; (THUMB_WIDTH * THUMB_HEIGHT) as usize]
    }

    #[test]
    fn black_is_nearly_all_dark_and_a_lit_corner_is_not() {
        assert!(is_black(&flat(16)));
        assert!(!is_black(&flat(128)));
        let mut logo = flat(16);
        // A bug in a corner (1% of the frame) still reads black.
        for pixel in logo.iter_mut().take(23) {
            *pixel = 235;
        }
        assert!(is_black(&logo));
        // A lower third (10%) does not.
        for pixel in logo.iter_mut().take(230) {
            *pixel = 235;
        }
        assert!(!is_black(&logo));
        assert!(!is_black(&[]));
    }

    #[test]
    fn coding_noise_is_the_same_picture_and_a_moved_bar_is_not() {
        let still = flat(100);
        let mut noisy = still.clone();
        for (index, pixel) in noisy.iter_mut().enumerate() {
            if index % 4 == 0 {
                *pixel = 101;
            }
        }
        assert!(!differs(&still, &noisy), "a quarter of pixels off by one");
        let mut moved = still.clone();
        for pixel in moved.iter_mut().take((THUMB_WIDTH * 4) as usize) {
            *pixel = 200;
        }
        assert!(differs(&still, &moved), "four rows changed");
        assert!(differs(&still, &still[1..]), "another size");
    }

    fn thumbnail(name: &str) -> Vec<u8> {
        let path = crate::media::test_fixtures::checked_in_fixture(&format!(
            "test/fixtures/stream-qa/{name}"
        ))
        .unwrap();
        std::fs::read(path).unwrap()
    }

    /// Thumbnails FFmpeg made with the sampler's scaler from its own
    /// sources (see test/fixtures/stream-qa/): drawn cards read as slates,
    /// textured pictures do not.
    #[test]
    fn drawn_cards_are_slates_and_textured_pictures_are_not() {
        for slate in ["slate-bars.gray", "slate-card.gray", "slate-gradient.gray"] {
            assert!(looks_like_slate(&thumbnail(slate)), "{slate}");
        }
        for picture in ["textured-fractal.gray", "textured-cells.gray"] {
            assert!(!looks_like_slate(&thumbnail(picture)), "{picture}");
        }
        assert!(looks_like_slate(&flat(90)));
        assert!(!looks_like_slate(&flat(90)[1..]), "another size");
    }
}

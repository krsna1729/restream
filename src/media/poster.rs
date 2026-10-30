//! Posters for stored media: one still frame from a little way into a file,
//! scaled down and written as a PNG by the external FFmpeg, so a library
//! reads as pictures. No codec runs inside the engine: a file that crashes
//! FFmpeg costs only its poster.

use std::path::Path;
use std::time::Duration;

/// Poster width in pixels (narrower sources keep theirs); the height keeps
/// the picture's shape, rounded to even.
pub const POSTER_WIDTH: u32 = 320;
/// How far in to look: past a fade-in or a slate.
const POSTER_AT_SEC: u32 = 5;
/// A poster that takes longer than this is abandoned.
const POSTER_WITHIN: Duration = Duration::from_secs(20);

/// The FFmpeg arguments for a PNG of the frame `at_sec` into `path`.
pub fn poster_args(path: &Path, at_sec: u32) -> Vec<String> {
    vec![
        "-nostdin".into(),
        "-v".into(),
        "error".into(),
        "-ss".into(),
        at_sec.to_string(),
        "-i".into(),
        path.display().to_string(),
        "-map".into(),
        "0:v:0".into(),
        "-frames:v".into(),
        "1".into(),
        "-vf".into(),
        format!("scale=w='min({POSTER_WIDTH},iw)':h=-2"),
        "-f".into(),
        "image2pipe".into(),
        "-c:v".into(),
        "png".into(),
        "pipe:1".into(),
    ]
}

/// A PNG of one frame of the file's video, at most `POSTER_WIDTH` wide: 5 s
/// in, or the first frame of a shorter file.
pub async fn extract_poster(path: &Path) -> Result<Vec<u8>, String> {
    let ffmpeg = crate::media::ffmpeg_binary::ffmpeg_bin_path();
    for at_sec in [POSTER_AT_SEC, 0] {
        let png = crate::media::ffmpeg_binary::run_tool(
            ffmpeg,
            &poster_args(path, at_sec),
            POSTER_WITHIN,
        )
        .await?;
        if png.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Ok(png);
        }
    }
    Err("no frame decoded".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_poster_asks_for_one_scaled_png_of_the_first_video() {
        let args = poster_args(Path::new("/m/a b.mp4"), 5).join(" ");
        assert!(args.contains("-ss 5 -i /m/a b.mp4 -map 0:v:0 -frames:v 1"));
        assert!(args.contains("scale=w='min(320,iw)':h=-2 -f image2pipe -c:v png pipe:1"));
    }

    #[tokio::test]
    async fn a_fixture_file_yields_a_poster_the_right_width() {
        let path = crate::test_fixtures::checked_in_fixture(
            "test/fixtures/media-library/colorbar-timer-2v16a.mp4",
        )
        .unwrap();
        let png = extract_poster(&path).await.expect("poster");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
        assert_eq!(width, POSTER_WIDTH);
        assert_eq!(height % 2, 0, "even height");
    }

    #[tokio::test]
    async fn a_file_shorter_than_five_seconds_still_gets_its_first_frame() {
        let path = crate::test_fixtures::checked_in_fixture(
            "test/fixtures/media-library/sparse-gop-5s.mp4",
        )
        .unwrap();
        // 5 s in is at or past its end; the first frame is used.
        let png = extract_poster(&path).await.expect("poster");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }
}

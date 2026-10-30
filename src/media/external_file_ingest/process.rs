use std::process::Stdio;

use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tracing::warn;

use super::ExternalFileIngestSource;

pub(super) struct SpawnedExternalFileIngest {
    pub(super) child: Child,
    pub(super) stdout: ChildStdout,
    pub(super) stderr: ChildStderr,
}

/// Spawns FFmpeg for item `index` of the source; `seek` applies the start
/// time (the first item of the first pass only).
pub(super) fn spawn_child(
    source: &ExternalFileIngestSource,
    index: usize,
    seek: bool,
) -> Result<SpawnedExternalFileIngest, String> {
    let ffmpeg_bin = crate::media::ffmpeg_binary::ffmpeg_bin_path();
    let args = build_ffmpeg_args(source, index, seek);
    let mut child = Command::new(ffmpeg_bin)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("Failed to spawn ffmpeg: {error}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Failed to capture ffmpeg stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Failed to capture ffmpeg stderr".to_string())?;

    Ok(SpawnedExternalFileIngest {
        child,
        stdout,
        stderr,
    })
}

fn build_ffmpeg_args(source: &ExternalFileIngestSource, index: usize, seek: bool) -> Vec<String> {
    let mut args = vec![
        "-nostdin".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "warning".into(),
        "-re".into(),
    ];
    // One file loops inside FFmpeg; a playlist loops by starting over.
    if source.loop_enabled && !source.is_playlist() {
        args.extend(["-stream_loop".into(), "-1".into()]);
    }
    if seek && !source.start_time.is_empty() {
        args.extend(["-ss".into(), source.start_time.clone()]);
    }
    let file = source.files.get(index).or(source.files.first());
    args.extend([
        "-i".into(),
        file.map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
    ]);
    if source.live_optimized {
        let target_gop_seconds = source.target_gop_seconds.max(1);
        args.extend([
            "-map".into(),
            "0:v:0".into(),
            "-map".into(),
            "0:a?".into(),
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "veryfast".into(),
            "-tune".into(),
            "zerolatency".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
            "-sc_threshold".into(),
            "0".into(),
            "-force_key_frames".into(),
            format!("expr:gte(t,n_forced*{target_gop_seconds})"),
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            "192k".into(),
            "-ar".into(),
            "48000".into(),
        ]);
    } else {
        args.extend(["-map".into(), "0".into(), "-c".into(), "copy".into()]);
    }
    args.extend([
        "-mpegts_flags".into(),
        "resend_headers+pat_pmt_at_frames".into(),
        "-pes_payload_size".into(),
        "0".into(),
        "-omit_video_pes_length".into(),
        "0".into(),
        "-flush_packets".into(),
        "1".into(),
        "-muxdelay".into(),
        "0".into(),
        "-muxpreload".into(),
        "0".into(),
        "-f".into(),
        "mpegts".into(),
        "pipe:1".into(),
    ]);
    args
}

pub(super) async fn capture_stderr(
    ingest_id: &str,
    mut stderr: ChildStderr,
) -> Result<(), std::io::Error> {
    const STDERR_CAP: usize = 64 * 1024;
    let mut buf = [0u8; 4096];
    let mut captured = Vec::new();
    let mut truncated = false;

    loop {
        match stderr.read(&mut buf).await {
            Ok(0) => break,
            Ok(read) => {
                let remaining = STDERR_CAP.saturating_sub(captured.len());
                if remaining > 0 {
                    captured.extend_from_slice(&buf[..read.min(remaining)]);
                } else if !truncated {
                    truncated = true;
                    warn!(
                        ingest_id = %ingest_id,
                        cap = STDERR_CAP,
                        "ffmpeg stderr truncated"
                    );
                }
            }
            Err(error) => return Err(error),
        }
    }

    if !captured.is_empty() {
        warn!(
            ingest_id = %ingest_id,
            stderr = %String::from_utf8_lossy(&captured).trim(),
            "ffmpeg stderr"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ExternalFileIngestSource, build_ffmpeg_args};

    fn source(live_optimized: bool) -> ExternalFileIngestSource {
        ExternalFileIngestSource {
            files: vec![PathBuf::from("/media/clip.mp4")],
            start_time: "00:00:05".to_string(),
            loop_enabled: true,
            live_optimized,
            target_gop_seconds: 4,
        }
    }

    #[test]
    fn a_playlist_item_plays_its_own_file_once_and_seeks_only_when_asked() {
        let playlist = ExternalFileIngestSource {
            files: vec![PathBuf::from("/media/a.mp4"), PathBuf::from("/media/b.mp4")],
            ..source(false)
        };
        let second = build_ffmpeg_args(&playlist, 1, false);
        assert!(has_arg_pair(&second, "-i", "/media/b.mp4"));
        assert!(
            !second.iter().any(|arg| arg == "-stream_loop"),
            "the list loops, not the item"
        );
        assert!(!second.iter().any(|arg| arg == "-ss"));
        assert!(has_arg_pair(
            &build_ffmpeg_args(&playlist, 0, true),
            "-ss",
            "00:00:05"
        ));
    }

    fn has_arg_pair(args: &[String], first: &str, second: &str) -> bool {
        args.windows(2)
            .any(|window| window[0] == first && window[1] == second)
    }

    #[test]
    fn build_ffmpeg_args_uses_copy_path_by_default() {
        let args = build_ffmpeg_args(&source(false), 0, true);

        assert!(has_arg_pair(&args, "-stream_loop", "-1"));
        assert!(has_arg_pair(&args, "-ss", "00:00:05"));
        assert!(has_arg_pair(&args, "-c", "copy"));
        assert!(has_arg_pair(&args, "-f", "mpegts"));
        assert_eq!(args.last().map(String::as_str), Some("pipe:1"));
    }

    #[test]
    fn build_ffmpeg_args_transcodes_live_optimized_inputs() {
        let args = build_ffmpeg_args(&source(true), 0, true);

        assert!(has_arg_pair(&args, "-c:v", "libx264"));
        assert!(has_arg_pair(&args, "-c:a", "aac"));
        assert!(has_arg_pair(
            &args,
            "-force_key_frames",
            "expr:gte(t,n_forced*4)"
        ));
        assert!(!has_arg_pair(&args, "-c", "copy"));
    }

    #[test]
    fn build_ffmpeg_args_clamps_live_gop_seconds() {
        let mut source = source(true);
        source.target_gop_seconds = 0;

        let args = build_ffmpeg_args(&source, 0, true);

        assert!(has_arg_pair(
            &args,
            "-force_key_frames",
            "expr:gte(t,n_forced*1)"
        ));
    }
}

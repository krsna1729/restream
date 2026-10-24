//! The sampler: an external FFmpeg per checked pipeline, so no codec runs
//! inside the engine. The source ring is fed to its stdin as MPEG-TS by
//! the transcoder's input pump (on its own thread, with back-pressure: a
//! slow child makes this reader lag and the ring moves it forward; the
//! writer and the outputs never wait). FFmpeg decodes only keyframes and
//! writes, per keyframe, a 64x36 grey thumbnail to stdout; it writes every
//! audio track (up to [`MAX_SOUND_TRACKS`]) as 48 kHz stereo f32, the
//! tracks side by side, to a Unix socket. Two threads read those and feed
//! the [`QaTracker`]. A child that dies is started again with
//! backoff: a crash in FFmpeg costs this pipeline's checks a few seconds
//! and nothing else.

use std::io::Read;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::loudness::LoudnessMeter;
use super::picture::{THUMB_HEIGHT, THUMB_WIDTH};
use super::{QaTracker, StreamQa};
use crate::media::engine::MediaEngine;
use crate::media::ffmpeg::stage_input::StageInputPump;
use crate::media::ring_buffer::RingBuffer;
use crate::media::stage_metrics::StageMetrics;

/// The audio FFmpeg hands back: 48 kHz, two channels per track, f32.
pub const SOUND_RATE: u32 = 48_000;
pub const SOUND_CHANNELS: usize = 2;
/// Tracks measured; a source with more has the rest left unchecked.
pub const MAX_SOUND_TRACKS: usize = 16;
/// A loudness reading every half second.
const SOUND_EVERY_MS: u64 = 500;
/// Backoff between restarts of a child that exited, doubling to the cap;
/// a child that ran this long resets it.
const RESTART_MIN: Duration = Duration::from_secs(2);
const RESTART_MAX: Duration = Duration::from_secs(30);
const RAN_LONG_ENOUGH: Duration = Duration::from_secs(60);

/// FFmpeg's arguments: TS on stdin; keyframes only, decoded on one thread;
/// per keyframe a grey thumbnail on stdout (raw, `THUMB_WIDTH` x
/// `THUMB_HEIGHT` bytes); with `sound` (a Unix socket the engine listens
/// on, and the source's track count) each audio track as stereo f32, the
/// tracks merged side by side into one stream (track `t` is channels `2t`
/// and `2t + 1`). A side the source lacks is left out (FFmpeg refuses a map
/// that matches nothing).
pub fn qa_ffmpeg_args(input: &str, video: bool, sound: Option<(&str, usize)>) -> Vec<String> {
    let mut args: Vec<String> = [
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-threads",
        "1",
        "-filter_threads",
        "1",
    ]
    .map(String::from)
    .to_vec();
    // A video decoder option: FFmpeg refuses it on an input with no video.
    if video {
        args.extend(["-skip_frame", "nokey"].map(String::from));
    }
    if input == "pipe:0" {
        args.extend(["-f", "mpegts"].map(String::from));
    }
    args.extend(["-i".to_string(), input.to_string()]);
    if video {
        args.extend(
            [
                "-map",
                "0:v:0",
                "-fps_mode",
                "passthrough",
                "-vf",
                &format!("scale={THUMB_WIDTH}:{THUMB_HEIGHT}:flags=area,format=gray"),
                "-f",
                "rawvideo",
                "pipe:1",
            ]
            .map(String::from),
        );
    }
    if let Some((sound, tracks)) = sound {
        let tracks = tracks.clamp(1, MAX_SOUND_TRACKS);
        if tracks == 1 {
            args.extend(
                [
                    "-map",
                    "0:a:0",
                    "-ac",
                    &SOUND_CHANNELS.to_string(),
                    "-ar",
                    &SOUND_RATE.to_string(),
                ]
                .map(String::from),
            );
        } else {
            let mut graph = String::new();
            for track in 0..tracks {
                graph.push_str(&format!(
                    "[0:a:{track}]aresample={SOUND_RATE},aformat=sample_fmts=flt:channel_layouts=stereo[a{track}];"
                ));
            }
            for track in 0..tracks {
                graph.push_str(&format!("[a{track}]"));
            }
            graph.push_str(&format!("amerge=inputs={tracks}[sound]"));
            args.extend(["-filter_complex".to_string(), graph]);
            args.extend(["-map", "[sound]"].map(String::from));
        }
        args.extend([
            "-f".to_string(),
            "f32le".to_string(),
            format!("unix:{sound}"),
        ]);
    }
    args
}

/// The tracker and where its findings are published, shared by the two
/// reader threads.
#[derive(Clone)]
pub struct Findings {
    tracker: Arc<Mutex<QaTracker>>,
    published: Arc<Mutex<StreamQa>>,
}

impl Findings {
    pub fn new(published: Arc<Mutex<StreamQa>>) -> Self {
        Self {
            tracker: Arc::default(),
            published,
        }
    }

    fn update(&self, change: impl FnOnce(&mut QaTracker)) {
        let mut tracker = crate::sync::lock(&self.tracker);
        change(&mut tracker);
        *crate::sync::lock(&self.published) = tracker.snapshot().clone();
    }

    /// A restarted child starts a new picture sequence.
    fn forget_picture(&self) {
        crate::sync::lock(&self.tracker).forget_picture();
    }
}

/// Reads grey thumbnails until the pipe ends: each is one picture.
pub fn read_pictures(mut pipe: impl Read, findings: &Findings) {
    let mut thumb = vec![0u8; (THUMB_WIDTH * THUMB_HEIGHT) as usize];
    while pipe.read_exact(&mut thumb).is_ok() {
        let now = MediaEngine::now_epoch_ms();
        findings.update(|tracker| tracker.picture(now, thumb.clone()));
    }
}

/// Reads interleaved f32 (`tracks` stereo pairs side by side) until the
/// pipe ends, measuring each track, and reports their loudness every half
/// second.
pub fn read_sound(mut pipe: impl Read, findings: &Findings, tracks: usize) {
    let tracks = tracks.clamp(1, MAX_SOUND_TRACKS);
    let Some(mut meters) = (0..tracks)
        .map(|_| LoudnessMeter::new(SOUND_RATE, SOUND_CHANNELS))
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    // 100 ms of audio per read.
    let frame_bytes = tracks * SOUND_CHANNELS * 4;
    let mut bytes = vec![0u8; (SOUND_RATE as usize / 10) * frame_bytes];
    let mut planes = vec![vec![Vec::new(); SOUND_CHANNELS]; tracks];
    let mut last_reading = 0;
    let mut readings = Vec::with_capacity(tracks);
    loop {
        let filled = match read_some(&mut pipe, &mut bytes) {
            0 => return,
            filled => filled - filled % frame_bytes,
        };
        for plane in planes.iter_mut().flatten() {
            plane.clear();
        }
        for frame in bytes[..filled].chunks_exact(frame_bytes) {
            for (channel, sample) in frame.chunks_exact(4).enumerate() {
                planes[channel / SOUND_CHANNELS][channel % SOUND_CHANNELS].push(
                    f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]),
                );
            }
        }
        for (meter, track) in meters.iter_mut().zip(&planes) {
            let views: Vec<&[f32]> = track.iter().map(Vec::as_slice).collect();
            meter.push_planar(&views);
        }
        let now = MediaEngine::now_epoch_ms();
        if now.saturating_sub(last_reading) < SOUND_EVERY_MS {
            continue;
        }
        readings.clear();
        readings.extend(meters.iter().map_while(LoudnessMeter::short_term_lufs));
        if readings.len() == tracks {
            last_reading = now;
            findings.update(|tracker| tracker.sound(now, &readings));
        }
    }
}

/// Fills `buf` as far as the pipe allows; 0 at its end.
fn read_some(pipe: &mut impl Read, buf: &mut [u8]) -> usize {
    let mut filled = 0;
    while filled < buf.len() {
        match pipe.read(&mut buf[filled..]) {
            Ok(0) | Err(_) => break,
            Ok(read) => filled += read,
        }
    }
    filled
}

/// Where FFmpeg sends the sound: a Unix socket the engine listens on (no
/// descriptor passing, so no unsafe code), removed when dropped.
pub struct SoundSocket {
    path: std::path::PathBuf,
    listener: std::os::unix::net::UnixListener,
}

impl SoundSocket {
    pub fn bind() -> std::io::Result<Self> {
        let path =
            std::env::temp_dir().join(format!("restream-qa-{:016x}.sock", rand::random::<u64>()));
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        Ok(Self { path, listener })
    }

    pub fn path(&self) -> String {
        self.path.display().to_string()
    }

    /// Waits for FFmpeg to connect, up to `within`, or until `stop`.
    pub fn accept(
        &self,
        within: Duration,
        stop: &CancellationToken,
    ) -> Option<std::os::unix::net::UnixStream> {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline && !stop.is_cancelled() {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).ok()?;
                    return Some(stream);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return None,
            }
        }
        None
    }
}

impl Drop for SoundSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Runs the checks for `pipeline_id` until `cancel`: one child at a time,
/// restarted with backoff when it exits.
pub async fn run(
    engine: Arc<MediaEngine>,
    pipeline_id: String,
    ring: Arc<RingBuffer>,
    published: Arc<Mutex<StreamQa>>,
    cancel: CancellationToken,
) {
    let findings = Findings::new(published);
    let mut backoff = RESTART_MIN;
    while !cancel.is_cancelled() {
        let started = std::time::Instant::now();
        if let Err(error) = run_child(&engine, &pipeline_id, &ring, &findings, &cancel).await {
            warn!(pipeline_id = %pipeline_id, err = %error, "stream checks: ffmpeg ended");
        }
        if started.elapsed() >= RAN_LONG_ENOUGH {
            backoff = RESTART_MIN;
        }
        findings.forget_picture();
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(RESTART_MAX);
    }
}

/// One child's life: feed it, read it, reap it.
async fn run_child(
    engine: &Arc<MediaEngine>,
    pipeline_id: &str,
    ring: &Arc<RingBuffer>,
    findings: &Findings,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let (video, audio) = {
        let ingests = engine.ingests.active.read().await;
        let Some(ingest) = ingests.get(pipeline_id) else {
            return Err("no live source".to_string());
        };
        (
            ingest.metadata().video.clone(),
            crate::sync::lock(&ingest.audio_tracks).to_vec(),
        )
    };
    let has_video = video.is_some();
    let has_audio = !audio.is_empty();
    let sound_tracks = audio.len().min(MAX_SOUND_TRACKS);
    if !has_video && !has_audio {
        return Err("the source has reported no track yet".to_string());
    }
    let (sequence_header, _) = engine.get_sequence_headers(pipeline_id).await;
    let pump = StageInputPump::new(
        format!("qa:{pipeline_id}"),
        ring.clone(),
        0,
        video.as_ref(),
        &audio,
        true,
        Arc::new(StageMetrics::new()),
    )
    .with_video_sequence_header(sequence_header)
    .with_engine(engine.clone(), pipeline_id.to_string());

    let sound = if has_audio {
        Some(SoundSocket::bind().map_err(|error| format!("sound socket: {error}"))?)
    } else {
        None
    };
    let mut child = tokio::process::Command::new(crate::ffmpeg_binary::ffmpeg_bin_path())
        .args(qa_ffmpeg_args(
            "pipe:0",
            has_video,
            sound
                .as_ref()
                .map(|socket| (socket.path(), sound_tracks))
                .as_ref()
                .map(|(path, tracks)| (path.as_str(), *tracks)),
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn: {error}"))?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let mut stderr = child.stderr.take().ok_or("no stderr")?;

    let mut threads = Vec::with_capacity(3);
    let child_cancel = cancel.child_token();
    let (input_done, input_result) = tokio::sync::oneshot::channel();
    threads.push(
        crate::media::external_transcoder::spawn_external_stdin_writer(
            stdin,
            pump.into_queue_refill(child_cancel.clone()),
            Arc::new(crate::media::pipe_metrics::PipeMetrics::default()),
            input_done,
        )
        .map_err(|error| error.to_string())?,
    );
    let pictures = crate::media::external_transcoder::blocking_pipe(
        stdout.into_owned_fd().map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let picture_findings = findings.clone();
    threads.push(
        std::thread::Builder::new()
            .name("stream-qa-pic".to_string())
            .spawn(move || read_pictures(pictures, &picture_findings))
            .map_err(|error| error.to_string())?,
    );
    if let Some(sound) = sound {
        let sound_findings = findings.clone();
        let stop = child_cancel.clone();
        threads.push(
            std::thread::Builder::new()
                .name("stream-qa-snd".to_string())
                .spawn(move || {
                    // FFmpeg connects once it has decoded the first audio.
                    if let Some(stream) = sound.accept(Duration::from_secs(30), &stop) {
                        read_sound(stream, &sound_findings, sound_tracks);
                    }
                })
                .map_err(|error| error.to_string())?,
        );
    }

    // The last of stderr says why a child gave up.
    let tail = tokio::spawn(async move {
        let mut text = Vec::new();
        let _ = stderr.read_to_end(&mut text).await;
        let start = text.len().saturating_sub(600);
        String::from_utf8_lossy(&text[start..]).trim().to_string()
    });
    let cancelled = tokio::select! {
        () = cancel.cancelled() => true,
        _ = child.wait() => false,
        _ = input_result => false,
    };
    child_cancel.cancel();
    crate::media::external_transcoder::reap_external_child(&mut child, true, threads).await;
    let why = tail.await.unwrap_or_default();
    if cancelled {
        Ok(())
    } else {
        Err(if why.is_empty() {
            "exited".to_string()
        } else {
            why
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_leave_out_a_side_the_source_lacks() {
        let both = qa_ffmpeg_args("pipe:0", true, Some(("/tmp/s.sock", 1))).join(" ");
        assert!(both.contains("-skip_frame nokey -f mpegts -i pipe:0"));
        assert!(both.contains("scale=64:36:flags=area,format=gray -f rawvideo pipe:1"));
        assert!(both.contains("-map 0:a:0 -ac 2 -ar 48000 -f f32le unix:/tmp/s.sock"));
        let three = qa_ffmpeg_args("pipe:0", false, Some(("/s", 3))).join(" ");
        assert!(
            !three.contains("-skip_frame"),
            "an audio-only source gets no video decoder option"
        );
        assert!(three.contains(
            "[0:a:2]aresample=48000,aformat=sample_fmts=flt:channel_layouts=stereo[a2];"
        ));
        assert!(three.contains("[a0][a1][a2]amerge=inputs=3[sound] -map [sound] -f f32le unix:/s"));
        assert!(
            !qa_ffmpeg_args("pipe:0", true, Some(("/s", 99)))
                .join(" ")
                .contains("[0:a:16]"),
            "tracks past the cap are not checked"
        );
        assert!(
            !qa_ffmpeg_args("pipe:0", false, Some(("/s", 1)))
                .join(" ")
                .contains("pipe:1")
        );
        assert!(
            !qa_ffmpeg_args("pipe:0", true, None)
                .join(" ")
                .contains("unix:")
        );
        assert!(
            !qa_ffmpeg_args("in.mp4", true, None)
                .join(" ")
                .contains("mpegts")
        );
    }

    #[test]
    fn pictures_and_sound_are_read_from_their_pipes() {
        let published = Arc::new(Mutex::new(StreamQa::default()));
        let findings = Findings::new(published.clone());
        // Two black thumbnails.
        let frame = (THUMB_WIDTH * THUMB_HEIGHT) as usize;
        read_pictures(&vec![8u8; frame * 2][..], &findings);
        assert!(published.lock().unwrap().black_since_ms.is_some());
        // 4 s of a 1 kHz stereo tone at -23 dBFS, as FFmpeg writes it.
        let amplitude = 10f64.powf(-23.0 / 20.0);
        let mut bytes = Vec::new();
        for n in 0..SOUND_RATE as usize * 4 {
            let sample = (amplitude
                * (2.0 * std::f64::consts::PI * 1000.0 * n as f64 / f64::from(SOUND_RATE)).sin())
                as f32;
            bytes.extend_from_slice(&sample.to_le_bytes());
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        read_sound(&bytes[..], &findings, 1);
        let lufs = published.lock().unwrap().loudness_lufs.unwrap();
        assert!((lufs + 23.0).abs() < 0.2, "{lufs}");

        // Two tracks side by side: the tone, then silence.
        let mut pair = Vec::new();
        for frame in bytes.chunks_exact(8) {
            pair.extend_from_slice(frame);
            pair.extend_from_slice(&[0u8; 8]);
        }
        read_sound(&pair[..], &findings, 2);
        let qa = published.lock().unwrap().clone();
        assert!((qa.tracks[0].loudness_lufs + 23.0).abs() < 0.2);
        assert_eq!(qa.tracks[1].loudness_lufs, -70.0);
        assert!(qa.tracks[1].silent_since_ms.is_some());
    }

    /// The real FFmpeg on the checked-in clip, with the arguments the
    /// sampler uses (a file instead of stdin): keyframes come out as moving
    /// colour bars and each of its 16 tracks as a loudness.
    #[test]
    fn ffmpeg_turns_the_fixture_into_pictures_and_a_loudness() {
        let path = crate::test_fixtures::checked_in_fixture(
            "test/fixtures/media-library/colorbar-timer-2v16a.mp4",
        )
        .unwrap();
        let socket = SoundSocket::bind().unwrap();
        let mut args = qa_ffmpeg_args(
            &path.display().to_string(),
            true,
            Some((&socket.path(), MAX_SOUND_TRACKS)),
        );
        // Ten seconds is enough for 3 s of loudness and a few keyframes.
        let input = args.iter().position(|arg| arg == "-i").unwrap();
        args.splice(input..input, ["-t".to_string(), "10".to_string()]);
        let mut child = std::process::Command::new(crate::ffmpeg_binary::ffmpeg_bin_path())
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let published = Arc::new(Mutex::new(StreamQa::default()));
        let findings = Findings::new(published.clone());
        let sound_findings = findings.clone();
        let sound = std::thread::spawn(move || {
            let stream = socket
                .accept(Duration::from_secs(10), &CancellationToken::new())
                .expect("ffmpeg connects for the sound");
            read_sound(stream, &sound_findings, MAX_SOUND_TRACKS);
        });
        read_pictures(child.stdout.take().unwrap(), &findings);
        sound.join().unwrap();
        assert!(child.wait().unwrap().success());
        let qa = published.lock().unwrap().clone();
        assert!(qa.picture_checked_ms.is_some(), "keyframes decoded");
        assert_eq!(qa.black_since_ms, None, "colour bars are not black");
        let lufs = qa.loudness_lufs.expect("3 s of audio measured");
        assert!((-70.0..0.0).contains(&lufs), "loudness {lufs}");
        assert_eq!(qa.tracks.len(), MAX_SOUND_TRACKS, "every track measured");
    }

    /// A clip whose second track is silent: the checks name that track.
    #[test]
    fn ffmpeg_finds_the_silent_second_track() {
        let path = crate::test_fixtures::checked_in_fixture(
            "test/fixtures/stream-qa/tone-then-silent-track.mp4",
        )
        .unwrap();
        let socket = SoundSocket::bind().unwrap();
        let args = qa_ffmpeg_args(
            &path.display().to_string(),
            false,
            Some((&socket.path(), 2)),
        );
        let mut child = std::process::Command::new(crate::ffmpeg_binary::ffmpeg_bin_path())
            .args(&args)
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let published = Arc::new(Mutex::new(StreamQa::default()));
        let findings = Findings::new(published.clone());
        let stream = socket
            .accept(Duration::from_secs(10), &CancellationToken::new())
            .expect("ffmpeg connects for the sound");
        read_sound(stream, &findings, 2);
        assert!(child.wait().unwrap().success());
        let qa = published.lock().unwrap().clone();
        assert_eq!(qa.tracks.len(), 2);
        assert_eq!(qa.silent_since_ms, None, "the first track carries a tone");
        let now = qa.sound_checked_ms.unwrap();
        assert_eq!(
            qa.other_silent_tracks(now)
                .first()
                .map(|silent| silent.track),
            Some(1)
        );
    }

    /// An enhanced RTMP H.265 publish (the checked-in H.265 clip as the
    /// egress converter writes it) yields pictures: the feeder turns its
    /// tags into TS led by the hvcC parameter sets, as the sampler's pump
    /// does, and FFmpeg decodes keyframes from it with the sampler's
    /// arguments.
    #[test]
    fn an_enhanced_rtmp_hevc_source_yields_pictures() {
        use crate::media::codec;
        use crate::media::feeder::{PacketFeedConfig, TsPacketFeeder};
        use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
        use std::io::Write;

        let (video, _, packets) =
            crate::test_fixtures::primary_av_packets_for_codec("h265").unwrap();
        let video_packets: Vec<&MediaPacket> = packets
            .iter()
            .filter(|p| p.media_type == MediaType::Video)
            .collect();
        let header = codec::build_hevc_enhanced_rtmp_sequence_header(
            &video_packets
                .iter()
                .find(|p| p.is_keyframe)
                .unwrap()
                .payload,
        )
        .unwrap();
        let mut feeder = TsPacketFeeder::new(
            Some(&video),
            Arc::new(Vec::new()),
            PacketFeedConfig::default(),
        );
        feeder.set_video_sequence_header_from_avcc(&header);
        let (mut ts, mut tag) = (Vec::new(), Vec::new());
        for packet in video_packets {
            let composition = i32::try_from(packet.pts - packet.dts).unwrap();
            codec::hevc_video_for_enhanced_rtmp_with_composition_into(
                &packet.payload,
                packet.is_keyframe,
                composition,
                &mut tag,
            );
            let flv = MediaPacket {
                payload: bytes::Bytes::copy_from_slice(&tag),
                format: PayloadFormat::Flv,
                ..packet.clone()
            };
            feeder.extend_ts_for_packet(&flv, &mut ts);
        }
        assert!(!ts.is_empty(), "the feeder muxed the enhanced tags");

        let mut child = std::process::Command::new(crate::ffmpeg_binary::ffmpeg_bin_path())
            .args(qa_ffmpeg_args("pipe:0", true, None))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&ts);
        });
        let published = Arc::new(Mutex::new(StreamQa::default()));
        read_pictures(
            child.stdout.take().unwrap(),
            &Findings::new(published.clone()),
        );
        writer.join().unwrap();
        assert!(child.wait().unwrap().success());
        let qa = published.lock().unwrap().clone();
        assert!(qa.picture_checked_ms.is_some(), "keyframes decoded");
        assert_eq!(qa.black_since_ms, None);
    }
}

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use proptest::prelude::*;

use super::*;
use crate::media::packet::{MediaPacket, PayloadFormat};

/// A clean stream: video every 33 ms and audio every 21 ms, each arriving
/// exactly when its DTS says (plus `audio_late_ms` for audio), for
/// `seconds`, starting at `start_ms`.
fn feed(timeline: &mut Timeline, start_ms: i64, seconds: i64, audio_late_ms: i64) {
    let end = start_ms + seconds * 1_000;
    let mut video = start_ms;
    let mut audio = start_ms;
    while video < end || audio < end {
        if video <= audio {
            timeline.observe(
                MediaType::Video,
                0,
                video + 66,
                video,
                (video * 1_000) as u64,
                None,
            );
            video += 33;
        } else {
            timeline.observe(
                MediaType::Audio,
                0,
                audio,
                audio,
                ((audio + audio_late_ms) * 1_000) as u64,
                None,
            );
            audio += 21;
        }
    }
}

fn track<'a>(inspection: &'a StreamInspection, media: &str, index: u32) -> &'a TrackInspection {
    inspection
        .tracks
        .iter()
        .find(|track| track.media == media && track.track == index)
        .expect("track present")
}

#[test]
fn a_clean_stream_has_no_errors_and_audio_keeps_time() {
    let mut timeline = Timeline::default();
    timeline.inspect(0);
    feed(&mut timeline, 0, 10, 0);
    let inspection = timeline.inspect(10_000);
    assert_eq!(inspection.tracks.len(), 2);
    assert_eq!(inspection.tracks[0].media, "video", "video first");
    let video = track(&inspection, "video", 0);
    assert_eq!(
        (video.backwards, video.jumps, video.pts_before_dts),
        (0, 0, 0)
    );
    assert!(
        (video.packets_per_sec - 30.3).abs() < 0.5,
        "{}",
        video.packets_per_sec
    );
    let audio = track(&inspection, "audio", 0);
    assert_eq!((audio.backwards, audio.jumps), (0, 0));
    assert_eq!(audio.av_offset_ms, Some(0));
    assert_eq!(
        audio.clock_drift_ms_per_min, None,
        "no AAC frame length given"
    );
    assert_eq!(video.av_offset_ms, None, "offsets are audio's");
    assert_eq!(inspection.unread, 0);
}

#[test]
fn a_clock_reset_is_backwards_and_a_media_clock_jump_is_a_jump() {
    let mut timeline = Timeline::default();
    timeline.inspect(0);
    timeline.observe(MediaType::Video, 0, 1_000, 1_000, 1_000_000, None);
    timeline.observe(MediaType::Video, 0, 1_033, 1_033, 1_033_000, None);
    // The encoder's clock restarts.
    timeline.observe(MediaType::Video, 0, 10, 10, 1_066_000, None);
    timeline.observe(MediaType::Video, 0, 43, 43, 1_099_000, None);
    // Its clock leaps 5 s ahead while arrival moves 33 ms.
    timeline.observe(MediaType::Video, 0, 5_076, 5_076, 1_132_000, None);
    let video = track(&timeline.inspect(2_000), "video", 0).clone();
    assert_eq!(video.backwards, 1);
    assert_eq!(video.jumps, 1);
    assert_eq!(video.largest_jump_ms, Some(5_033 - 33));
}

#[test]
fn a_network_stall_moves_both_clocks_and_is_not_a_jump() {
    let mut timeline = Timeline::default();
    timeline.inspect(0);
    timeline.observe(MediaType::Video, 0, 0, 0, 0, None);
    // Nothing for 3 s, then the media resumes where real time is.
    timeline.observe(MediaType::Video, 0, 3_000, 3_000, 3_000_000, None);
    // A burst flushed after a sender-side stall: small steps, small gaps.
    for step in 1..10 {
        timeline.observe(
            MediaType::Video,
            0,
            3_000 + step * 33,
            3_000 + step * 33,
            3_000_000 + step as u64 * 100,
            None,
        );
    }
    let video = track(&timeline.inspect(4_000), "video", 0).clone();
    assert_eq!((video.backwards, video.jumps), (0, 0));
}

#[test]
fn a_picture_shown_before_it_decodes_is_counted() {
    let mut timeline = Timeline::default();
    timeline.observe(MediaType::Video, 0, 90, 100, 100_000, None);
    timeline.observe(MediaType::Video, 0, 200, 133, 133_000, None);
    assert_eq!(
        track(&timeline.inspect(1_000), "video", 0).pts_before_dts,
        1
    );
}

#[test]
fn audio_arriving_late_reads_as_an_offset_from_the_video() {
    let mut timeline = Timeline::default();
    timeline.inspect(0);
    feed(&mut timeline, 0, 10, 250);
    let audio = track(&timeline.inspect(10_000), "audio", 0).clone();
    assert_eq!(audio.av_offset_ms, Some(250));
    assert_eq!(
        audio.clock_drift_ms_per_min, None,
        "no AAC frame length given"
    );
}

/// An AAC track at 48 kHz whose timestamps run `scale` times the sample
/// clock, for `seconds`; arrivals follow the timestamps (a muxer that
/// interleaves by timestamp hides drift from arrival times).
fn aac(timeline: &mut Timeline, seconds: u32, scale: f64, frame_ms_declared: f64) {
    let frame_ms = 1024.0 / 48.0;
    let frames = (f64::from(seconds) * 1_000.0 / frame_ms) as u64;
    for frame in 0..frames {
        let dts = (frame as f64 * frame_ms * scale).round() as i64;
        timeline.observe(
            MediaType::Audio,
            0,
            dts,
            dts,
            dts as u64 * 1_000,
            Some(frame_ms_declared),
        );
    }
}

#[test]
fn an_aac_clock_that_runs_fast_against_its_samples_is_drift() {
    let frame_ms = aac_frame_ms(48_000).expect("rate");
    assert!((frame_ms - 21.333).abs() < 0.001);
    let mut fast = Timeline::default();
    aac(&mut fast, 40, 1.005, frame_ms);
    let drift = track(&fast.inspect(40_000), "audio", 0)
        .clock_drift_ms_per_min
        .expect("drift");
    assert!(
        (drift - 300.0).abs() < 3.0,
        "0.5% fast is 300 ms a minute, got {drift}"
    );
    assert_eq!(
        fast.inspect(40_000).tracks[0].av_offset_ms,
        None,
        "no video to compare"
    );

    let mut true_clock = Timeline::default();
    aac(&mut true_clock, 40, 1.0, frame_ms);
    let drift = track(&true_clock.inspect(40_000), "audio", 0)
        .clock_drift_ms_per_min
        .expect("drift");
    assert!(
        drift.abs() < 1.0,
        "millisecond rounding is not drift, got {drift}"
    );
}

#[test]
fn drift_needs_twenty_seconds_and_a_plausible_frame_length() {
    let frame_ms = aac_frame_ms(48_000).expect("rate");
    let mut short = Timeline::default();
    aac(&mut short, 10, 1.005, frame_ms);
    assert_eq!(
        track(&short.inspect(10_000), "audio", 0).clock_drift_ms_per_min,
        None
    );
    // HE-AAC declared at its core rate: frames are twice as long as assumed.
    let mut wrong = Timeline::default();
    aac(&mut wrong, 40, 1.0, frame_ms * 2.0);
    assert_eq!(
        track(&wrong.inspect(40_000), "audio", 0).clock_drift_ms_per_min,
        None
    );
}

#[test]
fn a_broken_audio_clock_starts_a_new_run() {
    let frame_ms = aac_frame_ms(48_000).expect("rate");
    let mut timeline = Timeline::default();
    aac(&mut timeline, 40, 1.005, frame_ms);
    // The encoder restarts: its clock goes back; 25 s of true clock after.
    for frame in 0..1_172u64 {
        let dts = (frame as f64 * frame_ms).round() as i64;
        timeline.observe(
            MediaType::Audio,
            0,
            dts,
            dts,
            50_000_000 + dts as u64 * 1_000,
            Some(frame_ms),
        );
    }
    let audio = track(&timeline.inspect(60_000), "audio", 0).clone();
    assert_eq!(audio.backwards, 1);
    let drift = audio
        .clock_drift_ms_per_min
        .expect("25 s of unbroken clock");
    assert!(
        drift.abs() < 1.0,
        "only the run since the reset counts, got {drift}"
    );
}

#[test]
fn counts_cover_the_last_minute_only() {
    let mut timeline = Timeline::default();
    timeline.inspect(0);
    timeline.observe(MediaType::Video, 0, 1_000, 1_000, 1_000_000, None);
    timeline.observe(MediaType::Video, 0, 10, 10, 1_033_000, None);
    assert_eq!(track(&timeline.inspect(1_000), "video", 0).backwards, 1);
    for second in 2..=61u64 {
        let dts = (second * 1_000) as i64;
        timeline.observe(
            MediaType::Video,
            0,
            dts,
            dts,
            second * 1_000_000 + 10_000_000,
            None,
        );
        timeline.inspect(second * 1_000);
    }
    let inspection = timeline.inspect(62_000);
    assert_eq!(
        track(&inspection, "video", 0).backwards,
        0,
        "a reset over a minute ago has aged out"
    );
    assert_eq!(inspection.window_sec, 60.0);
}

#[test]
fn no_jump_is_counted_across_packets_that_were_not_read() {
    let mut timeline = Timeline::default();
    timeline.inspect(0);
    timeline.observe(MediaType::Video, 0, 0, 0, 0, None);
    timeline.skipped(500);
    timeline.observe(MediaType::Video, 0, 20_000, 20_000, 1_000_000, None);
    let inspection = timeline.inspect(1_000);
    assert_eq!(track(&inspection, "video", 0).jumps, 0);
    assert_eq!(inspection.unread, 500);
}

#[test]
fn a_track_gone_for_the_whole_window_is_dropped() {
    let mut timeline = Timeline::default();
    timeline.inspect(0);
    timeline.observe(MediaType::Audio, 1, 0, 0, 0, None);
    assert_eq!(timeline.inspect(1_000).tracks.len(), 1);
    assert!(timeline.inspect(62_000).tracks.is_empty());
}

fn packet(media_type: MediaType, dts: i64) -> MediaPacket {
    MediaPacket {
        media_type,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0,
        pts: dts,
        dts,
        payload: Bytes::from_static(&[0]),
    }
}

#[test]
fn a_scan_counts_aac_frames_but_not_flv_configuration() {
    let ring = RingBuffer::new(4096);
    let mut timeline = Timeline::default();
    let mut next = ring.get_write_idx();
    let frame_ms = aac_frame_ms(48_000).expect("rate");
    let flv = |payload: &'static [u8], dts: i64| MediaPacket {
        media_type: MediaType::Audio,
        format: PayloadFormat::Flv,
        payload: Bytes::from_static(payload),
        ..packet(MediaType::Audio, dts)
    };
    // A sequence header at the start and again at 10 s, as a publisher
    // that re-sends its configuration does; frames run 0.5% fast.
    for frame in 0..1_500u64 {
        let dts = (frame as f64 * frame_ms * 1.005).round() as i64;
        if frame % 470 == 0 {
            ring.push(flv(&[0xaf, 0, 0x11, 0x90], dts));
        }
        ring.push(flv(&[0xaf, 1, 0x21], dts));
        if frame % 500 == 499 {
            scan(
                &ring,
                &mut next,
                &mut timeline,
                &HashMap::from([(0, frame_ms)]),
            );
        }
    }
    scan(
        &ring,
        &mut next,
        &mut timeline,
        &HashMap::from([(0, frame_ms)]),
    );
    let drift = track(&timeline.inspect(40_000), "audio", 0)
        .clock_drift_ms_per_min
        .expect("drift");
    assert!(
        (drift - 300.0).abs() < 3.0,
        "configuration packets are not frames, got {drift}"
    );
}

#[test]
fn a_scan_reads_what_the_ring_gained_and_counts_what_it_lost() {
    let ring = RingBuffer::new(64);
    let mut timeline = Timeline::default();
    let mut next = ring.get_write_idx();
    for dts in 0..10 {
        ring.push(packet(MediaType::Video, dts * 33));
    }
    scan(&ring, &mut next, &mut timeline, &HashMap::new());
    assert_eq!(next, 10);
    let inspection = timeline.inspect(1_000);
    assert_eq!(inspection.tracks.len(), 1);
    assert_eq!(inspection.unread, 0);

    // The ring laps the inspector: only what is a quarter of the ring behind
    // the writer is read, the rest is unread.
    for dts in 10..210 {
        ring.push(packet(MediaType::Video, dts * 33));
    }
    scan(&ring, &mut next, &mut timeline, &HashMap::new());
    assert_eq!(next, 210);
    let inspection = timeline.inspect(2_000);
    assert_eq!(
        inspection.unread,
        200 - 48,
        "48 = 64 - 64 / 4 slots were readable"
    );
    assert_eq!(track(&inspection, "video", 0).backwards, 0);
}

#[test]
fn the_thread_inspects_the_rings_it_is_given_and_stops_when_dropped() {
    let ring = Arc::new(RingBuffer::new(256));
    let inspector = StreamInspector::start().expect("thread");
    inspector.watch(vec![Watched {
        pipeline_id: "p1".to_string(),
        ring: Arc::downgrade(&ring),
        aac_frame_ms: HashMap::new(),
    }]);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut dts = 0;
    let found = loop {
        ring.push(packet(MediaType::Audio, dts));
        dts += 21;
        if let Some(inspection) = inspector.inspection("p1")
            && !inspection.tracks.is_empty()
        {
            break inspection;
        }
        assert!(Instant::now() < deadline, "no inspection within 5 s");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(found.tracks[0].media, "audio");
    assert!(inspector.inspection("p2").is_none());
    let shared = Arc::downgrade(&inspector.shared);
    drop(inspector);
    // The thread holds the last reference until it exits.
    let deadline = Instant::now() + Duration::from_secs(5);
    while shared.upgrade().is_some() {
        assert!(Instant::now() < deadline, "the thread did not stop");
        std::thread::sleep(Duration::from_millis(10));
    }
}

proptest! {
    /// Against a model: backwards steps are exactly the strict decreases in
    /// DTS, and a stream whose DTS follows its arrival never jumps.
    #[test]
    fn backwards_and_jumps_follow_the_model(steps in prop::collection::vec(-50_i64..200, 1..300)) {
        let mut timeline = Timeline::default();
        let mut dts = 10_000_i64;
        let mut arrival_ms = 10_000_i64;
        let mut expected_backwards = 0u32;
        timeline.observe(MediaType::Video, 0, dts, dts, (arrival_ms * 1_000) as u64, None);
        for step in steps {
            if step < 0 {
                expected_backwards += 1;
            }
            dts += step;
            // Arrival follows the media clock whenever it moves forward.
            arrival_ms += step.max(0);
            timeline.observe(MediaType::Video, 0, dts, dts, (arrival_ms * 1_000) as u64, None);
        }
        let inspection = timeline.inspect(1_000);
        let video = track(&inspection, "video", 0);
        prop_assert_eq!(video.backwards, expected_backwards);
        prop_assert_eq!(video.jumps, 0);
        prop_assert_eq!(video.pts_before_dts, 0);
    }
}

#[test]
fn captions_seen_then_stopped_keep_being_reported() {
    use crate::media::codec::FrameCaptions;
    let mut timeline = Timeline::default();
    timeline.inspect(0);
    let captioned = FrameCaptions {
        cea608: true,
        cea708: false,
        text: true,
    };
    let padding = FrameCaptions {
        cea608: true,
        cea708: false,
        text: false,
    };
    for second in 0..5u64 {
        let dts = (second * 1_000) as i64;
        timeline.observe(MediaType::Video, 0, dts, dts, second * 1_000_000, None);
        timeline.observe_captions(0, if second < 3 { captioned } else { padding });
        timeline.inspect((second + 1) * 1_000);
    }
    let captions = track(&timeline.inspect(5_000), "video", 0)
        .captions
        .clone()
        .expect("captions seen");
    assert!(captions.cea608 && !captions.cea708);
    assert_eq!(captions.last_seen_sec, 0.0);
    assert_eq!(captions.last_text_sec, Some(2.0), "padding is not text");

    // The source keeps sending video but no captions, past the window.
    for second in 5..95u64 {
        let dts = (second * 1_000) as i64;
        timeline.observe(MediaType::Video, 0, dts, dts, second * 1_000_000, None);
        timeline.observe_captions(0, FrameCaptions::default());
        timeline.inspect((second + 1) * 1_000);
    }
    let captions = track(&timeline.inspect(95_000), "video", 0)
        .captions
        .clone()
        .expect("a dropout outlives the window");
    assert_eq!(captions.last_seen_sec, 90.0);
}

#[test]
fn a_source_without_captions_reports_none() {
    let mut timeline = Timeline::default();
    timeline.observe(MediaType::Video, 0, 0, 0, 0, None);
    timeline.observe_captions(0, crate::media::codec::FrameCaptions::default());
    assert_eq!(track(&timeline.inspect(1_000), "video", 0).captions, None);
}

#[test]
fn a_scan_reads_captions_from_flv_video_in_the_ring() {
    // One SEI with an A/53 CEA-608 pair ("HE") and a slice, as a legacy
    // FLV H.264 frame with four-byte NAL lengths.
    let message: Vec<u8> = [
        &[0xB5, 0x00, 0x31, b'G', b'A', b'9', b'4', 0x03, 0x41, 0xFF][..],
        &[0xFC, b'H' | 0x80, b'E', 0xFF][..],
    ]
    .concat();
    let sei: Vec<u8> = [&[0x06, 4, message.len() as u8][..], &message, &[0x80]].concat();
    let slice = [0x65u8, 0x88, 0x84];
    let mut payload = vec![0x17, 1, 0, 0, 0];
    for nal in [&sei[..], &slice[..]] {
        payload.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        payload.extend_from_slice(nal);
    }
    let ring = RingBuffer::new(64);
    let mut next = ring.get_write_idx();
    for frame in 0..10 {
        ring.push(MediaPacket {
            format: PayloadFormat::Flv,
            payload: Bytes::from(payload.clone()),
            ..packet(MediaType::Video, frame * 33)
        });
    }
    let mut timeline = Timeline::default();
    scan(&ring, &mut next, &mut timeline, &HashMap::new());
    let captions = track(&timeline.inspect(1_000), "video", 0)
        .captions
        .clone()
        .expect("captions");
    assert!(captions.cea608);
    assert_eq!(captions.last_text_sec, Some(0.0));
}

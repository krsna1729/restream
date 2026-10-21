//! A variant and its separate audio rendition through one publisher: merged
//! onto the ring in DTS order, the probe recorded once both are known.

use std::collections::VecDeque;
use std::time::Duration;

use bytes::Bytes;

use super::tests::harness;
use super::*;
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::packet::PayloadFormat;

/// A demux that releases its next scripted batch per fed chunk.
#[derive(Default)]
struct Scripted {
    batches: VecDeque<Vec<MediaPacket>>,
    ready: Vec<MediaPacket>,
    probe: Option<DemuxProbe>,
}

impl SegmentDemux for Scripted {
    fn feed(&mut self, _chunk: &[u8]) -> Result<(), MediaRejection> {
        if let Some(batch) = self.batches.pop_front() {
            self.ready.extend(batch);
        }
        Ok(())
    }

    fn drain_into(&mut self, out: &mut Vec<MediaPacket>) -> usize {
        let moved = self.ready.len();
        out.append(&mut self.ready);
        moved
    }

    fn end_object(&mut self) -> Result<(), MediaRejection> {
        Ok(())
    }

    fn flush(&mut self) {}

    fn discontinuity(&mut self) {}

    fn take_probe(&mut self) -> Option<DemuxProbe> {
        self.probe.take()
    }
}

fn packet(media_type: MediaType, dts: i64) -> MediaPacket {
    MediaPacket {
        media_type,
        format: PayloadFormat::Raw,
        is_keyframe: media_type == MediaType::Video && dts == 0,
        track_index: 0,
        pts: dts,
        dts,
        payload: Bytes::from_static(b"frame"),
    }
}

/// The variant: video every 40 ms over 2 s, with a muxed audio packet the
/// separate rendition must displace.
fn variant() -> Box<dyn SegmentDemux> {
    let mut batch: Vec<MediaPacket> = (0..50)
        .map(|frame| packet(MediaType::Video, frame * 40))
        .collect();
    batch.push(packet(MediaType::Audio, 510));
    Box::new(Scripted {
        batches: VecDeque::from([batch]),
        probe: video_probe(),
        ..Scripted::default()
    })
}

fn video_probe() -> Option<DemuxProbe> {
    Some(DemuxProbe {
        video: Some(VideoMeta {
            codec: "h264".into(),
            ..VideoMeta::default()
        }),
        video_sequence_header: None,
        video_track_count: 1,
        audio_tracks: Vec::new(),
    })
}

/// The audio rendition: a frame every 20 ms over the same 2 s.
fn audio_rendition() -> Box<dyn SegmentDemux> {
    Box::new(Scripted {
        batches: VecDeque::from([(0..100)
            .map(|frame| packet(MediaType::Audio, frame * 20))
            .collect()]),
        probe: Some(DemuxProbe {
            video: None,
            video_sequence_header: None,
            video_track_count: 0,
            audio_tracks: vec![AudioMeta {
                codec: "aac".into(),
                ..AudioMeta::default()
            }],
        }),
        ..Scripted::default()
    })
}

async fn recorded(engine: &MediaEngine, pipeline: &str) -> (bool, bool) {
    let ingests = engine.ingests.active.read().await;
    let ingest = ingests.get(pipeline).expect("the ingest is registered");
    let metadata = crate::sync::read(&ingest.metadata);
    (metadata.video.is_some(), metadata.audio.is_some())
}

fn on_ring(ring: &RingBuffer, start: usize) -> Vec<(MediaType, i64)> {
    (start..ring.get_write_idx())
        .filter_map(|index| ring.read_at(index))
        .map(|packet| (packet.media_type, packet.dts))
        .collect()
}

#[tokio::test]
async fn two_renditions_reach_the_ring_merged_in_dts_order() {
    let (engine, target, ring) = harness("renditions-merged").await;
    let start = ring.get_write_idx();
    let mut publisher = SegmentPublisher::new(variant(), IngestTimestamps::default());
    publisher.interleave(2, Duration::from_secs(10), &target);

    publisher
        .publish(&target, b"variant segment")
        .await
        .unwrap();
    assert!(
        on_ring(&ring, start).is_empty(),
        "the variant waits for the audio of the same time"
    );
    assert_eq!(
        recorded(&engine, "renditions-merged").await,
        (false, false),
        "no probe opens the output start gate before the audio rendition's"
    );

    publisher.select_rendition(1, audio_rendition);
    publisher.publish(&target, b"audio segment").await.unwrap();
    assert_eq!(
        recorded(&engine, "renditions-merged").await,
        (true, true),
        "the recorded probe has the variant's video and the rendition's audio"
    );
    publisher.flush(&target);

    let published = on_ring(&ring, start);
    assert!(
        published.windows(2).all(|pair| pair[0].1 <= pair[1].1),
        "the renditions interleave in DTS order: {published:?}"
    );
    let audio: Vec<i64> = published
        .iter()
        .filter(|(media_type, _)| *media_type == MediaType::Audio)
        .map(|(_, dts)| *dts)
        .collect();
    assert_eq!(
        audio,
        (0..100).map(|frame| frame * 20).collect::<Vec<_>>(),
        "every rendition audio frame, and not the variant's own"
    );
    assert_eq!(published.len(), 150);
}

/// A rendition that never comes holds the other back for the window, once:
/// while the queued media spans less than the window it waits; past that the
/// missing rendition is absent and nothing waits for it again.
#[tokio::test]
async fn a_missing_rendition_holds_the_variant_for_the_window_only() {
    let (engine, target, ring) = harness("renditions-window").await;
    let start = ring.get_write_idx();
    let demux = Box::new(Scripted {
        batches: VecDeque::from([video(0), video(2_000), video(4_000)]),
        probe: video_probe(),
        ..Scripted::default()
    });
    let mut publisher = SegmentPublisher::new(demux, IngestTimestamps::default());
    publisher.interleave(2, Duration::from_millis(3_000), &target);

    publisher.publish(&target, b"v0").await.unwrap();
    assert!(
        on_ring(&ring, start).is_empty(),
        "within the window the variant waits for the audio"
    );
    publisher.publish(&target, b"v1").await.unwrap();
    assert_eq!(
        on_ring(&ring, start).last(),
        Some(&(MediaType::Video, 3_960)),
        "past the window it leaves without the audio"
    );
    assert_eq!(
        recorded(&engine, "renditions-window").await,
        (true, false),
        "media that left without the audio rendition records the probe it has"
    );
    publisher.publish(&target, b"v2").await.unwrap();
    assert_eq!(
        on_ring(&ring, start).last(),
        Some(&(MediaType::Video, 5_960)),
        "the absent rendition is not waited for again"
    );
}

fn video(from: i64) -> Vec<MediaPacket> {
    (0..50)
        .map(|frame| packet(MediaType::Video, from + frame * 40))
        .collect()
}

fn audio(from: i64) -> Vec<MediaPacket> {
    (0..100)
        .map(|frame| packet(MediaType::Audio, from + frame * 20))
        .collect()
}

/// An encoder restart crosses both renditions: the video crosses first, the
/// audio of the old timeline's last segment is released after it, then the
/// audio crosses. Each rendition re-anchors on its own timeline, and the
/// second to cross takes the first one's offset (they share the source
/// clock), so the two stay aligned and neither has a gap.
#[tokio::test]
async fn a_discontinuity_crossed_by_both_renditions_keeps_them_aligned() {
    let (_engine, target, ring) = harness("renditions-discontinuity").await;
    let start = ring.get_write_idx();
    // Old timeline 0..4 s, then the restarted encoder from 0 again.
    let variant = Box::new(Scripted {
        batches: VecDeque::from([video(0), video(2_000), video(0)]),
        ..Scripted::default()
    });
    let rendition = || -> Box<dyn SegmentDemux> {
        Box::new(Scripted {
            batches: VecDeque::from([audio(0), audio(2_000), audio(0)]),
            ..Scripted::default()
        })
    };
    let mut publisher = SegmentPublisher::new(variant, IngestTimestamps::default());
    publisher.interleave(2, Duration::from_secs(10), &target);

    publisher.publish(&target, b"v0").await.unwrap();
    publisher.select_rendition(1, rendition);
    publisher.publish(&target, b"a0").await.unwrap();
    publisher.select_rendition(0, rendition);
    publisher.publish(&target, b"v1").await.unwrap();
    // The restart reaches the video first...
    publisher.reanchor(&target, DiscontinuityKind::Timeline { generation: 1 });
    publisher.publish(&target, b"v0'").await.unwrap();
    // ...while the audio of the old timeline's last segment is still to come.
    publisher.select_rendition(1, rendition);
    publisher.publish(&target, b"a1").await.unwrap();
    publisher.reanchor(&target, DiscontinuityKind::Timeline { generation: 1 });
    publisher.publish(&target, b"a0'").await.unwrap();
    publisher.flush(&target);

    let published = on_ring(&ring, start);
    let track = |media_type: MediaType| -> Vec<i64> {
        published
            .iter()
            .filter(|(kind, _)| *kind == media_type)
            .map(|(_, dts)| *dts)
            .collect()
    };
    let (video, audio) = (track(MediaType::Video), track(MediaType::Audio));
    assert_eq!((video.len(), audio.len()), (150, 300), "nothing is lost");
    for (name, track, frame) in [("video", &video, 40), ("audio", &audio, 20)] {
        assert!(
            track
                .windows(2)
                .all(|pair| pair[1] > pair[0] && pair[1] - pair[0] <= frame),
            "{name} has a gap or goes backwards: {track:?}"
        );
    }
    // After the restart both start together, within one audio frame.
    let (video_restart, audio_restart) = (video[100], audio[200]);
    assert!(
        (audio_restart - video_restart).abs() <= 20,
        "after the restart video starts at {video_restart} and audio at {audio_restart}"
    );
}

/// An SSAI ad starts a few seconds past the content on both renditions: a
/// forward jump inside the merge window at a timeline break. The video
/// crosses first and must not adopt the audio's old-timeline offset, or the
/// jump would stay in the output as a gap; the audio then joins it on the new
/// generation.
#[tokio::test]
async fn a_sub_window_forward_jump_at_a_timeline_break_leaves_no_gap() {
    let (_engine, target, ring) = harness("renditions-timeline-break").await;
    let start = ring.get_write_idx();
    let variant = Box::new(Scripted {
        batches: VecDeque::from([video(0), video(5_000)]),
        ..Scripted::default()
    });
    let rendition = || -> Box<dyn SegmentDemux> {
        Box::new(Scripted {
            batches: VecDeque::from([audio(0), audio(5_000)]),
            ..Scripted::default()
        })
    };
    let mut publisher = SegmentPublisher::new(variant, IngestTimestamps::default());
    // Three target durations: the 5 s jump is inside the window.
    publisher.interleave(2, Duration::from_secs(10), &target);

    publisher.publish(&target, b"v0").await.unwrap();
    publisher.select_rendition(1, rendition);
    publisher.publish(&target, b"a0").await.unwrap();
    // The ad break reaches the video first. Its raw time is close to the
    // audio's last one, but the audio has not crossed generation 1.
    publisher.select_rendition(0, rendition);
    publisher.reanchor(&target, DiscontinuityKind::Timeline { generation: 1 });
    publisher.publish(&target, b"v0-ad").await.unwrap();
    // The audio crosses the same generation and joins the video's offset.
    publisher.select_rendition(1, rendition);
    publisher.reanchor(&target, DiscontinuityKind::Timeline { generation: 1 });
    publisher.publish(&target, b"a0-ad").await.unwrap();
    publisher.flush(&target);

    let published = on_ring(&ring, start);
    let track = |media_type: MediaType| -> Vec<i64> {
        published
            .iter()
            .filter(|(kind, _)| *kind == media_type)
            .map(|(_, dts)| *dts)
            .collect()
    };
    let (video, audio) = (track(MediaType::Video), track(MediaType::Audio));
    assert_eq!((video.len(), audio.len()), (100, 200), "nothing is lost");
    for (name, track, frame) in [("video", &video, 40), ("audio", &audio, 20)] {
        assert!(
            track
                .windows(2)
                .all(|pair| pair[1] > pair[0] && pair[1] - pair[0] <= frame),
            "{name} has a gap or goes backwards: {track:?}"
        );
    }
    assert_eq!(
        (video[50], audio[100]),
        (1_981, 1_981),
        "the ad starts right after the content on both renditions"
    );
}

/// A variant switch marks a discontinuity on rendition 0 only, on the same
/// source clock. The clock-keeping mark keeps the proximity rule: the new
/// variant adopts the audio rendition's offset instead of re-anchoring after
/// it, so the two stay aligned even though the switch jumped ahead.
#[tokio::test]
async fn a_variant_switch_keeps_the_renditions_aligned() {
    let (_engine, target, ring) = harness("renditions-variant-switch").await;
    let start = ring.get_write_idx();
    let variant = Box::new(Scripted {
        batches: VecDeque::from([video(0), video(3_000)]),
        ..Scripted::default()
    });
    let rendition = || -> Box<dyn SegmentDemux> {
        Box::new(Scripted {
            batches: VecDeque::from([audio(0)]),
            ..Scripted::default()
        })
    };
    let mut publisher = SegmentPublisher::new(variant, IngestTimestamps::default());
    publisher.interleave(2, Duration::from_secs(10), &target);

    publisher.publish(&target, b"v0").await.unwrap();
    publisher.select_rendition(1, rendition);
    publisher.publish(&target, b"a0").await.unwrap();
    // The master selects another variant: only rendition 0 re-anchors.
    publisher.select_rendition(0, rendition);
    publisher.reanchor(&target, DiscontinuityKind::Clock);
    publisher.publish(&target, b"v1").await.unwrap();
    publisher.flush(&target);

    let video: Vec<i64> = on_ring(&ring, start)
        .into_iter()
        .filter(|(kind, _)| *kind == MediaType::Video)
        .map(|(_, dts)| dts)
        .collect();
    assert_eq!(video.len(), 100, "nothing is lost");
    assert_eq!(
        video[50], 3_000,
        "the switched variant kept the audio rendition's offset"
    );
}

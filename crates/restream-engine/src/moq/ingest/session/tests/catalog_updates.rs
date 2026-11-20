//! Catalog versions after the first: a publisher that lists a track only in a
//! later version still has it ingested; removals and retypes change nothing.

use super::*;
use restream_core::MediaType;

fn audio_config() -> AudioConfig {
    AudioConfig {
        sample_rate: 48_000,
        channels: 2,
        asc: Some(Bytes::from_static(&[0x11, 0x90])),
        container: TrackContainer::Legacy,
    }
}

fn avc3_config() -> VideoConfig {
    VideoConfig {
        codec: VideoCodec::H264,
        description: None,
        coded_width: None,
        coded_height: None,
        framerate: None,
        jitter: None,
        container: TrackContainer::Legacy,
    }
}

fn catalog(video: Option<&str>, audio: &[&str]) -> CatalogTracks {
    CatalogTracks {
        video: video.map(|name| (name.to_string(), avc3_config())),
        audio: audio
            .iter()
            .map(|name| (name.to_string(), audio_config()))
            .collect(),
        ..CatalogTracks::default()
    }
}

#[test]
fn a_later_catalog_adds_video_and_probes_again_after_the_first_answer() {
    let ring = Arc::new(RingBuffer::new(16));
    let (mut ingest, _) = ingest(&ring, SessionLimits::default());
    assert_eq!(
        ingest
            .set_tracks(TrackSet::new(catalog(None, &["audio"])))
            .unwrap(),
        ["audio"]
    );
    let (_, first) = ingest.ready_probe().expect("audio only: complete at once");
    assert!(first.video.is_none());

    // A second audio track lands while the first probe is still with Tokio:
    // its probe is complete at once, but waits for that answer.
    assert_eq!(
        ingest.add_tracks(catalog(None, &["audio", "dub"])),
        [(1, "dub".to_string())]
    );
    assert!(
        ingest.ready_probe().is_none(),
        "one probe in flight: a newer one waits for its answer"
    );
    ingest.probe_applied(None);
    let (_, second) = ingest.ready_probe().expect("then the re-probe goes out");
    assert_eq!(second.audio_tracks.len(), 2);
    ingest.probe_applied(None);

    assert_eq!(
        ingest.add_tracks(catalog(Some("video"), &["audio", "dub"])),
        [(2, "video".to_string())]
    );
    assert!(
        ingest.ready_probe().is_none(),
        "the new video track has no parameter sets yet"
    );
    let (_, probe) = ingest
        .frame(2, legacy(0, &annexb(&[SPS, PPS, IDR])))
        .unwrap()
        .expect("the video keyframe completes the re-probe");
    assert!(probe.video.is_some());
    assert_eq!(probe.audio_tracks.len(), 2, "the audio tracks stay");
    ingest.probe_applied(None);
    assert!(
        ingest
            .frame(0, legacy(21_333, &[0x21, 0x10]))
            .unwrap()
            .is_none()
    );
    let video = ring
        .read_at(0)
        .expect("the added video track reaches the ring");
    assert_eq!(
        (video.media_type, video.track_index, video.is_keyframe),
        (MediaType::Video, 0, true)
    );
    let audio = ring.read_at(1).expect("audio still reaches the ring");
    assert_eq!((audio.media_type, audio.track_index), (MediaType::Audio, 0));
}

#[test]
fn a_later_catalog_that_removes_retypes_or_repeats_tracks_changes_nothing() {
    let ring = Arc::new(RingBuffer::new(16));
    let (mut ingest, _) = ingest(&ring, SessionLimits::default());
    ingest
        .set_tracks(TrackSet::new(catalog(Some("video"), &["audio"])))
        .unwrap();
    for update in [
        catalog(Some("video"), &["audio"]),
        catalog(None, &[]),
        // "audio" retyped as video, "video" retyped as audio.
        catalog(Some("audio"), &["video"]),
        catalog(Some("second-video"), &[]),
    ] {
        assert!(ingest.add_tracks(update).is_empty());
    }
    let (_, probe) = ingest
        .frame(0, legacy(0, &annexb(&[SPS, PPS, IDR])))
        .unwrap()
        .expect("the keyframe completes the only probe");
    assert_eq!(probe.audio_tracks.len(), 1);
    ingest.probe_applied(None);
    assert!(
        ingest.ready_probe().is_none(),
        "no update asked for another"
    );
}

/// The gate for Q-116 over a real MoQ session: the publisher's first catalog
/// lists only audio, a later version adds video, and both reach the ring.
#[test]
fn a_publisher_adding_video_in_a_later_catalog_gets_both_tracks_ingested() {
    use crate::moq::egress::media::MediaFrame;
    use crate::moq::egress::publish::HangBroadcast;
    use crate::moq::transport::test_support::{Loopback, run};
    use restream_moq::media::LocBuffer;
    use restream_moq::media::TrackKind;
    use restream_moq::media::hang_catalog;

    // As in the other loopback case: debug-build session futures need room.
    std::thread::Builder::new()
        .name("moq-catalog-update".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            run(async {
                let loopback = Loopback::new().await;
                let (client, accepted) = loopback.native().await;
                let (origin, origin_driver, budget) = restream_moq::budget::origin();
                let mut broadcast =
                    HangBroadcast::create(&origin, budget, "tenant/moq-key.hang", true, true)
                        .unwrap();
                let audio = audio_config();
                broadcast
                    .publish_catalog(&hang_catalog(None, Some(&audio)).unwrap())
                    .unwrap();
                compio::runtime::spawn(transport::run_driver(origin_driver)).detach();
                let ring = Arc::new(RingBuffer::new(256));
                let recorder = Recorder {
                    ring: Some(ring.clone()),
                    ..Recorder::default()
                };
                let shutdown = CancellationToken::new();
                let peer: SocketAddr = "198.51.100.10:4443".parse().unwrap();
                let server = async {
                    let handshake = moq_net::Server::new()
                        .accept_request(Instant::now(), accepted.session.clone())
                        .await
                        .unwrap();
                    publish(
                        &recorder,
                        peer,
                        &accepted.path,
                        handshake,
                        SessionLimits::default(),
                        &shutdown,
                    )
                    .await
                };
                let has = |media_type: MediaType| {
                    (0..ring.get_write_idx())
                        .filter_map(|index| ring.read_at(index))
                        .any(|packet| packet.media_type == media_type)
                };
                let publisher = async {
                    let (session, driver) = moq_net::Client::new()
                        .with_publisher(&origin)
                        .connect(Instant::now(), client)
                        .await
                        .unwrap();
                    compio::runtime::spawn(transport::run_driver(driver)).detach();
                    let video =
                        VideoConfig::from_parameter_sets(VideoCodec::H264, &annexb(&[SPS, PPS]))
                            .expect("an avcC record from SPS and PPS");
                    let mut pts_us = 0;
                    let mut added = false;
                    for _ in 0..500 {
                        if has(MediaType::Video) && has(MediaType::Audio) {
                            break;
                        }
                        if !added && has(MediaType::Audio) {
                            broadcast
                                .publish_catalog(&hang_catalog(Some(&video), Some(&audio)).unwrap())
                                .unwrap();
                            added = true;
                        }
                        broadcast
                            .write(MediaFrame {
                                track: TrackKind::Audio,
                                keyframe: false,
                                pts_us,
                                data: LocBuffer::from_payload(&[0x21, 0x10, 0x05]),
                            })
                            .unwrap();
                        if added {
                            // One length-prefixed IDR access unit; the
                            // parameter sets are in the catalog's record.
                            broadcast
                                .write(MediaFrame {
                                    track: TrackKind::Video,
                                    keyframe: true,
                                    pts_us,
                                    data: LocBuffer::from_payload(&[
                                        0, 0, 0, 4, 0x65, 0x88, 0x84, 0x00,
                                    ]),
                                })
                                .unwrap();
                        }
                        pts_us += 21_333;
                        compio::time::sleep(Duration::from_millis(10)).await;
                    }
                    shutdown.cancel();
                    session
                };
                let (end, _session) = futures_util::future::join(server, publisher).await;
                assert!(matches!(end.error, Error::Cancel), "{end:?}");
                assert!(has(MediaType::Audio), "the first catalog's audio");
                assert!(has(MediaType::Video), "the later catalog's video");
            });
        })
        .unwrap()
        .join()
        .unwrap();
}

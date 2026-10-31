//! The publisher session core: stream-key routing, track selection, frames
//! into the ring, the probe hold, bounds and the release of the input.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicI64, AtomicU64};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwapOption;
use bytes::BytesMut;

use super::*;
use crate::engine::IngestRegistration;
use crate::input_gate::InputPacketGate;
use crate::moq::ingest::publisher::{ProbeHoldLimits, PublisherCounters};
use crate::stage_metrics::StageMetrics;
use restream_moq::media::AudioConfig;
use restream_moq::media::CatalogTracks;
use restream_moq::media::TrackContainer;
use restream_moq::media::VideoCodec;
use restream_moq::media::VideoConfig;

const SPS: &[u8] = &[0x67, 0x42, 0x00, 0x1e, 0xf4, 0x05, 0x01, 0xec, 0x80];
const PPS: &[u8] = &[0x68, 0xce, 0x06, 0xe2];
const IDR: &[u8] = &[0x65, 0x88, 0x84, 0x00];

/// Records what the session asks of Tokio; admits every key when it has a
/// ring to publish into.
#[derive(Clone, Default)]
struct Recorder {
    ring: Option<Arc<RingBuffer>>,
    admitted: Rc<RefCell<Vec<String>>>,
    finished: Rc<RefCell<Vec<String>>>,
}

impl Lifecycle for Recorder {
    async fn admit(&self, _peer: SocketAddr, key: &str) -> Result<Admission, Rejection> {
        let ring = self.ring.clone().ok_or(Rejection::Unauthorized)?;
        self.admitted.borrow_mut().push(key.to_string());
        Ok(admission(ring))
    }

    async fn apply_probe(
        &self,
        _session: &PublisherSession,
        _probe: MoqProbe,
    ) -> Option<Arc<RingBuffer>> {
        None
    }

    fn finish(&self, _session: PublisherSession, reason: String) {
        self.finished.borrow_mut().push(reason);
    }
}

fn admission(ring: Arc<RingBuffer>) -> Admission {
    let registration = IngestRegistration {
        cancel_token: CancellationToken::new(),
        attempt_id: 1,
        input_id: "input-moq".to_string(),
        gate: Arc::new(InputPacketGate::active()),
        last_forwarded_dts: Arc::new(AtomicI64::new(i64::MIN)),
        preview_ring: Arc::new(ArcSwapOption::empty()),
    };
    let counters = PublisherCounters {
        bytes_received: Arc::new(AtomicU64::new(0)),
        first_media_ms: Arc::new(AtomicU64::new(0)),
        ingest_metrics: Arc::new(StageMetrics::new()),
        last_progress_ms: Arc::new(AtomicU64::new(0)),
        keyframe_times: Arc::new(Mutex::new(Vec::new())),
    };
    Admission {
        session: PublisherSession {
            pipeline_id: "pipe-moq".to_string(),
            registration: registration.clone(),
        },
        media: PublisherMedia::new(registration, ring, counters, ProbeHoldLimits::default()),
    }
}

fn audio_tracks() -> TrackSet {
    TrackSet::new(CatalogTracks {
        audio: vec![(
            "audio".to_string(),
            AudioConfig {
                sample_rate: 48_000,
                channels: 2,
                asc: Some(Bytes::from_static(&[0x11, 0x90])),
                container: TrackContainer::Legacy,
            },
        )],
        ..CatalogTracks::default()
    })
}

fn avc3_tracks() -> TrackSet {
    TrackSet::new(CatalogTracks {
        video: Some((
            "video".to_string(),
            VideoConfig {
                codec: VideoCodec::H264,
                description: None,
                coded_width: None,
                coded_height: None,
                framerate: None,
                jitter: None,
                container: TrackContainer::Legacy,
            },
        )),
        ..CatalogTracks::default()
    })
}

/// A hang legacy frame, as hang writes it.
fn legacy(us: u64, payload: &[u8]) -> Bytes {
    let mut frame = BytesMut::new();
    hang::container::Frame {
        timestamp: hang::container::Timestamp::from_micros(us).unwrap(),
        payload: Bytes::copy_from_slice(payload),
    }
    .encode(&mut frame)
    .unwrap();
    frame.freeze()
}

fn annexb(nalus: &[&[u8]]) -> Vec<u8> {
    nalus
        .iter()
        .flat_map(|nalu| [&[0, 0, 0, 1][..], nalu].concat())
        .collect()
}

fn ingest(ring: &Arc<RingBuffer>, limits: SessionLimits) -> (Ingest<Recorder>, Recorder) {
    let recorder = Recorder::default();
    (
        Ingest::new(recorder.clone(), admission(ring.clone()), limits),
        recorder,
    )
}

#[test]
fn the_connection_path_names_the_stream_key_before_the_broadcast() {
    // The URL path wins whatever the broadcast is called.
    assert_eq!(stream_key("/key", "live").as_deref(), Some("key"));
    assert_eq!(stream_key("/key", "").as_deref(), Some("key"));
    assert_eq!(
        stream_key("/key?token=abc", "x.hang").as_deref(),
        Some("key")
    );
    // An empty path falls back to the broadcast's last segment.
    assert_eq!(stream_key("", "tenant/key.hang").as_deref(), Some("key"));
    assert_eq!(stream_key("/", "key").as_deref(), Some("key"));
    assert_eq!(stream_key("/?a=b", "key.hang").as_deref(), Some("key"));
    assert_eq!(stream_key("/", "tenant/key.msf").as_deref(), Some("key"));
    assert_eq!(stream_key("/", ".msf"), None);
    assert_eq!(stream_key("/", ""), None);
    assert_eq!(stream_key("", ".hang"), None);
}

#[test]
fn a_refused_publisher_hears_why() {
    let cases = [
        (Rejection::Banned, "banned"),
        (Rejection::Unauthorized, "stream key"),
        (Rejection::Duplicate, "already has a publisher"),
        (
            Rejection::Unavailable("lookup down".to_string()),
            "lookup down",
        ),
    ];
    for (rejection, words) in cases {
        let end = SessionEnd::rejected(&rejection);
        assert!(end.reason.contains(words), "{}", end.reason);
        let expected = match rejection {
            Rejection::Banned | Rejection::Unauthorized => matches!(end.error, Error::Unauthorized),
            Rejection::Duplicate => matches!(end.error, Error::Duplicate),
            Rejection::Unavailable(_) => matches!(end.error, Error::Unroutable),
        };
        assert!(expected, "{rejection:?} → {:?}", end.error);
    }
}

#[test]
fn a_catalog_without_usable_tracks_ends_the_session() {
    let ring = Arc::new(RingBuffer::new(16));
    let (mut ingest, _) = ingest(&ring, SessionLimits::default());
    let end = ingest.set_tracks(TrackSet::default()).unwrap_err();
    assert!(matches!(end.error, Error::Unsupported), "{end:?}");
}

#[test]
fn audio_frames_wait_for_the_probe_then_reach_the_ring() {
    let ring = Arc::new(RingBuffer::new(16));
    let (mut ingest, recorder) = ingest(&ring, SessionLimits::default());
    assert_eq!(ingest.set_tracks(audio_tracks()).unwrap(), ["audio"]);
    let (_, probe) = ingest.ready_probe().expect("audio only: complete at once");
    assert_eq!(probe.audio_tracks.len(), 1);
    assert!(ingest.ready_probe().is_none(), "sent once");

    for us in [0, 21_333, 42_666] {
        assert!(
            ingest
                .frame(0, legacy(us, &[0x21, 0x10]))
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(ring.get_write_idx(), 0, "held while the probe is applied");
    ingest.probe_applied(None);
    assert_eq!(ring.get_write_idx(), 3);
    assert!(
        ingest.frame(0, legacy(64_000, &[])).unwrap().is_none(),
        "an end marker"
    );
    assert_eq!(ring.get_write_idx(), 3, "an end marker is skipped");
    assert!(
        ingest.frame(7, legacy(0, &[1])).unwrap().is_none(),
        "unknown track ignored"
    );

    ingest.finish("publisher left".to_string());
    assert_eq!(*recorder.finished.borrow(), ["publisher left"]);
}

#[test]
fn the_probe_waits_for_the_first_keyframe_with_parameter_sets() {
    let ring = Arc::new(RingBuffer::new(16));
    let (mut ingest, _) = ingest(&ring, SessionLimits::default());
    ingest.set_tracks(avc3_tracks()).unwrap();
    assert!(ingest.ready_probe().is_none());
    assert!(
        ingest
            .frame(0, legacy(0, &annexb(&[IDR])))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        ring.get_write_idx(),
        1,
        "no probe pending: straight to the ring"
    );
    let (_, probe) = ingest
        .frame(0, legacy(33_000, &annexb(&[SPS, PPS, IDR])))
        .unwrap()
        .expect("the keyframe completes the probe");
    assert!(probe.video.is_some());
    assert_eq!(
        ring.get_write_idx(),
        1,
        "the keyframe waits for the probe answer"
    );
    ingest.probe_applied(None);
    assert_eq!(ring.get_write_idx(), 2);
}

#[test]
fn an_oversized_frame_ends_the_session() {
    let ring = Arc::new(RingBuffer::new(16));
    let limits = SessionLimits {
        max_frame_bytes: 8,
        ..SessionLimits::default()
    };
    let (mut ingest, _) = ingest(&ring, limits);
    ingest.set_tracks(audio_tracks()).unwrap();
    assert!(ingest.frame(0, legacy(0, &[0; 7])).is_ok());
    let end = ingest
        .frame(0, legacy(0, &[0; 8]))
        .err()
        .expect("an oversized frame ends the session");
    assert!(matches!(end.error, Error::FrameTooLarge), "{end:?}");
    assert!(end.reason.contains("8-byte bound"), "{}", end.reason);
}

#[test]
fn a_run_of_malformed_frames_ends_the_session_and_a_good_one_resets_it() {
    let ring = Arc::new(RingBuffer::new(16));
    let limits = SessionLimits {
        max_malformed_run: 3,
        ..SessionLimits::default()
    };
    let (mut ingest, _) = ingest(&ring, limits);
    ingest.set_tracks(avc3_tracks()).unwrap();
    let garbage = || legacy(0, &[1, 2, 3]);
    assert!(ingest.frame(0, garbage()).unwrap().is_none());
    assert!(ingest.frame(0, garbage()).unwrap().is_none());
    assert!(
        ingest.frame(0, legacy(0, &annexb(&[IDR]))).is_ok(),
        "resets the run"
    );
    assert!(ingest.frame(0, garbage()).is_ok());
    assert!(
        ingest.frame(0, Bytes::new()).is_ok(),
        "a truncated prefix is malformed too"
    );
    let end = ingest
        .frame(0, garbage())
        .err()
        .expect("the malformed run ends the session");
    assert!(matches!(end.error, Error::ProtocolViolation), "{end:?}");
    assert!(
        end.reason.starts_with("3 malformed frames"),
        "{}",
        end.reason
    );
}

#[test]
fn a_dropped_session_releases_its_input_exactly_once() {
    let ring = Arc::new(RingBuffer::new(16));
    let (mut ingest, recorder) = ingest(&ring, SessionLimits::default());
    ingest.set_tracks(audio_tracks()).unwrap();
    ingest.ready_probe();
    ingest.frame(0, legacy(0, &[0x21])).unwrap();
    drop(ingest);
    assert_eq!(
        *recorder.finished.borrow(),
        ["the MoQ ingest listener stopped"]
    );
    assert_eq!(
        ring.get_write_idx(),
        1,
        "held packets are flushed on release"
    );
}

/// A real moq-lite publisher (moq-net client over the Compio transport,
/// publishing Restream's own LOC broadcast) through [`publish`] into the
/// ring.
#[test]
fn a_loopback_publisher_reaches_the_ring_and_shutdown_releases_it() {
    use crate::moq::egress::media::MediaFrame;
    use crate::moq::egress::publish::HangBroadcast;
    use crate::moq::transport::test_support::{Loopback, run};
    use restream_moq::media::LocBuffer;
    use restream_moq::media::TrackKind;
    use restream_moq::media::hang_catalog;

    // The publish/connect futures are large in a debug build; the default
    // 2 MiB test stack overflows. Run this case on a thread with room.
    std::thread::Builder::new()
        .name("moq-loopback-publisher".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            run(async {
                let loopback = Loopback::new().await;
                let (client, accepted) = loopback.native().await;
                let (origin, origin_driver, budget) = restream_moq::budget::origin();
                let mut broadcast =
                    HangBroadcast::create(&origin, budget, "tenant/moq-key.hang", false, true)
                        .unwrap();
                let audio = AudioConfig {
                    sample_rate: 48_000,
                    channels: 2,
                    asc: Some(Bytes::from_static(&[0x11, 0x90])),
                    container: TrackContainer::Legacy,
                };
                broadcast
                    .publish_catalog(&hang_catalog(None, Some(&audio)).unwrap())
                    .unwrap();
                compio::runtime::spawn(transport::run_driver(origin_driver)).detach();
                let ring = Arc::new(RingBuffer::new(64));
                let recorder = Recorder {
                    ring: Some(ring.clone()),
                    ..Recorder::default()
                };
                let shutdown = CancellationToken::new();
                let peer: SocketAddr = "198.51.100.9:4443".parse().unwrap();
                // The client's connect completes only once the server answers its
                // SETUP, which `publish` does: run both sides together.
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
                let publisher = async {
                    let (session, driver) = moq_net::Client::new()
                        .with_publisher(&origin)
                        .connect(Instant::now(), client)
                        .await
                        .unwrap();
                    compio::runtime::spawn(transport::run_driver(driver)).detach();
                    let mut pts_us = 0;
                    while ring.get_write_idx() < 5 {
                        let frame = MediaFrame {
                            track: TrackKind::Audio,
                            keyframe: false,
                            pts_us,
                            data: LocBuffer::from_payload(&[0x21, 0x10, 0x05]),
                        };
                        broadcast.write(frame).unwrap();
                        pts_us += 21_333;
                        compio::time::sleep(Duration::from_millis(10)).await;
                    }
                    shutdown.cancel();
                    session
                };
                let (end, _session) = futures_util::future::join(server, publisher).await;
                assert!(matches!(end.error, Error::Cancel), "{end:?}");
                assert_eq!(*recorder.admitted.borrow(), ["moq-key"]);
                assert_eq!(*recorder.finished.borrow(), ["Restream is shutting down"]);
                let first = ring.read_at(0).expect("published packet");
                assert_eq!((first.track_index, first.is_keyframe), (0, false));
                assert_eq!(&first.payload[7..], &[0x21, 0x10, 0x05], "ADTS-wrapped AAC");
            });
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn a_silent_publisher_is_cut_off_after_the_idle_bound() {
    crate::moq::transport::test_support::run(async {
        let last_frame = Cell::new(Instant::now());
        let started = Instant::now();
        let end = idle_watch(&last_frame, Duration::from_millis(50)).await;
        assert!(matches!(end.error, Error::Timeout), "{end:?}");
        assert!(started.elapsed() >= Duration::from_millis(50));
    });
}

/// hang parses the catalog from peer bytes; a panic in it ends the session
/// as a protocol violation instead of unwinding the session task.
#[test]
fn a_panicking_catalog_parser_ends_the_session_as_a_protocol_violation() {
    let end = crate::test_support::with_expected_panic_suppressed(|| {
        crate::moq::transport::test_support::run(first_catalog(
            std::future::poll_fn(
                |_| -> std::task::Poll<hang::Result<Option<hang::Catalog>>> {
                    panic!("end byte index 2 is not a char boundary")
                },
            ),
            Duration::from_secs(1),
        ))
    })
    .unwrap_err();
    assert!(matches!(end.error, Error::ProtocolViolation), "{end:?}");
    assert_eq!(
        end.reason,
        "catalog.json/MSF catalog: the parser panicked: end byte index 2 is not a char boundary"
    );
}

#[test]
fn msf_only_catalog_maps_loc_h264_h265_and_aac_into_the_ring() {
    use restream_moq::media::LocBuffer;

    crate::moq::transport::test_support::run(async {
        for (codec, nalus) in [
            ("avc3.42001e", annexb(&[SPS, PPS, IDR])),
            ("hev1.1.6.L93.B0", annexb(&[&[19 << 1, 1, 0xaf]])),
        ] {
            let mut video = moq_msf::Track::new("video", moq_msf::Packaging::Loc);
            video.role = Some(moq_msf::Role::Video);
            video.codec = Some(codec.to_string());
            let mut audio = moq_msf::Track::new("audio", moq_msf::Packaging::Loc);
            audio.role = Some(moq_msf::Role::Audio);
            audio.codec = Some("mp4a.40.2".to_string());
            audio.init_data = Some("EZA=".to_string());
            // Omit optional samplerate/channelConfig: upstream derives
            // both from the ASC, not local guesses or a second parser.
            let catalog = moq_msf::Catalog::<()>::new(vec![video, audio]);
            let broadcast = moq_net::broadcast::Info::new().produce();
            let track = broadcast
                .create_track(moq_msf::DEFAULT_NAME, hang::Catalog::default_track_info())
                .unwrap();
            let mut group = track.append_group().unwrap();
            group
                .write_frame(
                    moq_net::Timestamp::ZERO,
                    Bytes::from(catalog.to_json().unwrap()),
                )
                .unwrap();
            group.finish().unwrap();
            let catalog = first_catalog(
                read_catalog(&broadcast.consume(), None),
                Duration::from_secs(1),
            )
            .await
            .expect("MSF-only publisher must not wait for catalog.json");
            let tracks = from_hang_catalog(&catalog);
            assert_eq!(
                tracks.video.as_ref().unwrap().1.container,
                TrackContainer::Loc
            );
            assert_eq!(tracks.audio[0].1.sample_rate, 48_000);
            assert_eq!(tracks.audio[0].1.channels, 2);
            let ring = Arc::new(RingBuffer::new(16));
            let (mut ingest, _) = ingest(&ring, SessionLimits::default());
            assert_eq!(
                ingest.set_tracks(TrackSet::new(tracks)).unwrap(),
                ["video", "audio"]
            );
            ingest
                .frame(
                    0,
                    LocBuffer::from_payload(&nalus)
                        .into_frame(2_000_000)
                        .unwrap(),
                )
                .unwrap();
            ingest.probe_applied(None);
            ingest
                .frame(
                    1,
                    LocBuffer::from_payload(&[0x21, 0x10])
                        .into_frame(2_021_000)
                        .unwrap(),
                )
                .unwrap();
            ingest.probe_applied(None);
            let video = ring.read_at(0).expect("LOC video entered the ring");
            assert!(video.is_keyframe);
            assert_eq!((video.pts, video.dts), (2_000, 2_000));
            assert_eq!(video.payload.as_ref(), nalus.as_slice());
            let audio = ring.read_at(1).expect("LOC AAC entered the ring");
            assert_eq!((audio.pts, audio.dts), (2_021, 2_021));
            assert_eq!(&audio.payload[7..], &[0x21, 0x10]);
        }
    });
}

#[test]
fn a_missing_catalog_profile_falls_back_but_malformed_input_does_not() {
    use moq_mux::Error as CatalogError;

    for error in [Error::NotFound, Error::Unroutable, Error::Unsupported] {
        assert!(missing_catalog(&Err(CatalogError::Moq(error.clone()))));
        assert!(missing_catalog(&Err(CatalogError::Hang(hang::Error::Moq(
            error
        )))));
    }
    assert!(missing_catalog(&Ok(None)));
    assert!(!missing_catalog(&Err(CatalogError::Hang(
        hang::Error::Json("bad JSON".to_string(),)
    ))));
    assert!(!missing_catalog(&Err(CatalogError::Moq(
        Error::ProtocolViolation
    ))));
    crate::moq::transport::test_support::run(async {
        let broadcast = moq_net::broadcast::Info::new().produce();
        let track = broadcast
            .create_track(
                hang::Catalog::DEFAULT_NAME,
                hang::Catalog::default_track_info(),
            )
            .unwrap();
        let mut group = track.append_group().unwrap();
        group
            .write_frame(moq_net::Timestamp::ZERO, Bytes::from_static(b"{"))
            .unwrap();
        group.finish().unwrap();
        let consumer = broadcast.consume();
        let result = first_catalog(read_catalog(&consumer, None), Duration::from_secs(1))
            .now_or_never()
            .expect("malformed hang must not wait on an absent MSF track");
        assert!(matches!(
            result.unwrap_err().error,
            Error::ProtocolViolation
        ));
    });
}

#[test]
fn a_suffix_preference_waits_for_its_catalog_instead_of_parsing_a_malformed_alternate() {
    crate::moq::transport::test_support::run(async {
        for (name, preferred_name, alternate_name, json) in [
            (
                "live/moq-key.msf",
                moq_msf::DEFAULT_NAME,
                hang::Catalog::DEFAULT_NAME,
                r#"{"version":"draft-01","tracks":[]}"#,
            ),
            (
                "live/moq-key.hang",
                hang::Catalog::DEFAULT_NAME,
                moq_msf::DEFAULT_NAME,
                "{}",
            ),
        ] {
            let broadcast = moq_net::broadcast::Info::new().produce();
            let preferred = broadcast
                .create_track(preferred_name, hang::Catalog::default_track_info())
                .unwrap();
            let alternate = broadcast
                .create_track(alternate_name, hang::Catalog::default_track_info())
                .unwrap();
            let mut group = alternate.append_group().unwrap();
            group
                .write_frame(moq_net::Timestamp::ZERO, Bytes::from_static(b"{"))
                .unwrap();
            group.finish().unwrap();
            let consumer = broadcast.consume();
            let mut read = pin!(first_catalog(
                read_catalog(&consumer, CatalogFormat::detect(name)),
                Duration::from_secs(1),
            ));
            assert!(
                read.as_mut().now_or_never().is_none(),
                "{name} must wait for the preferred catalog, not parse its ready alternate",
            );
            let mut group = preferred.append_group().unwrap();
            group
                .write_frame(
                    moq_net::Timestamp::ZERO,
                    Bytes::from_static(json.as_bytes()),
                )
                .unwrap();
            group.finish().unwrap();
            read.await.expect("the announced profile wins");
        }
    });
}

#[test]
fn a_suffix_preference_falls_back_only_when_its_catalog_is_missing() {
    crate::moq::transport::test_support::run(async {
        for (name, preferred_name, alternate_name, json) in [
            (
                "moq-key.msf",
                moq_msf::DEFAULT_NAME,
                hang::Catalog::DEFAULT_NAME,
                "{}",
            ),
            (
                "moq-key.hang",
                hang::Catalog::DEFAULT_NAME,
                moq_msf::DEFAULT_NAME,
                r#"{"version":"draft-01","tracks":[]}"#,
            ),
        ] {
            for malformed in [false, true] {
                let broadcast = moq_net::broadcast::Info::new().produce();
                let alternate = broadcast
                    .create_track(alternate_name, hang::Catalog::default_track_info())
                    .unwrap();
                let mut group = alternate.append_group().unwrap();
                group
                    .write_frame(
                        moq_net::Timestamp::ZERO,
                        Bytes::from_static(json.as_bytes()),
                    )
                    .unwrap();
                group.finish().unwrap();
                let preferred = malformed.then(|| {
                    let track = broadcast
                        .create_track(preferred_name, hang::Catalog::default_track_info())
                        .unwrap();
                    let mut group = track.append_group().unwrap();
                    group
                        .write_frame(moq_net::Timestamp::ZERO, Bytes::from_static(b"{"))
                        .unwrap();
                    group.finish().unwrap();
                    track
                });
                let consumer = broadcast.consume();
                let result = first_catalog(
                    read_catalog(&consumer, CatalogFormat::detect(name)),
                    Duration::from_secs(1),
                )
                .now_or_never()
                .expect("missing or malformed preferred catalogs resolve promptly");
                if preferred.is_some() {
                    assert!(
                        matches!(result.unwrap_err().error, Error::ProtocolViolation),
                        "{name} must not hide malformed preferred input behind its valid alternate",
                    );
                } else {
                    result.expect("a missing preferred profile falls back");
                }
            }
        }
    });
}

#[test]
fn bounded_draft01_shared_init_references_reach_the_ring() {
    use restream_moq::media::LocBuffer;

    crate::moq::transport::test_support::run(async {
        let json = br#"{
            "version":"draft-01",
            "initDataList":[{"id":"aac","type":"inline","data":"EZA="}],
            "tracks":[
                {"name":"audio-a","packaging":"loc","role":"audio","codec":"mp4a.40.2","initRef":"aac"},
                {"name":"audio-b","packaging":"loc","role":"audio","codec":"mp4a.40.2","initRef":"aac"}
            ]
        }"#;
        assert!(json.len() < moq_msf::MAX_CATALOG_BYTES);
        let broadcast = moq_net::broadcast::Info::new().produce();
        let track = broadcast
            .create_track(moq_msf::DEFAULT_NAME, hang::Catalog::default_track_info())
            .unwrap();
        let mut group = track.append_group().unwrap();
        group
            .write_frame(moq_net::Timestamp::ZERO, Bytes::from_static(json))
            .unwrap();
        group.finish().unwrap();
        let catalog = first_catalog(
            read_catalog(&broadcast.consume(), Some(CatalogFormat::Msf)),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        let tracks = from_hang_catalog(&catalog);
        assert_eq!(tracks.audio.len(), 2);
        for (_, audio) in &tracks.audio {
            assert_eq!((audio.sample_rate, audio.channels), (48_000, 2));
            assert_eq!(audio.container, TrackContainer::Loc);
        }
        let ring = Arc::new(RingBuffer::new(16));
        let (mut ingest, _) = ingest(&ring, SessionLimits::default());
        assert_eq!(
            ingest.set_tracks(TrackSet::new(tracks)).unwrap(),
            ["audio-a", "audio-b"],
        );
        ingest.ready_probe().expect("shared ASC supplies the probe");
        ingest.probe_applied(None);
        for index in 0..2 {
            ingest
                .frame(
                    index,
                    LocBuffer::from_payload(&[0x21, 0x10])
                        .into_frame(2_000_000 + index as u64 * 21_000)
                        .unwrap(),
                )
                .unwrap();
            let packet = ring
                .read_at(index)
                .expect("shared-init AAC entered the ring");
            assert_eq!(packet.pts, 2_000 + index as i64 * 21);
            assert_eq!(&packet.payload[7..], &[0x21, 0x10]);
        }
    });
}

#[test]
fn an_oversized_declared_frame_is_rejected_before_waiting_for_payload() {
    crate::moq::transport::test_support::run(async {
        let broadcast = moq_net::broadcast::Info::new().produce();
        let track = broadcast
            .create_track("audio", moq_net::track::Info::default())
            .unwrap();
        let mut subscriber = track
            .subscribe(None::<moq_net::track::Subscription>)
            .ordered();
        let mut group = track.append_group().unwrap();
        group
            .write_frame(moq_net::Timestamp::ZERO, Bytes::from_static(b"12345678"))
            .unwrap();
        let _unfinished = group
            .create_frame(moq_net::frame::Info {
                size: 9,
                timestamp: moq_net::Timestamp::ZERO,
            })
            .unwrap();
        let mut received = subscriber.next_group().await.unwrap().unwrap();
        assert_eq!(
            bounded_frame(&mut received, 8).await.unwrap(),
            Some(Bytes::from_static(b"12345678")),
            "the exact bound remains accepted",
        );
        let result = bounded_frame(&mut received, 8)
            .now_or_never()
            .expect("the declared size is enough to reject an unfinished frame");
        assert!(matches!(result, Err(Error::FrameTooLarge)));
    });
}

#[test]
fn an_ietf_msf_preferred_publisher_reaches_the_ring() {
    use crate::moq::transport::test_support::{Loopback, run};
    use restream_moq::media::LocBuffer;

    std::thread::Builder::new()
        .name("moq-ietf-publisher".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            run(async {
                let loopback = Loopback::new().await;
                let (client, accepted) = loopback.native_offering(&["moqt-19"]).await;
                let (origin, driver, _budget) = restream_moq::budget::origin();
                let broadcast = origin.create_broadcast("tenant/moq-key.msf").unwrap();
                let alternate = broadcast
                    .create_track(
                        hang::Catalog::DEFAULT_NAME,
                        hang::Catalog::default_track_info(),
                    )
                    .unwrap();
                let mut alternate_group = alternate.append_group().unwrap();
                alternate_group
                    .write_frame(moq_net::Timestamp::ZERO, Bytes::from_static(b"{"))
                    .unwrap();
                alternate_group.finish().unwrap();
                let catalog = broadcast
                    .create_track(moq_msf::DEFAULT_NAME, hang::Catalog::default_track_info())
                    .unwrap();
                let mut audio_config = moq_msf::Track::new("audio", moq_msf::Packaging::Loc);
                audio_config.role = Some(moq_msf::Role::Audio);
                audio_config.codec = Some("mp4a.40.2".to_string());
                audio_config.init_data = Some("EZA=".to_string());
                let json = moq_msf::Catalog::<()>::new(vec![audio_config])
                    .to_json()
                    .unwrap();
                let mut catalog_group = catalog.append_group().unwrap();
                catalog_group
                    .write_frame(moq_net::Timestamp::ZERO, Bytes::from(json))
                    .unwrap();
                catalog_group.finish().unwrap();
                let audio = broadcast
                    .create_track("audio", moq_net::track::Info::default())
                    .unwrap();
                broadcast.announce(Default::default()).unwrap();
                compio::runtime::spawn(transport::run_driver(driver)).detach();
                let ring = Arc::new(RingBuffer::new(64));
                let recorder = Recorder {
                    ring: Some(ring.clone()),
                    ..Recorder::default()
                };
                let shutdown = CancellationToken::new();
                let server = async {
                    let handshake = moq_net::Server::new()
                        .accept_request(Instant::now(), accepted.session.clone())
                        .await
                        .unwrap();
                    publish(
                        &recorder,
                        "198.51.100.9:4443".parse().unwrap(),
                        &accepted.path,
                        handshake,
                        SessionLimits::default(),
                        &shutdown,
                    )
                    .await
                };
                let publisher = async {
                    let (session, driver) = moq_net::Client::new()
                        .with_publisher(&origin)
                        .connect(Instant::now(), client)
                        .await
                        .unwrap();
                    compio::runtime::spawn(transport::run_driver(driver)).detach();
                    let mut group = audio.append_group().unwrap();
                    let mut timestamp = 0;
                    while ring.get_write_idx() < 3 {
                        group
                            .write_frame(
                                moq_net::Timestamp::ZERO,
                                LocBuffer::from_payload(&[0x21, 0x10])
                                    .into_frame(timestamp)
                                    .unwrap(),
                            )
                            .unwrap();
                        timestamp += 21_333;
                        compio::time::sleep(Duration::from_millis(10)).await;
                    }
                    shutdown.cancel();
                    session
                };
                let (end, _session) = futures_util::future::join(server, publisher).await;
                assert!(matches!(end.error, Error::Cancel), "{end:?}");
                assert_eq!(*recorder.admitted.borrow(), ["moq-key"]);
                assert_eq!(*recorder.finished.borrow(), ["Restream is shutting down"]);
                assert_eq!(&ring.read_at(0).unwrap().payload[7..], &[0x21, 0x10]);
            });
        })
        .unwrap()
        .join()
        .unwrap();
}

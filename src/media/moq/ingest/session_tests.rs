//! The publisher session core: stream-key routing, track selection, frames
//! into the ring, the probe hold, bounds and the release of the input.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicI64, AtomicU64};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwapOption;
use bytes::{BufMut, BytesMut};

use super::*;
use crate::media::engine::IngestRegistration;
use crate::media::input_gate::InputPacketGate;
use crate::media::moq::ingest::publisher::{ProbeHoldLimits, PublisherCounters};
use crate::media::moq::media::{
    AudioConfig, CatalogTracks, TrackContainer, VideoCodec, VideoConfig, write_legacy_prefix,
};
use crate::media::stage_metrics::StageMetrics;

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

fn legacy(us: u64, payload: &[u8]) -> Bytes {
    let mut frame = BytesMut::new();
    write_legacy_prefix(us, &mut frame).unwrap();
    frame.put_slice(payload);
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
/// publishing a hang broadcast) through [`publish`] into the ring.
#[test]
fn a_loopback_publisher_reaches_the_ring_and_shutdown_releases_it() {
    use crate::media::moq::egress::media::HangFrame;
    use crate::media::moq::egress::publish::HangBroadcast;
    use crate::media::moq::media::{TrackKind, hang_catalog};
    use crate::media::moq::transport::test_support::{Loopback, run};

    run(async {
        let loopback = Loopback::new().await;
        let (client, accepted) = loopback.native().await;
        let (origin, origin_driver) =
            moq_net::origin::Producer::new(moq_net::origin::Config::default());
        let mut broadcast =
            HangBroadcast::create(&origin, "tenant/moq-key.hang", false, true).unwrap();
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
                .accept_request_lite(Instant::now(), accepted.session.clone())
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
                .connect_lite(Instant::now(), client)
                .await
                .unwrap();
            compio::runtime::spawn(transport::run_driver(driver)).detach();
            let mut pts_us = 0;
            while ring.get_write_idx() < 5 {
                let frame = HangFrame {
                    track: TrackKind::Audio,
                    keyframe: false,
                    pts_us,
                    data: legacy(pts_us, &[0x21, 0x10, 0x05]),
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
}

#[test]
fn a_silent_publisher_is_cut_off_after_the_idle_bound() {
    crate::media::moq::transport::test_support::run(async {
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
        crate::media::moq::transport::test_support::run(first_catalog(
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
        "catalog.json: the parser panicked: end byte index 2 is not a char boundary"
    );
}

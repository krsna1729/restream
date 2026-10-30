use std::time::{Duration, Instant};

use bytes::Bytes;

use super::*;
use crate::moq::transport::test_support::{Loopback, run};
use restream_core::metadata::{AudioMeta, VideoMeta};
use restream_core::{MediaPacket, MediaType, PayloadFormat};
use restream_moq::media::AUDIO_TRACK;
use restream_moq::media::VIDEO_TRACK;
use restream_moq::media::parse_loc_frame;

/// An origin with a Tokio runtime for its pipeline lookups (kept alive by
/// the caller).
fn origin(max_viewers: usize) -> (Rc<Origin>, Arc<MediaEngine>, tokio::runtime::Runtime) {
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let engine = Arc::new(MediaEngine::new());
    let origin = Origin::new(engine.clone(), max_viewers);
    (origin, engine, tokio)
}

#[test]
fn the_token_comes_from_the_connect_query_else_the_setup_path() {
    assert_eq!(token_in(Some("a=1&token=t1"), "/x?token=t2"), Some("t1"));
    assert_eq!(token_in(None, "/x?b=2&token=t2"), Some("t2"));
    assert_eq!(token_in(Some("x=1"), "?token=t3"), Some("t3"));
    assert_eq!(token_in(Some("token="), ""), None);
    assert_eq!(token_in(None, "/x"), None);
    assert_eq!(token_in(Some("tokens=1"), "/token=4"), None);
}

#[test]
fn admission_verifies_the_token_for_its_pipeline() {
    let (origin, _engine, _tokio) = origin(8);
    let now = unix_now();
    let (token, _) = token::mint(token::process_key(), "pipe-a", now);

    let grant = origin.admit_token(Some(&token), now).unwrap();
    assert_eq!(grant.pipeline_id(), "pipe-a");
    drop(grant);

    assert_eq!(
        origin.admit_token(None, now).unwrap_err(),
        Reject::MissingToken
    );
    assert_eq!(
        origin.admit_token(Some(&token), now + 61).unwrap_err(),
        Reject::Token(TokenError::Expired)
    );
    let (foreign, _) = token::mint(&[1; 32], "pipe-a", now);
    assert_eq!(
        origin.admit_token(Some(&foreign), now).unwrap_err(),
        Reject::Token(TokenError::Forged)
    );
    // Pipeline b's id under pipeline a's signature.
    let (other, _) = token::mint(token::process_key(), "pipe-b", now);
    let mut parts: Vec<&str> = token.split('.').collect();
    parts[1] = other.split('.').nth(1).unwrap();
    assert_eq!(
        origin.admit_token(Some(&parts.join(".")), now).unwrap_err(),
        Reject::Token(TokenError::Forged)
    );
    assert_eq!(origin.slots_in_use(), 0, "a refusal takes no slot");

    assert!(matches!(
        Reject::MissingToken.error(),
        moq_net::Error::Unauthorized
    ));
    assert!(matches!(
        Reject::Full { max_viewers: 1 }.error(),
        moq_net::Error::App(ORIGIN_FULL_CODE)
    ));
}

#[test]
fn the_viewer_cap_refuses_past_its_maximum_and_frees_slots_on_drop() {
    let (origin, _engine, _tokio) = origin(2);
    let now = unix_now();
    let (token, _) = token::mint(token::process_key(), "pipe-a", now);
    let first = origin.admit_token(Some(&token), now).unwrap();
    let _second = origin.admit_token(Some(&token), now).unwrap();
    assert_eq!(
        origin.admit_token(Some(&token), now).unwrap_err(),
        Reject::Full { max_viewers: 2 }
    );
    assert_eq!(origin.slots_in_use(), 2);
    drop(first);
    assert_eq!(origin.slots_in_use(), 1);
    assert!(origin.admit_token(Some(&token), now).is_ok());
}

#[test]
fn viewer_slots_never_overshoot_across_threads() {
    let slots = ViewerSlots::new(3);
    let peak = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let slots = slots.clone();
            let peak = peak.clone();
            scope.spawn(move || {
                for _ in 0..1000 {
                    if let Some(slot) = slots.try_acquire() {
                        peak.fetch_max(slots.in_use(), Ordering::AcqRel);
                        drop(slot);
                    }
                }
            });
        }
    });
    assert!(peak.load(Ordering::Acquire) <= 3);
    assert_eq!(slots.in_use(), 0);
}

const PARAMETER_SETS: &[u8] = &[
    0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80, 0, 0, 0, 1, 0x68, 0xce, 0x06, 0xe2,
];
const IDR: &[u8] = &[0x65, 0x88, 0x84];

#[test]
fn the_origin_serves_only_a_known_pipeline_with_media() {
    let (origin, engine, tokio) = origin(4);
    let rings = |id: &str| {
        tokio
            .block_on(engine.ingests.pipelines.read())
            .contains_key(id)
    };
    run(async {
        // A token outlives its pipeline by up to 60 s: no ring is created.
        let gone = origin.clone().attach("pipe-gone").await.err();
        assert_eq!(gone.as_deref(), Some("no such pipeline"));
    });
    assert!(!rings("pipe-gone"), "a refused pipeline gets no ring");

    let ring = tokio.block_on(engine.get_or_create_pipeline("pipe-idle"));
    run(async {
        let idle = origin.clone().attach("pipe-idle").await.err();
        assert_eq!(idle.as_deref(), Some("the pipeline has no media yet"));
    });
    assert_eq!(
        origin.viewers("pipe-idle"),
        0,
        "a refused start is not served"
    );

    ring.set_codec_hint("h264");
    ring.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 40,
        dts: 40,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: Bytes::from([PARAMETER_SETS, &[0, 0, 0, 1], IDR].concat()),
    });
    run(async {
        assert!(
            origin.clone().attach("pipe-idle").await.is_ok(),
            "media arrived"
        );
    });
}

#[test]
fn a_viewer_with_a_token_plays_the_pipeline_over_loopback_quic() {
    let (origin, engine, tokio) = origin(4);
    let ring = tokio.block_on(engine.get_or_create_pipeline("pipe-o"));
    ring.set_codec_hint("h264");
    ring.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 40,
        dts: 40,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: Bytes::from([PARAMETER_SETS, &[0, 0, 0, 1], IDR].concat()),
    });

    run(async {
        let loopback = Loopback::new().await;
        let (client, accepted) = loopback.native().await;
        let (token, _) = mint_token("pipe-o");
        let server = origin.clone();
        compio::runtime::spawn(async move {
            let handshake = moq_net::Server::new()
                .accept_request(Instant::now(), accepted.session.clone())
                .await
                .unwrap();
            let grant = server
                .admit(&accepted, request_token(&accepted, handshake.path()))
                .unwrap();
            server.serve(grant, handshake).await;
        })
        .detach();

        let (viewer, driver) = origin::Producer::new(origin::Config::new(moq_net::Hop::random()));
        compio::runtime::spawn(transport::run_driver(driver)).detach();
        let (_session, driver) = moq_net::Client::new()
            .with_subscriber(viewer.clone())
            .with_path(format!("/?token={token}"))
            .connect(Instant::now(), client)
            .await
            .unwrap();
        compio::runtime::spawn(transport::run_driver(driver)).detach();

        let consumer = viewer.consume();
        consumer.routed("pipe-o").await.unwrap();
        let broadcast = consumer.request_broadcast("pipe-o").await.unwrap();
        let mut catalog = broadcast
            .track("catalog.json")
            .unwrap()
            .subscribe(None)
            .await
            .unwrap();
        let mut group = catalog.recv_group().await.unwrap().unwrap();
        let frame = group.read_frame().await.unwrap().unwrap();
        let catalog: hang::Catalog = hang::Catalog::from_slice(&frame.payload).unwrap();
        let expected = restream_moq::media::VideoConfig::from_parameter_sets(
            restream_moq::media::VideoCodec::H264,
            PARAMETER_SETS,
        )
        .unwrap();
        assert_eq!(
            catalog.video.renditions[VIDEO_TRACK].description,
            expected.description
        );

        let mut video = broadcast
            .track(VIDEO_TRACK)
            .unwrap()
            .subscribe(None)
            .await
            .unwrap();
        let mut gop = video.recv_group().await.unwrap().unwrap();
        let frame = gop.read_frame().await.unwrap().unwrap();
        let mut idr = (IDR.len() as u32).to_be_bytes().to_vec();
        idr.extend_from_slice(IDR);
        assert_eq!(
            parse_loc_frame(frame.payload),
            Ok((40_000, Bytes::from(idr)))
        );
        assert_eq!(origin.viewers("pipe-o"), 1);
        assert_eq!(origin.slots_in_use(), 1);
    });
}

#[test]
fn a_track_set_change_replaces_the_served_generation() {
    let (origin, engine, tokio) = origin(8);
    let ring = tokio.block_on(engine.get_or_create_pipeline("pipe-gen"));
    ring.set_audio_tracks(vec![AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        track_index: 0,
        ..AudioMeta::default()
    }]);

    run(async {
        let loopback = Loopback::new().await;

        // Generation 1: audio only. The packet follows the subscription so
        // the pump publishes the catalog and the audio group.
        let (token, _) = mint_token("pipe-gen");
        let (client, accepted) = loopback.native().await;
        let server = origin.clone();
        compio::runtime::spawn(async move {
            let handshake = moq_net::Server::new()
                .accept_request(Instant::now(), accepted.session.clone())
                .await
                .unwrap();
            let grant = server
                .admit(&accepted, request_token(&accepted, handshake.path()))
                .unwrap();
            server.serve(grant, handshake).await;
        })
        .detach();

        let (viewer, driver) = origin::Producer::new(origin::Config::new(moq_net::Hop::random()));
        compio::runtime::spawn(transport::run_driver(driver)).detach();
        let (_session, driver) = moq_net::Client::new()
            .with_subscriber(viewer.clone())
            .with_path(format!("/?token={token}"))
            .connect(Instant::now(), client)
            .await
            .unwrap();
        compio::runtime::spawn(transport::run_driver(driver)).detach();

        let consumer = viewer.consume();
        consumer.routed("pipe-gen").await.unwrap();
        let first_broadcast = consumer.request_broadcast("pipe-gen").await.unwrap();
        let mut first_catalog = first_broadcast
            .track("catalog.json")
            .unwrap()
            .subscribe(None)
            .await
            .unwrap();

        let first = origin
            .served
            .borrow()
            .get("pipe-gen")
            .cloned()
            .expect("generation 1");
        assert_eq!(first.generation, 1);
        assert_eq!(first.viewers.get(), 1);

        ring.push(MediaPacket {
            media_type: MediaType::Audio,
            track_index: 0,
            pts: 0,
            dts: 0,
            is_keyframe: false,
            format: PayloadFormat::Raw,
            payload: Bytes::from_static(&[0x11, 0x22, 0x33, 0x44]),
        });
        let mut group = first_catalog.recv_group().await.unwrap().unwrap();
        let frame = group.read_frame().await.unwrap().unwrap();
        let catalog: hang::Catalog = hang::Catalog::from_slice(&frame.payload).unwrap();
        assert!(
            catalog.video.renditions.is_empty(),
            "generation 1 has no video"
        );
        assert!(
            catalog.audio.renditions.contains_key(AUDIO_TRACK),
            "generation 1 carries audio"
        );
        drop(first_catalog);

        // The ring gains video while generation 1 is still served.
        ring.set_codec_hint("h264");
        ring.set_video_parameter_sets(PARAMETER_SETS.to_vec());
        ring.push(MediaPacket {
            media_type: MediaType::Video,
            track_index: 0,
            pts: 40,
            dts: 40,
            is_keyframe: true,
            format: PayloadFormat::Raw,
            payload: Bytes::from([PARAMETER_SETS, &[0, 0, 0, 1], IDR].concat()),
        });

        // A new viewer attaches: the origin resolves the new track set,
        // starts generation 2 and closes generation 1.
        let (token, _) = mint_token("pipe-gen");
        let (client, accepted) = loopback.native().await;
        let server = origin.clone();
        compio::runtime::spawn(async move {
            let handshake = moq_net::Server::new()
                .accept_request(Instant::now(), accepted.session.clone())
                .await
                .unwrap();
            let grant = server
                .admit(&accepted, request_token(&accepted, handshake.path()))
                .unwrap();
            server.serve(grant, handshake).await;
        })
        .detach();

        let (viewer, driver) = origin::Producer::new(origin::Config::new(moq_net::Hop::random()));
        compio::runtime::spawn(transport::run_driver(driver)).detach();
        let (_session, driver) = moq_net::Client::new()
            .with_subscriber(viewer.clone())
            .with_path(format!("/?token={token}"))
            .connect(Instant::now(), client)
            .await
            .unwrap();
        compio::runtime::spawn(transport::run_driver(driver)).detach();

        let consumer = viewer.consume();
        consumer.routed("pipe-gen").await.unwrap();
        let second_broadcast = consumer.request_broadcast("pipe-gen").await.unwrap();

        assert_eq!(origin.served_generation("pipe-gen"), Some(2));
        assert!(first.is_closed(), "generation 1 is closed");
        for _ in 0..200 {
            if first.viewers.get() == 0 {
                break;
            }
            compio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(first.viewers.get(), 0, "generation 1's viewer detached");
        assert_eq!(
            origin.viewers("pipe-gen"),
            1,
            "only the new viewer is served"
        );

        let mut second_catalog = second_broadcast
            .track("catalog.json")
            .unwrap()
            .subscribe(None)
            .await
            .unwrap();
        let mut group = second_catalog.recv_group().await.unwrap().unwrap();
        let frame = group.read_frame().await.unwrap().unwrap();
        let catalog: hang::Catalog = hang::Catalog::from_slice(&frame.payload).unwrap();
        assert!(
            catalog.video.renditions.contains_key(VIDEO_TRACK),
            "generation 2 publishes video"
        );
        assert!(
            catalog.audio.renditions.contains_key(AUDIO_TRACK),
            "generation 2 keeps audio"
        );

        let mut video = second_broadcast
            .track(VIDEO_TRACK)
            .unwrap()
            .subscribe(None)
            .await
            .unwrap();
        let mut gop = video.recv_group().await.unwrap().unwrap();
        let frame = gop.read_frame().await.unwrap().unwrap();
        let mut idr = (IDR.len() as u32).to_be_bytes().to_vec();
        idr.extend_from_slice(IDR);
        assert_eq!(
            parse_loc_frame(frame.payload),
            Ok((40_000, Bytes::from(idr)))
        );
    });
}

#[test]
fn a_track_set_change_replaces_the_generation_without_an_attach() {
    let (origin, engine, tokio) = origin(8);
    let ring = tokio.block_on(engine.get_or_create_pipeline("pipe-watch"));
    ring.set_audio_tracks(vec![AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        track_index: 0,
        ..AudioMeta::default()
    }]);

    run(async {
        let loopback = Loopback::new().await;

        // One viewer starts generation 1 (audio only).
        let (token, _) = mint_token("pipe-watch");
        let (client, accepted) = loopback.native().await;
        let server = origin.clone();
        compio::runtime::spawn(async move {
            let handshake = moq_net::Server::new()
                .accept_request(Instant::now(), accepted.session.clone())
                .await
                .unwrap();
            let grant = server
                .admit(&accepted, request_token(&accepted, handshake.path()))
                .unwrap();
            server.serve(grant, handshake).await;
        })
        .detach();

        let (viewer, driver) = origin::Producer::new(origin::Config::new(moq_net::Hop::random()));
        compio::runtime::spawn(transport::run_driver(driver)).detach();
        let (_session, driver) = moq_net::Client::new()
            .with_subscriber(viewer.clone())
            .with_path(format!("/?token={token}"))
            .connect(Instant::now(), client)
            .await
            .unwrap();
        compio::runtime::spawn(transport::run_driver(driver)).detach();

        let consumer = viewer.consume();
        consumer.routed("pipe-watch").await.unwrap();
        let broadcast = consumer.request_broadcast("pipe-watch").await.unwrap();
        let mut catalog = broadcast
            .track("catalog.json")
            .unwrap()
            .subscribe(None)
            .await
            .unwrap();

        let first = origin
            .served
            .borrow()
            .get("pipe-watch")
            .cloned()
            .expect("generation 1");
        assert_eq!(first.generation, 1);
        assert_eq!(first.viewers.get(), 1);

        ring.push(MediaPacket {
            media_type: MediaType::Audio,
            track_index: 0,
            pts: 0,
            dts: 0,
            is_keyframe: false,
            format: PayloadFormat::Raw,
            payload: Bytes::from_static(&[0x11, 0x22, 0x33, 0x44]),
        });
        let mut group = catalog.recv_group().await.unwrap().unwrap();
        let frame = group.read_frame().await.unwrap().unwrap();
        let catalog: hang::Catalog = hang::Catalog::from_slice(&frame.payload).unwrap();
        assert!(
            catalog.video.renditions.is_empty(),
            "generation 1 has no video"
        );
        drop(catalog);

        // The ring gains video. No viewer attaches; the watcher must replace
        // the generation on the ring's own metadata events.
        ring.set_video_parameter_sets(PARAMETER_SETS.to_vec());
        ring.set_codec_hint("h264");
        ring.push(MediaPacket {
            media_type: MediaType::Video,
            track_index: 0,
            pts: 40,
            dts: 40,
            is_keyframe: true,
            format: PayloadFormat::Raw,
            payload: Bytes::from([PARAMETER_SETS, &[0, 0, 0, 1], IDR].concat()),
        });

        let mut second = None;
        for _ in 0..500 {
            if let Some(current) = origin.served.borrow().get("pipe-watch").cloned()
                && current.generation == 2
            {
                second = Some(current);
                break;
            }
            compio::time::sleep(Duration::from_millis(10)).await;
        }
        let second = second.expect("generation 2 replaced generation 1 without an attach");
        assert!(first.is_closed(), "generation 1 is closed");
        for _ in 0..200 {
            if first.viewers.get() == 0 {
                break;
            }
            compio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(first.viewers.get(), 0, "generation 1's viewer detached");
        assert_eq!(
            origin.viewers("pipe-watch"),
            0,
            "the replacement has no viewers until one attaches"
        );

        // Read the replacement's own catalog and media, with no viewer.
        let mut catalog = second
            .probe
            .consume()
            .track("catalog.json")
            .unwrap()
            .subscribe(None)
            .await
            .unwrap();
        let mut group = catalog.recv_group().await.unwrap().unwrap();
        let frame = group.read_frame().await.unwrap().unwrap();
        let catalog: hang::Catalog = hang::Catalog::from_slice(&frame.payload).unwrap();
        assert!(
            catalog.video.renditions.contains_key(VIDEO_TRACK),
            "generation 2 publishes video"
        );
        assert!(
            catalog.audio.renditions.contains_key(AUDIO_TRACK),
            "generation 2 keeps audio"
        );

        let mut video = second
            .probe
            .consume()
            .track(VIDEO_TRACK)
            .unwrap()
            .subscribe(None)
            .await
            .unwrap();
        let mut gop = video.recv_group().await.unwrap().unwrap();
        let frame = gop.read_frame().await.unwrap().unwrap();
        let mut idr = (IDR.len() as u32).to_be_bytes().to_vec();
        idr.extend_from_slice(IDR);
        assert_eq!(
            parse_loc_frame(frame.payload),
            Ok((40_000, Bytes::from(idr)))
        );
    });
}

fn test_audio_meta() -> AudioMeta {
    AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        track_index: 0,
        ..AudioMeta::default()
    }
}

/// Register a live ingest for the pipeline with the requested track presence,
/// exactly as an elected input reports its probe.
async fn register_live_ingest(
    engine: &MediaEngine,
    pipeline_id: &str,
    stream_key: &str,
    video: bool,
    audio: bool,
) {
    let registration = engine
        .try_register_pipeline_input_attempt(pipeline_id, stream_key, stream_key, "rtmp", true)
        .await
        .expect("an ingest slot");
    let video_meta = video.then(|| VideoMeta {
        codec: "h264".to_string(),
        width: 640,
        height: 360,
        fps: 30.0,
        ..VideoMeta::default()
    });
    let audio_meta = audio.then(test_audio_meta);
    engine
        .update_ingest_session_meta(pipeline_id, &registration, video_meta, audio_meta, None)
        .await;
    if audio {
        engine
            .update_ingest_session_audio_tracks(pipeline_id, &registration, vec![test_audio_meta()])
            .await;
    }
    engine.select_pipeline_input(pipeline_id, stream_key).await;
}

/// A viewer on the current generation, with its session kept alive by the
/// returned handle.
async fn attach_viewer(
    origin: &Rc<Origin>,
    loopback: &Loopback,
    pipeline_id: &str,
) -> (moq_net::broadcast::Consumer, moq_net::Session) {
    let (token, _) = mint_token(pipeline_id);
    let (client, accepted) = loopback.native().await;
    let server = origin.clone();
    compio::runtime::spawn(async move {
        let handshake = moq_net::Server::new()
            .accept_request(Instant::now(), accepted.session.clone())
            .await
            .unwrap();
        let grant = server
            .admit(&accepted, request_token(&accepted, handshake.path()))
            .unwrap();
        server.serve(grant, handshake).await;
    })
    .detach();

    let (viewer, driver) = origin::Producer::new(origin::Config::new(moq_net::Hop::random()));
    compio::runtime::spawn(transport::run_driver(driver)).detach();
    let (session, driver) = moq_net::Client::new()
        .with_subscriber(viewer.clone())
        .with_path(format!("/?token={token}"))
        .connect(Instant::now(), client)
        .await
        .unwrap();
    compio::runtime::spawn(transport::run_driver(driver)).detach();

    let consumer = viewer.consume();
    consumer.routed(pipeline_id).await.unwrap();
    let broadcast = consumer.request_broadcast(pipeline_id).await.unwrap();
    (broadcast, session)
}

fn push_audio(ring: &restream_sync::RingBuffer, pts: i64) {
    ring.push(MediaPacket {
        media_type: MediaType::Audio,
        track_index: 0,
        pts,
        dts: pts,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: Bytes::from_static(&[0x11, 0x22, 0x33, 0x44]),
    });
}

fn push_video_keyframe(ring: &restream_sync::RingBuffer) {
    ring.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 40,
        dts: 40,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: Bytes::from([PARAMETER_SETS, &[0, 0, 0, 1], IDR].concat()),
    });
}

#[test]
fn continuous_publication_does_not_starve_a_media_loss() {
    let (origin, engine, tokio) = origin(8);
    let ring = tokio.block_on(engine.get_or_create_pipeline("pipe-loss"));
    ring.set_video_parameter_sets(PARAMETER_SETS.to_vec());
    tokio.block_on(register_live_ingest(
        &engine,
        "pipe-loss",
        "key",
        true,
        true,
    ));

    run(async {
        push_audio(&ring, 0);
        push_video_keyframe(&ring);
        let loopback = Loopback::new().await;
        let (broadcast, _session) = attach_viewer(&origin, &loopback, "pipe-loss").await;
        let mut catalog = broadcast
            .track("catalog.json")
            .unwrap()
            .subscribe(None)
            .await
            .unwrap();
        assert!(catalog.recv_group().await.unwrap().is_some());

        let first = origin
            .served
            .borrow()
            .get("pipe-loss")
            .cloned()
            .expect("generation 1");
        assert_eq!(first.generation, 1);
        assert_eq!(first.viewers.get(), 1);

        // The publisher disconnects. The ring keeps its codec hint, recorded
        // parameter sets and audio metadata, and keeps publishing audio.
        engine.unregister_ingest("pipe-loss").await;

        let mut closed = false;
        for pts in 1..=500 {
            push_audio(&ring, pts);
            // Publication-only stimuli must not postpone the idle deadline:
            // an end-of-stream mark wakes subscribers without changing the
            // ring's track set, and the watcher only resets its deadline on a
            // track-set change.
            ring.mark_end_of_stream();
            if origin.served_generation("pipe-loss").is_none() && first.viewers.get() == 0 {
                closed = true;
                break;
            }
            compio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            closed,
            "the absolute track-set deadline must fire under continuous publication"
        );
        assert_eq!(origin.served_generation("pipe-loss"), None);
        assert!(first.is_closed(), "the stale generation is closed");
        assert_eq!(first.viewers.get(), 0, "its viewer detached");
        assert_eq!(origin.viewers("pipe-loss"), 0);
    });
}

include!("tests/generations.rs");

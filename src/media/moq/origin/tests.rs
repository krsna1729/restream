use std::time::Instant;

use bytes::Bytes;

use super::*;
use crate::media::moq::media::{VIDEO_TRACK, parse_loc_frame};
use crate::media::moq::transport::test_support::{Loopback, run};
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};

/// An origin with a Tokio runtime for its pipeline lookups (kept alive by
/// the caller).
fn origin(max_viewers: usize) -> (Rc<Origin>, Arc<MediaEngine>, tokio::runtime::Runtime) {
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let engine = Arc::new(MediaEngine::new());
    let origin = Origin::new(engine.clone(), tokio.handle().clone(), max_viewers);
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
        let expected = crate::media::moq::media::VideoConfig::from_parameter_sets(
            crate::media::moq::media::VideoCodec::H264,
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

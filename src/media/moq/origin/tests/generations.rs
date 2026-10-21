async fn wait_for_generation(
    origin: &Rc<Origin>,
    pipeline_id: &str,
    generation: u64,
) -> Rc<Served> {
    for _ in 0..500 {
        if let Some(current) = origin.served.borrow().get(pipeline_id).cloned()
            && current.generation == generation
        {
            return current;
        }
        compio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("generation {generation} was never served for {pipeline_id}");
}

async fn wait_for_no_viewers(served: &Served) {
    for _ in 0..200 {
        if served.viewers.get() == 0 {
            return;
        }
        compio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("viewers stayed on generation {}", served.generation);
}

/// A pipeline-target backup (a recirculation loop) reports routing but no
/// probe metadata, then is promoted while the target is serving.
#[test]
fn a_promoted_pipeline_input_with_unknown_metadata_keeps_and_replaces_the_generation() {
    let (origin, engine, tokio) = origin(8);
    let ring = tokio.block_on(engine.get_or_create_pipeline("pipe-recirc"));
    tokio.block_on(register_live_ingest(
        &engine,
        "pipe-recirc",
        "primary",
        false,
        true,
    ));

    run(async {
        let loopback = Loopback::new().await;
        let (broadcast, _session) = attach_viewer(&origin, &loopback, "pipe-recirc").await;
        // An audio-only generation has no video sync point, so it starts at
        // the live edge and the first packet must follow the attach.
        push_audio(&ring, 0);
        let mut catalog = broadcast
            .track("catalog.json")
            .unwrap()
            .subscribe(None)
            .await
            .unwrap();
        let mut group = catalog.recv_group().await.unwrap().unwrap();
        let frame = group.read_frame().await.unwrap().unwrap();
        let catalog: hang::Catalog = hang::Catalog::from_slice(&frame.payload).unwrap();
        assert!(catalog.video.renditions.is_empty());
        assert!(catalog.audio.renditions.contains_key(AUDIO_TRACK));

        let first = origin
            .served
            .borrow()
            .get("pipe-recirc")
            .cloned()
            .expect("generation 1");
        assert_eq!(first.generation, 1);
        assert_eq!(first.viewers.get(), 1);

        // A pipeline: backup forwards the target's output and knows only its
        // routing, so its probe metadata is empty.
        let _backup = engine
            .try_register_pipeline_input_attempt(
                "pipe-recirc",
                "recirc",
                "pipeline:out",
                "pipeline",
                false,
            )
            .await
            .expect("a recirculation input");
        assert!(engine.select_pipeline_input("pipe-recirc", "recirc").await);

        // Absent metadata from the newly selected input must not read as
        // "no media": the ring still carries the audio being forwarded.
        compio::time::sleep(TRACK_SET_CHECK_INTERVAL + Duration::from_millis(300)).await;
        assert_eq!(
            origin.served_generation("pipe-recirc"),
            Some(1),
            "the generation is kept while the ring still has media"
        );
        assert!(!first.is_closed());

        let (_broadcast2, _session2) = attach_viewer(&origin, &loopback, "pipe-recirc").await;
        assert_eq!(first.viewers.get(), 2);

        // The target's media gains video while the backup is selected. Write
        // the keyframe and configuration before the hint starts generation 2,
        // so its pump starts at a sync point rather than racing the push.
        ring.set_video_parameter_sets(PARAMETER_SETS.to_vec());
        push_video_keyframe(&ring);
        ring.set_codec_hint("h264");

        let second = wait_for_generation(&origin, "pipe-recirc", 2).await;
        assert!(
            first.is_closed(),
            "anonymous metadata did not keep it forever"
        );
        wait_for_no_viewers(&first).await;
        assert_eq!(origin.viewers("pipe-recirc"), 0);

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
        assert!(catalog.video.renditions.contains_key(VIDEO_TRACK));
        assert!(catalog.audio.renditions.contains_key(AUDIO_TRACK));

        let (broadcast3, _session3) = attach_viewer(&origin, &loopback, "pipe-recirc").await;
        let mut video = broadcast3
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

/// Changing the selected input replaces the generation with that input's
/// track set, even when the old input's ring state is still retained.
#[test]
fn a_selection_switch_installs_the_new_inputs_tracks() {
    let (origin, engine, tokio) = origin(8);
    let ring = tokio.block_on(engine.get_or_create_pipeline("pipe-sel"));
    tokio.block_on(register_live_ingest(
        &engine, "pipe-sel", "audio", false, true,
    ));

    run(async {
        let loopback = Loopback::new().await;
        let (broadcast, _session) = attach_viewer(&origin, &loopback, "pipe-sel").await;
        // An audio-only generation has no video sync point, so it starts at
        // the live edge and the first packet must follow the attach.
        push_audio(&ring, 0);
        let mut catalog = broadcast
            .track("catalog.json")
            .unwrap()
            .subscribe(None)
            .await
            .unwrap();
        let mut group = catalog.recv_group().await.unwrap().unwrap();
        let frame = group.read_frame().await.unwrap().unwrap();
        let catalog: hang::Catalog = hang::Catalog::from_slice(&frame.payload).unwrap();
        assert!(catalog.video.renditions.is_empty());
        assert!(catalog.audio.renditions.contains_key(AUDIO_TRACK));

        let first = origin
            .served
            .borrow()
            .get("pipe-sel")
            .cloned()
            .expect("generation 1");
        assert_eq!(first.generation, 1);

        // A second input carries video only. Its metadata is not copied to
        // the ring until it is selected.
        let video_input = engine
            .try_register_pipeline_input_attempt("pipe-sel", "video", "key-video", "rtmp", false)
            .await
            .expect("a video ingest slot");
        engine
            .update_ingest_session_meta(
                "pipe-sel",
                &video_input,
                Some(VideoMeta {
                    codec: "h264".to_string(),
                    width: 640,
                    height: 360,
                    fps: 30.0,
                    ..VideoMeta::default()
                }),
                None,
                None,
            )
            .await;

        // Install the new ring configuration before the switch so the
        // watcher resolves one coherent video-only set, and push the keyframe
        // before the selection installs the hint so generation 2 starts at a
        // sync point.
        ring.set_video_parameter_sets(PARAMETER_SETS.to_vec());
        ring.set_audio_tracks(Vec::new());
        push_video_keyframe(&ring);
        assert!(engine.select_pipeline_input("pipe-sel", "video").await);

        let second = wait_for_generation(&origin, "pipe-sel", 2).await;
        assert!(first.is_closed(), "the old input's generation is closed");
        wait_for_no_viewers(&first).await;
        assert_eq!(origin.viewers("pipe-sel"), 0);

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
        assert!(catalog.video.renditions.contains_key(VIDEO_TRACK));
        assert!(
            catalog.audio.renditions.is_empty(),
            "the old input's audio is not retained"
        );

        let (broadcast2, _session2) = attach_viewer(&origin, &loopback, "pipe-sel").await;
        let mut video = broadcast2
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

/// A handshake still waiting on the transport must not hold its viewer slot
/// after its generation is closed.
#[test]
fn a_closed_generation_abandons_a_pending_handshake_and_frees_the_slot() {
    let slots = ViewerSlots::new(1);
    let slot = slots.try_acquire().expect("the slot");
    let closed = CancellationToken::new();

    run(async {
        let generation_closed = closed.clone();
        let task = compio::runtime::spawn(async move {
            let _slot = slot;
            let _ = Origin::await_handshake(std::future::pending::<()>(), &generation_closed).await;
        });
        yield_now().await;
        assert!(!task.is_finished(), "the handshake is deliberately stalled");

        closed.cancel();
        compio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("a closed generation must cancel the pending handshake")
            .expect("the cancelled handshake task completes");
    });

    assert_eq!(
        slots.in_use(),
        0,
        "the cancelled handshake released its slot"
    );
}

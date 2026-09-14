use super::*;

struct RecordingSender {
    submitted: Vec<usize>,
}

impl RtmpNativeSender for RecordingSender {
    fn submit_send(
        &mut self,
        _fd: std::os::unix::io::RawFd,
        _slot: u32,
        _generation: u64,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        self.submitted.push(bytes.len());
        Ok(())
    }

    fn submit_send_vectored(
        &mut self,
        _fd: std::os::unix::io::RawFd,
        _slot: u32,
        _generation: u64,
        buffers: &[&[u8]],
    ) -> std::io::Result<()> {
        self.submitted
            .push(buffers.iter().map(|buffer| buffer.len()).sum());
        Ok(())
    }
}

#[test]
fn publishing_can_hold_a_wire_buffer_across_native_send_completion() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_full_server_peer(stream);
    });

    let client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = RtmpConnection::plain(client_stream);
    let mut metadata = StreamMetadata::new();
    metadata.encoder = Some("native-send-test".to_string());
    let startup = RtmpPublishStartup {
        publish_metadata: Some(metadata),
        startup_video_sequence_header: Some(Bytes::from_static(&[0x01; 32])),
        startup_audio_sequence_header: Some(Bytes::from_static(&[0x02; 16])),
        ..RtmpPublishStartup::default()
    };
    let mut engine = RtmpFabricEngine::new_client(test_parts(), 4096, false, startup).unwrap();
    let feed = dummy_feed();
    let mut cursor = FeedCursor::new(0, 0);
    drive_to(
        &mut engine,
        &mut client_stream,
        &feed,
        &mut cursor,
        RtmpFabricEngine::is_publish_accepted,
    );
    let queued = engine.pending_application_bytes();
    assert!(queued > 0);

    let mut sender = RecordingSender {
        submitted: Vec::with_capacity(8),
    };
    let progress = engine.advance_native(
        &mut client_stream,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
        RtmpNativeSend {
            sender: &mut sender,
            slot: 3,
            generation: 11,
            send_result: None,
        },
    );
    assert!(matches!(
        progress,
        EngineProgress::Needs(WaitCondition::Io(_))
    ));
    assert_eq!(sender.submitted.len(), 1);
    assert!(sender.submitted[0] <= queued);
    assert_eq!(engine.pending_application_bytes(), queued);

    let mut submitted = sender.submitted[0] as i32;
    while engine.pending_application_bytes() != 0 {
        crate::test_alloc::begin();
        let progress = engine.advance_native(
            &mut client_stream,
            Readiness::WRITABLE,
            &feed,
            &mut cursor,
            budget(),
            RtmpNativeSend {
                sender: &mut sender,
                slot: 3,
                generation: 11,
                send_result: Some(submitted),
            },
        );
        assert_eq!(crate::test_alloc::end(), 0);
        assert!(!matches!(progress, EngineProgress::Failed(_)));
        submitted = *sender.submitted.last().expect("next send was submitted") as i32;
    }
    assert_eq!(engine.pending_application_bytes(), 0);
    server.join().unwrap();
}

#[test]
fn steady_state_media_native_submission_does_not_allocate_after_warmup() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_full_server_peer(stream);
    });

    let client_stream = TcpStream::connect(addr).unwrap();
    client_stream.set_nonblocking(true).unwrap();
    let mut client_stream = RtmpConnection::plain(client_stream);
    let ring = Arc::new(crate::media::ring_buffer::RingBuffer::new(4));
    ring.push(MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 100,
        dts: 80,
        payload: Bytes::from_static(&[
            0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80, 0, 0, 0, 1, 0x68, 0xce,
            0x06, 0xe2, 0, 0, 0, 1, 0x65, 0x88,
        ]),
    });
    let feed = RingFeed::new(ring, Arc::new(FeedEpoch::new()));
    let mut cursor = FeedCursor::new(0, 0);
    let mut engine =
        RtmpFabricEngine::new_client(test_parts(), 4096, false, RtmpPublishStartup::default())
            .unwrap();
    drive_to(
        &mut engine,
        &mut client_stream,
        &feed,
        &mut cursor,
        RtmpFabricEngine::is_publish_accepted,
    );

    let mut sender = RecordingSender {
        submitted: Vec::with_capacity(8),
    };
    let progress = engine.advance_native(
        &mut client_stream,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
        RtmpNativeSend {
            sender: &mut sender,
            slot: 4,
            generation: 12,
            send_result: None,
        },
    );
    assert!(matches!(
        progress,
        EngineProgress::Needs(WaitCondition::Io(_))
    ));
    assert_eq!(sender.submitted.len(), 1);
    let mut submitted = sender.submitted[0] as i32;

    while engine.pending_application_bytes() != 0 {
        crate::test_alloc::begin();
        let progress = engine.advance_native(
            &mut client_stream,
            Readiness::WRITABLE,
            &feed,
            &mut cursor,
            budget(),
            RtmpNativeSend {
                sender: &mut sender,
                slot: 4,
                generation: 12,
                send_result: Some(submitted),
            },
        );
        assert_eq!(crate::test_alloc::end(), 0);
        assert!(!matches!(progress, EngineProgress::Failed(_)));
        submitted = *sender.submitted.last().expect("next send was submitted") as i32;
    }

    server.join().unwrap();
}

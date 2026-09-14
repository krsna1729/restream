use super::*;

#[test]
fn publishing_can_hold_a_wire_buffer_across_native_send_completion() {
    struct RecordingSender {
        submitted: Vec<Vec<u8>>,
    }

    impl RtmpNativeSender for RecordingSender {
        fn submit_send(
            &mut self,
            _fd: std::os::unix::io::RawFd,
            _slot: u32,
            _generation: u64,
            bytes: &[u8],
        ) -> std::io::Result<()> {
            self.submitted.push(bytes.to_vec());
            Ok(())
        }

        fn submit_send_vectored(
            &mut self,
            _fd: std::os::unix::io::RawFd,
            _slot: u32,
            _generation: u64,
            buffers: &[&[u8]],
        ) -> std::io::Result<()> {
            self.submitted.push(
                buffers
                    .iter()
                    .flat_map(|buffer| buffer.iter().copied())
                    .collect(),
            );
            Ok(())
        }
    }

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
        submitted: Vec::with_capacity(4),
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
    assert_eq!(sender.submitted.iter().map(Vec::len).sum::<usize>(), queued);
    assert_eq!(engine.pending_application_bytes(), queued);

    let submitted = sender.submitted[0].len() as i32;
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
    assert!(matches!(
        progress,
        EngineProgress::Needs(WaitCondition::FeedOrIo(_))
            | EngineProgress::Progress {
                wait: WaitCondition::FeedOrIo(_),
                ..
            }
    ));
    assert_eq!(engine.pending_application_bytes(), 0);
    server.join().unwrap();
}

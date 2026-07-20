#[test]
fn mux_demux_two_audio_tracks_round_trip() {
    // TsMuxer assigns separate PIDs to each audio track.
    // TsDemuxer must recover both with distinct track_index values
    // and correct packet counts.
    let video = VideoMeta {
        codec: "h264".to_string(),
        width: 320,
        height: 240,
        fps: 30.0,
        bw: None,
        pid: Some(0x100),
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };
    let audio0 = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: Some("eng".to_string()),
        title: None,
        profile: None,
    };
    let audio1 = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 44100,
        channels: 1,
        channel_layout: None,
        track_index: 1,
        pid: None,
        language: Some("spa".to_string()),
        title: None,
        profile: None,
    };

    let mut muxer = TsMuxer::new(Some(&video), &[audio0, audio1]);
    let mut all_ts = Vec::new();

    // Probe-ready H.264 access unit (contains SPS/PPS) so the demuxer's
    // metadata-completeness gate can build the probe.
    let (video_payload, _) = first_probe_ready_payloads();
    // ADTS frame for AAC-LC 48 kHz stereo (7-byte header, no CRC)
    let audio0_payload = vec![0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC, 0x21, 0x10];
    // ADTS frame for AAC-LC 44.1 kHz mono
    let audio1_payload = vec![0xFF, 0xF1, 0x58, 0x40, 0x02, 0x1F, 0xFC, 0x21, 0x10];

    for i in 0..3u32 {
        let pts = (i as i64) * 33;
        let ts = muxer.mux_packet(MediaType::Video, 0, pts, pts, i == 0, &video_payload);
        all_ts.extend_from_slice(ts);
        let ts = muxer.mux_packet(MediaType::Audio, 0, pts, pts, false, &audio0_payload);
        all_ts.extend_from_slice(ts);
        let ts = muxer.mux_packet(MediaType::Audio, 1, pts, pts, false, &audio1_payload);
        all_ts.extend_from_slice(ts);
    }

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&all_ts);
    demuxer.flush();
    let packets = demuxer.drain();

    let video_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Video)
        .count();
    let audio0_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Audio && p.track_index == 0)
        .count();
    let audio1_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Audio && p.track_index == 1)
        .count();

    assert_eq!(video_count, 3, "should demux 3 video packets");
    assert_eq!(audio0_count, 3, "should demux 3 packets for audio track 0");
    assert_eq!(audio1_count, 3, "should demux 3 packets for audio track 1");

    // Verify both audio track indices appear in the demuxed stream
    let audio_track_indices: std::collections::HashSet<u32> = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Audio)
        .map(|p| p.track_index)
        .collect();
    assert!(
        audio_track_indices.contains(&0),
        "track_index 0 must be present"
    );
    assert!(
        audio_track_indices.contains(&1),
        "track_index 1 must be present"
    );

    let probe = demuxer
        .take_probe()
        .expect("round-trip should produce a probe");
    assert_eq!(probe.video.as_ref().and_then(|v| v.pid), Some(0x100));
    assert_eq!(probe.audio_tracks.len(), 2);
    assert_eq!(probe.audio_tracks[0].pid, Some(0x101));
    assert_eq!(probe.audio_tracks[0].language.as_deref(), Some("eng"));
    assert_eq!(probe.audio_tracks[1].pid, Some(0x102));
    assert_eq!(probe.audio_tracks[1].language.as_deref(), Some("spa"));
}

#[test]
fn mux_demux_32_audio_tracks_spans_pmt_packets() {
    let video = VideoMeta {
        codec: "h264".to_string(),
        width: 640,
        height: 360,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };
    let languages = [
        "eng", "spa", "fra", "deu", "ita", "por", "nld", "swe", "nor", "dan", "fin", "pol", "ces",
        "slk", "hun", "ron", "bul", "ell", "tur", "rus", "ukr", "ara", "heb", "hin", "tam", "tel",
        "jpn", "kor", "zho", "vie", "tha", "ind",
    ];
    let audio_tracks = languages
        .iter()
        .enumerate()
        .map(|(index, language)| AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 1,
            channel_layout: None,
            track_index: index as u32,
            pid: None,
            language: Some((*language).to_string()),
            title: None,
            profile: None,
        })
        .collect::<Vec<_>>();

    let mut muxer = TsMuxer::new(Some(&video), &audio_tracks);
    let mut all_ts = Vec::new();
    // Probe-ready H.264 access unit (contains SPS/PPS) so the demuxer's
    // metadata-completeness gate can build the probe.
    let (video_payload, _) = first_probe_ready_payloads();
    let audio_payload = vec![0xFF, 0xF1, 0x4C, 0x40, 0x02, 0x1F, 0xFC, 0x21, 0x10];

    all_ts.extend_from_slice(muxer.mux_packet(MediaType::Video, 0, 0, 0, true, &video_payload));
    for index in 0..audio_tracks.len() {
        all_ts.extend_from_slice(muxer.mux_packet(
            MediaType::Audio,
            index as u32,
            0,
            0,
            false,
            &audio_payload,
        ));
    }

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&all_ts);
    demuxer.flush();
    let packets = demuxer.drain();
    let probe = demuxer
        .take_probe()
        .expect("32-track round-trip should produce a probe");

    assert_eq!(probe.video.as_ref().and_then(|v| v.pid), Some(0x100));
    assert_eq!(probe.audio_tracks.len(), 32);
    assert_eq!(probe.audio_tracks[0].pid, Some(0x101));
    assert_eq!(probe.audio_tracks[0].language.as_deref(), Some("eng"));
    assert_eq!(probe.audio_tracks[31].pid, Some(0x120));
    assert_eq!(probe.audio_tracks[31].language.as_deref(), Some("ind"));
    assert_eq!(
        packets
            .iter()
            .filter(|packet| packet.media_type == MediaType::Audio)
            .map(|packet| packet.track_index)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        32
    );
}

#[test]
fn remux_segment_view_splits_video_and_selected_audio() {
    let video = VideoMeta {
        codec: "h264".to_string(),
        width: 640,
        height: 360,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };
    let audio_tracks = (0..16)
        .map(|index| AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 2,
            channel_layout: None,
            track_index: index,
            pid: None,
            language: None,
            title: None,
            profile: None,
        })
        .collect::<Vec<_>>();

    let mut muxer = TsMuxer::new(Some(&video), &audio_tracks);
    let mut source = Vec::new();
    let video_payload = vec![0x00, 0x00, 0x00, 0x01, 0x65, 0x88];
    let audio_payload = vec![0xFF, 0xF1, 0x4C, 0x80, 0x02, 0x1F, 0xFC, 0x21, 0x10];

    for frame in 0..3 {
        let pts = frame * 33;
        source.extend_from_slice(muxer.mux_packet(
            MediaType::Video,
            0,
            pts,
            pts,
            frame == 0,
            &video_payload,
        ));
        for track_index in 0..audio_tracks.len() {
            source.extend_from_slice(muxer.mux_packet(
                MediaType::Audio,
                track_index as u32,
                pts,
                pts,
                false,
                &audio_payload,
            ));
        }
    }

    let video_only = remux_segment_view(&source, Some(&video), &audio_tracks, TsSegmentView::Video)
        .expect("video rendition should contain media");
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&video_only);
    demuxer.flush();
    let packets = demuxer.drain();
    assert!(
        packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video)
    );
    assert!(
        packets
            .iter()
            .all(|packet| packet.media_type == MediaType::Video)
    );

    let audio_only = remux_segment_view(
        &source,
        Some(&video),
        &audio_tracks,
        TsSegmentView::Audio(15),
    )
    .expect("audio rendition should contain media");
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&audio_only);
    demuxer.flush();
    let packets = demuxer.drain();
    assert!(
        packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Audio)
    );
    assert!(
        packets
            .iter()
            .all(|packet| packet.media_type == MediaType::Audio)
    );
    assert_eq!(
        packets
            .iter()
            .map(|packet| packet.track_index)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        1,
        "audio rendition should expose exactly one logical track"
    );
}

#[derive(Clone, Copy, Debug)]
enum GeneratedVideoCodec {
    H264,
    H265,
}

impl GeneratedVideoCodec {
    fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "hevc",
        }
    }

    fn payload(self, is_keyframe: bool, payload_len: usize, seed: u8) -> Vec<u8> {
        let mut payload = match (self, is_keyframe) {
            (Self::H264, true) => vec![0x00, 0x00, 0x00, 0x01, 0x65],
            (Self::H264, false) => vec![0x00, 0x00, 0x00, 0x01, 0x41],
            (Self::H265, true) => vec![0x00, 0x00, 0x00, 0x01, 0x26, 0x01],
            (Self::H265, false) => vec![0x00, 0x00, 0x00, 0x01, 0x02, 0x01],
        };
        payload.extend((0..payload_len).map(|offset| seed.wrapping_add(offset as u8)));
        payload
    }
}

#[derive(Clone, Debug)]
struct GeneratedMuxPacket {
    media_type: MediaType,
    track_index: u32,
    pts_ms: i64,
    dts_ms: i64,
    is_keyframe: bool,
    payload: Vec<u8>,
}

fn generated_audio_payload(track_index: u32, payload_len: usize, seed: u8) -> Vec<u8> {
    let raw_len = payload_len.max(1);
    let mut payload = Vec::from(crate::media::codec::build_adts_header(raw_len, 48_000, 2));
    payload.extend((0..raw_len).map(|offset| {
        seed.wrapping_add(track_index as u8)
            .wrapping_add(offset as u8)
    }));
    payload
}

fn generated_mux_sequence(
    codec: GeneratedVideoCodec,
    include_video: bool,
    audio_track_count: usize,
    events: Vec<(usize, u8, u8, bool, u8)>,
) -> Vec<GeneratedMuxPacket> {
    let stream_count = usize::from(include_video) + audio_track_count;
    let mut next_dts_by_stream = vec![0_i64; stream_count];
    let mut packets: Vec<GeneratedMuxPacket> = Vec::with_capacity(events.len());

    for (selector, delta_ms, payload_len, keyframe_hint, pts_offset_units) in events {
        let stream_idx = selector % stream_count;
        let dts = next_dts_by_stream[stream_idx] + i64::from(delta_ms % 40);
        next_dts_by_stream[stream_idx] = dts + 1;
        let payload_len = usize::from(payload_len % 97) + 1;
        let pts_offset = i64::from(pts_offset_units % 4) * 8;

        if include_video && stream_idx == 0 {
            let is_keyframe =
                keyframe_hint || packets.iter().all(|p| p.media_type != MediaType::Video);
            packets.push(GeneratedMuxPacket {
                media_type: MediaType::Video,
                track_index: 0,
                pts_ms: dts + pts_offset,
                dts_ms: dts,
                is_keyframe,
                payload: codec.payload(is_keyframe, payload_len, payload_len as u8),
            });
        } else {
            let track_index = if include_video {
                stream_idx - 1
            } else {
                stream_idx
            } as u32;
            packets.push(GeneratedMuxPacket {
                media_type: MediaType::Audio,
                track_index,
                pts_ms: dts,
                dts_ms: dts,
                is_keyframe: false,
                payload: generated_audio_payload(track_index, payload_len, payload_len as u8),
            });
        }
    }

    packets
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn proptest_ts_muxer_demuxer_preserves_stream_invariants(
        codec_choice in any::<bool>(),
        include_video in any::<bool>(),
        audio_track_count in 0usize..=30,
        events in proptest::collection::vec((0usize..64, 0u8..80, 1u8..160, any::<bool>(), 0u8..8), 1..96),
    ) {
        prop_assume!(include_video || audio_track_count > 0);

        let codec = if codec_choice {
            GeneratedVideoCodec::H265
        } else {
            GeneratedVideoCodec::H264
        };
        let video = include_video.then(|| VideoMeta {
            codec: codec.name().to_string(),
            width: 1920,
            height: 1080,
            fps: 30.0,
            bw: None,
            pid: None,
            language: None,
            title: None,
            profile: None,
            level: None,
            pixel_format: None,
        });
        let audio_tracks = (0..audio_track_count)
            .map(|track_index| AudioMeta {
                codec: "aac".to_string(),
                sample_rate: 48_000,
                channels: 2,
                channel_layout: None,
                track_index: track_index as u32,
                pid: None,
                language: None,
                title: None,
                profile: None,
            })
            .collect::<Vec<_>>();
        let generated = generated_mux_sequence(codec, include_video, audio_track_count, events);

        let mut muxer = TsMuxer::new(video.as_ref(), &audio_tracks);
        let mut ts = Vec::new();
        let mut expected_streams = BTreeSet::new();
        for packet in &generated {
            expected_streams.insert((packet.media_type as u8, packet.track_index));
            ts.extend_from_slice(muxer.mux_packet(
                packet.media_type,
                packet.track_index,
                packet.pts_ms,
                packet.dts_ms,
                packet.is_keyframe,
                &packet.payload,
            ));
        }

        prop_assert!(!ts.is_empty());
        prop_assert_eq!(ts.len() % TS_PACKET_SIZE, 0);
        prop_assert!(ts.chunks_exact(TS_PACKET_SIZE).all(|chunk| chunk[0] == TS_SYNC_BYTE));

        let mut demuxer = TsDemuxer::new();
        demuxer.feed(&ts);
        demuxer.flush();
        let packets = demuxer.drain();

        prop_assert_eq!(packets.len(), generated.len());
        type StreamKey = (u8, u32);
        type ExpectedPacketsByStream = BTreeMap<StreamKey, VecDeque<(Vec<u8>, bool)>>;

        let mut last_dts_by_stream: BTreeMap<StreamKey, i64> = BTreeMap::new();
        let mut seen_streams = BTreeSet::new();
        let mut expected_by_stream: ExpectedPacketsByStream = BTreeMap::new();
        for expected in &generated {
            expected_by_stream
                .entry((expected.media_type as u8, expected.track_index))
                .or_default()
                .push_back((expected.payload.clone(), expected.is_keyframe));
        }

        for actual in &packets {
            let stream_key = (actual.media_type as u8, actual.track_index);
            let Some(expected_queue) = expected_by_stream.get_mut(&stream_key) else {
                prop_assert!(false, "unexpected stream in demux output: {:?}", stream_key);
                unreachable!();
            };
            let Some((expected_payload, expected_keyframe)) = expected_queue.pop_front() else {
                prop_assert!(false, "too many packets for stream {:?}", stream_key);
                unreachable!();
            };

            prop_assert_eq!(actual.payload.as_ref(), expected_payload.as_slice());
            prop_assert_eq!(actual.is_keyframe, expected_keyframe);
            prop_assert!(actual.pts >= actual.dts);

            if let Some(previous_dts) = last_dts_by_stream.insert(stream_key, actual.dts) {
                prop_assert!(
                    actual.dts >= previous_dts,
                    "DTS regressed for {:?}: {} -> {}",
                    stream_key,
                    previous_dts,
                    actual.dts
                );
            }
            seen_streams.insert(stream_key);
        }

        for (stream_key, expected_queue) in expected_by_stream {
            prop_assert!(
                expected_queue.is_empty(),
                "missing demux output packets for stream {:?}",
                stream_key
            );
        }
        prop_assert_eq!(seen_streams, expected_streams);
    }
}


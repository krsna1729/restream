use super::*;

fn staged(buffers: &mut IoBuffers) -> Vec<Bytes> {
    buffers.seal_copied();
    std::mem::take(&mut buffers.outgoing)
}

#[test]
fn staging_keeps_wire_order_and_shares_large_payload_without_copying() {
    let mut buffers = IoBuffers::default();
    let payload = Bytes::from(vec![7_u8; 4096]);
    buffers.stage_copy(b"hd1");
    buffers.stage_share(payload.clone());
    buffers.stage_copy(b"hd2");
    buffers.stage_share(Bytes::from_static(b"tiny"));
    buffers.stage_copy(b"hd3");
    let segments = staged(&mut buffers);

    let wire: Vec<u8> = segments.iter().flat_map(|s| s.iter().copied()).collect();
    let mut expected = b"hd1".to_vec();
    expected.extend_from_slice(&payload);
    expected.extend_from_slice(b"hd2tinyhd3");
    assert_eq!(wire, expected);
    assert_eq!(segments.len(), 3, "run, shared payload, run");
    assert_eq!(
        segments[1].as_ptr(),
        payload.as_ptr(),
        "large payload is sent from its own buffer"
    );
}

/// A message staged in prefixes stays open (its sends carry MSG_MORE)
/// until its last byte is staged; plain shared writes never open one.
#[test]
fn a_message_is_open_until_its_last_byte_is_staged() {
    let buffers = IoBuffers::new();
    let mut buffers = buffers.borrow_mut();
    buffers.transmit_capacity = 1000;
    let body = Bytes::from(vec![1_u8; 1500]);
    let parts = [TxPart::Copy(b"head"), TxPart::Share(body.clone())];
    assert_eq!(buffers.stage_parts(&parts, true).unwrap(), 1000);
    assert!(buffers.message_open);
    buffers.pending_write_bytes = 0; // the worker sent the first batch
    let rest = [TxPart::Share(body.slice(996..))];
    assert_eq!(buffers.stage_parts(&rest, true).unwrap(), 504);
    assert!(!buffers.message_open, "the last byte is staged");
    buffers.pending_write_bytes = 0;
    buffers.stage_parts(&parts, false).unwrap();
    assert!(!buffers.message_open, "a plain write never opens a message");
}

/// However large the TX bound, one batch stays within `UIO_MAXIOV`
/// iovecs, or every send of it would fail with EINVAL.
#[test]
fn a_large_tx_bound_still_keeps_a_batch_within_the_iovec_limit() {
    let buffers = IoBuffers::new();
    let mut buffers = buffers.borrow_mut();
    buffers.transmit_capacity = 64 * 1024 * 1024;
    let slice = Bytes::from(vec![2_u8; SHARE_MIN_BYTES]);
    let parts: Vec<TxPart<'_>> = (0..2 * MAX_WRITE_SEGMENTS)
        .flat_map(|_| [TxPart::Copy(b"h"), TxPart::Share(slice.clone())])
        .collect();
    let count = buffers.stage_parts(&parts, false).unwrap();
    assert!(
        count
            < parts
                .iter()
                .map(|part| part.as_slice().len())
                .sum::<usize>()
    );
    let segments = staged(&mut buffers);
    assert!(
        segments.len() <= MAX_WRITE_SEGMENTS,
        "{} segments in one batch",
        segments.len()
    );
}

#[test]
fn consume_segments_drops_written_segments_and_advances_a_partial_one() {
    let mut segments = vec![
        Bytes::from_static(b"abc"),
        Bytes::from_static(b"defg"),
        Bytes::from_static(b"hi"),
    ];
    consume_segments(&mut segments, 3);
    assert_eq!(
        segments,
        vec![Bytes::from_static(b"defg"), Bytes::from_static(b"hi")]
    );
    consume_segments(&mut segments, 2);
    assert_eq!(
        segments,
        vec![Bytes::from_static(b"fg"), Bytes::from_static(b"hi")]
    );
    consume_segments(&mut segments, 4);
    assert!(segments.is_empty());
}

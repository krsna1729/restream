#[test]
fn pat_pmt_parsing() {
    // Build a minimal PAT + PMT
    let mut ts_data = Vec::new();

    // PAT packet
    let mut pat_pkt = [0xFFu8; 188];
    pat_pkt[0] = 0x47;
    pat_pkt[1] = 0x40; // PUSI, PID=0
    pat_pkt[2] = 0x00;
    pat_pkt[3] = 0x10; // payload only, CC=0
    pat_pkt[4] = 0x00; // pointer
    pat_pkt[5] = 0x00; // table_id = PAT
    pat_pkt[6] = 0xB0;
    pat_pkt[7] = 13; // section_length
    pat_pkt[8] = 0x00;
    pat_pkt[9] = 0x01; // TSID
    pat_pkt[10] = 0xC1; // version
    pat_pkt[11] = 0x00;
    pat_pkt[12] = 0x00;
    // Program 1 → PMT PID 0x1000
    pat_pkt[13] = 0x00;
    pat_pkt[14] = 0x01;
    pat_pkt[15] = 0xF0;
    pat_pkt[16] = 0x00;
    let crc = crc32_mpeg2(&pat_pkt[5..17]);
    pat_pkt[17] = (crc >> 24) as u8;
    pat_pkt[18] = (crc >> 16) as u8;
    pat_pkt[19] = (crc >> 8) as u8;
    pat_pkt[20] = crc as u8;
    ts_data.extend_from_slice(&pat_pkt);

    // PMT packet (1 video + 1 audio)
    let mut pmt_pkt = [0xFFu8; 188];
    pmt_pkt[0] = 0x47;
    pmt_pkt[1] = 0x50; // PUSI, PID=0x1000
    pmt_pkt[2] = 0x00;
    pmt_pkt[3] = 0x10;
    pmt_pkt[4] = 0x00;
    pmt_pkt[5] = 0x02; // table_id = PMT
    let section_len = 9 + 10 + 4; // 9 fixed + 2 streams — 5 + CRC
    pmt_pkt[6] = 0xB0;
    pmt_pkt[7] = section_len as u8;
    pmt_pkt[8] = 0x00;
    pmt_pkt[9] = 0x01;
    pmt_pkt[10] = 0xC1;
    pmt_pkt[11] = 0x00;
    pmt_pkt[12] = 0x00;
    pmt_pkt[13] = 0xE1;
    pmt_pkt[14] = 0x00; // PCR PID = 0x100
    pmt_pkt[15] = 0xF0;
    pmt_pkt[16] = 0x00; // program_info_length = 0
    // Video: H.264, PID=0x100
    pmt_pkt[17] = 0x1B;
    pmt_pkt[18] = 0xE1;
    pmt_pkt[19] = 0x00;
    pmt_pkt[20] = 0xF0;
    pmt_pkt[21] = 0x00;
    // Audio: AAC, PID=0x101
    pmt_pkt[22] = 0x0F;
    pmt_pkt[23] = 0xE1;
    pmt_pkt[24] = 0x01;
    pmt_pkt[25] = 0xF0;
    pmt_pkt[26] = 0x00;
    let crc2 = crc32_mpeg2(&pmt_pkt[5..27]);
    pmt_pkt[27] = (crc2 >> 24) as u8;
    pmt_pkt[28] = (crc2 >> 16) as u8;
    pmt_pkt[29] = (crc2 >> 8) as u8;
    pmt_pkt[30] = crc2 as u8;
    ts_data.extend_from_slice(&pmt_pkt);

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&ts_data);

    assert!(demuxer.has_streams());
    assert_eq!(demuxer.streams.len(), 2);
    assert_eq!(demuxer.streams[0].kind, StreamKind::H264);
    assert_eq!(demuxer.streams[0].pid, 0x100);
    assert_eq!(demuxer.streams[1].kind, StreamKind::AacAdts);
    assert_eq!(demuxer.streams[1].pid, 0x101);
}


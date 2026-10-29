use crate::workflow::{Error, WorkflowResult};
use std::{fs, path::Path};

pub(super) fn length_prefixed(out: &mut Vec<u8>, payload: &[u8]) -> WorkflowResult {
    let len = u16::try_from(payload.len())
        .map_err(|_| "seed payload exceeds the two-byte length field")?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(())
}

pub(super) fn http(out: &Path) -> WorkflowResult {
    fs::create_dir_all(out)?;
    for (i, response) in HTTP.iter().enumerate() {
        let len =
            u8::try_from(response.len()).map_err(|_| "HTTP seed exceeds one read's length byte")?;
        let mut seed = Vec::with_capacity(response.len() + 1);
        seed.push(len);
        seed.extend_from_slice(response);
        fs::write(out.join(format!("seed-{i}")), seed)?;
    }
    Ok(())
}

pub(super) fn hls(out: &Path) -> WorkflowResult {
    fs::create_dir_all(out)?;
    for (i, playlist) in HLS.iter().enumerate() {
        for selector in [0u8, 1] {
            let mut seed = Vec::with_capacity(playlist.len() + 1);
            seed.push(selector);
            seed.extend_from_slice(playlist);
            fs::write(out.join(format!("seed-{i}-{selector:02x}")), seed)?;
        }
    }
    Ok(())
}

pub(super) fn moq(out: &Path) -> WorkflowResult {
    fs::create_dir_all(out)?;
    let mut legacy = catalog_seed(LEGACY_CATALOG)?;
    frame(&mut legacy, 0, LEGACY_VIDEO)?;
    frame(&mut legacy, 1, b"\x00\x21\x10\x05\x00")?;
    fs::write(out.join("seed-av"), legacy)?;
    fs::write(out.join("regression-h264-non-ascii-codec"), REGRESSION)?;
    for version in ["1", "\"draft-01\""] {
        for codec in ["avc3.42001e", "hev1.1.6.L93.B0"] {
            let catalog = format!(
                "{{\"version\":{version},\"tracks\":[{{\"name\":\"video\",\"packaging\":\"loc\",\"role\":\"video\",\"codec\":\"{codec}\"}},{{\"name\":\"audio\",\"packaging\":\"loc\",\"role\":\"audio\",\"codec\":\"mp4a.40.2\",\"samplerate\":48000,\"channelConfig\":\"2\"}}]}}"
            );
            let mut seed = catalog_seed(&catalog)?;
            if codec.starts_with("avc3.") {
                // LOC properties/timestamp followed by the same Annex-B access unit.
                let mut video = Vec::with_capacity(32);
                video.extend_from_slice(b"\x02\x10\x00");
                video.extend_from_slice(&LEGACY_VIDEO[1..]);
                frame(&mut seed, 0, &video)?;
            } else {
                frame(&mut seed, 0, b"\x02\x10\x00\x00\x00\x00\x01\x26\x01\xaf")?;
            }
            frame(&mut seed, 1, b"\x02\x10\x00\x21\x10")?;
            // Valid 90 kHz timescale, zero timescale, missing timestamp, truncated properties.
            frame(
                &mut seed,
                1,
                b"\x0a\x08\x80\x01\x5f\x90\x08\x80\x01\x5f\x90\x21\x10",
            )?;
            frame(&mut seed, 1, b"\x04\x08\x00\x08\x01")?;
            frame(&mut seed, 0, b"\x00")?;
            frame(&mut seed, 0, b"\x05\x10\x01")?;
            // Quotes in the draft-01 filename are intentional: preserve the corpus layout.
            fs::write(out.join(format!("seed-msf-{version}-{codec}")), seed)?;
        }
    }
    for count in [2, 65] {
        let mut seed = catalog_seed(&shared_init_catalog(count))?;
        frame(&mut seed, 0, b"\x02\x10\x00\x21\x10")?;
        frame(&mut seed, 1, b"\x02\x10\x00\x21\x10")?;
        fs::write(out.join(format!("seed-msf-shared-init-{count}")), seed)?;
    }
    Ok(())
}

fn catalog_seed(catalog: &str) -> Result<Vec<u8>, Error> {
    let mut seed = Vec::with_capacity(catalog.len() + 128);
    length_prefixed(&mut seed, catalog.as_bytes())?;
    Ok(seed)
}

fn shared_init_catalog(count: usize) -> String {
    let mut catalog = String::from(
        "{\"version\":\"draft-01\",\"initDataList\":[{\"id\":\"aac\",\"type\":\"inline\",\"data\":\"EZA=\"}],\"tracks\":[",
    );
    for index in 0..count {
        if index != 0 {
            catalog.push(',');
        }
        catalog.push_str(&format!("{{\"name\":\"audio-{index}\",\"packaging\":\"loc\",\"role\":\"audio\",\"codec\":\"mp4a.40.2\",\"initRef\":\"aac\"}}"));
    }
    catalog.push_str("]}");
    catalog
}

fn frame(out: &mut Vec<u8>, track: u8, payload: &[u8]) -> WorkflowResult {
    out.push(track);
    length_prefixed(out, payload)
}

const HTTP: [&[u8]; 5] = [
    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
    b"HTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n",
    b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n",
    b"HTTP/1.0 503 Service Unavailable\r\n\r\nbusy",
    b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
];
const HLS: [&[u8]; 2] = [
    b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:7\n#EXTINF:2.000,\nindex7.ts\n#EXT-X-DISCONTINUITY\n#EXTINF:1.5,\n../v1/index8.ts\n#EXT-X-ENDLIST\n",
    b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000,CODECS=\"avc1.64001f,mp4a.40.2\"\nlow/index.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2500000\nhigh/index.m3u8\n",
];
const LEGACY_CATALOG: &str = "{\"video\":{\"renditions\":{\"video\":{\"codec\":\"avc1.42001e\",\"codedWidth\":64,\"codedHeight\":36,\"container\":{\"kind\":\"legacy\"}}}},\"audio\":{\"renditions\":{\"audio\":{\"codec\":\"mp4a.40.2\",\"sampleRate\":48000,\"numberOfChannels\":2,\"description\":\"1190\",\"container\":{\"kind\":\"legacy\"}}}}}";
const LEGACY_VIDEO: &[u8] = b"\x00\x00\x00\x00\x01\x67\x42\x00\x1e\xf4\x05\x01\xec\x80\x00\x00\x00\x01\x68\xce\x06\xe2\x00\x00\x00\x01\x65\x88\x84\x00";
// Exact decoded bytes of the original crash regression, including its malformed fields.
const REGRESSION: &[u8] = b"\x01\x0b\x7b\x22\x76\x69\x64\x65\x6f\x22\x3a\x7b\x22\x72\x65\x6e\x64\x69\x74\x69\x6f\x6e\x73\x22\x3a\x7b\x22\x2b\x76\x69\x64\x65\x6f\x22\x3a\x7b\x22\x63\x6f\x64\x65\x63\x22\x3a\x22\x61\x76\x63\x31\x2e\x34\xce\xa8\x30\x31\x65\x22\x2c\x22\x63\x6f\x64\x65\x64\x57\x69\x64\x74\x68\x22\x3a\x36\x34\x2c\x22\x63\x6f\x64\x65\x64\x48\x65\x69\x67\x68\x74\x22\x3a\x33\x36\x2c\x22\x66\x72\x61\x3b\x65\x72\x61\x74\x65\x22\x00\x00\x00\x00\x00\x90\x00\x30\x3a\x7b\x22\x6b\x69\x6e\x64\x22\x3a\x22\x6c\x65\x6f\x61\x63\x79\x22\x7d\x7d\x7d\x7b\x2c\x22\x6e\x75\x6d\x62\x65\x72\x4f\x66\x43\x68\x61\x07\x00\x6e\x6e\x65\x6c\x73\x22\x3a\xf7\xff\xff\xff\x65\x73\x63\x61\x72\x63\x68\x69\x76\x65\x3a\x3a\x22\x30\x35\x39\x35\x22\x2c\x22\x63\x6f\x6e\x74\x61\x69\x6e";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn length_fields_enforce_boundaries_without_truncation() {
        let mut seed = Vec::new();
        length_prefixed(&mut seed, &[7; 256]).unwrap();
        assert_eq!(&seed[..2], &[1, 0]);
        let mut boundary = Vec::new();
        length_prefixed(&mut boundary, &vec![0; 65535]).unwrap();
        assert_eq!(&boundary[..2], &[255, 255]);
        let before = boundary.len();
        assert!(length_prefixed(&mut boundary, &vec![0; 65536]).is_err());
        assert_eq!(boundary.len(), before);
    }
    #[test]
    fn frame_encoding_has_track_then_big_endian_payload_length() {
        let mut encoded = Vec::new();
        frame(&mut encoded, 1, b"\x02\x10\x00\x21\x10").unwrap();
        assert_eq!(encoded, b"\x01\x00\x05\x02\x10\x00\x21\x10");
        let mut legacy = Vec::new();
        frame(&mut legacy, 0, LEGACY_VIDEO).unwrap();
        assert_eq!(&legacy[..3], &[0, 0, 30]);
    }
    #[test]
    fn shared_init_catalog_reaches_track_limit_without_losing_references() {
        for count in [2, 65] {
            let catalog: serde_json::Value =
                serde_json::from_str(&shared_init_catalog(count)).unwrap();
            let tracks = catalog["tracks"].as_array().unwrap();
            assert_eq!(tracks.len(), count);
            assert!(tracks.iter().all(|t| t["initRef"] == "aac"));
            assert_eq!(catalog["initDataList"][0]["data"], "EZA=");
        }
    }
}

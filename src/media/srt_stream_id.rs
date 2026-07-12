#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SrtConnectionMode {
    Publish,
    Read,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedStreamId {
    pub(super) mode: SrtConnectionMode,
    pub(super) stream_key: String,
}

pub(super) fn strip_query(value: &str) -> &str {
    value.split_once('?').map(|(path, _)| path).unwrap_or(value)
}

pub(super) fn normalize_srt_stream_key(value: &str) -> String {
    let without_query = strip_query(value).trim();
    let decoded = percent_decode(without_query);
    strip_query(&decoded).trim().to_string()
}

/// Decode percent-encoded characters in a URL query parameter value.
/// Handles `%XX` sequences where XX is a two-digit hex byte value.
/// Non-UTF8 sequences are passed through as-is.
pub(super) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

pub(super) fn parse_srt_stream_id(streamid: &str) -> ParsedStreamId {
    let raw = streamid.trim_matches('\0').trim();
    if raw.is_empty() {
        return ParsedStreamId {
            mode: SrtConnectionMode::Publish,
            stream_key: String::new(),
        };
    }

    if let Some(rest) = raw.strip_prefix("#!::") {
        let mut mode = SrtConnectionMode::Publish;
        let mut resource = "";
        for part in rest.split(',') {
            if let Some((key, value)) = part.split_once('=') {
                match key {
                    "r" | "streamid" => resource = value,
                    "m" => {
                        if matches!(value, "request" | "read" | "play" | "subscriber") {
                            mode = SrtConnectionMode::Read;
                        }
                    }
                    _ => {}
                }
            }
        }
        let stream_key = normalize_srt_stream_key(resource);
        return ParsedStreamId { mode, stream_key };
    }

    let (mode, rest) = if let Some((prefix, value)) = raw.split_once(':') {
        let mode = if matches!(prefix, "play" | "read" | "subscriber" | "request") {
            SrtConnectionMode::Read
        } else {
            SrtConnectionMode::Publish
        };
        (mode, value)
    } else {
        (SrtConnectionMode::Publish, raw)
    };

    let stream_key = normalize_srt_stream_key(rest);
    ParsedStreamId { mode, stream_key }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SrtConnectionMode {
    Publish,
    Read,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedStreamId {
    pub(crate) mode: SrtConnectionMode,
    pub(crate) stream_key: String,
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

pub(crate) fn parse_srt_stream_id(streamid: &str) -> ParsedStreamId {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_query_splits_on_first_question_mark_only() {
        assert_eq!(strip_query("key"), "key");
        assert_eq!(strip_query("key?a=1"), "key");
        assert_eq!(strip_query("key?a=1?b=2"), "key");
        assert_eq!(strip_query("?a=1"), "");
        assert_eq!(strip_query(""), "");
    }

    #[test]
    fn percent_decode_handles_truncated_and_malformed_escapes() {
        // Trailing `%` with no following digits must be kept literal, not panic.
        assert_eq!(percent_decode("abc%"), "abc%");
        // Only one hex digit after `%` before the string ends.
        assert_eq!(percent_decode("abc%2"), "abc%2");
        // Non-hex digits after `%` must be passed through literally, and
        // scanning must resume from the `%` itself (not skip past "GG").
        assert_eq!(percent_decode("%GGhi"), "%GGhi");
        assert_eq!(percent_decode(""), "");
    }

    #[test]
    fn percent_decode_is_case_insensitive_for_hex_digits() {
        assert_eq!(percent_decode("%2f"), "/");
        assert_eq!(percent_decode("%2F"), "/");
        assert_eq!(percent_decode("%2f%2F"), "//");
    }

    #[test]
    fn percent_decode_only_unwraps_one_layer() {
        // "%2525" decodes its outer layer to "%25" and stops; it must not
        // recursively decode into "%".
        assert_eq!(percent_decode("%2525"), "%25");
    }

    #[test]
    fn percent_decode_falls_back_to_lossy_utf8_on_invalid_byte_sequences() {
        // %FF is not valid UTF-8 on its own; must not panic and must not
        // silently drop the whole string.
        let decoded = percent_decode("prefix%FFsuffix");
        assert!(decoded.starts_with("prefix"));
        assert!(decoded.ends_with("suffix"));
        assert!(decoded.contains('\u{FFFD}'));
    }

    #[test]
    fn percent_decode_preserves_embedded_nul_bytes() {
        assert_eq!(percent_decode("a%00b"), "a\0b");
    }

    #[test]
    fn normalize_srt_stream_key_trims_whitespace_around_decoding() {
        assert_eq!(normalize_srt_stream_key("  key  "), "key");
        assert_eq!(normalize_srt_stream_key(""), "");
        assert_eq!(normalize_srt_stream_key("   "), "");
    }

    // Pin the two-pass strip_query behavior: a literal `?` is stripped
    // before decoding, but a percent-encoded `?` (`%3F`) is only revealed
    // *after* decoding, and the second `strip_query` call still catches it.
    // This means a stream key can never contain a literal `?`, encoded or
    // not — anything from a decoded `?` onward is treated as query data.
    #[test]
    fn normalize_srt_stream_key_strips_query_revealed_by_decoding() {
        assert_eq!(normalize_srt_stream_key("abc?real_query=1"), "abc");
        assert_eq!(
            normalize_srt_stream_key("abc%3Fnot_a_real_key_suffix"),
            "abc"
        );
    }

    #[test]
    fn parse_srt_stream_id_treats_whitespace_and_nul_only_input_as_empty() {
        for input in ["", "   ", "\0\0\0", "\0  \0"] {
            let parsed = parse_srt_stream_id(input);
            assert_eq!(parsed.mode, SrtConnectionMode::Publish, "input={input:?}");
            assert_eq!(parsed.stream_key, "", "input={input:?}");
        }
    }

    // `trim_matches('\0')` only strips NUL runs from the two ends of the
    // string; a NUL byte sandwiched between other characters survives into
    // the parsed stream key untouched (`.trim()` only removes whitespace,
    // not NUL).
    #[test]
    fn parse_srt_stream_id_preserves_interior_nul_bytes() {
        let parsed = parse_srt_stream_id("\0 \0 \0");
        assert_eq!(parsed.mode, SrtConnectionMode::Publish);
        assert_eq!(parsed.stream_key, "\0");
    }

    // Any raw (non-`#!::`) stream id containing a colon has everything
    // before the first colon discarded as a mode marker, even if that
    // prefix is not a recognized mode keyword. Only the recognized
    // keywords flip the mode to Read; an unrecognized prefix still gets
    // dropped, just defaulting to Publish.
    #[test]
    fn parse_srt_stream_id_discards_unrecognized_colon_prefix_as_mode_marker() {
        let parsed = parse_srt_stream_id("abc:def");
        assert_eq!(parsed.mode, SrtConnectionMode::Publish);
        assert_eq!(parsed.stream_key, "def");
    }

    #[test]
    fn parse_srt_stream_id_mode_keywords_are_case_sensitive() {
        // "Play" does not match "play"; the prefix is still stripped (mode
        // detection failed, not skipped), so only "key" survives, under
        // the default Publish mode.
        let parsed = parse_srt_stream_id("Play:key");
        assert_eq!(parsed.mode, SrtConnectionMode::Publish);
        assert_eq!(parsed.stream_key, "key");
    }

    #[test]
    fn parse_srt_stream_id_keeps_extra_colons_in_the_stream_key() {
        let parsed = parse_srt_stream_id("play:key:with:colons");
        assert_eq!(parsed.mode, SrtConnectionMode::Read);
        assert_eq!(parsed.stream_key, "key:with:colons");
    }

    #[test]
    fn parse_srt_stream_id_bracket_format_last_r_key_wins() {
        let parsed = parse_srt_stream_id("#!::r=first,r=second");
        assert_eq!(parsed.stream_key, "second");
    }

    #[test]
    fn parse_srt_stream_id_bracket_format_r_and_streamid_last_wins() {
        let parsed = parse_srt_stream_id("#!::r=first,streamid=second");
        assert_eq!(parsed.stream_key, "second");

        let parsed = parse_srt_stream_id("#!::streamid=first,r=second");
        assert_eq!(parsed.stream_key, "second");
    }

    #[test]
    fn parse_srt_stream_id_bracket_format_ignores_malformed_parts() {
        // A part with no `=` must be skipped, not cause a panic or corrupt
        // later parts.
        let parsed = parse_srt_stream_id("#!::novalue,r=key,alsonovalue");
        assert_eq!(parsed.stream_key, "key");
    }

    #[test]
    fn parse_srt_stream_id_bracket_format_value_may_contain_equals() {
        // split_once('=') must split on the first '=' only.
        let parsed = parse_srt_stream_id("#!::r=a=b=c");
        assert_eq!(parsed.stream_key, "a=b=c");
    }

    #[test]
    fn parse_srt_stream_id_bracket_format_unrecognized_mode_value_defaults_publish() {
        let parsed = parse_srt_stream_id("#!::r=key,m=nonsense");
        assert_eq!(parsed.mode, SrtConnectionMode::Publish);
        assert_eq!(parsed.stream_key, "key");
    }

    #[test]
    fn parse_srt_stream_id_bracket_format_keys_are_case_sensitive() {
        // Uppercase "R=" does not match the lowercase "r"/"streamid" keys,
        // so the resource is never set.
        let parsed = parse_srt_stream_id("#!::R=key");
        assert_eq!(parsed.stream_key, "");
    }

    #[test]
    fn parse_srt_stream_id_bracket_format_empty_rest_yields_empty_key() {
        let parsed = parse_srt_stream_id("#!::");
        assert_eq!(parsed.mode, SrtConnectionMode::Publish);
        assert_eq!(parsed.stream_key, "");
    }

    #[test]
    fn parse_srt_stream_id_trims_outer_nul_padding_before_parsing() {
        let parsed = parse_srt_stream_id("\0play:key\0");
        assert_eq!(parsed.mode, SrtConnectionMode::Read);
        assert_eq!(parsed.stream_key, "key");
    }
}

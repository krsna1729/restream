//! Trace-friendly secret display helpers for logs.

pub fn redact_secret(value: &str) -> String {
    let value = value.trim();
    let chars: Vec<char> = value.chars().collect();
    match chars.len() {
        0 => "<empty>".to_string(),
        1..=4 => format!("{}...", chars[0]),
        5..=8 => format!(
            "{}{}...{}{}",
            chars[0],
            chars[1],
            chars[chars.len() - 2],
            chars[chars.len() - 1]
        ),
        _ => {
            let prefix: String = chars.iter().take(4).collect();
            let suffix: String = chars[chars.len() - 4..].iter().collect();
            format!("{prefix}...{suffix}")
        }
    }
}

pub fn redact_url(raw: &str) -> String {
    let Some((scheme, rest)) = raw.split_once("://") else {
        return redact_secret(raw);
    };
    let (authority_and_path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, path) = split_authority_path(authority_and_path);
    let authority = redact_authority(authority);
    let path = redact_path_tail(path);

    if query.is_empty() {
        format!("{scheme}://{authority}{path}")
    } else {
        format!("{scheme}://{authority}{path}?{}", redact_query(query))
    }
}

fn split_authority_path(value: &str) -> (&str, &str) {
    match value.find('/') {
        Some(index) => (&value[..index], &value[index..]),
        None => (value, ""),
    }
}

fn redact_authority(authority: &str) -> String {
    match authority.rsplit_once('@') {
        Some((userinfo, host)) => format!("{}@{host}", redact_secret(userinfo)),
        None => authority.to_string(),
    }
}

fn redact_path_tail(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    // A trailing slash (common on some platforms' egress URLs, or a simple
    // typo) must not defeat redaction: strip it before finding the last
    // segment, then reattach it so the secret segment still gets redacted.
    let trimmed = path.trim_end_matches('/');
    let trailing_slashes = &path[trimmed.len()..];
    if trimmed.is_empty() {
        return path.to_string();
    }
    let Some((prefix, tail)) = trimmed.rsplit_once('/') else {
        return format!("{}{trailing_slashes}", redact_secret(trimmed));
    };
    format!("{prefix}/{}{trailing_slashes}", redact_secret(tail))
}

fn redact_query(query: &str) -> String {
    query
        .split('&')
        .map(|part| {
            let Some((key, value)) = part.split_once('=') else {
                return part.to_string();
            };
            if value.is_empty() || !is_sensitive_query_key(key) {
                part.to_string()
            } else {
                format!("{key}={}", redact_secret(value))
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn is_sensitive_query_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "streamid" | "passphrase" | "key" | "stream_key" | "token" | "cid"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secret_with_traceable_edges() {
        let redacted = redact_secret("stream-key-secret-value");

        assert_eq!(redacted, "stre...alue");
        assert!(!redacted.contains("stream-key-secret-value"));
    }

    #[test]
    fn redacts_path_tail_even_with_a_trailing_slash() {
        let redacted = redact_url("rtmp://host.example.com/live/superSecretKey123/");

        assert!(!redacted.contains("superSecretKey123"));
        assert!(redacted.ends_with('/'));
        assert!(redacted.starts_with("rtmp://host.example.com/live/"));
    }

    #[test]
    fn redacts_url_credentials_path_tail_and_query_values() {
        let raw = "srt://user:pass@example.com/stream-key-secret?streamid=publish:stream-key-secret&passphrase=secret-pass-123&pbkeylen=16";

        let redacted = redact_url(raw);

        assert!(redacted.contains("srt://user...pass@example.com/stre...cret?"));
        assert!(redacted.contains("streamid=publ...cret"));
        assert!(redacted.contains("passphrase=secr...-123"));
        assert!(redacted.contains("pbkeylen=16"));
        assert!(!redacted.contains("stream-key-secret"));
        assert!(!redacted.contains("secret-pass-123"));
    }
}

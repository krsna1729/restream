use crate::domain::output_spec::OutputUrlScheme;
use percent_encoding::percent_decode_str;
use reqwest::Url;

pub(crate) struct RtmpUrlParts {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) app: String,
    pub(crate) stream_key: String,
    pub(crate) tls: bool,
}

// Standard RTMP URL parser helper
pub(crate) fn parse_rtmp_url(url: &str) -> Option<RtmpUrlParts> {
    let tls = match OutputUrlScheme::from_url(url) {
        OutputUrlScheme::Rtmp => false,
        OutputUrlScheme::Rtmps => true,
        _ => return None,
    };
    let parsed = Url::parse(url).ok()?;
    let host = parsed.host_str()?.trim_matches(['[', ']']).to_string();
    let port = parsed.port().unwrap_or(1935);
    // OBS/librtmp convention: when the query holds a '/', the stream key is
    // whatever follows the last '/' of `path?query`, and the app keeps the
    // query (YouTube backup ingest: `rtmp://host/live2?backup=1/<key>`).
    let key_in_query = parsed
        .query()
        .filter(|query| query.contains('/'))
        .map(|query| format!("{}?{query}", parsed.path().trim_start_matches('/')));
    let (app, stream_key) = match key_in_query.as_deref() {
        Some(app_and_key) => {
            let (app, key) = app_and_key.rsplit_once('/')?;
            (app, key.to_string())
        }
        None => {
            let mut path_segments = parsed.path_segments()?;
            let app = path_segments.next()?;
            (app, path_segments.collect::<Vec<_>>().join("/"))
        }
    };
    if app.is_empty() || stream_key.is_empty() {
        return None;
    }
    // Path segments are percent-encoded as parsed; decode them so an
    // app/stream key containing URL-reserved characters (e.g. a stream key
    // with a literal '/' encoded as %2F) reaches the destination RTMP
    // server as the operator intended, not still escaped.
    let app = percent_decode_str(app).decode_utf8_lossy().into_owned();
    let stream_key = percent_decode_str(&stream_key)
        .decode_utf8_lossy()
        .into_owned();

    Some(RtmpUrlParts {
        host,
        port,
        app,
        stream_key,
        tls,
    })
}

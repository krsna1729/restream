use crate::domain::output_spec::OutputUrlScheme;
use crate::media::snapshots::PublisherQuality;
use crate::media::tcp_stats::collect_rtmp_sender_stats;
use percent_encoding::percent_decode_str;
use reqwest::Url;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpSocket, TcpStream, lookup_host};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

const RTMP_EGRESS_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const RTMP_EGRESS_TLS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub(crate) struct RtmpUrlParts {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) app: String,
    pub(crate) stream_key: String,
    pub(crate) tls: bool,
}

pub(super) enum RtmpEgressStream {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl AsyncRead for RtmpEgressStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            RtmpEgressStream::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            RtmpEgressStream::Tls(stream) => Pin::new(stream.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for RtmpEgressStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            RtmpEgressStream::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            RtmpEgressStream::Tls(stream) => Pin::new(stream.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            RtmpEgressStream::Plain(stream) => Pin::new(stream).poll_flush(cx),
            RtmpEgressStream::Tls(stream) => Pin::new(stream.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            RtmpEgressStream::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            RtmpEgressStream::Tls(stream) => Pin::new(stream.as_mut()).poll_shutdown(cx),
        }
    }
}

impl RtmpEgressStream {
    fn tcp_stream(&self) -> &TcpStream {
        match self {
            Self::Plain(stream) => stream,
            Self::Tls(stream) => stream.get_ref().0,
        }
    }
}

fn rustls_client_config() -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

pub(super) async fn connect_rtmp_egress_stream(
    parts: &RtmpUrlParts,
    buffer_size: usize,
) -> io::Result<RtmpEgressStream> {
    let tcp = tokio::time::timeout(RTMP_EGRESS_CONNECT_TIMEOUT, async {
        connect_tcp_with_options(parts, buffer_size).await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "RTMP TCP connect timed out"))??;

    if !parts.tls {
        return Ok(RtmpEgressStream::Plain(tcp));
    }

    let server_name = ServerName::try_from(parts.host.clone())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid RTMPS host name"))?;
    let connector = TlsConnector::from(rustls_client_config());
    let tls = tokio::time::timeout(RTMP_EGRESS_TLS_TIMEOUT, connector.connect(server_name, tcp))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "RTMPS TLS handshake timed out"))??;
    Ok(RtmpEgressStream::Tls(Box::new(tls)))
}

async fn connect_tcp_with_options(
    parts: &RtmpUrlParts,
    buffer_size: usize,
) -> io::Result<TcpStream> {
    let target = format_host_port(&parts.host, parts.port);
    let mut last_error = None;
    for addr in lookup_host(&target).await? {
        let socket = tcp_socket_for_addr(addr)?;
        configure_rtmp_socket(&socket, buffer_size);
        match socket.connect(addr).await {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no addresses resolved for RTMP target {target}"),
        )
    }))
}

fn tcp_socket_for_addr(addr: SocketAddr) -> io::Result<TcpSocket> {
    if addr.is_ipv4() {
        TcpSocket::new_v4()
    } else {
        TcpSocket::new_v6()
    }
}

fn configure_rtmp_socket(socket: &TcpSocket, buffer_size: usize) {
    let _ = socket.set_nodelay(true);
    if let Ok(buffer_size) = u32::try_from(buffer_size) {
        let _ = socket.set_send_buffer_size(buffer_size);
        let _ = socket.set_recv_buffer_size(buffer_size);
    }
}

pub(super) fn format_host_port(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

pub(super) fn rtmp_sender_quality(
    socket: &RtmpEgressStream,
    previous_tcp_bytes: &mut Option<(u64, Instant)>,
) -> PublisherQuality {
    let now = Instant::now();
    match collect_rtmp_sender_stats(socket.tcp_stream()) {
        Ok(stats) => {
            let send_rate = stats.tcp_bytes_sent.and_then(|bytes| {
                let rate = previous_tcp_bytes.and_then(|(previous, sampled_at)| {
                    let elapsed = now.duration_since(sampled_at).as_secs_f64();
                    let delta = bytes.checked_sub(previous)?;
                    (elapsed > 0.0).then_some((delta as f64 * 8.0) / (elapsed * 1_000_000.0))
                });
                *previous_tcp_bytes = Some((bytes, now));
                rate
            });
            PublisherQuality {
                tcp_congestion_algorithm: stats.tcp_congestion_algorithm,
                tcp_rtt_ms: stats.tcp_rtt_ms,
                tcp_rtt_var_ms: stats.tcp_rtt_var_ms,
                tcp_bytes_sent: stats.tcp_bytes_sent,
                tcp_bytes_acked: stats.tcp_bytes_acked,
                tcp_bytes_retrans: stats.tcp_bytes_retrans,
                tcp_last_snd_ms: stats.tcp_last_snd_ms,
                tcp_snd_mss: stats.tcp_snd_mss,
                tcp_pmtu: stats.tcp_pmtu,
                tcp_unacked: stats.tcp_unacked,
                tcp_sacked: stats.tcp_sacked,
                tcp_lost: stats.tcp_lost,
                tcp_retrans: stats.tcp_retrans,
                tcp_snd_cwnd: stats.tcp_snd_cwnd,
                tcp_snd_ssthresh: stats.tcp_snd_ssthresh,
                tcp_advmss: stats.tcp_advmss,
                tcp_reordering: stats.tcp_reordering,
                tcp_notsent_bytes: stats.tcp_notsent_bytes,
                tcp_total_retrans: stats.tcp_total_retrans,
                tcp_pacing_rate_bps: stats.tcp_pacing_rate_bps,
                tcp_max_pacing_rate_bps: stats.tcp_max_pacing_rate_bps,
                tcp_delivery_rate_bps: stats.tcp_delivery_rate_bps,
                tcp_segs_out: stats.tcp_segs_out,
                tcp_data_segs_out: stats.tcp_data_segs_out,
                tcp_delivered: stats.tcp_delivered,
                tcp_delivered_ce: stats.tcp_delivered_ce,
                tcp_busy_time_ms: stats.tcp_busy_time_ms,
                tcp_rwnd_limited_ms: stats.tcp_rwnd_limited_ms,
                tcp_sndbuf_limited_ms: stats.tcp_sndbuf_limited_ms,
                tcp_dsack_dups: stats.tcp_dsack_dups,
                tcp_reord_seen: stats.tcp_reord_seen,
                tcp_snd_wnd: stats.tcp_snd_wnd,
                tcp_total_rto: stats.tcp_total_rto,
                tcp_total_rto_recoveries: stats.tcp_total_rto_recoveries,
                tcp_total_rto_time_ms: stats.tcp_total_rto_time_ms,
                tcp_skmem_wmem_alloc: stats.tcp_skmem_wmem_alloc,
                tcp_skmem_wmem_max: stats.tcp_skmem_wmem_max,
                tcp_send_rate_mbps: send_rate,
                ..PublisherQuality::default()
            }
        }
        Err(error) => PublisherQuality {
            tcp_stats_unavailable_reason: Some(
                match error.kind() {
                    std::io::ErrorKind::Unsupported => "not_linux",
                    _ => "collection_failed",
                }
                .to_string(),
            ),
            ..PublisherQuality::default()
        },
    }
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
    let mut path_segments = parsed.path_segments()?;
    let app = path_segments.next()?;
    let stream_key = path_segments.collect::<Vec<_>>().join("/");
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

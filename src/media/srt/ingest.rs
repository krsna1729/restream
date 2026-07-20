use std::net::SocketAddr;
use std::os::raw::{c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::SrtServer;
use super::ingest_packets;
use super::socket::{
    SrtSocketGuard, add_srt_group_quality, is_srt_group, last_srt_error, srt_group_summary,
    streamid_from_getsockopt_buffer,
};
use super::srt_quality::{SrtCounterSnapshot, quality_from_stats as srt_quality_from_stats};
use super::srt_stream_id::{SrtConnectionMode, parse_srt_stream_id};
use super::sys::*;
use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::PipelineAccessMode;
use crate::media::input_gate::InputTimestampMapper;
use crate::media::security::RateLimitScope;
use crate::media::snapshots::PublisherQuality;
use crate::media::standby_gop::StandbyGopCache;
use crate::secret_display::redact_secret;

pub(super) const SRT_INGEST_READINESS_RETRY: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SrtReceiveErrorAction {
    WaitForReadiness,
    Disconnect,
}

pub(super) fn classify_srt_receive_error(error_code: c_int) -> SrtReceiveErrorAction {
    match error_code {
        SRT_EASYNCRCV | SRT_ETIMEOUT => SrtReceiveErrorAction::WaitForReadiness,
        SRT_ESCLOSED | SRT_ECONNLOST | SRT_ENOCONN => SrtReceiveErrorAction::Disconnect,
        _ => SrtReceiveErrorAction::Disconnect,
    }
}

pub(super) struct EpollWaiterSignal {
    state: std::sync::Mutex<EpollWaiterState>,
    wakeups: std::sync::Condvar,
}

#[derive(Default)]
struct EpollWaiterState {
    wait_requested: bool,
    stopped: bool,
}

impl EpollWaiterSignal {
    pub(super) fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(EpollWaiterState::default()),
            wakeups: std::sync::Condvar::new(),
        }
    }

    pub(super) fn request_wait(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.wait_requested = true;
        self.wakeups.notify_one();
    }

    pub(super) fn stop(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.stopped = true;
        self.wakeups.notify_one();
    }

    fn is_stopped(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).stopped
    }

    pub(super) fn wait_for_request(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if state.stopped {
                return false;
            }
            if state.wait_requested {
                state.wait_requested = false;
                return true;
            }
            state = self.wakeups.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }
}

pub(super) async fn wait_for_srt_ingest_readiness(
    data_ready: &AtomicBool,
    epoll_signal: &EpollWaiterSignal,
    notify: &Notify,
    cancel_token: &CancellationToken,
) -> bool {
    if data_ready.swap(false, Ordering::Acquire) {
        return true;
    }

    epoll_signal.request_wait();
    tokio::select! {
        _ = notify.notified() => true,
        _ = tokio::time::sleep(SRT_INGEST_READINESS_RETRY) => true,
        _ = cancel_token.cancelled() => false,
    }
}

struct EpollStopGuard {
    signal: Arc<EpollWaiterSignal>,
    notify: Arc<Notify>,
}

impl Drop for EpollStopGuard {
    fn drop(&mut self) {
        self.signal.stop();
        self.notify.notify_one();
    }
}

impl SrtServer {
    pub(super) async fn handle_client(&self, client_sock: SRTSOCKET, client_addr: SocketAddr) {
        let is_group = is_srt_group(client_sock);
        let client_ip = client_addr.ip().to_string();

        if let Some(remaining) = self
            .security
            .is_ip_banned_for(RateLimitScope::SrtPublish, &client_ip)
            .or_else(|| {
                self.security
                    .is_ip_banned_for(RateLimitScope::SrtRead, &client_ip)
            })
        {
            error!(
                "[srt] Rejecting banned IP {} (ban expires in {:.1}s)",
                client_ip,
                remaining.as_secs_f64()
            );
            // SAFETY: client_sock is a live accepted socket whose ownership
            // has not yet moved into an ingest or play session.
            unsafe { srt_close(client_sock) };
            return;
        }

        let mut streamid_buf = [0u8; 512];
        let mut optlen = streamid_buf.len() as c_int;
        // SAFETY: the output buffer and length pointer remain live and match
        // the exact storage passed to libsrt for this accepted socket.
        let res = unsafe {
            srt_getsockopt(
                client_sock,
                0,
                SRTO_STREAMID,
                streamid_buf.as_mut_ptr() as *mut c_void,
                &mut optlen,
            )
        };

        let streamid = if res >= 0 {
            match streamid_from_getsockopt_buffer(&streamid_buf, optlen) {
                Some(streamid) => streamid,
                None => {
                    warn!(
                        "[srt] Rejecting connection with invalid StreamID length {}",
                        optlen
                    );
                    // SAFETY: no session guard exists yet, so this path retains
                    // exclusive close ownership of client_sock.
                    unsafe { srt_close(client_sock) };
                    return;
                }
            }
        } else {
            String::new()
        };

        info!(
            "[srt] {} accepted (id={}). StreamID: {}",
            if is_group {
                "Bonded group"
            } else {
                "Connection"
            },
            client_sock,
            redact_secret(&streamid)
        );

        let parsed = parse_srt_stream_id(&streamid);
        let is_reader = parsed.mode == SrtConnectionMode::Read;
        let stream_key = parsed.stream_key.as_str();
        let access_mode = if is_reader {
            PipelineAccessMode::SrtRead
        } else {
            PipelineAccessMode::SrtPublish
        };

        let pipeline = match self
            .pipeline_access
            .authenticate(access_mode, stream_key, &client_ip)
            .await
        {
            Ok(pipeline) => pipeline,
            Err(_) => {
                warn!(
                    stream_key = %redact_secret(stream_key),
                    "unauthorized connection for stream key"
                );
                // SAFETY: authentication failed before socket ownership moved
                // to a session guard or sender thread.
                unsafe {
                    srt_close(client_sock);
                }
                return;
            }
        };

        info!(
            "[srt] Authenticated stream key: {} for pipeline: {} (mode={})",
            redact_secret(stream_key),
            pipeline.id,
            if is_reader { "read" } else { "publish" }
        );

        if is_reader {
            self.handle_play(client_sock, &pipeline.id).await;
            return;
        }

        let mut ring_buffer = self.engine.get_or_create_pipeline(&pipeline.id).await;
        let Some(registration) = self
            .engine
            .try_register_pipeline_input_attempt(
                &pipeline.id,
                &pipeline.input_id,
                stream_key,
                "srt",
                pipeline.selected,
            )
            .await
        else {
            error!(
                "[srt] Rejecting duplicate publisher for input {}",
                pipeline.input_id
            );
            // SAFETY: registration failed before a session guard was created.
            unsafe { srt_close(client_sock) };
            return;
        };
        self.engine
            .update_ingest_session_meta(
                &pipeline.id,
                &registration,
                None,
                None,
                Some(client_addr.to_string()),
            )
            .await;
        if is_group {
            match srt_group_summary(client_sock) {
                Some(summary) => info!(
                    sock = client_sock,
                    members = summary.member_count,
                    connected = summary.connected_members,
                    active = summary.active_members,
                    broken = summary.broken_members,
                    "bonded ingest group accepted",
                ),
                None => warn!(
                    sock = client_sock,
                    "bonded ingest group accepted but member state not available"
                ),
            }
        }

        let Some((bytes_received, ingest_metrics, last_progress_ms)) = self
            .engine
            .with_ingest_session(&registration, |ingest| {
                (
                    ingest.bytes_received.clone(),
                    ingest.metrics.clone(),
                    ingest.last_progress_ms.clone(),
                )
            })
            .await
        else {
            error!(
                "[srt] Ingest vanished before receive loop for pipeline {}",
                pipeline.id
            );
            self.engine
                .unregister_ingest_if_current(&pipeline.id, &registration)
                .await;
            // SAFETY: the registered session vanished before a socket guard
            // was created, so this path still owns client_sock.
            unsafe { srt_close(client_sock) };
            return;
        };

        let cached_keyframe_times = self
            .engine
            .with_ingest_session(&registration, |ingest| ingest.keyframe_times.clone())
            .await;

        let mut demuxer = crate::media::mpegts::TsDemuxer::new();
        let mut timestamp_mapper = InputTimestampMapper::default();
        let mut standby_gop = StandbyGopCache::default();
        let mut packets = Vec::with_capacity(16);
        let mut probe_sent = false;
        let mut disconnect_phase: Option<String> = None;
        let mut disconnect_reason: Option<String> = None;
        let mut disconnect_had_error = false;

        let zero: c_int = 0;
        // SAFETY: client_sock is live and zero has the exact c_int layout
        // required to select non-blocking receive mode.
        unsafe {
            srt_setsockopt(
                client_sock,
                0,
                SRTO_RCVSYN,
                &zero as *const _ as *const c_void,
                std::mem::size_of::<c_int>() as c_int,
            );
        }

        // SAFETY: the returned epoll handle is checked before use and is
        // released by the waiter task or the registration-failure path.
        let eid = unsafe { srt_epoll_create() };
        if eid < 0 {
            error!("Failed to create epoll instance");
            self.engine
                .unregister_ingest_if_current(&pipeline.id, &registration)
                .await;
            // SAFETY: no client guard exists on this early failure path.
            unsafe { srt_close(client_sock) };
            return;
        }
        let epoll_events = SRT_EPOLL_IN | SRT_EPOLL_ERR;
        // SAFETY: eid and client_sock are live handles, and epoll_events is a
        // correctly typed stack value that outlives this registration call.
        if unsafe { srt_epoll_add_usock(eid, client_sock, &epoll_events) } < 0 {
            error!("Failed to add socket to epoll");
            self.engine
                .unregister_ingest_if_current(&pipeline.id, &registration)
                .await;
            // SAFETY: this path exclusively owns both handles and releases
            // them in reverse creation order.
            unsafe {
                srt_epoll_release(eid);
                srt_close(client_sock)
            };
            return;
        }

        let _client_sock_guard = SrtSocketGuard::new(client_sock);
        let mut buf = vec![0u8; if is_group { 2048 } else { 1316 }];
        let mut previous_stats: Option<SrtCounterSnapshot> = None;
        let mut last_stats_sample = Instant::now() - Duration::from_secs(1);

        let data_ready = Arc::new(AtomicBool::new(false));
        let epoll_signal = Arc::new(EpollWaiterSignal::new());
        let notify = Arc::new(Notify::new());

        let w_data_ready = data_ready.clone();
        let w_signal = epoll_signal.clone();
        let w_notify = notify.clone();
        let mut epoll_waiter = Some(tokio::task::spawn_blocking(move || {
            while w_signal.wait_for_request() {
                loop {
                    if w_signal.is_stopped() {
                        break;
                    }

                    let mut read_ready = [SRTSOCKET::default(); 1];
                    let mut rnum = 1i32;
                    // SAFETY: the waiter owns eid until it releases it below;
                    // read_ready and rnum are valid for the duration of the
                    // blocking call and all unused fd sets are null.
                    let ret = unsafe {
                        srt_epoll_wait(
                            eid,
                            read_ready.as_mut_ptr(),
                            &mut rnum,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            200,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                        )
                    };
                    if ret > 0 {
                        w_data_ready.store(true, Ordering::Release);
                        w_notify.notify_one();
                        break;
                    }
                }
            }

            // SAFETY: this waiter is the sole owner of eid after successful
            // registration and releases it exactly once.
            unsafe {
                srt_epoll_release(eid);
            }
            w_data_ready.store(true, Ordering::Release);
            w_notify.notify_one();
        }));

        let _epoll_stop_guard = EpollStopGuard {
            signal: epoll_signal.clone(),
            notify: notify.clone(),
        };

        loop {
            if registration.cancel_token.is_cancelled() {
                break;
            }

            // SAFETY: client_sock remains live under SrtSocketGuard and buf
            // provides writable storage matching the exact length passed.
            let n = unsafe {
                if is_group {
                    srt_recvmsg2(
                        client_sock,
                        buf.as_mut_ptr(),
                        buf.len() as c_int,
                        std::ptr::null_mut(),
                    )
                } else {
                    srt_recv(client_sock, buf.as_mut_ptr(), buf.len() as c_int)
                }
            };
            if n > 0 {
            } else if n == 0 {
                disconnect_phase = Some("disconnect".to_string());
                disconnect_reason = Some("publisher disconnected".to_string());
                break;
            } else {
                let (error_code, error_message) = last_srt_error();
                match classify_srt_receive_error(error_code) {
                    SrtReceiveErrorAction::WaitForReadiness => {
                        if !wait_for_srt_ingest_readiness(
                            &data_ready,
                            &epoll_signal,
                            &notify,
                            &registration.cancel_token,
                        )
                        .await
                        {
                            break;
                        }
                    }
                    SrtReceiveErrorAction::Disconnect => {
                        error!(
                            "[srt] Receive ended for pipeline {}: code={} {}",
                            pipeline.id, error_code, error_message
                        );
                        disconnect_phase = Some("receive".to_string());
                        disconnect_reason = Some(format!("code={error_code} {error_message}"));
                        disconnect_had_error = true;
                        break;
                    }
                }
                continue;
            }

            demuxer.feed(&buf[..n as usize]);
            if demuxer.drain_into(&mut packets) > 0 {
                ingest_packets::forward_ingest_packets(
                    &mut packets,
                    &ring_buffer,
                    &registration,
                    &mut timestamp_mapper,
                    &mut standby_gop,
                    cached_keyframe_times.as_ref(),
                );
            }

            if !probe_sent && let Some(probe) = demuxer.take_probe() {
                probe_sent = true;
                let video_fps = probe.video.as_ref().map(|v| v.fps).unwrap_or(30.0);
                let audio_track_count = probe.audio_tracks.len();
                if let Some(ref video) = probe.video {
                    info!(
                        "[srt] Probed video: {} {}x{} {:.1}fps profile={:?}",
                        video.codec, video.width, video.height, video.fps, video.profile
                    );
                }
                for audio in &probe.audio_tracks {
                    info!(
                        "[srt] Probed audio track {}: {} {}Hz {}ch",
                        audio.track_index, audio.codec, audio.sample_rate, audio.channels
                    );
                }
                let first_audio = probe.audio_tracks.first().cloned();
                let selected_video_track_index = probe.video.as_ref().map(|_| 0);
                self.engine
                    .update_ingest_session_meta(
                        &pipeline.id,
                        &registration,
                        probe.video,
                        first_audio,
                        None,
                    )
                    .await;
                self.engine
                    .update_ingest_session_video_track_selection(
                        &registration,
                        probe.video_track_count,
                        selected_video_track_index,
                    )
                    .await;
                if !probe.audio_tracks.is_empty() {
                    self.engine
                        .update_ingest_session_audio_tracks(
                            &pipeline.id,
                            &registration,
                            probe.audio_tracks,
                        )
                        .await;
                }
                if self
                    .engine
                    .is_ingest_session_selected(&pipeline.id, &registration)
                    .await
                    && let Some(new_ring) = self
                        .engine
                        .adapt_pipeline_ring(&pipeline.id, video_fps, audio_track_count)
                        .await
                {
                    ring_buffer = new_ring;
                }
            }

            bytes_received.fetch_add(n as u64, Ordering::Relaxed);
            ingest_metrics.record_in(n as u64);
            last_progress_ms.store(MediaEngine::now_epoch_ms(), Ordering::Relaxed);

            if last_stats_sample.elapsed() >= Duration::from_secs(1) {
                // SAFETY: all-zero is a valid initialization for the C stats
                // struct before libsrt fills its fields.
                let mut stats: SrtTraceBStats = unsafe { std::mem::zeroed() };
                let sampled_at = Instant::now();
                let group_summary = is_group.then(|| srt_group_summary(client_sock)).flatten();
                // SAFETY: client_sock is live and stats is correctly sized,
                // aligned, initialized writable storage.
                if unsafe { srt_bistats(client_sock, &mut stats, 0, 1) } >= 0 {
                    let (mut quality, snapshot) =
                        srt_quality_from_stats(&stats, previous_stats, sampled_at);
                    add_srt_group_quality(&mut quality, is_group, group_summary);
                    previous_stats = Some(snapshot);
                    self.engine
                        .update_ingest_session_quality(&registration, quality)
                        .await;
                } else {
                    let mut quality = PublisherQuality::default();
                    add_srt_group_quality(&mut quality, is_group, group_summary);
                    self.engine
                        .update_ingest_session_quality(&registration, quality)
                        .await;
                }
                last_stats_sample = sampled_at;
            }
        }

        demuxer.flush();
        if demuxer.drain_into(&mut packets) > 0 {
            ingest_packets::forward_ingest_packets(
                &mut packets,
                &ring_buffer,
                &registration,
                &mut timestamp_mapper,
                &mut standby_gop,
                None,
            );
        }

        info!("Ingest stream finished for pipeline: {}", pipeline.id);
        self.engine
            .record_ingest_disconnect_if_current(
                &pipeline.id,
                &registration,
                disconnect_phase.as_deref(),
                disconnect_reason,
                disconnect_had_error,
            )
            .await;
        self.engine
            .unregister_ingest_if_current(&pipeline.id, &registration)
            .await;

        epoll_signal.stop();
        notify.notify_one();
        if let Some(handle) = epoll_waiter.take() {
            let _ = handle.await;
        }
    }
}

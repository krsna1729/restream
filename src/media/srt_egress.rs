use super::srt_crypto::{apply_srt_crypto_config, apply_srt_crypto_socket, srt_crypto_from_url};
use super::srt_url::parse_srt_egress_url;
use super::*;
use crate::secret_display::redact_url;

impl Drop for SrtServer {
    fn drop(&mut self) {
        // srt_cleanup() is intentionally NOT called here.
        //
        // SrtServer is Arc-owned by a tokio task that may be dropped during
        // runtime shutdown, at which point SRT egress sender OS threads may
        // still hold open SRTSOCKET handles.  Calling srt_cleanup() while live
        // sockets exist violates the libsrt API contract and can produce SIGSEGV
        // or assertion failures inside libsrt.
        //
        // Instead, call crate::media::srt::teardown_srt() explicitly from
        // run_app() AFTER all OS threads have been joined (and therefore all
        // SRT sockets have been closed via srt_close() in their cleanup paths).
    }
}

/// Call srt_cleanup() to release libsrt global state.
///
/// Must be called AFTER all SRT sockets (server + egress) are closed and
/// their OS threads have been joined.  run_app() calls this at the very end
/// of the graceful-shutdown sequence, after drain_os_thread_handles().
// SAFETY: srt_cleanup must be called after all SRT sockets are closed
// and all OS threads using libsrt have been joined. run_app() enforces
// this by calling teardown_srt() as the final step of graceful shutdown.
pub fn teardown_srt() {
    unsafe {
        srt_cleanup();
    }
}

async fn resolve_host(host_port: &str) -> Option<SocketAddr> {
    match host_port.parse::<SocketAddr>() {
        Ok(a) => Some(a),
        Err(_) => tokio::net::lookup_host(host_port)
            .await
            .ok()
            .and_then(|mut addrs| addrs.next()),
    }
}

fn to_libc_sockaddr(addr: SocketAddr) -> (libc::sockaddr_storage, c_int) {
    // SAFETY: zeroed() is valid for sockaddr_storage (all-zero is a
    // valid uninitialized socket address). Raw pointer writes through
    // a correctly-typed pointer (sockaddr_in or sockaddr_in6) cast
    // from the storage reference. The family field is set first to
    // identify the variant before any other field is written.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    match addr {
        SocketAddr::V4(v4) => {
            let sin = &mut storage as *mut _ as *mut libc::sockaddr_in;
            // SAFETY: sin is a valid pointer to the storage buffer cast
            // to the correct sockaddr_in variant. The struct is zero-
            // initialized above; we write all required fields.
            unsafe {
                (*sin).sin_family = libc::AF_INET as libc::sa_family_t;
                (*sin).sin_port = v4.port().to_be();
                (*sin).sin_addr.s_addr = u32::from_ne_bytes(v4.ip().octets());
            }
            (storage, std::mem::size_of::<libc::sockaddr_in>() as c_int)
        }
        SocketAddr::V6(v6) => {
            let sin6 = &mut storage as *mut _ as *mut libc::sockaddr_in6;
            // SAFETY: sin6 is a valid pointer to the storage buffer.
            // AF_INET6 is set first to identify the variant; subsequent
            // fields (port, addr) are written to the correct variant.
            unsafe {
                (*sin6).sin6_family = libc::AF_INET6 as libc::sa_family_t;
                (*sin6).sin6_port = v6.port().to_be();
                (*sin6).sin6_addr.s6_addr = v6.ip().octets();
            }
            (storage, std::mem::size_of::<libc::sockaddr_in6>() as c_int)
        }
    }
}

pub fn start_shared_ts_muxer(
    pipeline_id: &str,
    source_ring: Arc<RingBuffer>,
    engine: Arc<MediaEngine>,
    cancel: CancellationToken,
) -> Arc<TsChunkRing> {
    let ts_ring = Arc::new(TsChunkRing::new(
        engine.config.ts_ring_capacity,
        cancel.clone(),
    ));
    let ts_ring_clone = ts_ring.clone();
    let pipeline_id_str = pipeline_id.to_string();

    tokio::spawn(async move {
        // Wait for ingest metadata before starting the MPEG-TS muxer
        let (video_meta, audio_tracks) = loop {
            if cancel.is_cancelled() {
                return;
            }
            let result = engine
                .with_active_ingest(&pipeline_id_str, |ingest| {
                    let video = ingest.video.clone();
                    video.as_ref()?;
                    let tracks = if let Some(routed_tracks) = source_ring.audio_tracks()
                        && !routed_tracks.is_empty()
                    {
                        std::sync::Arc::new(routed_tracks.to_vec())
                    } else {
                        let lock = ingest
                            .audio_tracks
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        if lock.is_empty()
                            && let Some(audio) = ingest.audio.clone()
                        {
                            std::sync::Arc::new(vec![audio])
                        } else {
                            std::sync::Arc::clone(&lock)
                        }
                    };
                    Some((video, tracks))
                })
                .await
                .flatten();
            if let Some(meta) = result {
                break meta;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            if !engine.has_active_ingest(&pipeline_id_str).await {
                error!(
                    "[srt-shared-muxer] Ingest gone while waiting for probe: {}",
                    pipeline_id_str
                );
                cancel.cancel();
                return;
            }
        };

        // Feed loop: read from source_ring, mux inline, write to ts_ring
        let muxer_video_meta = {
            let ring_codec = source_ring.codec_hint_str();
            let ingest_codec = video_meta.as_ref().map(|v| v.codec.as_str()).unwrap_or("");
            if !ring_codec.is_empty() && ring_codec != ingest_codec {
                error!(
                    "[srt-shared-muxer] codec_hint override: ingest={} ring={}",
                    ingest_codec, ring_codec
                );
                let mut vm = video_meta.clone();
                if let Some(ref mut v) = vm {
                    v.codec = ring_codec.to_string();
                }
                vm
            } else {
                video_meta.clone()
            }
        };

        let mut muxer =
            crate::media::mpegts::TsMuxer::new(muxer_video_meta.as_ref(), &audio_tracks);
        let num_streams = (video_meta.is_some() as usize) + audio_tracks.len();
        let mut dts_enforcer = crate::media::ring_buffer::DtsEnforcer::new(num_streams);
        let mut nalu_len_size: usize = 4;
        // source_ring's own cache always wins: for a preset/transcoded egress
        // muxer, source_ring is the transcoder's output ring, which describes
        // a different resolution/codec than the pipeline-level ingest
        // sequence-header cache below. That ingest cache is keyed only by
        // pipeline_id (see MediaEngine::cache_sequence_header), so it cannot
        // distinguish "source" from "preset" — only fall back to it when the
        // ring itself has nothing cached yet.
        let mut sps_pps_cache: Vec<u8> =
            if let Some(parameter_sets) = source_ring.video_parameter_sets() {
                parameter_sets.to_vec()
            } else {
                let (vsh, _) = engine.get_sequence_headers(&pipeline_id_str).await;
                if let Some(ref flv_sh) = vsh {
                    if flv_sh.len() > 5 {
                        let (nls, annexb) = crate::media::codec::parse_avcc_config(&flv_sh[5..]);
                        nalu_len_size = nls;
                        annexb
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                }
            };

        let mut reader = Reader::new(
            format!("ts_shared_muxer:{}", pipeline_id_str),
            source_ring.clone(),
        );
        let mut video_conv_buf = Vec::<u8>::new();
        let mut audio_conv_buf = Vec::<u8>::new();
        // `chunk_ends` records (byte_offset_end, is_keyframe) for each muxed chunk so
        // we can slice a single `BytesMut` into per-chunk `Bytes` after the inner loop.
        // This converts N malloc+memcpy calls (one per chunk) to 1 malloc per burst.
        let mut chunk_ends: Vec<(usize, bool)> = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
        let mut pull_packets = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = reader.wait_for_data() => {
                    pull_packets.clear();
                    match reader.pull_burst(&mut pull_packets, MEDIA_PULL_BURST_PACKETS) {
                        Ok(0) | Err(_) => {}
                        Ok(_) => {
                            chunk_ends.clear();
                            // One allocation for the burst's TS output, sized to
                            // the actual media payloads. A fixed 64 KiB floor
                            // pins excessive memory in the retained TS ring
                            // when the muxer wakes for one small packet.
                            let mut ts_accum = bytes::BytesMut::with_capacity(
                                estimate_ts_accum_capacity(&pull_packets),
                            );
                            for pkt in &pull_packets {
                                let payload: &[u8] = match pkt.media_type {
                                    MediaType::Video => {
                                        if sps_pps_cache.is_empty()
                                            && let Some(parameter_sets) =
                                                reader.current_ring().video_parameter_sets()
                                        {
                                            sps_pps_cache.extend_from_slice(&parameter_sets);
                                        }
                                        match crate::media::codec::video_for_ts_into(
                                            &pkt.payload,
                                            pkt.format,
                                            &mut nalu_len_size,
                                            &mut sps_pps_cache,
                                            &mut video_conv_buf,
                                        ) {
                                            Some(p) => p,
                                            None => continue,
                                        }
                                    }
                                    MediaType::Audio => {
                                        let track = audio_tracks
                                            .iter()
                                            .find(|a| a.track_index == pkt.track_index)
                                            .or(audio_tracks.first());
                                        let (sr, ch) = track
                                            .map(|a| (a.sample_rate, a.channels))
                                            .unwrap_or((48000, 1));
                                        match crate::media::codec::audio_for_ts_into(
                                            &pkt.payload,
                                            pkt.format,
                                            sr,
                                            ch,
                                            &mut audio_conv_buf,
                                        ) {
                                            Some(p) => p,
                                            None => continue,
                                        }
                                    }
                                };

                                let stream_idx = match pkt.media_type {
                                    MediaType::Video => 0,
                                    MediaType::Audio => {
                                        let video_offset = video_meta.is_some() as usize;
                                        match audio_tracks
                                            .iter()
                                            .position(|a| a.track_index == pkt.track_index)
                                        {
                                            Some(i) => i + video_offset,
                                            None => continue,
                                        }
                                    }
                                };

                                let (pts, dts) = dts_enforcer.enforce(stream_idx, pkt.pts, pkt.dts);
                                let ts_bytes = muxer.mux_packet(
                                    pkt.media_type,
                                    pkt.track_index,
                                    pts,
                                    dts,
                                    pkt.is_keyframe,
                                    payload,
                                );
                                if !ts_bytes.is_empty() {
                                    ts_accum.extend_from_slice(ts_bytes);
                                    chunk_ends.push((ts_accum.len(), pkt.is_keyframe));
                                }
                            }
                            if !chunk_ends.is_empty() {
                                // freeze() promotes ts_accum to a shared Arc-backed Bytes.
                                // slice() below only bumps the refcount — no extra allocations.
                                let frozen = ts_accum.freeze();
                                let mut prev = 0usize;
                                ts_ring_clone.push_batch(chunk_ends.drain(..).map(
                                    move |(end, is_kf)| {
                                        let chunk = frozen.slice(prev..end);
                                        prev = end;
                                        (chunk, is_kf)
                                    },
                                ));
                            }
                        }
                    }
                }
            }
            if !engine
                .ingests
                .active
                .read()
                .await
                .contains_key(&pipeline_id_str)
            {
                break;
            }
        }
        cancel.cancel();
    });

    ts_ring
}

pub(super) fn estimate_ts_accum_capacity(packets: &[Arc<MediaPacket>]) -> usize {
    packets
        .iter()
        .map(|packet| packet.payload.len().saturating_add(188 * 4))
        .sum::<usize>()
        .max(188)
}

// SRT Egress Client
pub async fn start_srt_egress(
    output_id: String,
    pipeline_id: String,
    encoding: String,
    target_url: String,
    ring_buffer: Arc<RingBuffer>,
    engine: Arc<MediaEngine>,
    registration: EgressRegistration,
) {
    let cancel_token = registration.cancel_token.clone();
    macro_rules! egress_error {
        ($phase:expr, $message:expr) => {{
            engine
                .record_egress_error_if_current(&output_id, &registration, $phase, $message)
                .await;
        }};
    }
    macro_rules! egress_phase {
        ($phase:expr) => {{
            engine
                .update_egress_phase_if_current(&output_id, &registration, $phase)
                .await;
        }};
    }
    macro_rules! egress_target_addr {
        ($addr:expr) => {{
            engine
                .update_egress_target_addr_if_current(&output_id, &registration, $addr)
                .await;
        }};
    }
    let parsed = parse_srt_egress_url(&target_url);
    let host_port = &parsed.host_port;
    let streamid = parsed.streamid;
    let bond_addrs = parsed.bond_addrs;
    let url_crypto = srt_crypto_from_url(parsed.passphrase, parsed.pbkeylen);

    egress_phase!(EgressPhase::Resolving);
    let addr = match resolve_host(host_port).await {
        Some(a) => a,
        None => {
            error!("Failed to resolve target: {}", redact_url(&target_url));
            egress_error!("resolve", "failed to resolve target");
            return;
        }
    };
    egress_target_addr!(addr.to_string());

    // Resolve bond addresses
    let mut all_addrs = vec![addr];
    for bond_hp in &bond_addrs {
        match resolve_host(bond_hp).await {
            Some(a) => all_addrs.push(a),
            None => error!(addr = %bond_hp, "failed to resolve bond address"),
        }
    }

    let use_bonding = all_addrs.len() > 1;

    if !use_bonding {
        engine
            .wait_for_upstream_warmup(
                &output_id,
                &registration,
                ring_buffer.clone(),
                cancel_token.clone(),
            )
            .await;
        if cancel_token.is_cancelled() {
            return;
        }
    }

    egress_phase!(EgressPhase::Connecting);

    // srt_connect/srt_connect_group block the calling OS thread until the
    // handshake completes or times out. Running that inline on a Tokio
    // worker thread starves every other async task in the process under
    // fanout (AGENTS.md: blocking SRT calls belong on dedicated OS threads),
    // so the whole connect step (socket/group creation through connect)
    // runs via spawn_blocking instead.
    let connect_result = tokio::task::spawn_blocking(move || -> Result<SRTSOCKET, String> {
        let client_sock: SRTSOCKET;
        if use_bonding {
            // Create a bonding group (backup mode: one active, failover to next)
            // SAFETY: srt_create_group creates a bonding group socket.
            // SRT_GTYPE_BACKUP configures active/passive failover mode.
            // The returned handle is closed on all exit paths below.
            client_sock = unsafe { srt_create_group(SRT_GTYPE_BACKUP) };
            if client_sock < 0 {
                error!("Failed to create bonding group");
                return Err("failed to create bonding group".to_string());
            }

            let streamid_c = if streamid.is_empty() {
                None
            } else {
                match std::ffi::CString::new(streamid.as_str()) {
                    Ok(c) => Some(c),
                    Err(_) => {
                        error!("Stream ID contains null bytes");
                        // SAFETY: Valid group socket, clean up on invalid streamid.
                        unsafe {
                            srt_close(client_sock);
                        }
                        return Err("stream ID contains null bytes".to_string());
                    }
                }
            };
            let config_needed = streamid_c.is_some() || url_crypto.is_some();
            let config = if config_needed {
                // SAFETY: srt_create_config allocates a per-member config.
                // Ownership transfers to SRT on successful srt_connect_group;
                // on failure config is freed via srt_delete_config below.
                let config = unsafe { srt_create_config() };
                if config.is_null() {
                    unsafe {
                        srt_close(client_sock);
                    }
                    return Err("failed to create bonded SRT member config".to_string());
                }
                if let Some(streamid_c) = &streamid_c {
                    unsafe {
                        check_srt_option_result(
                            "SRTO_STREAMID",
                            srt_config_add(
                                config,
                                SRTO_STREAMID,
                                streamid_c.as_ptr() as *const c_void,
                                streamid.len() as c_int,
                            ),
                        )
                    }
                    .inspect_err(|_| unsafe {
                        srt_delete_config(config);
                        srt_close(client_sock);
                    })?;
                }
                if let Some(crypto) = &url_crypto
                    && let Err(error) = unsafe { apply_srt_crypto_config(config, crypto) }
                {
                    unsafe {
                        srt_delete_config(config);
                        srt_close(client_sock);
                    }
                    return Err(error);
                }
                config
            } else {
                std::ptr::null_mut()
            };

            let connect_error = {
                let mut members: Vec<SrtGroupMemberConfig> = Vec::new();
                for (i, &peer_addr) in all_addrs.iter().enumerate() {
                    let (peer_storage, addrlen) = to_libc_sockaddr(peer_addr);
                    // SAFETY: srt_prepare_endpoint creates a group member
                    // descriptor from a sockaddr. The peer_storage is
                    // stack-allocated and valid for this call.
                    let mut member = unsafe {
                        srt_prepare_endpoint(
                            std::ptr::null(),
                            &peer_storage as *const _ as *const libc::sockaddr,
                            addrlen,
                        )
                    };
                    member.weight = if i == 0 { 1 } else { 0 };
                    if !config.is_null() {
                        member.config = config;
                    }
                    members.push(member);
                }

                // SAFETY: srt_connect_group opens all member connections.
                // members is a correctly sized Vec of SrtGroupMemberConfig.
                // On failure, client_sock and config are cleaned up.
                let conn_res = unsafe {
                    srt_connect_group(client_sock, members.as_mut_ptr(), members.len() as c_int)
                };
                if conn_res < 0 {
                    // SAFETY: srt_getlasterror_str returns a thread-local
                    // static string valid until the next SRT call.
                    let err = unsafe { std::ffi::CStr::from_ptr(srt_getlasterror_str()) };
                    let message = format!("bonded connection failed: {}", err.to_string_lossy());
                    error!(
                        "[srt-egress] Bonded connection failed: {}",
                        err.to_string_lossy()
                    );
                    // SAFETY: Clean up group socket and per-member config
                    // on connection failure. Order: close socket, then
                    // free config (config must not outlive the socket).
                    unsafe {
                        srt_close(client_sock);
                        if !config.is_null() {
                            srt_delete_config(config);
                        }
                    }
                    Some(message)
                } else {
                    None
                }
            };
            if let Some(message) = connect_error {
                return Err(message);
            }
            // config ownership transfers to SRT on successful connect

            info!(
                "[srt-egress] Bonded connection ({} links) to {}",
                all_addrs.len(),
                redact_url(&target_url)
            );
            srt_set_highbitrate_opts(client_sock);
            srt_log_effective_opts(client_sock, "egress-bonded");
        } else {
            // SAFETY: srt_create_socket creates a new SRT socket handle.
            // The returned handle is closed on all exit paths below
            // (connection failure, cancel, sender exit).
            // Single connection (original path)
            client_sock = unsafe { srt_create_socket() };
            if client_sock < 0 {
                error!("Failed to create socket");
                return Err("failed to create socket".to_string());
            }
            srt_set_highbitrate_opts(client_sock);
            if let Some(crypto) = &url_crypto
                && let Err(error) = apply_srt_crypto_socket(client_sock, crypto)
            {
                unsafe {
                    srt_close(client_sock);
                }
                return Err(error);
            }

            if !streamid.is_empty() {
                let streamid_c = match std::ffi::CString::new(streamid.as_str()) {
                    Ok(c) => c,
                    Err(_) => {
                        error!("Invalid stream ID (contains null byte)");
                        // SAFETY: Valid socket, clean up on invalid streamid.
                        unsafe {
                            srt_close(client_sock);
                        }
                        return Err("stream ID contains null bytes".to_string());
                    }
                };
                // SAFETY: Sets SRTO_STREAMID on a valid socket with a
                // correctly-sized NUL-terminated C string.
                unsafe {
                    srt_setsockopt(
                        client_sock,
                        0,
                        SRTO_STREAMID,
                        streamid_c.as_ptr() as *const c_void,
                        streamid.len() as c_int,
                    );
                }
            }

            let sin = to_sockaddr_in(addr);

            // SAFETY: srt_connect opens a connection to the target address.
            // sin is a correctly-sized sockaddr_in; client_sock is valid.
            let conn_res = unsafe {
                srt_connect(
                    client_sock,
                    &sin,
                    std::mem::size_of::<sockaddr_in>() as c_int,
                )
            };
            if conn_res < 0 {
                error!("Connection failed to {}", redact_url(&target_url));
                // SAFETY: Valid socket, clean up on connection failure.
                unsafe {
                    srt_close(client_sock);
                }
                return Err("connection failed".to_string());
            }

            info!("Connected to {}", redact_url(&target_url));
            srt_log_effective_opts(client_sock, "egress");
        }
        Ok(client_sock)
    })
    .await;

    let client_sock: SRTSOCKET = match connect_result {
        Ok(Ok(sock)) => sock,
        Ok(Err(message)) => {
            egress_error!("connect", message);
            return;
        }
        Err(join_error) => {
            error!("[srt-egress] connect task panicked: {}", join_error);
            egress_error!("connect", "connect task panicked");
            return;
        }
    };

    let shared_muxer = engine
        .get_or_create_ts_muxer_stage(&pipeline_id, &encoding, ring_buffer.clone())
        .await;
    egress_phase!(EgressPhase::Sending);

    let out_queue = Arc::new(crate::media::avio::MemoryQueue::new_with_capacity(
        engine.config.avio_capacity,
    ));
    if !engine
        .register_egress_queue_if_current(&output_id, &registration, out_queue.clone())
        .await
    {
        out_queue.close();
        // SAFETY: Valid socket, clean up when a replacement attempt won the slot.
        unsafe {
            srt_close(client_sock);
        }
        return;
    }

    // Sender thread: reads MPEG-TS from out_queue, sends via SRT
    let out_queue_send = out_queue.clone();
    let oid = output_id.clone();
    let (
        egress_bytes_sent,
        egress_metrics,
        egress_last_progress_ms,
        egress_phase,
        egress_last_error,
        egress_last_error_ms,
        egress_failure_phase,
        egress_quality,
    ) = {
        engine
            .with_active_egress(&output_id, |egress| {
                (
                    Some(egress.bytes_sent.clone()),
                    Some(egress.metrics.clone()),
                    Some(egress.last_progress_ms.clone()),
                    Some(egress.phase.clone()),
                    Some(egress.last_error.clone()),
                    Some(egress.last_error_ms.clone()),
                    Some(egress.failure_phase.clone()),
                    Some(egress.quality.clone()),
                )
            })
            .await
            .unwrap_or((None, None, None, None, None, None, None, None))
    };
    // Sender thread: reads MPEG-TS from out_queue, sends via SRT.
    // Wrapped in catch_unwind so a panic cannot crash the process (AGENTS.md).
    // Acquire a semaphore permit to cap concurrent SRT sender threads at 512.
    let permit = match try_acquire_srt_sender_permit(engine.sender_semaphore_handle()) {
        Ok(p) => p,
        Err(_) => {
            error!(
                "[srt-egress] Sender thread limit reached — rejecting egress {}",
                output_id
            );
            egress_error!("capacity", "SRT sender thread limit reached");
            // SAFETY: Valid socket, clean up on capacity rejection.
            unsafe {
                srt_close(client_sock);
            }
            return;
        }
    };
    let cancel_token_c = cancel_token.clone();
    let egress_sender_handle = std::thread::spawn(move || {
        let _permit = permit; // dropped when thread exits → releases semaphore slot
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut buf = vec![0u8; 1316];
            let progress_sample_interval = Duration::from_millis(250);
            let mut last_progress_sample = Instant::now() - progress_sample_interval;
            let quality_sample_interval = Duration::from_secs(1);
            let mut last_quality_sample = Instant::now() - quality_sample_interval;
            let mut previous_sender_stats: Option<SrtSenderCounterSnapshot> = None;
            loop {
                let n = out_queue_send.read(&mut buf);
                if n == 0 {
                    break;
                }
                // SAFETY: srt_send transmits data over a valid connected
                // SRT socket. buf is correctly sized; n ≤ buf.len().
                let sent = unsafe { srt_send(client_sock, buf.as_ptr(), n as c_int) };
                if sent < 0 {
                    // SAFETY: srt_getlasterror_str returns a thread-local
                    // static string for error diagnostics.
                    let err_str = unsafe { std::ffi::CStr::from_ptr(srt_getlasterror_str()) }
                        .to_string_lossy();
                    error!("srt_send failed for {}: {}", oid, err_str);
                    if let Some(ref phase) = egress_phase {
                        *phase.lock().unwrap_or_else(|e| e.into_inner()) = EgressPhase::Failed;
                    }
                    if let Some(ref failure_phase) = egress_failure_phase {
                        *failure_phase.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some("send".to_string());
                    }
                    if let Some(ref last_error) = egress_last_error {
                        *last_error.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(format!("srt_send failed: {}", err_str));
                    }
                    if let Some(ref last_error_ms) = egress_last_error_ms {
                        last_error_ms.store(
                            chrono::Utc::now().timestamp_millis().max(0) as u64,
                            Ordering::Relaxed,
                        );
                    }
                    cancel_token_c.cancel();
                    break;
                }
                if let Some(ref counter) = egress_bytes_sent {
                    counter.fetch_add(sent as u64, Ordering::Relaxed);
                }
                if let Some(ref m) = egress_metrics {
                    m.record_out(sent as u64);
                }
                if last_progress_sample.elapsed() >= progress_sample_interval {
                    if let Some(ref progress) = egress_last_progress_ms {
                        progress.store(
                            chrono::Utc::now().timestamp_millis().max(0) as u64,
                            Ordering::Relaxed,
                        );
                    }
                    last_progress_sample = Instant::now();
                }
                if last_quality_sample.elapsed() >= quality_sample_interval {
                    let mut stats: SrtTraceBStats = unsafe { std::mem::zeroed() };
                    let sampled_at = Instant::now();
                    let group_summary = use_bonding
                        .then(|| srt_group_summary(client_sock))
                        .flatten();
                    let mut quality = if unsafe { srt_bistats(client_sock, &mut stats, 0, 1) } >= 0
                    {
                        let (quality, snapshot) = srt_sender_quality_from_stats(
                            &stats,
                            previous_sender_stats,
                            sampled_at,
                        );
                        previous_sender_stats = Some(snapshot);
                        quality
                    } else {
                        PublisherQuality::default()
                    };
                    add_srt_group_quality(&mut quality, use_bonding, group_summary);
                    if let Some(ref quality_slot) = egress_quality {
                        *quality_slot.lock().unwrap_or_else(|e| e.into_inner()) = quality;
                    }
                    last_quality_sample = sampled_at;
                }
            }
        }));
        if result.is_err() {
            error!("Sender thread panicked for {}", oid);
        } else {
            info!("Sender thread finished for {}", oid);
        }
        // SAFETY: client_sock was created/connected in start_srt_egress
        // and passed to this sender thread. Closed exactly once here
        // after the sender loop exits.
        unsafe {
            srt_close(client_sock);
        }
    });
    engine.register_os_thread(egress_sender_handle);

    let preroll_packets = startup_policy::srt_egress_keyframe_preroll_packets(&encoding);
    let mut reader = if preroll_packets == 0 {
        TsChunkReader::new(format!("srt_egress:{}", output_id), &shared_muxer)
    } else {
        // 1080p SRT egress needs a short pre-keyframe replay window under
        // mixed HEVC load so late readers inherit enough mux context to avoid
        // the selected-audio sync gaps seen at the strict keyframe edge.
        TsChunkReader::new_with_keyframe_preroll(
            format!("srt_egress:{}", output_id),
            &shared_muxer,
            preroll_packets,
        )
    };
    // Accumulation buffer: collect all muxed TS bytes for a burst, then
    // write them in a single out_queue.write() call (one lock acquisition
    // per burst instead of one per packet).
    let mut ts_batch: Vec<u8> = Vec::with_capacity(MEDIA_TS_BATCH_TARGET_BYTES);
    let mut packets = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => break,
            wake = reader.wait_for_data_or_cancelled() => {
                packets.clear();
                if reader.pull_burst(&mut packets, MEDIA_PULL_BURST_PACKETS).is_ok() {
                    for pkt in &packets {
                        if !pkt.payload.is_empty() {
                            ts_batch.extend_from_slice(&pkt.payload);
                        }
                    }
                    // One lock acquisition for the whole burst.
                    if !ts_batch.is_empty() {
                        out_queue.write(&ts_batch).await;
                        ts_batch.clear();
                    }
                }
                if matches!(wake, crate::media::ts_chunk_ring::TsChunkWaitResult::Cancelled) {
                    break;
                }
            }
        }
    }

    out_queue.close();
    engine
        .remove_egress_queue_if_current(&output_id, &registration)
        .await;
}

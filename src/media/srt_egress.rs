use super::srt_crypto::{apply_srt_crypto_socket, srt_crypto_from_url};
use super::srt_egress_connect::{SrtSingleEgressConnectConfig, connect_single_srt_egress_socket};
use super::srt_url::parse_srt_egress_url;
use super::*;
use super::{
    apply_srt_egress_stream_id, claim_srt_egress_muxer_port,
    resolve_srt_egress_host as resolve_host, to_libc_sockaddr,
};
use crate::secret_display::redact_url;

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
    let srt_egress_muxer_port = engine.srt_egress_muxer_port_handle();
    let reuse_local_srt_egress_port = engine.config.srt_egress_reuse_local_port;
    let srt_connect_timeout_ms = engine.config.srt_connect_timeout_ms;
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

            // Passphrase/PBKEYLEN/ENFORCEDENCRYPTION and StreamID are
            // group-wide settings in libsrt bonding: they must be applied to
            // the group socket itself via srt_setsockopt, not smuggled into
            // a per-member SRT_SOCKOPT_CONFIG. libsrt's per-member config
            // object rejects both option families outright (see
            // SRT_SocketOptionObject::add in socketconfig.cpp, which has no
            // case for SRTO_PASSPHRASE, SRTO_PBKEYLEN, or SRTO_STREAMID and
            // falls through to `return false`), so applying them there
            // always failed the connect attempt with a misleading "Success
            // (0)" error (srt_config_add's failure path does not populate
            // the thread-local last-error state that check_srt_option_result
            // reads).
            if let Some(crypto) = &url_crypto
                && let Err(error) = apply_srt_crypto_socket(client_sock, crypto)
            {
                unsafe {
                    srt_close(client_sock);
                }
                return Err(error);
            }

            if !streamid.is_empty() {
                apply_srt_egress_stream_id(client_sock, &streamid)
                    .inspect_err(|_| unsafe {
                        srt_close(client_sock);
                    })
                    .map_err(|error| error.to_string())?;
            }

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
                    members.push(member);
                }

                // SAFETY: srt_connect_group opens all member connections.
                // members is a correctly sized Vec of SrtGroupMemberConfig.
                // On failure, client_sock is cleaned up.
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
                    // SAFETY: Clean up group socket on connection failure.
                    unsafe {
                        srt_close(client_sock);
                    }
                    Some(message)
                } else {
                    None
                }
            };
            if let Some(message) = connect_error {
                return Err(message);
            }

            info!(
                "[srt-egress] Bonded connection ({} links) to {}",
                all_addrs.len(),
                redact_url(&target_url)
            );
            srt_set_highbitrate_opts(client_sock);
            srt_log_effective_opts(client_sock, "egress-bonded");
        } else {
            let muxer_port_claim = reuse_local_srt_egress_port
                .then(|| claim_srt_egress_muxer_port(&srt_egress_muxer_port));
            client_sock = match connect_single_srt_egress_socket(SrtSingleEgressConnectConfig {
                peer_addr: addr,
                stream_id: &streamid,
                crypto: url_crypto.as_ref(),
                connect_timeout_ms: srt_connect_timeout_ms,
                muxer_port_claim,
            }) {
                Ok(socket) => socket,
                Err(error) => {
                    if error == "connection failed" {
                        error!("Connection failed to {}", redact_url(&target_url));
                    }
                    return Err(error);
                }
            };

            info!("Connected to {}", redact_url(&target_url));
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

    let muxer_stage_key = engine
        .assign_srt_egress_muxer_stage(&pipeline_id, &encoding, &output_id, registration.attempt_id)
        .await;
    let shared_muxer = engine
        .get_or_create_ts_muxer_stage(&pipeline_id, &muxer_stage_key, ring_buffer.clone())
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
        engine
            .release_srt_egress_muxer_stage(
                &pipeline_id,
                &encoding,
                &output_id,
                registration.attempt_id,
            )
            .await;
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

#[cfg(test)]
#[path = "srt_egress_tests.rs"]
mod tests;

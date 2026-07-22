#[tokio::test]
async fn srt_server_shutdown_exits_with_no_connections() {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    let engine = Arc::new(MediaEngine::new());
    let security = Arc::new(IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG));
    let pipeline_store =
        Arc::new(crate::infrastructure::sqlite_ports::SqlitePipelineStore::new(pool.clone()));
    let input_store =
        Arc::new(crate::infrastructure::pipeline_input_store::SqlitePipelineInputStore::new(pool));
    let pipeline_access = Arc::new(
        crate::application::ingest::PipelineStoreIngestAuthenticator::new(
            pipeline_store,
            input_store,
            security.clone(),
        ),
    );
    let _srt_runtime = SrtTestRuntime::lock();
    let server = Arc::new(SrtServer::new(
        pipeline_access,
        engine.clone(),
        security,
        Arc::new(SrtIngestPolicyStore::new(
            SrtGlobalIngestConfig::default(),
            &[],
        )),
    ));

    let handle = tokio::spawn(server.run(0));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if !engine
            .runtime
            .listener_shutdowns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "SRT listener never registered a shutdown hook"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    engine.shutdown_listeners();
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("SRT server did not exit after listener shutdown")
        .expect("SRT server task panicked");
    // SrtServer::new() called srt_startup() but SrtServer::Drop intentionally
    // skips srt_cleanup() (see impl Drop for SrtServer). Balance the refcount
    // here while the test lock still serializes access.
    teardown_srt();
}

#[test]
fn receive_error_classifier_waits_only_for_transient_readiness() {
    assert_eq!(
        classify_srt_receive_error(SRT_EASYNCRCV),
        SrtReceiveErrorAction::WaitForReadiness
    );
    assert_eq!(
        classify_srt_receive_error(SRT_ETIMEOUT),
        SrtReceiveErrorAction::WaitForReadiness
    );
}

#[test]
fn receive_error_classifier_disconnects_closed_publishers() {
    for code in [SRT_ESCLOSED, SRT_ECONNLOST, SRT_ENOCONN, -1, 0] {
        assert_eq!(
            classify_srt_receive_error(code),
            SrtReceiveErrorAction::Disconnect,
            "code={code}"
        );
    }
}

#[test]
fn sysctl_check_does_not_panic() {
    // Smoke test: runs on any Linux, should not panic even if paths don't exist
    check_sysctl_limits();
}

#[test]
fn socket_option_constants_match_srt_header() {
    // Guard against regression: these values are from srt.h SRT_SOCKOPT enum
    assert_eq!(SRTO_SNDSYN, 1);
    assert_eq!(SRTO_RCVSYN, 2);
    assert_eq!(SRTO_FC, 4);
    assert_eq!(SRTO_SNDBUF, 5);
    assert_eq!(SRTO_RCVBUF, 6);
    assert_eq!(SRTO_UDP_SNDBUF, 8);
    assert_eq!(SRTO_UDP_RCVBUF, 9);
    assert_eq!(SRTO_REUSEADDR, 15);
    assert_eq!(SRTO_MAXBW, 16);
    assert_eq!(SRTO_LATENCY, 23);
    assert_eq!(SRTO_LOSSMAXTTL, 42);
    assert_eq!(SRTO_RCVLATENCY, 43);
    assert_eq!(SRTO_PEERLATENCY, 44);
    assert_eq!(SRTO_STREAMID, 46);
    assert_eq!(SRTO_TRANSTYPE, 50);
    assert_eq!(SRTO_GROUPCONNECT, 57);
    assert_eq!(SRTGROUP_MASK, 1 << 30);
    assert_eq!(SRT_EPOLL_IN, 0x1);
    assert_eq!(SRT_EPOLL_OUT, 0x4);
    assert_eq!(SRT_EPOLL_ERR, 0x8);
}

#[test]
fn detects_srt_group_ids() {
    assert!(!is_srt_group(42));
    assert!(is_srt_group(SRTGROUP_MASK | 42));
}

// --- Regression: issue #7 (Round 5) — Semaphore caps concurrent SRT sender threads ---
// Before the fix there was no limit on how many OS threads could be spawned
// for SRT play / egress connections. 1 thread per connection × 1000 connections
// = 1000 threads = 8+ GB virtual address space.
// The semaphore must be exhaustible and must release on drop.
#[test]
fn srt_sender_semaphore_is_bounded() {
    use std::sync::Arc;
    // Create a tiny semaphore (capacity 2) to simulate the cap.
    let sem = Arc::new(tokio::sync::Semaphore::new(2));
    let _p1 = try_acquire_srt_sender_permit(sem.clone()).expect("first permit available");
    let _p2 = try_acquire_srt_sender_permit(sem.clone()).expect("second permit available");
    // Third acquire must fail when semaphore is exhausted.
    assert!(
        try_acquire_srt_sender_permit(sem.clone()).is_err(),
        "semaphore must reject when exhausted"
    );
}

#[test]
fn srt_sender_semaphore_releases_on_drop() {
    use std::sync::Arc;
    let sem = Arc::new(tokio::sync::Semaphore::new(1));
    {
        let _p = try_acquire_srt_sender_permit(sem.clone()).expect("permit available");
        // permit is held — semaphore exhausted.
        assert!(
            try_acquire_srt_sender_permit(sem.clone()).is_err(),
            "should be exhausted"
        );
    }
    // After the permit is dropped, the slot must be returned.
    assert!(
        try_acquire_srt_sender_permit(sem.clone()).is_ok(),
        "semaphore should release permit on drop"
    );
}

// --- Regression: Round 6 #5 — SRT play muxer must not start without video ---
// The probe-wait loop in handle_play requires `video.as_ref()?` before
// breaking — it must not yield metadata when video is None.
// This is the same guard used by start_srt_egress.
#[test]
fn linked_libsrt_exposes_group_connect_when_required() {
    let _srt_runtime = SrtTestRuntime::startup();

    let listener = unsafe { srt_create_socket() };
    assert!(listener >= 0);
    if let Err(error) = enable_srt_group_connect(listener) {
        unsafe {
            srt_close(listener);
        }
        if crate::AppConfig::from_env().require_srt_bonding {
            panic!(
                "RESTREAM_REQUIRE_SRT_BONDING is set, but linked libsrt rejected \
                     SRTO_GROUPCONNECT: {error}. Rebuild libsrt with ENABLE_BONDING=ON."
            );
        }
        warn!(err = %error, "bonding prerequisite unavailable; set RESTREAM_REQUIRE_SRT_BONDING=1 in bonding-enabled CI");
        return;
    }
    unsafe {
        srt_close(listener);
    }
}

#[test]
fn linked_libsrt_accepts_every_supported_pbkeylen_via_socket_option() {
    let _srt_runtime = SrtTestRuntime::startup();

    for pbkeylen in [16, 24, 32] {
        let crypto = srt_crypto_from_url("s3cret-passphrase".to_string(), Some(pbkeylen))
            .expect("non-empty passphrase must yield a crypto config");
        let sock = unsafe { srt_create_socket() };
        assert!(sock >= 0);

        let result = apply_srt_crypto_socket(sock, &crypto);
        unsafe {
            srt_close(sock);
        }
        assert!(
            result.is_ok(),
            "pbkeylen={pbkeylen} should be accepted by libsrt via SRTO_PBKEYLEN: {result:?}"
        );
    }
}

#[test]
fn linked_libsrt_rejects_out_of_range_pbkeylen_via_socket_option() {
    let _srt_runtime = SrtTestRuntime::startup();

    let crypto = srt_crypto_from_url("s3cret-passphrase".to_string(), Some(999))
        .expect("non-empty passphrase must yield a crypto config");
    let sock = unsafe { srt_create_socket() };
    assert!(sock >= 0);

    let result = apply_srt_crypto_socket(sock, &crypto);
    unsafe {
        srt_close(sock);
    }

    let error =
        result.expect_err("libsrt must reject an out-of-range SRTO_PBKEYLEN through the FFI");
    assert!(
        error.contains("SRTO_PBKEYLEN"),
        "expected the FFI error surface to name the rejected option, got: {error}"
    );
}

/// Documents a real libsrt bonding quirk that once caused a production bug:
/// the per-member `SRT_SOCKOPT_CONFIG` object (`srt_create_config` /
/// `srt_config_add`) silently rejects `SRTO_PASSPHRASE` and `SRTO_STREAMID`
/// (see `SRT_SocketOptionObject::add` in libsrt's `socketconfig.cpp`, which
/// has no case for either option and falls through to `return false`), and
/// `srt_config_add`'s failure path never calls `CUDT::APIError`, so
/// `check_srt_option_result` misreports the failure as "Success (0)". Bonded
/// SRT egress applies these as group-wide socket options instead (see
/// `linked_libsrt_group_socket_accepts_crypto_via_setsockopt` and
/// `linked_libsrt_group_socket_accepts_streamid_via_setsockopt` below, and
/// the production call sites in `srt_egress.rs`). If a future libsrt version
/// starts accepting these through the per-member config, this test's
/// failure is the signal that the workaround can be revisited.
#[test]
fn linked_libsrt_member_config_rejects_passphrase_and_streamid() {
    let _srt_runtime = SrtTestRuntime::startup();

    let config = unsafe { srt_create_config() };
    assert!(!config.is_null());

    let passphrase_c = std::ffi::CString::new("s3cret-passphrase").unwrap();
    let passphrase_result = unsafe {
        srt_config_add(
            config,
            SRTO_PASSPHRASE,
            passphrase_c.as_ptr() as *const c_void,
            17,
        )
    };
    let streamid_c = std::ffi::CString::new("probe").unwrap();
    let streamid_result = unsafe {
        srt_config_add(
            config,
            SRTO_STREAMID,
            streamid_c.as_ptr() as *const c_void,
            5,
        )
    };
    unsafe {
        srt_delete_config(config);
    }

    assert_eq!(
        passphrase_result, -1,
        "libsrt's per-member config unexpectedly accepted SRTO_PASSPHRASE; \
         the srt_egress.rs group-socket workaround may no longer be needed"
    );
    assert_eq!(
        streamid_result, -1,
        "libsrt's per-member config unexpectedly accepted SRTO_STREAMID; \
         the srt_egress.rs group-socket workaround may no longer be needed"
    );
}

#[test]
fn linked_libsrt_group_socket_accepts_crypto_via_setsockopt() {
    let _srt_runtime = SrtTestRuntime::startup();
    let group = unsafe { srt_create_group(SRT_GTYPE_BACKUP) };
    assert!(group >= 0, "group={group}");

    let crypto = srt_crypto_from_url("s3cret-passphrase".to_string(), Some(16))
        .expect("non-empty passphrase must yield a crypto config");
    let result = apply_srt_crypto_socket(group, &crypto);
    unsafe {
        srt_close(group);
    }
    assert!(
        result.is_ok(),
        "bonded group sockets must accept crypto via SRTO_PASSPHRASE/SRTO_PBKEYLEN \
         setsockopt: {result:?}"
    );
}

#[test]
fn linked_libsrt_group_socket_accepts_streamid_via_setsockopt() {
    let _srt_runtime = SrtTestRuntime::startup();
    let group = unsafe { srt_create_group(SRT_GTYPE_BACKUP) };
    assert!(group >= 0, "group={group}");
    let streamid_c = std::ffi::CString::new("probe").unwrap();
    let result = unsafe {
        check_srt_option_result(
            "SRTO_STREAMID",
            srt_setsockopt(
                group,
                0,
                SRTO_STREAMID,
                streamid_c.as_ptr() as *const c_void,
                5,
            ),
        )
    };
    unsafe {
        srt_close(group);
    }
    assert!(
        result.is_ok(),
        "bonded group sockets must accept StreamID via setsockopt: {result:?}"
    );
}

#[tokio::test]
async fn start_srt_egress_handles_invalid_streamid_without_panic() {
    let ring_buffer = Arc::new(RingBuffer::new(16));
    let engine = Arc::new(crate::media::engine::MediaEngine::new());
    let registration = engine
        .register_egress_attempt(
            "out-id",
            "pipe-id",
            "srt://127.0.0.1:12345?streamid=publish:mykey",
            None,
        )
        .await;
    start_srt_egress(
        "out-id".to_string(),
        "pipe-id".to_string(),
        "source".to_string(),
        "srt://127.0.0.1:12345?streamid=publish:\x00mykey".to_string(),
        ring_buffer,
        engine,
        registration,
    )
    .await;
}

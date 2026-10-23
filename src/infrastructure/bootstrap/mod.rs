//! Process bootstrap for the restream server, owning shared
//! application/runtime orchestration used by binary entrypoints.
//!
//! # Threading Model
//!
//! ```text
//! ┌─────────────────── tokio runtime (multi-threaded) ───────────────────┐
//! │  Axum web server          HTTP handlers, SSE streams                │
//! │  RTMP listener            per-connection async tasks                │
//! │  SRT accept loop          per-connection async tasks                │
//! │  Reconciler (1s tick)     output lifecycle + recording auto-start   │
//! │  Egress tasks             ring buffer reader → network send         │
//! │  HLS segmenter            TsMuxer → segment accumulator → HlsStore │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! ┌─────────────── std::thread (OS threads, catch_unwind) ──────────────┐
//! │  FFmpeg demuxer           RTMP/SRT ingest → RingBuffer push         │
//! │  TS recording writer      MemoryQueue → .ts recording file          │
//! │  FFmpeg child pipes       ring → stdin; stdout → ring               │
//! └─────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! Tokio tasks handle all network I/O and coordination. CPU-bound FFmpeg work
//! runs on dedicated OS threads to avoid starving the async runtime. All
//! `std::thread::spawn` calls are wrapped in `catch_unwind` so an FFmpeg panic
//! (e.g., from a corrupt stream) logs an error instead of taking down the process.

mod auth;
mod egress;
mod events;
mod hls_segmenter;
mod layout;
mod listener_supervisor;
#[forbid(unsafe_code)]
mod reconcile;
mod runtime;
mod shutdown;

pub use auth::initialize_auth_for_test;

use std::sync::Arc;

use tracing::{info, warn};

use crate::config::AppConfig;
use crate::db;
use crate::media::engine::MediaEngine;

pub async fn run_app(config: Arc<AppConfig>) {
    layout::set_rlimit(config.tuning.nofile_limit);
    layout::ensure_runtime_layout(&config).expect("Failed to create Restream runtime layout");

    let db_url = format!("sqlite:{}?mode=rwc", config.db_path);
    let pool = db::create_pool(&db_url)
        .await
        .expect("Failed to connect to SQLite database");
    db::setup_database_schema(&pool)
        .await
        .expect("Failed to set up SQLite schema");

    let logging_handles = crate::logging::init(pool.clone(), &config.log_dir, config.no_color);
    info!(
        event_class = "lifecycle",
        event_type = "restream.config.effective",
        http_port = config.ports.http,
        rtmp_port = config.ports.rtmp,
        srt_port = config.ports.srt,
        db_path = %config.db_path,
        media_dir = %config.media_dir,
        log_dir = %config.log_dir,
        external_ffmpeg_permits = config.external_ffmpeg_permits,
        ffmpeg_threads = ?config.ffmpeg_threads,
        recording_threads = ?config.recording_threads,
        summary = %config.effective_summary(),
        "effective startup configuration",
    );
    crate::host_tuning::warn_unmet();
    crate::ffmpeg_binary::ffmpeg_bin_path();
    match crate::malloc_tuning::applied() {
        Some(setting @ crate::malloc_tuning::ArenaSetting::Failed { .. })
        | Some(setting @ crate::malloc_tuning::ArenaSetting::InvalidOverride { .. }) => warn!(
            event_class = "lifecycle",
            event_type = "restream.malloc.arenas",
            setting = %setting,
            "malloc arena policy not applied as requested",
        ),
        Some(setting) => info!(
            event_class = "lifecycle",
            event_type = "restream.malloc.arenas",
            setting = %setting,
            "malloc arena policy",
        ),
        None => {}
    }
    for warning in config
        .egress_fabric
        .validate(crate::capacity::HostCapacity::get().cpus)
    {
        warn!(
            event_class = "lifecycle",
            event_type = "restream.config.warning",
            "{warning}",
        );
    }

    db::reset_running_jobs(&pool, &chrono::Utc::now().to_rfc3339())
        .await
        .expect("Failed to reset stale running jobs");

    let meta_store = crate::infrastructure::sqlite_ports::SqliteMetaStore::new(pool.clone());
    let sec_config =
        crate::application::ingest_security::load_ingest_security_config(&meta_store).await;
    let security = Arc::new(crate::media::security::IngestSecurityService::new(
        sec_config,
    ));
    let pipeline_store =
        crate::infrastructure::sqlite_ports::SqlitePipelineStore::new(pool.clone());
    let pipeline_catalog: Arc<dyn crate::application::ports::PipelineStore> =
        Arc::new(pipeline_store.clone());
    let pipeline_input_store =
        crate::infrastructure::pipeline_input_store::SqlitePipelineInputStore::new(pool.clone());
    let srt_ingest_policy_store = Arc::new(
        match crate::application::srt_ingest::load_policy_store(
            &meta_store,
            &pipeline_store,
            &pipeline_input_store,
            config.srt_passphrase.clone(),
            config.srt_pbkeylen,
        )
        .await
        {
            Ok(store) => store,
            Err(error) => {
                warn!(
                    err = %error,
                    "pipeline catalog error initializing SRT ingest policy store"
                );
                crate::media::srt::SrtIngestPolicyStore::new(
                    crate::application::srt_ingest::load_global_srt_ingest_config(
                        &meta_store,
                        config.srt_passphrase.clone(),
                        config.srt_pbkeylen,
                    )
                    .await,
                    &[],
                )
            }
        },
    );

    let sessions = Arc::new(tokio::sync::RwLock::new(std::collections::HashSet::new()));
    let bootstrap_password_path =
        std::path::Path::new(&config.db_path).with_file_name("restream-initial-admin-password.txt");
    let auth_service =
        crate::infrastructure::service_wiring::SqliteServiceFactory::new(&pool).auth_service();
    auth::initialize_auth_with_bootstrap_file(
        &auth_service,
        &sessions,
        Some(&bootstrap_password_path),
        config.initial_admin_password.as_deref(),
    )
    .await;
    crate::application::transcode_profiles::load_transcode_profiles(&meta_store).await;

    let engine = Arc::new(MediaEngine::new_with_config(config.clone()));
    let pipeline_lookup: Arc<dyn crate::application::ports::PipelineStore> =
        Arc::new(pipeline_store);
    let ingest_authenticator = Arc::new(
        crate::application::ingest::PipelineStoreIngestAuthenticator::new(
            pipeline_lookup,
            Arc::new(pipeline_input_store.clone()),
            security.clone(),
        ),
    );

    // Events emitted before the consumer below starts wait in the channel.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    engine.set_event_sink(event_tx);

    let sessions_for_reconciler = sessions.clone();
    let recording_metadata =
        crate::infrastructure::recording_metadata::spawn_recording_metadata_reporter(pool.clone());
    let services =
        crate::infrastructure::service_wiring::SqliteServiceFactory::new(&pool).compose();
    let state = Arc::new(crate::api::AppState::new(
        services,
        pool.clone(),
        security.clone(),
        srt_ingest_policy_store.clone(),
        sessions,
        engine.clone(),
        logging_handles.broadcast_tx.clone(),
        crate::api::state::AppStateRuntimeConfig::from(config.as_ref()),
    ));

    let requests = state.reconcile.clone();
    let runtime_state = state.runtime_state.clone();
    let disconnect_grace =
        std::time::Duration::from_millis(config.tuning.ingest_disconnect_grace_ms);
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            crate::application::topology::request_pass_for_event(
                &requests,
                &event.kind,
                disconnect_grace,
            );
            runtime_state.mark_changed();
            events::persist_runtime_event(event);
        }
    });

    // Created before the reconciler because the reconciler converges HLS
    // pull inputs through it; the router gets the same registry from the
    // runtime launch below.
    let hls_ingest = crate::media::hls_ingest::HlsIngest::new(
        engine.clone(),
        ingest_authenticator.clone(),
        security.clone(),
        crate::media::hls_ingest::HlsIngestLimits::default(),
    );

    let reconciler = reconcile::Reconciler::new(
        config.clone(),
        pool.clone(),
        engine.clone(),
        pipeline_catalog,
        meta_store,
        sessions_for_reconciler,
        recording_metadata,
        state.reconcile.clone(),
        state.runtime_state.clone(),
        hls_ingest.clone(),
    );
    let projector_stop = tokio_util::sync::CancellationToken::new();
    tokio::spawn(crate::application::runtime_state::hub::run_projector(
        state.runtime_state.clone(),
        pool.clone(),
        engine.clone(),
        std::time::Duration::from_millis(config.tuning.reconciler_interval_ms),
        projector_stop.clone(),
    ));
    tokio::spawn(crate::application::fleet_history::run_fleet_sampler(
        state.fleet_history.clone(),
        state.runtime_state.clone(),
        pool.clone(),
        engine.clone(),
        crate::application::fleet_history::FLEET_SAMPLE_INTERVAL,
        projector_stop.clone(),
    ));
    tokio::spawn(crate::application::flight_recorder::run_flight_recorder(
        state.flight_recorder.clone(),
        state.runtime_state.clone(),
        pool.clone(),
        engine.clone(),
        crate::application::flight_recorder::FLIGHT_SAMPLE_INTERVAL,
        (config.flight_history_hours > 0)
            .then(|| std::time::Duration::from_secs(config.flight_history_hours * 3600)),
        projector_stop.clone(),
    ));
    tokio::spawn(crate::api::alerts::run_alert_notifier(
        state.clone(),
        crate::api::alerts::NOTIFY_INTERVAL,
        projector_stop.clone(),
    ));
    tokio::spawn(crate::api::schedules::run_scheduler(
        state.clone(),
        crate::api::schedules::SCHEDULER_TICK,
        projector_stop.clone(),
    ));
    let mut runtime = runtime::RuntimeTasks::launch(runtime::RuntimeLaunch {
        config,
        state,
        engine: engine.clone(),
        security,
        pipeline_access: ingest_authenticator,
        srt_ingest_policy_store,
        hls_ingest,
        hls_inputs: Arc::new(pipeline_input_store),
    })
    .await;
    let shutdown = shutdown::spawn_signal_watcher();

    reconciler.run(&mut runtime, &shutdown).await;
    projector_stop.cancel();

    let (http, rtmp, srt) = runtime.into_handles();
    shutdown::cleanup(&engine, &pool, http, rtmp, srt).await;
}

#[cfg(test)]
mod tests;

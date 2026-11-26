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
//! │  Reconciler (passes)      output lifecycle, input convergence       │
//! │  Housekeeping (1s tick)   recordings, stream checks, pruning        │
//! │  Listener supervisor      RTMP/SRT restart, dashboard exit          │
//! │  Egress tasks             ring buffer reader → network send         │
//! │  HLS segmenter            TsMuxer → segment accumulator → HlsStore │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! ┌─────────────── std::thread (OS threads, catch_unwind) ──────────────┐
//! │  TS recording writer      MemoryQueue → .ts recording file          │
//! │  FFmpeg child pipes       ring → stdin; stdout → ring               │
//! │  Stream check readers     FFmpeg child → thumbnails, f32 audio      │
//! │  Log writers              bounded queue → stdout, stderr, log file  │
//! └─────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! Tokio tasks handle all network I/O and coordination. Blocking pipe and file
//! work runs on dedicated OS threads to avoid starving the async runtime;
//! codecs run in FFmpeg child processes. All `std::thread::spawn` calls are
//! wrapped in `catch_unwind` so a panic (e.g. a demuxer bug on a corrupt
//! stream) logs an error instead of taking down the process.

mod auth;
mod egress;
mod events;
mod hls_segmenter;
mod housekeeping;
mod layout;
mod listener_supervisor;
#[forbid(unsafe_code)]
mod reconcile;
mod runtime;
mod shutdown;
pub(crate) mod supervised_task;
#[cfg(test)]
pub(crate) mod test_support;

pub use auth::initialize_auth_for_test;

use std::sync::Arc;

use tracing::{info, warn};

use crate::config::AppConfig;
use crate::db;
use restream_engine::engine::MediaEngine;

/// How long shutdown waits for a housekeeping tick in progress.
const HOUSEKEEPING_STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Runs the server until shutdown. Startup failures (runtime layout,
/// database, dashboard bind, initial password) return an error for `main`
/// to report and exit with, rather than panicking; one that happens once
/// logging is up is also logged, before the log sinks flush.
pub async fn run_app(config: Arc<AppConfig>) -> Result<(), String> {
    layout::set_rlimit(config.tuning.nofile_limit);
    layout::ensure_runtime_layout(&config)
        .map_err(|error| format!("failed to create the Restream runtime layout: {error}"))?;

    let db_url = format!("sqlite:{}?mode=rwc", config.db_path);
    let pool = db::create_pool(&db_url)
        .await
        .map_err(|error| format!("failed to connect to the SQLite database: {error}"))?;
    db::setup_database_schema(&pool)
        .await
        .map_err(|error| format!("failed to set up the SQLite schema: {error}"))?;
    // The shared control loops' own connections, each call deadline-bound.
    let lifecycle_pool = db::create_lifecycle_pool(&db_url)
        .await
        .map_err(|error| format!("failed to connect to the SQLite database: {error}"))?;
    let lifecycle = db::DeadlineDb::new(lifecycle_pool.clone(), db::SHARED_LOOP_DEADLINE);

    let logging_handles = crate::logging::init(
        pool.clone(),
        &config.log_dir,
        config.no_color,
        config.rust_backtrace.as_deref(),
    )?;
    supervised_task::spawn_supervised("app_log_drain", {
        let drain = logging_handles.history_drain.clone();
        move || drain.clone().run()
    });
    let result = serve(
        config,
        pool,
        lifecycle_pool,
        lifecycle,
        logging_handles.broadcast_tx.clone(),
    )
    .await;
    if let Err(error) = &result {
        tracing::error!(
            event_class = "lifecycle",
            event_type = "restream.startup.failed",
            %error,
            "startup failed"
        );
    }
    // `logging_handles` drops here, flushing the line above.
    result
}

/// Everything `run_app` does once logging is up.
async fn serve(
    config: Arc<AppConfig>,
    pool: sqlx::SqlitePool,
    lifecycle_pool: sqlx::SqlitePool,
    lifecycle: db::DeadlineDb,
    log_broadcast: tokio::sync::broadcast::Sender<crate::logging::LogBroadcast>,
) -> Result<(), String> {
    log_startup_settings(&config);

    db::reset_running_jobs(&pool, &restream_domain::timestamp::now_rfc3339())
        .await
        .map_err(|error| format!("failed to reset stale running jobs: {error}"))?;

    let api_db = db::DeadlineDb::api(pool.clone());
    let sec_config =
        crate::application::ingest_security::load_ingest_security_config(&api_db).await;
    let security = Arc::new(restream_engine::security::IngestSecurityService::new(
        sec_config,
    ));
    let srt_ingest_policy_store = Arc::new(load_srt_ingest_policy_store(&api_db, &config).await);

    let sessions = Arc::new(tokio::sync::RwLock::new(std::collections::HashSet::new()));
    let bootstrap_password_path =
        std::path::Path::new(&config.db_path).with_file_name("restream-initial-admin-password.txt");
    let auth_service = crate::application::services::AuthService::new(api_db.clone());
    let control_plane =
        crate::infrastructure::service_wiring::ControlPlane::new(api_db.clone(), lifecycle.clone());
    auth::initialize_auth_with_bootstrap_file(
        &auth_service,
        &sessions,
        Some(&bootstrap_password_path),
        config.initial_admin_password.as_deref(),
    )
    .await?;
    crate::application::transcode_profiles::load_transcode_profiles(&api_db).await;

    let engine = Arc::new(MediaEngine::new_with_config(Arc::new(
        config.engine.clone(),
    )));
    // Dropped ingest leases release on the main runtime, whichever runtime
    // (a listener thread's, a supervisor's) the attempt registered on.
    engine.start_ingest_release_drain();
    // Ingest admission reads this index, never SQLite; it is filled before
    // any listener (or the HLS ingest registry) can authenticate a connect.
    let catalog_auth = control_plane.catalog_auth.clone();
    catalog_auth
        .load()
        .await
        .map_err(|error| format!("failed to load the ingest stream-key catalog: {error}"))?;
    let ingest_authenticator = Arc::new(
        crate::application::catalog_auth::CatalogIngestAuthenticator::new(
            catalog_auth.clone(),
            security.clone(),
        ),
    );

    // Events emitted before the consumer below starts wait in the channel.
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    engine.set_event_sink(event_tx);

    let sessions_for_reconciler = sessions.clone();
    // The one recording catalog writer: the media library's and
    // housekeeping's recordings report to it.
    let recording_metadata = control_plane.recording_metadata.clone();
    let services = control_plane.services;
    let state = Arc::new(crate::api::AppState::new(
        services,
        security.clone(),
        srt_ingest_policy_store.clone(),
        sessions,
        engine.clone(),
        log_broadcast,
        crate::api::state::AppStateRuntimeConfig::from(config.clone()),
    ));

    spawn_runtime_event_consumer(event_rx, &state, &config);

    // Created before the reconciler because the reconciler converges HLS
    // pull inputs through it; the router gets the same registry from the
    // runtime launch below.
    let hls_ingest = restream_engine::hls_ingest::HlsIngest::new(
        engine.clone(),
        ingest_authenticator.clone(),
        security.clone(),
        restream_engine::hls_ingest::HlsIngestLimits::default(),
    );

    let reconciler = reconcile::Reconciler::new(
        config.clone(),
        lifecycle.clone(),
        engine.clone(),
        state.reconcile_handle(),
        state.runtime_state_handle(),
        hls_ingest.clone(),
    );
    let projector_stop = tokio_util::sync::CancellationToken::new();
    let housekeeping_task = supervised_task::spawn_supervised("housekeeping", {
        let housekeeping = Arc::new(housekeeping::Housekeeping::new(
            config.clone(),
            lifecycle.clone(),
            engine.clone(),
            sessions_for_reconciler,
            recording_metadata,
        ));
        let stop = projector_stop.clone();
        move || housekeeping.clone().run(stop.clone())
    });
    // Bounds how stale the admission index can get from a change no write
    // reported; `shutdown` below ends it and any reload retry.
    supervised_task::spawn_supervised("catalog_auth_reload", {
        let catalog = catalog_auth.clone();
        move || {
            catalog
                .clone()
                .reload_periodically(crate::application::catalog_auth::PERIODIC_RELOAD_INTERVAL)
        }
    });
    spawn_state_loops(&state, &lifecycle, &config, &projector_stop);
    let runtime::RuntimeTasks {
        http_stop,
        listeners,
    } = runtime::RuntimeTasks::launch(runtime::RuntimeLaunch {
        config,
        state,
        engine: engine.clone(),
        security,
        pipeline_access: ingest_authenticator,
        srt_ingest_policy_store,
        hls_ingest,
        hls_catalog: catalog_auth.clone(),
    })
    .await?;
    let shutdown = shutdown::spawn_signal_watcher();

    let listeners = run_control_loops(&reconciler, listeners, &shutdown).await;
    projector_stop.cancel();
    catalog_auth.shutdown();
    stop_housekeeping(housekeeping_task).await;

    shutdown::cleanup(&engine, &pool, listeners, http_stop).await;
    lifecycle_pool.close().await;
    Ok(())
}

/// Logs the effective configuration and the host settings it depends on.
fn log_startup_settings(config: &AppConfig) {
    info!(
        event_class = "lifecycle",
        event_type = "restream.config.effective",
        http_port = config.ports.http,
        rtmp_port = config.ports.rtmp,
        srt_port = config.ports.srt,
        db_path = %config.db_path,
        media_dir = %config.media_dir,
        log_dir = %config.log_dir,
        external_ffmpeg_permits = config.engine.external_ffmpeg_permits,
        ffmpeg_threads = ?config.ffmpeg_threads,
        recording_threads = ?config.engine.recording_threads,
        summary = %config.effective_summary(),
        "effective startup configuration",
    );
    crate::host::host_tuning::warn_unmet();
    restream_engine::ffmpeg_binary::ffmpeg_bin_path();
    tokio::spawn(restream_engine::ffmpeg_capabilities::check_at_startup());
    log_malloc_arena_policy();
    warn_egress_fabric_config(config);
}

fn log_malloc_arena_policy() {
    match crate::host::malloc_tuning::applied() {
        Some(setting @ crate::host::malloc_tuning::ArenaSetting::Failed { .. })
        | Some(setting @ crate::host::malloc_tuning::ArenaSetting::InvalidOverride { .. }) => {
            warn!(
                event_class = "lifecycle",
                event_type = "restream.malloc.arenas",
                setting = %setting,
                "malloc arena policy not applied as requested",
            )
        }
        Some(setting) => info!(
            event_class = "lifecycle",
            event_type = "restream.malloc.arenas",
            setting = %setting,
            "malloc arena policy",
        ),
        None => {}
    }
}

fn warn_egress_fabric_config(config: &AppConfig) {
    for warning in config
        .engine
        .egress_fabric
        .validate(restream_engine::host_capacity::HostCapacity::get().cpus)
    {
        warn!(
            event_class = "lifecycle",
            event_type = "restream.config.warning",
            "{warning}",
        );
    }
}

/// Loads the per-pipeline SRT ingest policies; a catalog error falls back
/// to the global policy alone.
async fn load_srt_ingest_policy_store(
    api_db: &db::DeadlineDb,
    config: &AppConfig,
) -> restream_engine::srt::SrtIngestPolicyStore {
    match crate::application::srt_ingest::load_policy_store(
        api_db,
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
            restream_engine::srt::SrtIngestPolicyStore::new(
                crate::application::srt_ingest::load_global_srt_ingest_config(
                    api_db,
                    config.srt_passphrase.clone(),
                    config.srt_pbkeylen,
                )
                .await,
                &[],
            )
        }
    }
}

/// Starts the one runtime-event consumer over the engine's event channel.
fn spawn_runtime_event_consumer(
    event_rx: tokio::sync::mpsc::UnboundedReceiver<restream_engine::events::Event>,
    state: &crate::api::AppState,
    config: &AppConfig,
) {
    let requests = state.reconcile_handle();
    let runtime_state = state.runtime_state_handle();
    let disconnect_grace =
        std::time::Duration::from_millis(config.tuning.ingest_disconnect_grace_ms);
    // Shared so a restart after a panic resumes the same channel; a tokio
    // Mutex is not poisoned by the panicking holder.
    let event_rx = Arc::new(tokio::sync::Mutex::new(event_rx));
    supervised_task::spawn_supervised("runtime_event_consumer", move || {
        consume_runtime_events(
            event_rx.clone(),
            requests.clone(),
            runtime_state.clone(),
            disconnect_grace,
        )
    });
}

/// Starts the loops that sample, project and act on runtime state; each
/// ends when `stop` is cancelled.
fn spawn_state_loops(
    state: &Arc<crate::api::AppState>,
    lifecycle: &db::DeadlineDb,
    config: &Arc<AppConfig>,
    stop: &tokio_util::sync::CancellationToken,
) {
    supervised_task::spawn_supervised("runtime_state_projector", {
        let (hub, db, engine, stop) = (
            state.runtime_state_handle(),
            state.db.clone(),
            state.engine.clone(),
            stop.clone(),
        );
        let interval = std::time::Duration::from_millis(config.tuning.reconciler_interval_ms);
        move || {
            crate::application::runtime_state::hub::run_projector(
                hub.clone(),
                db.clone(),
                engine.clone(),
                interval,
                stop.clone(),
            )
        }
    });
    supervised_task::spawn_supervised("fleet_sampler", {
        let (history, hub, db, engine, config, stop) = (
            state.fleet_history_sampler_handle(),
            state.runtime_state_handle(),
            state.db.clone(),
            state.engine.clone(),
            config.clone(),
            stop.clone(),
        );
        move || {
            crate::application::fleet_history::run_fleet_sampler(
                history.clone(),
                hub.clone(),
                db.clone(),
                engine.clone(),
                config.clone(),
                crate::application::fleet_history::FLEET_SAMPLE_INTERVAL,
                stop.clone(),
            )
        }
    });
    supervised_task::spawn_supervised("flight_recorder", {
        let (recorder, hub, db, engine, stop) = (
            state.flight_recorder_sampler_handle(),
            state.runtime_state_handle(),
            state.db.clone(),
            state.engine.clone(),
            stop.clone(),
        );
        let retention = (config.flight_history_hours > 0)
            .then(|| std::time::Duration::from_secs(config.flight_history_hours * 3600));
        move || {
            crate::application::flight_recorder::run_flight_recorder(
                recorder.clone(),
                hub.clone(),
                db.clone(),
                engine.clone(),
                crate::application::flight_recorder::FLIGHT_SAMPLE_INTERVAL,
                retention,
                stop.clone(),
            )
        }
    });
    supervised_task::spawn_supervised("alert_notifier", {
        let (state, stop) = (state.clone(), stop.clone());
        move || {
            crate::api::alerts::run_alert_notifier(
                state.clone(),
                crate::api::alerts::NOTIFY_INTERVAL,
                stop.clone(),
            )
        }
    });
    supervised_task::spawn_supervised("scheduler", {
        let (state, db, stop) = (state.clone(), lifecycle.clone(), stop.clone());
        move || {
            crate::api::schedules::run_scheduler(
                state.clone(),
                db.clone(),
                crate::api::schedules::SCHEDULER_TICK,
                crate::api::schedules::ACTION_DEADLINE,
                stop.clone(),
            )
        }
    });
}

/// A tick in progress ends before cleanup tears the runtime down, or is
/// cut short: it must not start a recording after cleanup.
async fn stop_housekeeping(mut task: tokio::task::JoinHandle<()>) {
    if tokio::time::timeout(HOUSEKEEPING_STOP_WAIT, &mut task)
        .await
        .is_err()
    {
        task.abort();
    }
}

/// Runs the reconciler's output passes on this future until shutdown, with
/// listener supervision in a task of its own beside it, so a stalled pass
/// never delays a faulted listener's restart. Returns the listener handles
/// for cleanup.
async fn run_control_loops(
    reconciler: &reconcile::Reconciler,
    listeners: runtime::Listeners,
    shutdown: &tokio_util::sync::CancellationToken,
) -> runtime::ListenerHandles {
    let supervision = listeners.supervise(shutdown.clone());
    reconciler.run(shutdown).await;
    supervision.stop().await
}

/// The one runtime-event consumer: wakes the reconciler, marks the runtime
/// state changed, and persists each event.
#[expect(
    clippy::await_holding_invalid_type,
    reason = "the one consumer holds the receiver for its lifetime; the mutex exists only so a restart after a panic resumes the same channel"
)]
async fn consume_runtime_events(
    event_rx: Arc<
        tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<restream_engine::events::Event>>,
    >,
    requests: Arc<crate::application::topology::ReconcileRequests>,
    runtime_state: Arc<crate::application::runtime_state::hub::StateHub>,
    disconnect_grace: std::time::Duration,
) {
    let mut event_rx = event_rx.lock().await;
    while let Some(event) = event_rx.recv().await {
        crate::application::topology::request_pass_for_event(
            &requests,
            &event.kind,
            disconnect_grace,
        );
        runtime_state.mark_changed();
        events::persist_runtime_event(event);
    }
}

#[cfg(test)]
mod tests;

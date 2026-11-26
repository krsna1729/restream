//! Catalog persistence operations exposed to API handlers.
//!
//! Handlers ask for the records they serve; the database handle and its
//! application query modules stay behind this boundary.

use restream_domain::domain::output_spec::OutputConfig;
use restream_domain::domain::state::DesiredOutputState;
use restream_domain::models::Output;

use super::AppState;
use crate::application::schedules::{Schedule, ScheduleDraft};
use crate::application::services::ServiceResult;
use crate::logging::types::{AppLogFilters, AppLogRow};

impl AppState {
    /// Every output in the catalog.
    pub(crate) async fn outputs(&self) -> ServiceResult<Vec<Output>> {
        crate::application::outputs::list_outputs(&self.db).await
    }

    /// The outputs attached to one pipeline.
    pub(crate) async fn outputs_of(&self, pipeline_id: &str) -> ServiceResult<Vec<Output>> {
        crate::application::outputs::list_for_pipeline(&self.db, pipeline_id).await
    }

    /// One output by pipeline and output ID.
    pub(crate) async fn output_of(&self, pipeline_id: &str, id: &str) -> ServiceResult<Output> {
        crate::application::outputs::get_by_id(&self.db, pipeline_id, id).await
    }

    /// Creates an output in the stopped desired state.
    pub(crate) async fn create_output(
        &self,
        id: &str,
        pipeline_id: &str,
        name: &str,
        url: &str,
        monitoring_url: Option<&str>,
        config: &OutputConfig,
    ) -> ServiceResult<Output> {
        crate::application::outputs::create_output(
            &self.db,
            id,
            pipeline_id,
            name,
            url,
            monitoring_url,
            DesiredOutputState::Stopped,
            config,
        )
        .await
    }

    /// Updates one output's mutable settings.
    pub(crate) async fn update_output(
        &self,
        pipeline_id: &str,
        id: &str,
        name: &str,
        url: &str,
        monitoring_url: Option<&str>,
        config: &OutputConfig,
    ) -> ServiceResult<Output> {
        crate::application::outputs::update_output(
            &self.db,
            pipeline_id,
            id,
            name,
            url,
            monitoring_url,
            config,
        )
        .await
    }

    /// Deletes one output.
    pub(crate) async fn delete_output(&self, pipeline_id: &str, id: &str) -> ServiceResult<bool> {
        crate::application::outputs::delete_output(&self.db, pipeline_id, id).await
    }

    /// Requests the running desired state for one output.
    pub(crate) async fn start_output(&self, pipeline_id: &str, id: &str) -> ServiceResult<Output> {
        crate::application::outputs::request_start(&self.db, pipeline_id, id).await
    }

    /// Requests the stopped desired state for one output.
    pub(crate) async fn stop_output(&self, pipeline_id: &str, id: &str) -> ServiceResult<Output> {
        crate::application::outputs::request_stop(&self.db, pipeline_id, id).await
    }

    /// Every schedule, evaluated at `now_ms`.
    pub(crate) async fn schedules(&self, now_ms: i64) -> ServiceResult<Vec<Schedule>> {
        crate::application::schedules::list(&self.db, now_ms).await
    }

    /// One schedule, evaluated at `now_ms`.
    pub(crate) async fn schedule(&self, id: &str, now_ms: i64) -> ServiceResult<Schedule> {
        crate::application::schedules::get(&self.db, id, now_ms).await
    }

    /// Adds a schedule.
    pub(crate) async fn create_schedule(
        &self,
        id: &str,
        draft: &ScheduleDraft,
        now_ms: i64,
    ) -> ServiceResult<Schedule> {
        crate::application::schedules::create(&self.db, id, draft, now_ms).await
    }

    /// Replaces a schedule's settings.
    pub(crate) async fn update_schedule(
        &self,
        id: &str,
        draft: &ScheduleDraft,
        now_ms: i64,
    ) -> ServiceResult<Schedule> {
        crate::application::schedules::update(&self.db, id, draft, now_ms).await
    }

    /// Deletes a schedule.
    pub(crate) async fn delete_schedule(&self, id: &str) -> ServiceResult<()> {
        crate::application::schedules::delete(&self.db, id).await
    }

    /// Drops one pipeline's replay history.
    pub(crate) async fn forget_flight_history(&self, pipeline_id: &str) -> ServiceResult<()> {
        crate::application::flight_recorder::forget_history(&self.db, pipeline_id).await
    }

    /// Lists persisted application logs.
    pub(crate) async fn app_logs(&self, filters: &AppLogFilters) -> ServiceResult<Vec<AppLogRow>> {
        crate::application::logs::list_logs(&self.db, filters).await
    }

    /// One page of the merged pipeline and restream log backfill.
    pub(crate) async fn log_backfill(
        &self,
        filters: &AppLogFilters,
        include_restream: bool,
    ) -> ServiceResult<Vec<AppLogRow>> {
        crate::application::logs::list_stream_backfill(&self.db, filters, include_restream).await
    }
}

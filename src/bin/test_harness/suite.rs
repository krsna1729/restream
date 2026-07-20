//! Suite and preflight orchestration for the test harness.

#[path = "suite/mode_execution.rs"]
mod mode_execution;
#[path = "suite/preflight.rs"]
mod preflight;
#[path = "suite/process_reporting.rs"]
mod process_reporting;

#[allow(unused_imports)]
pub(crate) use mode_execution::{suite_mode_is_parallelizable, suite_mode_timeout_secs, suite_run};
pub(crate) use preflight::preflight_check;
#[allow(unused_imports)]
pub(crate) use process_reporting::suite_format_elapsed;

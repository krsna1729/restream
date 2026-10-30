use crate::domain::ids::OutputId;
use crate::domain::stage::StageKey;
use crate::domain::state::EgressPhase;

/// Runtime explanation for a single output's egress status.
///
/// Combines the output's identity with its current phase, terminal media stage,
/// and the stage (if any) that is blocking progress. Used by the health and
/// alert layer to give operators a full causal picture without having to
/// correlate output IDs against stage registry entries manually.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputRuntimeExplanation {
    /// The output being described.
    pub output_id: OutputId,
    /// Human-readable name of the output.
    pub output_name: String,
    /// Encoding string, e.g. `"720p+atrack:0"`.
    pub encoding: String,
    /// Destination URL.
    pub url: String,
    /// Current observable phase of the egress worker.
    pub phase: EgressPhase,
    /// The terminal media stage this output consumes, if one has been planned.
    pub terminal_stage: Option<StageKey>,
    /// The upstream stage that is currently blocking this output, if any.
    pub blocked_by: Option<StageKey>,
}

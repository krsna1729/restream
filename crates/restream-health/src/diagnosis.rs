//! One answer to "why is this output degraded".
//!
//! An output's media passes four layers: the ingest, the feed (the shared
//! stages that shape it), the egress fabric shard that sends it, and the
//! destination (network and remote endpoint). [`diagnose`] walks them in
//! that order. The first unhealthy layer is the cause, and the healthy
//! layers before it are reported as ruled out. A destination symptom (no
//! progress) is therefore never blamed on the destination while the feed
//! or the shard explains it.
//!
//! The runtime state (`/api/v1/state`, so the dashboard and the harness),
//! the output alerts and the agent/MCP context all read this one
//! classification.

use serde::{Deserialize, Serialize};

/// Where an output's media path is broken, upstream first.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum Layer {
    Ingest,
    Feed,
    Shard,
    Destination,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(
    tag = "code",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Cause {
    /// No live publisher for the pipeline.
    NoPublisher,
    /// A shared stage the output reads is not producing yet (or failed).
    StageNotProducing { stage: String, phase: String },
    /// The fabric shard that sends the output panicked.
    ShardPanicked { shard: u32 },
    /// The fabric shard has made no progress past its stall threshold.
    ShardStalled { shard: u32 },
    /// The fabric shard's loop has ended while the output still runs on it.
    ShardStopped { shard: u32 },
    /// Admission refused the output: the feed's least-loaded egress shard is
    /// at its load ceiling. Restarts retry it until the budget is spent.
    CapacityRefused { error: String },
    /// The restart budget is spent.
    RetriesExhausted,
    /// Failing and close to spending the restart budget.
    RetryBudgetNearlySpent {
        attempts: u32,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        max: u64,
    },
    /// The destination or the network rejected the output.
    Rejected {
        phase: Option<String>,
        error: String,
    },
    /// Data is waiting on the send path and drains slowly or not at all.
    Backpressured { stalled: bool },
    /// Running, but no send completed within the stall threshold.
    NoProgress,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Diagnosis {
    pub layer: Layer,
    pub cause: Cause,
    /// Layers checked and healthy, upstream of `layer`.
    pub ruled_out: Vec<Layer>,
}

/// Fabric shard health, as the diagnosis sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShardHealth {
    Healthy,
    Stalled,
    Stopped,
    Panicked,
}

/// What [`diagnose`] reads about one output. Callers fill it from typed
/// engine state (runtime state) or from the health view (alerts).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputSignals {
    /// Desired running: only then can it be degraded.
    pub wants_running: bool,
    pub retries_exhausted: bool,
    pub has_ingest: bool,
    /// The first upstream stage that is not producing: (stage, phase).
    pub blocked_by: Option<(String, String)>,
    /// `(shard index, health)` for a fabric output.
    pub shard: Option<(u32, ShardHealth)>,
    /// The egress is failing now (phase `retrying` or `failed`, or waiting
    /// out a restart backoff).
    pub failing: bool,
    /// The latest failure: (phase, error).
    pub last_failure: Option<(Option<String>, String)>,
    /// `(attempts, max)` while a restart backoff runs.
    pub retry: Option<(u32, u64)>,
    /// Send-path classification: `backpressured` or `stalled`.
    pub backpressure: Option<String>,
    /// No send completed within the engine's stall threshold
    /// (`EGRESS_PROGRESS_STALE_MS`, the rule behind `flow: stalled`).
    pub stalled: bool,
}

/// Restart attempts at or above this share of the budget are flagged.
pub const RETRY_BUDGET_WARN_PCT: u64 = 80;

/// The failure phase an egress records when admission refuses it.
pub const CAPACITY_REFUSED_PHASE: &str = "egress_capacity";

/// The degraded layer and cause, or `None` for a healthy (or stopped)
/// output.
pub fn diagnose(signals: &OutputSignals) -> Option<Diagnosis> {
    if !signals.wants_running {
        return None;
    }
    let mut ruled_out = Vec::with_capacity(3);
    let found = |layer, cause, ruled_out: Vec<Layer>| {
        Some(Diagnosis {
            layer,
            cause,
            ruled_out,
        })
    };
    // An exhausted output runs no egress, so no layer can be inspected, and
    // the operator's action is the same whatever the publisher does: start
    // it again.
    if signals.retries_exhausted {
        return found(Layer::Destination, Cause::RetriesExhausted, ruled_out);
    }

    if !signals.has_ingest {
        return found(Layer::Ingest, Cause::NoPublisher, ruled_out);
    }
    ruled_out.push(Layer::Ingest);

    if let Some((stage, phase)) = &signals.blocked_by {
        return found(
            Layer::Feed,
            Cause::StageNotProducing {
                stage: stage.clone(),
                phase: phase.clone(),
            },
            ruled_out,
        );
    }
    ruled_out.push(Layer::Feed);

    if let Some((shard, health)) = signals.shard {
        match health {
            ShardHealth::Panicked => {
                return found(Layer::Shard, Cause::ShardPanicked { shard }, ruled_out);
            }
            ShardHealth::Stalled => {
                return found(Layer::Shard, Cause::ShardStalled { shard }, ruled_out);
            }
            ShardHealth::Stopped => {
                return found(Layer::Shard, Cause::ShardStopped { shard }, ruled_out);
            }
            ShardHealth::Healthy => ruled_out.push(Layer::Shard),
        }
    }

    if signals.failing
        && let Some((Some(phase), error)) = &signals.last_failure
        && phase == CAPACITY_REFUSED_PHASE
    {
        return found(
            Layer::Shard,
            Cause::CapacityRefused {
                error: error.clone(),
            },
            ruled_out,
        );
    }

    let destination = if let Some((attempts, max)) = signals.retry
        && max > 0
        && u64::from(attempts) * 100 >= max * RETRY_BUDGET_WARN_PCT
    {
        Some(Cause::RetryBudgetNearlySpent { attempts, max })
    } else if signals.failing {
        let (phase, error) = signals
            .last_failure
            .clone()
            .unwrap_or((None, "failed without a reported error".to_string()));
        Some(Cause::Rejected { phase, error })
    } else if let Some(reason) = &signals.backpressure {
        Some(Cause::Backpressured {
            stalled: reason == "stalled",
        })
    } else {
        signals.stalled.then_some(Cause::NoProgress)
    };
    destination.and_then(|cause| found(Layer::Destination, cause, ruled_out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running() -> OutputSignals {
        OutputSignals {
            wants_running: true,
            has_ingest: true,
            ..OutputSignals::default()
        }
    }

    #[test]
    fn a_healthy_or_stopped_output_has_no_diagnosis() {
        assert_eq!(diagnose(&running()), None);
        assert_eq!(diagnose(&OutputSignals::default()), None, "stopped");
    }

    #[test]
    fn an_exhausted_output_is_exhausted_with_or_without_a_publisher() {
        for has_ingest in [true, false] {
            let diagnosis = diagnose(&OutputSignals {
                retries_exhausted: true,
                has_ingest,
                ..running()
            })
            .unwrap();
            assert_eq!(diagnosis.cause, Cause::RetriesExhausted);
            assert!(diagnosis.ruled_out.is_empty());
        }
    }

    #[test]
    fn the_first_unhealthy_layer_upstream_is_the_cause() {
        // Every layer is unhealthy: the ingest explains it all.
        let all_broken = OutputSignals {
            has_ingest: false,
            blocked_by: Some(("pipe:video:720p".into(), "startingBackend".into())),
            shard: Some((3, ShardHealth::Stalled)),
            failing: true,
            stalled: true,
            ..running()
        };
        let diagnosis = diagnose(&all_broken).unwrap();
        assert_eq!(diagnosis.layer, Layer::Ingest);
        assert!(diagnosis.ruled_out.is_empty());

        let feed = diagnose(&OutputSignals {
            has_ingest: true,
            ..all_broken.clone()
        })
        .unwrap();
        assert_eq!(feed.layer, Layer::Feed);
        assert_eq!(feed.ruled_out, vec![Layer::Ingest]);

        let shard = diagnose(&OutputSignals {
            has_ingest: true,
            blocked_by: None,
            ..all_broken
        })
        .unwrap();
        assert_eq!(shard.cause, Cause::ShardStalled { shard: 3 });
        assert_eq!(
            shard.ruled_out,
            vec![Layer::Ingest, Layer::Feed],
            "a stalled shard is not blamed on the network"
        );
    }

    #[test]
    fn destination_causes_name_what_was_ruled_out() {
        let rejected = diagnose(&OutputSignals {
            shard: Some((0, ShardHealth::Healthy)),
            failing: true,
            last_failure: Some((Some("connect".into()), "connection refused".into())),
            ..running()
        })
        .unwrap();
        assert_eq!(rejected.layer, Layer::Destination);
        assert_eq!(
            rejected.cause,
            Cause::Rejected {
                phase: Some("connect".into()),
                error: "connection refused".into()
            }
        );
        assert_eq!(
            rejected.ruled_out,
            vec![Layer::Ingest, Layer::Feed, Layer::Shard]
        );

        let legacy = diagnose(&OutputSignals {
            stalled: true,
            ..running()
        })
        .unwrap();
        assert_eq!(
            legacy.ruled_out,
            vec![Layer::Ingest, Layer::Feed],
            "no shard to rule out for a non-fabric output"
        );
        assert_eq!(legacy.cause, Cause::NoProgress);
    }

    #[test]
    fn a_capacity_refusal_is_the_shard_layer_not_the_destination() {
        let refused = OutputSignals {
            failing: true,
            retry: Some((9, 10)),
            last_failure: Some((
                Some(CAPACITY_REFUSED_PHASE.into()),
                "refused: 61% busy".into(),
            )),
            ..running()
        };
        let diagnosis = diagnose(&refused).unwrap();
        assert_eq!(diagnosis.layer, Layer::Shard);
        assert_eq!(
            diagnosis.cause,
            Cause::CapacityRefused {
                error: "refused: 61% busy".into()
            },
            "the refusal, not the retry budget it spends, is the cause"
        );
        assert_eq!(diagnosis.ruled_out, vec![Layer::Ingest, Layer::Feed]);
        let recovered = OutputSignals {
            failing: false,
            retry: None,
            ..refused
        };
        assert_eq!(diagnose(&recovered), None, "an admitted output is healthy");
    }

    #[test]
    fn destination_causes_rank_budget_then_failure_then_pressure_then_progress() {
        let base = OutputSignals {
            failing: true,
            retry: Some((8, 10)),
            backpressure: Some("stalled".into()),
            stalled: true,
            ..running()
        };
        assert_eq!(
            diagnose(&OutputSignals {
                retries_exhausted: true,
                ..base.clone()
            })
            .unwrap()
            .cause,
            Cause::RetriesExhausted
        );
        assert_eq!(
            diagnose(&base).unwrap().cause,
            Cause::RetryBudgetNearlySpent {
                attempts: 8,
                max: 10
            }
        );
        let below_budget = OutputSignals {
            retry: Some((7, 10)),
            ..base.clone()
        };
        assert!(matches!(
            diagnose(&below_budget).unwrap().cause,
            Cause::Rejected { .. }
        ));
        let not_failing = OutputSignals {
            failing: false,
            retry: None,
            ..base.clone()
        };
        assert_eq!(
            diagnose(&not_failing).unwrap().cause,
            Cause::Backpressured { stalled: true }
        );
        let stalled = OutputSignals {
            backpressure: None,
            ..not_failing
        };
        assert_eq!(diagnose(&stalled).unwrap().cause, Cause::NoProgress);
        let recent = OutputSignals {
            stalled: false,
            ..stalled
        };
        assert_eq!(diagnose(&recent), None, "progress within the threshold");
    }
}

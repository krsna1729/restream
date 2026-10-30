//! The topology a change would leave behind: the outputs a pipeline wants
//! running grouped by the stage each one reads and its protocol, before and
//! after the change, priced on the same planner the reconciler uses.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::agent_core::{PlanRequest, ProposedChange};
use crate::application::models::Output;
use restream_domain::domain::output_spec::{EgressProtocol, OutputConfig, OutputUrlScheme};
use restream_domain::domain::stage::StageKind;
use restream_domain::planner::{PlannedOutput, plan_pipeline_graph};

/// Wanted-running outputs that read one stage over one protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopologyRow {
    /// The stage kind the outputs read (`source`, `video:720p:codec:h264`).
    pub stage: String,
    pub protocol: EgressProtocol,
    pub outputs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopologyDiff {
    pub pipeline_id: String,
    /// The live ingest's video codec the stages were planned for; `None`
    /// without a live ingest, when no codec bridge is planned.
    pub ingest_codec: Option<String>,
    pub before: Vec<TopologyRow>,
    pub after: Vec<TopologyRow>,
    /// Stages (beyond the source) the change would start.
    pub added_stages: Vec<String>,
    /// Stages no wanted output would read after the change.
    pub retired_stages: Vec<String>,
}

/// One wanted-running output, as the planner sees it.
#[derive(Clone, Copy)]
struct Wanted<'a> {
    id: Option<&'a str>,
    config: &'a OutputConfig,
    url: &'a str,
}

/// The request pipeline's topology before and after its changes, or `None`
/// for a request without a pipeline. Changes for another pipeline are
/// outside this diff.
pub(super) fn topology_diff(
    request: &PlanRequest,
    outputs: &[Output],
    ingest_codec: Option<&str>,
) -> Option<TopologyDiff> {
    let pipeline_id = request.pipeline_id.as_deref()?;
    let stored: Vec<&Output> = outputs
        .iter()
        .filter(|output| output.pipeline_id == pipeline_id)
        .collect();
    let before: Vec<Wanted> = stored
        .iter()
        .filter(|output| output.wants_running())
        .map(|output| Wanted {
            id: Some(output.id.as_str()),
            config: &output.config,
            url: output.url.as_str(),
        })
        .collect();
    let mut after = before.clone();
    for change in &request.proposed_changes {
        if change
            .pipeline_id
            .as_deref()
            .is_some_and(|pid| pid != pipeline_id)
        {
            continue;
        }
        apply(&mut after, &stored, change);
    }

    let (before_rows, before_stages) = shape(pipeline_id, ingest_codec, &before);
    let (after_rows, after_stages) = shape(pipeline_id, ingest_codec, &after);
    Some(TopologyDiff {
        pipeline_id: pipeline_id.to_string(),
        ingest_codec: ingest_codec.map(str::to_string),
        before: before_rows,
        after: after_rows,
        added_stages: after_stages.difference(&before_stages).cloned().collect(),
        retired_stages: before_stages.difference(&after_stages).cloned().collect(),
    })
}

fn apply<'a>(wanted: &mut Vec<Wanted<'a>>, stored: &[&'a Output], change: &'a ProposedChange) {
    // Trimmed as the apply trims them (`prepare_add_output`, `prepare_update_output`).
    let target = change.output_id.as_deref().map(str::trim);
    let desired = change.desired_state.as_deref().map(str::trim);
    let url = change.url.as_deref().map(str::trim);
    let position = |wanted: &[Wanted]| {
        target.and_then(|id| wanted.iter().position(|output| output.id == Some(id)))
    };
    match change.kind.as_str() {
        "addOutput" => {
            if desired == Some("running")
                && let (Some(config), Some(url)) = (&change.config, url)
            {
                wanted.push(Wanted {
                    // An explicit id lets a later change in the request find it.
                    id: target,
                    config,
                    url,
                });
            }
        }
        "updateOutput" => {
            // An update that sets a stopped output running starts it.
            if position(wanted).is_none()
                && desired == Some("running")
                && let Some(output) = stored
                    .iter()
                    .find(|output| Some(output.id.as_str()) == target)
            {
                wanted.push(Wanted {
                    id: Some(output.id.as_str()),
                    config: &output.config,
                    url: output.url.as_str(),
                });
            }
            let Some(at) = position(wanted) else { return };
            if let Some(config) = &change.config {
                wanted[at].config = config;
            }
            if let Some(url) = url {
                wanted[at].url = url;
            }
            if desired == Some("stopped") {
                wanted.remove(at);
            }
        }
        "startOutput" => {
            if position(wanted).is_none()
                && let Some(output) = stored
                    .iter()
                    .find(|output| Some(output.id.as_str()) == target)
            {
                wanted.push(Wanted {
                    id: Some(output.id.as_str()),
                    config: &output.config,
                    url: output.url.as_str(),
                });
            }
        }
        "stopOutput" | "removeOutput" => {
            if let Some(at) = position(wanted) {
                wanted.remove(at);
            }
        }
        _ => {}
    }
}

/// Rows grouped by (terminal stage, protocol), largest first (a few rows
/// per pipeline, so a linear find), and every
/// non-source stage the outputs need.
fn shape(
    pipeline_id: &str,
    ingest_codec: Option<&str>,
    wanted: &[Wanted],
) -> (Vec<TopologyRow>, BTreeSet<String>) {
    let mut rows: Vec<TopologyRow> = Vec::new();
    let mut stages = BTreeSet::new();
    for output in wanted {
        let planned = PlannedOutput::new(
            output.id.unwrap_or("planned-output"),
            output.config.clone(),
            output.url,
        );
        let plan = plan_pipeline_graph(pipeline_id, ingest_codec, &[planned], false);
        let protocol = OutputUrlScheme::from_url(output.url).protocol();
        let stage = plan.terminal_stage.kind.to_string();
        match rows
            .iter_mut()
            .find(|row| row.stage == stage && row.protocol == protocol)
        {
            Some(row) => row.outputs += 1,
            None => rows.push(TopologyRow {
                stage,
                protocol,
                outputs: 1,
            }),
        }
        stages.extend(
            plan.stages
                .iter()
                .filter(|stage| stage.kind != StageKind::Source)
                .map(|stage| stage.kind.to_string()),
        );
    }
    rows.sort_by(|a, b| {
        b.outputs
            .cmp(&a.outputs)
            .then_with(|| a.stage.cmp(&b.stage))
    });
    (rows, stages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use restream_domain::domain::state::DesiredOutputState;

    fn stored(id: &str, config: OutputConfig, url: &str, running: bool) -> Output {
        Output {
            id: id.to_string(),
            pipeline_id: "p".to_string(),
            name: id.to_string(),
            url: url.to_string(),
            monitoring_url: None,
            desired_state: if running {
                DesiredOutputState::Running
            } else {
                DesiredOutputState::Stopped
            },
            retries_exhausted: false,
            config,
        }
    }

    fn change(kind: &str, output_id: Option<&str>) -> ProposedChange {
        ProposedChange {
            kind: kind.to_string(),
            pipeline_id: None,
            output_id: output_id.map(str::to_string),
            name: None,
            url: None,
            monitoring_url: None,
            config: None,
            desired_state: None,
        }
    }

    fn add(config: OutputConfig, url: &str, desired: &str) -> ProposedChange {
        ProposedChange {
            url: Some(url.to_string()),
            config: Some(config),
            desired_state: Some(desired.to_string()),
            ..change("addOutput", None)
        }
    }

    fn request(changes: Vec<ProposedChange>) -> PlanRequest {
        PlanRequest {
            intent: "t".to_string(),
            pipeline_id: Some("p".to_string()),
            proposed_changes: changes,
        }
    }

    fn total(rows: &[TopologyRow]) -> u32 {
        rows.iter().map(|row| row.outputs).sum()
    }

    #[test]
    fn a_running_transcode_add_starts_its_stage_and_a_second_reuses_it() {
        let outputs = [stored("a", OutputConfig::source(), "srt://h:1", true)];
        let first = topology_diff(
            &request(vec![add(
                OutputConfig::preset("720p"),
                "rtmp://h/live/k",
                "running",
            )]),
            &outputs,
            Some("h264"),
        )
        .unwrap();
        assert_eq!(total(&first.before), 1);
        assert_eq!(total(&first.after), 2);
        assert!(!first.added_stages.is_empty(), "{first:?}");
        assert!(first.retired_stages.is_empty());

        // Two 720p RTMP adds read one stage: one row of two, the same stages.
        let both = topology_diff(
            &request(vec![
                add(OutputConfig::preset("720p"), "rtmp://h/live/k", "running"),
                add(OutputConfig::preset("720p"), "rtmp://h/live/j", "running"),
            ]),
            &outputs,
            Some("h264"),
        )
        .unwrap();
        assert_eq!(both.added_stages, first.added_stages);
        assert!(
            both.after
                .iter()
                .any(|row| row.protocol == EgressProtocol::Rtmp && row.outputs == 2),
            "{both:?}"
        );
    }

    #[test]
    fn only_wanted_running_outputs_count() {
        let outputs = [
            stored("on", OutputConfig::preset("720p"), "rtmp://h/live/a", true),
            stored("off", OutputConfig::source(), "srt://h:2", false),
        ];
        // A stopped add changes nothing; starting the stopped output adds it.
        let diff = topology_diff(
            &request(vec![
                add(OutputConfig::source(), "srt://h:3", "stopped"),
                change("startOutput", Some("off")),
            ]),
            &outputs,
            Some("h264"),
        )
        .unwrap();
        assert_eq!(total(&diff.before), 1);
        assert_eq!(total(&diff.after), 2);
    }

    #[test]
    fn stopping_the_last_reader_of_a_stage_retires_it() {
        let outputs = [
            stored("t", OutputConfig::preset("720p"), "rtmp://h/live/a", true),
            stored("s", OutputConfig::source(), "srt://h:2", true),
        ];
        for kind in ["stopOutput", "removeOutput"] {
            let diff = topology_diff(
                &request(vec![change(kind, Some("t"))]),
                &outputs,
                Some("h264"),
            )
            .unwrap();
            assert_eq!(total(&diff.after), 1, "{kind}");
            assert!(!diff.retired_stages.is_empty(), "{kind}: {diff:?}");
            assert!(diff.added_stages.is_empty(), "{kind}");
        }
    }

    #[test]
    fn an_update_moves_the_output_to_its_new_stage_and_protocol() {
        let outputs = [stored("a", OutputConfig::source(), "srt://h:1", true)];
        let update = ProposedChange {
            url: Some("rtmp://h/live/k".to_string()),
            config: Some(OutputConfig::preset("720p")),
            ..change("updateOutput", Some("a"))
        };
        let diff = topology_diff(&request(vec![update]), &outputs, Some("h264")).unwrap();
        assert_eq!(diff.before[0].protocol, EgressProtocol::Srt);
        assert_eq!(diff.after.len(), 1);
        assert_eq!(diff.after[0].protocol, EgressProtocol::Rtmp);
        assert_ne!(diff.after[0].stage, diff.before[0].stage);
        assert!(!diff.added_stages.is_empty());
    }

    #[test]
    fn an_update_that_sets_a_stopped_output_running_starts_it_with_its_new_url() {
        let outputs = [stored("a", OutputConfig::source(), "srt://h:1", false)];
        let update = ProposedChange {
            url: Some("rtmp://h/live/k".to_string()),
            desired_state: Some("running".to_string()),
            ..change("updateOutput", Some("a"))
        };
        let diff = topology_diff(&request(vec![update]), &outputs, Some("h264")).unwrap();
        assert!(diff.before.is_empty());
        assert_eq!(diff.after.len(), 1);
        assert_eq!(diff.after[0].protocol, EgressProtocol::Rtmp);
    }

    #[test]
    fn an_explicit_add_id_is_found_by_a_later_change_and_values_are_trimmed() {
        let mut first = add(OutputConfig::source(), " srt://h:1 ", " running ");
        first.output_id = Some("new".to_string());
        let stop = change("stopOutput", Some("new"));
        let added = topology_diff(&request(vec![first.clone()]), &[], None).unwrap();
        assert_eq!(total(&added.after), 1, "a trimmed running add counts");
        assert_eq!(added.after[0].protocol, EgressProtocol::Srt);
        let stopped = topology_diff(&request(vec![first, stop]), &[], None).unwrap();
        assert!(stopped.after.is_empty(), "{stopped:?}");
    }

    #[test]
    fn changes_for_another_pipeline_and_requests_without_one_are_outside() {
        let outputs = [stored("a", OutputConfig::source(), "srt://h:1", true)];
        let elsewhere = ProposedChange {
            pipeline_id: Some("q".to_string()),
            ..change("stopOutput", Some("a"))
        };
        let diff = topology_diff(&request(vec![elsewhere]), &outputs, None).unwrap();
        assert_eq!(diff.before, diff.after);
        let mut no_pipeline = request(Vec::new());
        no_pipeline.pipeline_id = None;
        assert!(topology_diff(&no_pipeline, &outputs, None).is_none());
    }
}

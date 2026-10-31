use std::collections::HashMap;

use super::*;

#[test]
fn alert_titles_name_their_pipeline_and_output() {
    let mut alerts = vec![
        Alert::new(
            "o".into(),
            Severity::Warning,
            Scope::Output,
            "Output 'out_1' reported an egress failure",
            "",
            vec![],
            "",
            "",
        ),
        Alert::new(
            "p".into(),
            Severity::Critical,
            Scope::Pipeline,
            "No active publisher",
            "",
            vec![],
            "",
            "",
        ),
        Alert::new(
            "e".into(),
            Severity::Warning,
            Scope::Engine,
            "Engine-wide",
            "",
            vec![],
            "",
            "",
        ),
        Alert::new(
            "u".into(),
            Severity::Warning,
            Scope::Output,
            "Output 'out_9' is not running",
            "",
            vec![],
            "",
            "",
        ),
    ];
    alerts[0].pipeline_id = Some("p1".into());
    alerts[0].output_id = Some("out_1".into());
    alerts[1].pipeline_id = Some("p1".into());
    // An output the runtime state no longer names keeps its id.
    alerts[3].pipeline_id = Some("p1".into());
    alerts[3].output_id = Some("out_9".into());
    let pipelines = HashMap::from([("p1".to_string(), "Newsroom".to_string())]);
    let outputs = HashMap::from([("out_1".to_string(), "YouTube main".to_string())]);
    name_subjects(&mut alerts, &pipelines, &outputs);
    assert_eq!(
        alerts[0].title,
        "Newsroom: YouTube main reported an egress failure"
    );
    assert_eq!(alerts[1].title, "Newsroom: No active publisher");
    assert_eq!(alerts[2].title, "Engine-wide");
    assert_eq!(alerts[3].title, "Newsroom: Output 'out_9' is not running");
    assert_eq!(
        alerts[0].output_id.as_deref(),
        Some("out_1"),
        "ids stay in their fields"
    );
}

//! The panels as MCP Apps resources: one renderer document, given each
//! panel's spec (`restream_agent_contract::panels`), so a panel shows exactly
//! the fields its contract declares.

use restream_agent_contract::panels::{PANEL_MIME_TYPE, PANELS, Panel};
use serde_json::{Value, json};

const RENDERER: &str = include_str!("panel.html");
const SPEC_SLOT: &str = "/*PANEL_SPEC*/null";

/// The panel's HTML document.
pub(crate) fn document(panel: &Panel) -> String {
    // `<` escaped so no value can close the script element.
    let spec = serde_json::to_string(panel)
        .expect("a panel spec serializes")
        .replace('<', "\\u003c");
    RENDERER.replacen(SPEC_SLOT, &spec, 1)
}

/// `resources/list` result.
pub(crate) fn list() -> Value {
    json!({
        "resources": PANELS
            .iter()
            .map(|panel| json!({
                "uri": panel.uri,
                "name": panel.name,
                "title": panel.title,
                "description": panel.description,
                "mimeType": PANEL_MIME_TYPE,
            }))
            .collect::<Vec<_>>()
    })
}

/// `resources/read` result for `uri`, or `None` for an unknown resource.
pub(crate) fn read(uri: &str) -> Option<Value> {
    let panel = PANELS.iter().find(|panel| panel.uri == uri)?;
    Some(json!({
        "contents": [{
            "uri": panel.uri,
            "mimeType": PANEL_MIME_TYPE,
            "text": document(panel),
            "_meta": { "ui": { "prefersBorder": true } },
        }]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use restream_agent_contract::panels::Field;

    /// The spec embedded in a document, parsed back.
    fn embedded_spec(document: &str) -> Value {
        let start = document.find("const PANEL = ").expect("spec slot") + "const PANEL = ".len();
        let end = start + document[start..].find(";\n").expect("spec end");
        serde_json::from_str(&document[start..end]).expect("the embedded spec is JSON")
    }

    /// Each document renders from exactly its panel's contract spec.
    #[test]
    fn each_document_embeds_its_contract_spec() {
        for panel in PANELS {
            assert_eq!(
                embedded_spec(&document(panel)),
                serde_json::to_value(panel).unwrap(),
                "{}",
                panel.uri
            );
        }
    }

    /// A spec value cannot end the script element it is embedded in.
    #[test]
    fn a_spec_cannot_close_the_script_element() {
        const HOSTILE: Panel = Panel {
            uri: "ui://restream/hostile",
            name: "hostile",
            title: "</script><script>alert(1)</script>",
            description: "",
            fields: &[Field {
                label: "</SCRIPT>",
                pointer: "/x",
                kind: restream_agent_contract::panels::FieldKind::Text,
            }],
            action: None,
            refresh: None,
        };
        let html = document(&HOSTILE);
        assert_eq!(html.to_ascii_lowercase().matches("</script>").count(), 1);
        assert_eq!(
            embedded_spec(&html)["title"],
            "</script><script>alert(1)</script>"
        );
    }
}

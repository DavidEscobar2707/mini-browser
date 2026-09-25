//! OWNER: desk-core. cua-driver `get_window_state` structured output -> Observation.

use super::redact::{is_secret_field, redact};
use crate::qa::{Observation, ObservedElement, Rect};
use serde_json::{json, Value};
use std::collections::HashMap;

const TEXT_CAP: usize = 8000;
const NO_ACTION_ROLES: &[&str] = &["Window", "Pane", "ScrollBar", "Text", "Group", "TitleBar"];

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn f(v: &Value, k: &str) -> f64 {
    v[k].as_f64().unwrap_or(0.0)
}

/// Plan 009 §Contract `kind`, or None when the element is text only.
fn kind(role: &str, actions: &[&str]) -> Option<&'static str> {
    let has = |a: &str| actions.contains(&a);
    if NO_ACTION_ROLES.contains(&role) {
        None
    } else if has("invoke") || has("toggle") || has("expand") {
        Some("click")
    } else if matches!(role, "TabItem" | "ListItem" | "MenuItem") && has("select") {
        Some("click")
    } else if matches!(role, "Edit" | "Document" | "ComboBox") && has("set_value") {
        Some("fill")
    } else {
        None
    }
}

/// Returns the observation and the node -> element_token map for that snapshot.
/// Rules: plan 009 §Contract. Secret fields are dropped before their value is read.
pub fn observation_from_state(state: &Value, app: &str) -> (Observation, HashMap<u64, String>) {
    let title = redact(s(state, "window_title"));
    let mut obs = Observation {
        url: format!("desk://{}/{}", app, title).to_lowercase().replace(' ', "-"),
        title,
        w: f(&state["window_bounds"], "width"),
        h: f(&state["window_bounds"], "height"),
        ..Default::default()
    };
    let mut tokens = HashMap::new();
    let mut text: Vec<String> = Vec::new();
    let mut marker = Vec::new();
    for e in state["elements"].as_array().into_iter().flatten() {
        let (role, raw_label) = (s(e, "role"), s(e, "label"));
        if is_secret_field(role, raw_label) {
            continue;
        }
        let frame = &e["frame"];
        let rect = Rect { x: f(frame, "x"), y: f(frame, "y"), w: f(frame, "w"), h: f(frame, "h") };
        if !frame.is_object() || rect.w * rect.h == 0.0 {
            continue;
        }
        let Some(node) = e["element_index"].as_u64() else { continue };
        let value = e["value"].as_str().map(redact);
        let label = if raw_label.is_empty() { value.clone().unwrap_or_default() } else { redact(raw_label) };
        if let Some(token) = e["element_token"].as_str() {
            tokens.insert(node, token.to_string());
        }
        marker.push(json!([label, value]));
        if matches!(role, "Text" | "Document" | "Edit") {
            text.push(label.clone());
            if let Some(v) = value.as_ref().filter(|v| !v.is_empty() && **v != label) {
                text.push(v.clone());
            }
        }
        let actions: Vec<&str> = e["actions"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let Some(k) = kind(role, &actions) else { continue };
        obs.actions.push(ObservedElement {
            id: node.to_string(),
            node,
            kind: k.into(),
            label,
            role: Some(role.into()),
            value,
            selected: e["selected"].as_bool().map(|b| b.to_string()),
            rect,
            path: e["element_token"].as_str().map(|t| format!("desk:{t}")),
            input_type: (k == "fill").then(|| "text".into()),
            disabled: !e["enabled"].as_bool().unwrap_or(true),
            ..Default::default()
        });
    }
    obs.text = text.join("\n").chars().take(TEXT_CAP).collect();
    obs.marker = Value::Array(marker);
    (obs, tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let p = format!("{}/tests/fixtures/desk/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    fn find<'a>(o: &'a Observation, label: &str) -> Option<&'a ObservedElement> {
        o.actions.iter().find(|a| a.label == label)
    }

    #[test]
    fn calculator_before() {
        let (o, tokens) = observation_from_state(&fixture("calculator_state.json"), "Calculator");
        assert_eq!(o.url, "desk://calculator/calculator");
        assert_eq!(o.title, "Calculator");
        assert_eq!((o.w, o.h), (1920.0, 1140.0));
        for l in ["Seven", "Equals", "Multiply by"] {
            let a = find(&o, l).unwrap_or_else(|| panic!("{l}"));
            assert_eq!(a.kind, "click");
            assert_eq!(tokens[&a.node], a.path.as_deref().unwrap().trim_start_matches("desk:"));
        }
        assert!(o.actions.iter().all(|a| !matches!(a.role.as_deref(), Some("Window" | "Text"))));
        assert!(o.actions.iter().all(|a| a.rect.w * a.rect.h > 0.0));
        // No-frame elements (MenuItem "System" = 1, Text 7) are dropped everywhere.
        assert!(!tokens.contains_key(&1) && !tokens.contains_key(&7));
        assert!(find(&o, "System").is_none());
        let history = o.actions.iter().find(|a| a.role.as_deref() == Some("TabItem")).unwrap();
        assert_eq!(history.selected.as_deref(), Some("true"));
        assert!(o.text.contains("Display is 0"));
        assert!(o.text.contains("Standard Calculator mode"));
    }

    /// GAP: the committed calculator_after_7x12.json is byte-identical to calculator_state.json
    /// except element tokens (it still reads "Display is 0"). Un-ignore once it is recaptured.
    #[test]
    fn calculator_after_fixture() {
        let (before, _) = observation_from_state(&fixture("calculator_state.json"), "Calculator");
        let (after, _) = observation_from_state(&fixture("calculator_after_7x12.json"), "Calculator");
        assert!(after.text.contains("Display is 84"));
        assert_ne!(before.marker, after.marker);
    }

    #[test]
    fn display_change_changes_text_and_marker() {
        let state = fixture("calculator_state.json");
        let mut after = state.clone();
        after["elements"][8]["label"] = json!("Display is 84");
        let (b, _) = observation_from_state(&state, "Calculator");
        let (a, _) = observation_from_state(&after, "Calculator");
        assert!(a.text.contains("Display is 84"));
        assert_ne!(b.marker, a.marker);
    }

    #[test]
    fn password_field_is_gone() {
        let state = json!({
            "window_title": "Sign in", "window_bounds": {"width": 800, "height": 600},
            "elements": [
                {"element_index": 0, "element_token": "s1:0", "role": "Edit", "label": "Password",
                 "value": "FAKEpass1234", "actions": ["set_value"], "enabled": true,
                 "frame": {"x": 0, "y": 0, "w": 100, "h": 20}},
                {"element_index": 1, "element_token": "s1:1", "role": "Edit", "label": "User",
                 "value": "alice", "actions": ["set_value"], "enabled": true,
                 "frame": {"x": 0, "y": 30, "w": 100, "h": 20}}
            ]
        });
        let (o, tokens) = observation_from_state(&state, "Demo");
        assert_eq!(o.actions.len(), 1);
        assert_eq!(o.actions[0].kind, "fill");
        assert_eq!(o.actions[0].input_type.as_deref(), Some("text"));
        assert!(!o.text.contains("Password") && !o.text.contains("FAKEpass"));
        assert!(!o.marker.to_string().contains("FAKEpass"));
        assert!(!tokens.contains_key(&0));
        assert_eq!(o.url, "desk://demo/sign-in");
    }
}

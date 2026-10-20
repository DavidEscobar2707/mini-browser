//! Tap-flow facts for touch viewports, computed in code after every explored
//! step: the two rules that found the OhhLook overlay bugs (dogfood checklist).
use serde::Deserialize;

use crate::qa::{Finding, FindingKind, SeveritySource, Viewport};

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct MobileState {
    pub ok: bool,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub open_overlays: Vec<String>,
    #[serde(default)]
    pub scroll_locked: bool,
    #[serde(default)]
    pub doc_height: f64,
    #[serde(default)]
    pub inner_height: f64,
}

pub fn parse(raw: &str) -> Result<MobileState, String> {
    let s: MobileState = serde_json::from_str(raw).map_err(|e| format!("mobileState: {e}"))?;
    if !s.ok {
        return Err("mobileState: page returned ok:false".into());
    }
    Ok(s)
}

/// Rules over the state before and after one step. `shot` is the step screenshot
/// path relative to the out dir; it is the evidence.
pub fn judge(before: &MobileState, after: &MobileState, vp: &Viewport, step: usize, shot: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let page_taller = after.doc_height > after.inner_height + 1.0;
    if after.scroll_locked && after.open_overlays.is_empty() && page_taller {
        out.push(finding(
            vp,
            step,
            shot,
            "scroll-lock-leaked",
            "Scroll lock leaked: the page cannot scroll",
            format!(
                "After step {step} on {} the document is taller than the viewport ({:.0} > {:.0}) but body/html overflow is hidden and no dialog is open.",
                after.path, after.doc_height, after.inner_height
            ),
            None,
        ));
    }
    if before.path != after.path {
        if let Some(stale) = after.open_overlays.iter().find(|p| before.open_overlays.contains(p)) {
            out.push(finding(
                vp,
                step,
                shot,
                "overlay-across-navigation",
                "Overlay stayed open across a navigation",
                format!("Step {step} navigated {} -> {} but {stale} is still open on top of the new page.", before.path, after.path),
                Some(stale.clone()),
            ));
        }
    }
    out
}

fn finding(vp: &Viewport, step: usize, shot: &str, rule: &str, title: &str, detail: String, path: Option<String>) -> Finding {
    Finding {
        id: format!("{}-{rule}-{step}", vp.name),
        kind: FindingKind::Geometry,
        severity: 2,
        severity_source: SeveritySource::Code,
        viewport: vp.name.to_string(),
        title: title.to_string(),
        detail,
        dom_path: path,
        step: Some(step),
        blocked_by_firewall: false,
        evidence: vec![shot.to_string()],
        screenshot: Some(shot.to_string()),
        score: None,
        confidence: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phone() -> Viewport {
        crate::qa::VIEWPORTS[0]
    }

    fn state(path: &str, overlays: &[&str], locked: bool) -> MobileState {
        MobileState {
            ok: true,
            path: path.into(),
            open_overlays: overlays.iter().map(|s| s.to_string()).collect(),
            scroll_locked: locked,
            doc_height: 3000.0,
            inner_height: 844.0,
        }
    }

    #[test]
    fn a_leaked_scroll_lock_is_a_severity_2_fact() {
        let out = judge(&state("/", &["dialog"], true), &state("/", &[], true), &phone(), 3, "shots/phone-step-03.jpg");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "phone-scroll-lock-leaked-3");
        assert_eq!(out[0].severity, 2);
        assert_eq!(out[0].step, Some(3));
        assert_eq!(out[0].screenshot.as_deref(), Some("shots/phone-step-03.jpg"));
    }

    #[test]
    fn a_lock_with_an_open_dialog_is_normal() {
        assert!(judge(&state("/", &[], false), &state("/", &["dialog"], true), &phone(), 1, "s").is_empty());
    }

    #[test]
    fn an_overlay_that_survives_a_navigation_is_reported_once_with_its_path() {
        let out = judge(&state("/", &["div.sheet"], true), &state("/about", &["div.sheet"], true), &phone(), 2, "s");
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].id, "phone-overlay-across-navigation-2");
        assert_eq!(out[0].dom_path.as_deref(), Some("div.sheet"));
    }

    #[test]
    fn a_new_overlay_on_the_new_page_is_not_stale() {
        assert!(judge(&state("/", &[], false), &state("/about", &["div.cookie"], false), &phone(), 2, "s").is_empty());
    }

    #[test]
    fn parse_refuses_ok_false() {
        assert!(parse(r#"{"ok":false,"error":"x"}"#).is_err());
        assert_eq!(parse(r#"{"ok":true,"path":"/","open_overlays":[],"scroll_locked":false,"doc_height":1,"inner_height":2}"#).unwrap().path, "/");
    }
}

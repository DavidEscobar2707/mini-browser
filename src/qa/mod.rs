//! `mini --qa <url>`: one-shot self-driving QA run.
//!
//! Frozen interface. The integration owner edits this file; every other file in
//! `src/qa/` implements against the types declared here and never redefines them.
//!
//! Module map — one writer per file:
//!   bridge.rs    UI-thread transport: Msg::Qa -> eval / CDP / screenshot / input.
//!                In-process (`ICoreWebView2::CallDevToolsProtocolMethod`), no socket.
//!   remote.rs    the same `Browser` over a remote Chromium (Chrome for Android via `adb forward`):
//!                CDP on a websocket (`cdp_bus`), recorder through a CDP binding, Fetch firewall.
//!   agent.rs     the loop: viewports -> settle -> probe -> explore -> triage -> claims -> report.
//!   jev.rs       Jev client (api.typesafe.ai) + typed answer validation, implements [`Judge`].
//!   mobile.rs    tap-flow rules over before/after page state (pure).
//!   questions.rs question builders: Q-A section, Q-B exploration, Q-C triage, claims.
//!   probes.rs    parsers for the JS payloads (observation, probe, sections, hit-test, act log).
//!   firewall.rs  mutation-firewall policy + first-party / blocklist predicates (pure).
//!   report.rs    report.md + report.json rendering and the verdict.
//!
//! Data flow: `bridge` and `jev` are the only I/O. `probes`, `firewall`, `report`
//! are pure functions over the types below. `agent` composes them.

pub mod agent;
pub mod bridge;
pub mod firewall;
pub mod jev;
pub mod mobile;
pub mod probes;
pub mod questions;
pub mod remote;
pub mod report;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default step budget for each explored viewport.
pub const DEFAULT_STEPS: usize = 25;
/// Default wall-clock budget for the whole run, seconds (SPEC: 4 minutes).
pub const DEFAULT_WALL_CLOCK_S: u64 = 240;
/// Every `Browser` request carries this deadline; a dropped WebView2 completion
/// must fail the request, never deadlock the QA thread.
pub const BRIDGE_TIMEOUT_MS: u64 = 10_000;
/// Jev model alias; pinned model IDs are accepted too.
pub const DEFAULT_MODEL: &str = "jev-latest";

// ---------------------------------------------------------------- page scripts

/// The page-side QA API (`window.__miniQA.*`), injected as initialization
/// scripts — in this order, before any page script runs. `settle` arms the
/// mutation/network observers, `sections` owns the region registry that
/// `snapshot`/`probe` scope to, and `act`/`probe` build on both.
pub const PAGE_SCRIPTS: &[&str] = &[
    include_str!("../../tools/qa-settle.js"),
    include_str!("../../tools/qa-sections.js"),
    include_str!("../../tools/qa-snapshot.js"),
    include_str!("../../tools/qa-act.js"),
    include_str!("../../tools/qa-probe.js"),
];

// ---------------------------------------------------------------- config

/// Everything the loop needs, resolved from the CLI by `main.rs`.
#[derive(Clone, Debug)]
pub struct QaConfig {
    pub url: String,
    /// Target host, lowercase, no port. Off-host navigation is refused under `--qa`.
    pub host: String,
    pub section: Option<String>,
    pub claims: Vec<String>,
    pub out_dir: PathBuf,
    /// false (default): every non-GET/HEAD/OPTIONS request is failed by the firewall.
    pub allow_writes: bool,
    pub steps: usize,
    /// Viewports Jev explores (clicks, types). Every other viewport is probe-only;
    /// empty = probe-only everywhere (`--explore none`).
    pub explore: Vec<String>,
    pub wall_clock_s: u64,
    pub model: String,
    pub api_key: Option<String>,
}

impl QaConfig {
    pub fn steps(&self) -> usize {
        self.steps.clamp(1, 200)
    }

    /// True when Jev drives controls at this viewport (else the viewport is probe-only).
    pub fn explores(&self, viewport: &str) -> bool {
        self.explore.iter().any(|v| v == viewport)
    }
}

// ---------------------------------------------------------------- viewports

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub dsf: f64,
    pub mobile: bool,
}

/// Run order, phone first. Look a viewport up by name, never by index.
pub const VIEWPORTS: [Viewport; 3] = [
    Viewport { name: "phone", width: 390, height: 844, dsf: 3.0, mobile: true },
    Viewport { name: "tablet", width: 768, height: 1024, dsf: 2.0, mobile: true },
    Viewport { name: "desktop", width: 1440, height: 900, dsf: 1.0, mobile: false },
];
/// Viewports Jev explores (clicks, types) when `--explore` is not given. Phone first: a
/// page is met as a phone user meets it, and the desktop pass is the second opinion.
pub const DEFAULT_EXPLORE: &[&str] = &["phone", "desktop"];
/// Wall-clock seconds added for every explored viewport beyond the first.
pub const EXTRA_WALL_CLOCK_S: u64 = 120;

// ---------------------------------------------------------------- I/O seams

/// The content webview, driven from the QA thread. Every method marshals work
/// onto the tao/UI thread and blocks with [`BRIDGE_TIMEOUT_MS`]; a timeout is an
/// `Err`, never a hang. Implemented by [`bridge::UiBridge`].
pub trait Browser: Send + Sync {
    /// Evaluate `js` in the content webview and return the raw result text
    /// (WebView2 serializes the expression's value as JSON).
    fn eval(&self, js: &str) -> Result<String, String>;
    /// In-process DevTools protocol call. Returns the parsed `result` object
    /// (`Null` when the method returns nothing).
    fn cdp(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String>;
    /// Emulate a viewport (device metrics override; falls back to real bounds).
    fn set_viewport(&self, v: Viewport) -> Result<(), String>;
    /// JPEG/PNG of the content webview to `path`.
    fn screenshot(&self, path: &Path) -> Result<(), String>;
    /// Real input events at CSS-pixel coordinates of the content webview.
    fn mouse_click(&self, x: f64, y: f64) -> Result<(), String>;
    /// One-finger tap at CSS-pixel coordinates. Transports without touch input
    /// fall back to a mouse click.
    fn tap(&self, x: f64, y: f64) -> Result<(), String> {
        self.mouse_click(x, y)
    }
    /// Insert text into the focused element (real input, fires input events).
    fn key_text(&self, text: &str) -> Result<(), String>;
    /// Rotate the JSONL recorder (see `debug.rs`); returns the new run dir.
    fn run_label(&self, label: &str, incident: &str) -> Result<PathBuf, String>;
    fn url(&self) -> Result<String, String>;
    /// Requests the firewall failed so far, oldest first.
    fn blocked(&self) -> Vec<firewall::Blocked>;
    /// `Ok(())` when the mutation firewall is armed. `Err(reason)` means the run
    /// must not proceed: a QA pass that cannot refuse writes is not a QA pass.
    /// Fakes may keep the default.
    fn firewall_status(&self) -> Result<(), String> {
        Ok(())
    }
    /// The engine's own version string, when the transport can read it (the page
    /// cannot: Mini spoofs the UA and the brand list derives from it).
    fn browser_version(&self) -> Option<String> {
        None
    }
    /// The whole engine line for the report header, when the transport knows more than a
    /// version: a remote Chrome names its device. Wins over [`Browser::browser_version`].
    fn engine_label(&self) -> Option<String> {
        None
    }
}

/// Jev. `body` is a complete `/v1/systemone` request; the return value is the
/// `answers` object. Implemented by [`jev::JevClient`]; faked in tests.
pub trait Judge: Send + Sync {
    fn answers(&self, body: &serde_json::Value) -> Result<serde_json::Value, String>;
    /// Completed requests, for the report header. Fakes may return 0.
    fn calls(&self) -> u32 {
        0
    }
    fn input_tokens(&self) -> u64 {
        0
    }
}

// ---------------------------------------------------------------- DOM facts

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// One entry of `snapshot().actions` (see `tools/qa-snapshot.js`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ObservedElement {
    pub id: String,
    /// Stable per-node id; the loop keys "already exercised" on it.
    pub node: u64,
    /// `click` | `fill` | `select` | `scroll` | `wait`.
    pub kind: String,
    pub label: String,
    pub role: Option<String>,
    pub value: Option<String>,
    /// aria-* state, stringified by the snapshot script.
    pub checked: Option<String>,
    pub selected: Option<String>,
    pub expanded: Option<String>,
    pub current_value: Option<String>,
    pub delta: Option<f64>,
    pub rect: Rect,
    /// QA additions: evidence path, and what the firewall/blocklist needs.
    pub path: Option<String>,
    pub input_type: Option<String>,
    pub name: Option<String>,
    /// Enclosing form's method ("post"/"get"), lowercased, when there is one.
    pub form_method: Option<String>,
    pub form_action: Option<String>,
    /// 1-based id of the enclosing `<form>`, stable for the page lifetime.
    /// `None` when the control is not in a form.
    pub form_id: Option<u64>,
    /// Host of the link target, when the element is a link.
    pub href_host: Option<String>,
    /// Raw href, when the element is a link (`/checkout?step=2` included).
    pub href: Option<String>,
    pub disabled: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Observation {
    pub url: String,
    pub title: String,
    pub w: f64,
    pub h: f64,
    pub text: String,
    pub actions: Vec<ObservedElement>,
    pub omitted_actions: usize,
    /// Opaque page-side marker used to decide `page_changed`.
    pub marker: serde_json::Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SectionCandidate {
    pub id: String,
    pub tag: String,
    pub heading: String,
    pub snippet: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HitTest {
    pub ok: bool,
    pub x: f64,
    pub y: f64,
    pub topmost: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ActEvent {
    pub kind: String,
    pub target: Option<String>,
    pub ts: f64,
}

/// What the page did between `arm()` and `actLog()` — the "is this control
/// wired / does it autosave" facts.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ActLog {
    pub events: Vec<ActEvent>,
    pub submits: u32,
    pub posts: u32,
    pub navigations: u32,
}

// ---------------------------------------------------------------- probe facts

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DocMetrics {
    pub scroll_width: f64,
    pub client_width: f64,
    pub scroll_height: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OverflowItem {
    pub path: String,
    pub right: f64,
    pub over_by: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlapItem {
    pub a: String,
    pub b: String,
    pub area: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SmallTap {
    pub path: String,
    pub w: f64,
    pub h: f64,
    pub role: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ClippedText {
    pub path: String,
    pub scroll_w: f64,
    pub client_w: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FixedOverlay {
    pub path: String,
    pub coverage: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct JunkItem {
    pub path: String,
    pub token: String,
    pub snippet: String,
}

/// `probe()` — geometry and text facts, computed in code, never by Jev.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProbeReport {
    pub w: f64,
    pub h: f64,
    pub doc: DocMetrics,
    pub overflow: Vec<OverflowItem>,
    pub overlaps: Vec<OverlapItem>,
    pub small_taps: Vec<SmallTap>,
    pub clipped: Vec<ClippedText>,
    pub fixed_overlays: Vec<FixedOverlay>,
    pub cls: f64,
    pub junk: Vec<JunkItem>,
    pub text: String,
}

// ---------------------------------------------------------------- findings

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum FindingKind {
    ConsoleError,
    FailedRequest,
    Geometry,
    Junk,
    Firewall,
}

impl FindingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FindingKind::ConsoleError => "console-error",
            FindingKind::FailedRequest => "failed-request",
            FindingKind::Geometry => "geometry",
            FindingKind::Junk => "junk",
            FindingKind::Firewall => "firewall",
        }
    }
}

/// Who decided the severity. `Code` for arithmetic, `Jev` for judgment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum SeveritySource {
    Code,
    Jev,
}

/// One row of the report. Severity 0 renders under "Facts" and never fails a run.
#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub id: String,
    pub kind: FindingKind,
    pub severity: u8,
    pub severity_source: SeveritySource,
    pub viewport: String,
    pub title: String,
    pub detail: String,
    pub dom_path: Option<String>,
    /// Exploration step that caused it; `None` for initial load.
    pub step: Option<usize>,
    /// The cause is arithmetically after a firewall block — demoted, never a finding.
    pub blocked_by_firewall: bool,
    /// `file:line` inside the run dir, plus any other artefacts.
    pub evidence: Vec<String>,
    /// Path relative to the run's output dir.
    pub screenshot: Option<String>,
    pub score: Option<f64>,
    pub confidence: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ClaimResult {
    pub claim: String,
    pub viewport: String,
    pub verified: bool,
    pub probability: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct StepRecord {
    pub step: usize,
    pub operation: String,
    pub target_id: Option<String>,
    pub target_label: Option<String>,
    pub text: Option<String>,
    pub confidence: f64,
    pub page_changed: bool,
    pub note: Option<String>,
    pub ms: u64,
}

/// Everything one viewport produced. `run_dir` is relative to `QaReport::out_dir`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ViewportFacts {
    pub viewport: String,
    /// Jev drove controls here: the viewport is in `QaConfig::explore` and exploration
    /// actually ran (it is skipped on a login/checkout/account page).
    pub explored: bool,
    pub run_dir: Option<String>,
    pub nav_failed: Option<String>,
    pub probe: Option<ProbeReport>,
    pub steps: Vec<StepRecord>,
    pub findings: Vec<Finding>,
    pub claims: Vec<ClaimResult>,
    pub screenshot: Option<String>,
    pub analysis: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct QaReport {
    pub url: String,
    pub host: String,
    pub section: Option<String>,
    pub claims: Vec<String>,
    pub engine: String,
    pub model: String,
    pub writes: &'static str,
    pub steps_budget: usize,
    pub started_ms: u64,
    pub duration_ms: u64,
    pub jev_calls: u32,
    pub jev_input_tokens: u64,
    pub viewports: Vec<ViewportFacts>,
    pub third_party_suppressed: u32,
    pub notes: Vec<String>,
    pub out_dir: PathBuf,
}

/// The verdict is computed from facts in code (`report::verdict`), never asked of Jev.
#[derive(Clone, Debug)]
pub struct Verdict {
    /// "PASS" | "BROKEN" | "INCONCLUSIVE"
    pub label: &'static str,
    /// 0 clean · 1 findings or unverified claims · 2 QA could not run.
    pub exit_code: i32,
    pub summary: String,
}

/// Parse a JS payload. Every `__miniQA.*` entry point returns
/// `{"ok":true, ...}` or `{"ok":false,"error":"..."}`; a parse failure or
/// `ok:false` is an `Err` carrying the page-side message.
pub fn parse_js<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, String> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("non-JSON reply: {e}"))?;
    if value.get("ok").and_then(|v| v.as_bool()) == Some(false) {
        let msg = value.get("error").and_then(|v| v.as_str()).unwrap_or("unknown JS error");
        return Err(msg.to_string());
    }
    serde_json::from_value(value).map_err(|e| format!("unexpected payload: {e}"))
}

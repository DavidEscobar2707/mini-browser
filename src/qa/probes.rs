//! Parsers for the `window.__miniQA.*` payloads and for the recorder's JSONL streams.
//! Pure functions: no I/O beyond reading files, no browser.
//!
//! Frozen signatures — implement the bodies, do not change the shapes.

use crate::qa::{firewall, parse_js, ActLog, HitTest, Observation, ProbeReport, SectionCandidate};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// `__miniQA.settleState()`.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SettleState {
    pub settled: bool,
    pub ms: u64,
    pub mutations: u32,
    pub inflight: u32,
}

/// One console record from `frontend.jsonl`.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ErrFact {
    pub ts_ms: u64,
    /// `error` | `uncaught` | `unhandledrejection` | `console`
    pub kind: String,
    pub message: String,
    /// Script URL the error came from, when the recorder captured one.
    pub source: Option<String>,
    pub stack: Option<String>,
}

/// One record from `net.jsonl`.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct NetFact {
    pub ts_ms: u64,
    pub method: String,
    pub url: String,
    pub host: String,
    pub status: Option<i64>,
    /// `response` | `error` | `start`
    pub phase: String,
    /// Computed against the QA target host.
    pub first_party: bool,
}

pub fn observation(raw: &str) -> Result<Observation, String> {
    parse_js(raw)
}

pub fn sections(raw: &str) -> Result<Vec<SectionCandidate>, String> {
    // The page script reports `{ok:true, sections:[…]}`; everything else is flat.
    #[derive(Deserialize)]
    struct Wrapper {
        sections: Vec<SectionCandidate>,
    }
    Ok(parse_js::<Wrapper>(raw)?.sections)
}

pub fn probe(raw: &str) -> Result<ProbeReport, String> {
    parse_js(raw)
}

pub fn hit_test(raw: &str) -> Result<HitTest, String> {
    parse_js(raw)
}

pub fn act_log(raw: &str) -> Result<ActLog, String> {
    parse_js(raw)
}

pub fn settled(raw: &str) -> Result<SettleState, String> {
    parse_js(raw)
}

/// Raw shape of a `frontend.jsonl` line — always `kind:"console"` (see
/// `tools/recorder.js` and `src/main.rs`'s IPC dispatch).
#[derive(Deserialize, Default)]
#[serde(default)]
struct ConsoleRecord {
    t: f64,
    level: String,
    args: Vec<String>,
}

/// Console errors / uncaught exceptions / unhandled rejections, oldest first.
///
/// `frontend.jsonl` lines are plain `console.*`/`error`/`unhandledrejection`
/// records; `log`/`info`/`warn`/`debug` levels and any line that fails to
/// parse are silently dropped rather than failing the whole read.
pub fn read_console_errors(run_dir: &Path) -> Result<Vec<ErrFact>, String> {
    let text = match std::fs::read_to_string(run_dir.join("frontend.jsonl")) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("reading frontend.jsonl: {e}")),
    };
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str::<ConsoleRecord>(line.trim()).ok())
        .filter_map(err_fact_from_console)
        .collect())
}

fn err_fact_from_console(rec: ConsoleRecord) -> Option<ErrFact> {
    if !matches!(rec.level.as_str(), "error" | "uncaught" | "unhandledrejection") {
        return None;
    }
    let (message, source) = match rec.level.as_str() {
        "uncaught" => (rec.args.first().cloned().unwrap_or_default(), rec.args.get(1).cloned()),
        "unhandledrejection" => (rec.args.first().cloned().unwrap_or_default(), None),
        _ => (rec.args.join(" "), None),
    };
    Some(ErrFact { ts_ms: rec.t.max(0.0).round() as u64, kind: rec.level, message, source, stack: None })
}

/// Raw shape of a `net.jsonl` line — always `kind:"net"` (see `tools/recorder.js`).
#[derive(Deserialize, Default)]
#[serde(default)]
struct NetRecord {
    t: f64,
    url: String,
    id: Option<u64>,
    phase: String,
    method: Option<String>,
    status: Option<i64>,
}

/// Network records, oldest first, with `first_party` computed (pass the QA host).
///
/// Only failures are surfaced: an explicit `error` phase (fetch/xhr rejected,
/// or a ws/sse `error` event) or a `recv` carrying an HTTP 4xx/5xx status.
/// `id` correlates a `recv`/`error` back to its `send` to recover the method;
/// ws/sse never send one, so those default to `GET`.
pub fn read_net(run_dir: &Path, host: &str) -> Result<Vec<NetFact>, String> {
    let text = match std::fs::read_to_string(run_dir.join("net.jsonl")) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("reading net.jsonl: {e}")),
    };

    let mut methods: HashMap<u64, String> = HashMap::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(rec) = serde_json::from_str::<NetRecord>(line.trim()) else { continue };
        if rec.phase == "send" || rec.phase == "open" {
            if let (Some(id), Some(method)) = (rec.id, &rec.method) {
                methods.insert(id, method.to_ascii_uppercase());
            }
            continue;
        }
        let is_error = rec.phase == "error";
        let is_failed_response = rec.phase == "recv" && rec.status.map_or(false, |s| s >= 400);
        if !is_error && !is_failed_response {
            continue;
        }
        let method =
            rec.id.and_then(|id| methods.get(&id)).cloned().unwrap_or_else(|| "GET".to_string());
        out.push(NetFact {
            ts_ms: rec.t.max(0.0).round() as u64,
            method,
            host: firewall::host_only(&rec.url),
            first_party: firewall::first_party(host, &rec.url),
            status: rec.status,
            phase: if is_error { "error".to_string() } else { "response".to_string() },
            url: rec.url,
        });
    }
    Ok(out)
}

/// WebSocket/SSE connections the page opened, oldest first. v1 cannot inspect
/// outbound frames, so the report lists them rather than implying coverage.
pub fn read_socket_opens(run_dir: &Path, host: &str) -> Result<Vec<NetFact>, String> {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct SocketRecord {
        t: f64,
        url: String,
        api: String,
        phase: String,
    }
    let text = match std::fs::read_to_string(run_dir.join("net.jsonl")) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("reading net.jsonl: {e}")),
    };
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str::<SocketRecord>(line.trim()).ok())
        .filter(|rec| matches!(rec.api.as_str(), "ws" | "sse") && rec.phase == "open")
        .map(|rec| NetFact {
            ts_ms: rec.t.max(0.0).round() as u64,
            method: "GET".to_string(),
            host: firewall::host_only(&rec.url),
            first_party: crate::qa::firewall::first_party(host, &rec.url),
            url: rec.url,
            status: None,
            phase: "open".to_string(),
        })
        .collect())
}

/// `analysis.json` written by `analyze::explain` for this run dir, if present.
pub fn analysis(run_dir: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(run_dir.join("analysis.json")).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mini-qa-probes-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ------------------------------------------------------------- parse_js wrappers

    #[test]
    fn observation_parses_a_literal_payload() {
        let raw = r#"{"ok":true,"url":"https://example.com/","title":"Home","w":1440,"h":900,
            "text":"hello","actions":[],"omitted_actions":0,"marker":42}"#;
        let obs = observation(raw).expect("observation should parse");
        assert_eq!(obs.url, "https://example.com/");
        assert_eq!(obs.title, "Home");
        assert_eq!(obs.omitted_actions, 0);
    }

    #[test]
    fn observation_surfaces_js_error() {
        let raw = r#"{"ok":false,"error":"no document"}"#;
        let err = observation(raw).unwrap_err();
        assert_eq!(err, "no document");
    }

    #[test]
    fn probe_parses_geometry_payload() {
        let raw = r#"{"ok":true,"w":1440,"h":900,"doc":{"scroll_width":1440,"client_width":1440,
            "scroll_height":2000},"overflow":[],"overlaps":[],"small_taps":[],"clipped":[],
            "fixed_overlays":[],"cls":0.02,"junk":[],"text":"body text"}"#;
        let p = probe(raw).expect("probe should parse");
        assert_eq!(p.cls, 0.02);
        assert_eq!(p.doc.scroll_height, 2000.0);
    }

    // ------------------------------------------------------------- read_console_errors

    #[test]
    fn read_console_errors_keeps_only_error_like_levels() {
        let dir = temp_dir("console");
        let lines = [
            r#"{"cmd":"debug","kind":"console","t":100.4,"url":"https://a/","level":"log","args":["hi"]}"#,
            r#"{"cmd":"debug","kind":"console","t":200.6,"url":"https://a/","level":"error","args":["boom","details"]}"#,
            r#"{"cmd":"debug","kind":"console","t":300.0,"url":"https://a/","level":"uncaught","args":["TypeError: x is undefined","app.js:12:4"]}"#,
            r#"{"cmd":"debug","kind":"console","t":400.0,"url":"https://a/","level":"unhandledrejection","args":["\"network down\""]}"#,
            "not json at all",
            "",
        ];
        fs::write(dir.join("frontend.jsonl"), lines.join("\n")).unwrap();

        let facts = read_console_errors(&dir).expect("should read console errors");
        assert_eq!(facts.len(), 3, "log line and the malformed line must be dropped");

        assert_eq!(facts[0].kind, "error");
        assert_eq!(facts[0].ts_ms, 201);
        assert_eq!(facts[0].message, "boom details");
        assert_eq!(facts[0].source, None);

        assert_eq!(facts[1].kind, "uncaught");
        assert_eq!(facts[1].message, "TypeError: x is undefined");
        assert_eq!(facts[1].source.as_deref(), Some("app.js:12:4"));

        assert_eq!(facts[2].kind, "unhandledrejection");
        assert_eq!(facts[2].message, "\"network down\"");
        assert_eq!(facts[2].source, None);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_console_errors_missing_file_is_empty_not_error() {
        let dir = temp_dir("console-missing");
        let facts = read_console_errors(&dir).expect("missing file is not an error");
        assert!(facts.is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    // ------------------------------------------------------------- read_net

    #[test]
    fn read_net_correlates_method_and_filters_to_failures() {
        let dir = temp_dir("net");
        let lines = [
            // Successful fetch: send + 200 recv — must not appear.
            r#"{"cmd":"debug","kind":"net","t":10.0,"url":"https://example.com/ok","api":"fetch","id":1,"phase":"send","method":"GET"}"#,
            r#"{"cmd":"debug","kind":"net","t":20.0,"url":"https://example.com/ok","api":"fetch","id":1,"phase":"recv","status":200,"ms":10}"#,
            // Failing POST: send + 500 recv, first-party.
            r#"{"cmd":"debug","kind":"net","t":30.0,"url":"https://example.com/api/save","api":"fetch","id":2,"phase":"send","method":"post"}"#,
            r#"{"cmd":"debug","kind":"net","t":40.7,"url":"https://example.com/api/save","api":"fetch","id":2,"phase":"recv","status":500,"ms":10}"#,
            // Network-level error, third-party.
            r#"{"cmd":"debug","kind":"net","t":50.0,"url":"https://tracker.example/collect","api":"fetch","id":3,"phase":"send","method":"POST"}"#,
            r#"{"cmd":"debug","kind":"net","t":60.0,"url":"https://tracker.example/collect","api":"fetch","id":3,"phase":"error","error":"Failed to fetch"}"#,
            // WS open/error with no method at all.
            r#"{"cmd":"debug","kind":"net","t":70.0,"url":"wss://example.com/socket","api":"ws","id":4,"phase":"open"}"#,
            r#"{"cmd":"debug","kind":"net","t":80.0,"url":"wss://example.com/socket","api":"ws","id":4,"phase":"error"}"#,
            "garbage",
        ];
        fs::write(dir.join("net.jsonl"), lines.join("\n")).unwrap();

        let facts = read_net(&dir, "example.com").expect("should read net facts");
        assert_eq!(facts.len(), 3, "only the two failing HTTP calls and the ws error should remain");

        let save = &facts[0];
        assert_eq!(save.method, "POST");
        assert_eq!(save.url, "https://example.com/api/save");
        assert_eq!(save.host, "example.com");
        assert_eq!(save.status, Some(500));
        assert_eq!(save.phase, "response");
        assert!(save.first_party);
        assert_eq!(save.ts_ms, 41);

        let tracker = &facts[1];
        assert_eq!(tracker.method, "POST");
        assert_eq!(tracker.phase, "error");
        assert_eq!(tracker.status, None);
        assert!(!tracker.first_party);

        let ws = &facts[2];
        assert_eq!(ws.method, "GET", "ws never sends a method, defaults to GET");
        assert_eq!(ws.phase, "error");
        assert!(ws.first_party);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_net_missing_file_is_empty_not_error() {
        let dir = temp_dir("net-missing");
        let facts = read_net(&dir, "example.com").expect("missing file is not an error");
        assert!(facts.is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    // ------------------------------------------------------------- analysis

    #[test]
    fn analysis_reads_the_analyzer_output_when_present() {
        let dir = temp_dir("analysis");
        let body = serde_json::json!({
            "run_dir": dir.to_string_lossy(),
            "marks": [],
            "flicker": [],
            "remounts": [],
        });
        fs::write(dir.join("analysis.json"), body.to_string()).unwrap();

        let parsed = analysis(&dir).expect("analysis.json should parse");
        assert_eq!(parsed["remounts"].as_array().unwrap().len(), 0);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn analysis_is_none_when_missing_or_invalid() {
        let dir = temp_dir("analysis-missing");
        assert!(analysis(&dir).is_none());

        fs::write(dir.join("analysis.json"), "not json").unwrap();
        assert!(analysis(&dir).is_none());
        fs::remove_dir_all(&dir).ok();
    }
}

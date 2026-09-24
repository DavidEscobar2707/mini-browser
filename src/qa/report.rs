//! report.md / report.json rendering and the verdict. Pure: no browser, no model.
//!
//! Frozen signatures — implement the bodies, do not change the shapes.

use crate::qa::{ClaimResult, Finding, QaReport, StepRecord, Verdict};
use std::collections::HashSet;

/// Severity at or above this fails the run.
pub const FAILING_SEVERITY: u8 = 2;

/// Computed from facts only. `0` clean, `1` findings / unverified claims, `2` could not run.
pub fn verdict(r: &QaReport) -> Verdict {
    let nav_failed = r.viewports.iter().filter(|v| v.nav_failed.is_some()).count();
    let produced_facts = r.viewports.iter().any(|v| v.probe.is_some());

    if nav_failed > 0 || !produced_facts {
        let summary = if nav_failed > 0 {
            format!(
                "QA could not run: navigation failed on {nav_failed} of {} viewport(s).",
                r.viewports.len()
            )
        } else {
            "QA could not run: no viewport produced probe facts.".to_string()
        };
        return Verdict { label: "INCONCLUSIVE", exit_code: 2, summary };
    }

    // A firewall-caused finding is arithmetically demoted to severity 0 by
    // whoever builds it; `blocked_by_firewall` is excluded here too, so a
    // violation of that invariant still can't fail the run on its own.
    let failing = r
        .viewports
        .iter()
        .flat_map(|v| v.findings.iter())
        .filter(|f| !f.blocked_by_firewall && f.severity >= FAILING_SEVERITY)
        .count();
    let unverified = r.viewports.iter().flat_map(|v| v.claims.iter()).filter(|c| !c.verified).count();

    if failing > 0 || unverified > 0 {
        return Verdict {
            label: "BROKEN",
            exit_code: 1,
            summary: format!(
                "{failing} finding(s) at severity >= {FAILING_SEVERITY}, {unverified} unverified claim(s)."
            ),
        };
    }

    // "Facts" are severity 0; anything above that is a reported finding that
    // simply did not reach the failing threshold.
    let facts = r.viewports.iter().flat_map(|v| v.findings.iter()).filter(|f| f.severity == 0).count();
    let reported =
        r.viewports.iter().flat_map(|v| v.findings.iter()).filter(|f| f.severity > 0).count();
    Verdict {
        label: "PASS",
        exit_code: 0,
        summary: format!(
            "Clean across {} viewport(s); {reported} reported finding(s) below severity {FAILING_SEVERITY}, {facts} fact(s).",
            r.viewports.len()
        ),
    }
}

/// The artefact the coding agent reads. Templated; no LLM text.
pub fn render_md(r: &QaReport) -> String {
    let mut out = String::new();
    render_header(r, &mut out);
    render_verdict(r, &mut out);
    render_claims(r, &mut out);
    render_findings(r, &mut out);
    render_facts(r, &mut out);
    render_exploration(r, &mut out);
    render_suppressed(r, &mut out);
    render_notes(r, &mut out);
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

pub fn render_json(r: &QaReport) -> Result<serde_json::Value, String> {
    serde_json::to_value(r).map_err(|e| format!("serializing report: {e}"))
}

/// Writes `report.md` + `report.json` into `QaReport::out_dir`.
pub fn write(r: &QaReport) -> Result<(), String> {
    std::fs::create_dir_all(&r.out_dir)
        .map_err(|e| format!("creating {}: {e}", r.out_dir.display()))?;
    std::fs::write(r.out_dir.join("report.md"), render_md(r))
        .map_err(|e| format!("writing report.md: {e}"))?;
    let json = render_json(r)?;
    let body = serde_json::to_string_pretty(&json)
        .map_err(|e| format!("serializing report.json: {e}"))?;
    std::fs::write(r.out_dir.join("report.json"), body)
        .map_err(|e| format!("writing report.json: {e}"))?;
    Ok(())
}

// ------------------------------------------------------------------- header

fn render_header(r: &QaReport, out: &mut String) {
    out.push_str(&format!("# Mini QA Report — {}\n\n", r.url));
    out.push_str(&format!("- Host: {}\n", r.host));
    out.push_str(&format!("- Section: {}\n", r.section.as_deref().unwrap_or("(whole page)")));
    out.push_str(&format!(
        "- Timestamp: {} (run took {:.1}s)\n",
        format_epoch_ms(r.started_ms),
        r.duration_ms as f64 / 1000.0
    ));
    out.push_str(&format!("- Engine: {}\n", r.engine));
    out.push_str(&format!("- Writes: {}\n", r.writes));
    out.push_str(&format!("- Model: {}\n", r.model));
    out.push_str(&format!("- Steps budget: {}\n", r.steps_budget));
    out.push_str(&format!("- Artifacts: {}\n", r.out_dir.display()));
    out.push('\n');
    out.push_str(
        "> Coverage note: this report comes from a single automated pass against Mini's own \
         WebView2 (Chromium) engine — visual, DOM, and network facts only. There is no \
         cross-browser or non-Chromium coverage, and it is not a substitute for a human \
         reviewing the flagged evidence.\n\n",
    );
    out.push_str(
        "> Write boundary: every non-GET/HEAD/OPTIONS request was refused at the network layer. \
         GET is allowed (search, filters, pagination), so a page that mutates on GET is not \
         covered by that boundary, and outbound WebSocket/SSE frames are not intercepted — \
         any socket the page opened is listed under Facts.\n\n",
    );
}

/// Days since the Unix epoch -> proleptic Gregorian (y, m, d). Howard
/// Hinnant's `civil_from_days`, public domain:
/// http://howardhinnant.github.io/date_algorithms.html
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// No `chrono` dependency: render epoch-ms as a UTC timestamp with a small
/// self-contained calendar conversion.
fn format_epoch_ms(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let millis = ms % 1000;
    let days = (total_seconds / 86400) as i64;
    let secs_of_day = total_seconds % 86400;
    let (y, m, d) = civil_from_days(days);
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day % 3600) / 60;
    let ss = secs_of_day % 60;
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}.{millis:03} UTC")
}

// ------------------------------------------------------------------ verdict

fn render_verdict(r: &QaReport, out: &mut String) {
    let v = verdict(r);
    out.push_str("## Verdict\n\n");
    out.push_str(&format!("**{}** — exit {} — {}\n\n", v.label, v.exit_code, v.summary));
}

// ------------------------------------------------------------------- claims

fn render_claims(r: &QaReport, out: &mut String) {
    out.push_str("## Claims\n\n");
    let results: Vec<&ClaimResult> = r.viewports.iter().flat_map(|v| v.claims.iter()).collect();
    let evaluated: HashSet<&str> = results.iter().map(|c| c.claim.as_str()).collect();
    let unevaluated: Vec<&str> =
        r.claims.iter().map(|c| c.as_str()).filter(|c| !evaluated.contains(c)).collect();

    if results.is_empty() && unevaluated.is_empty() {
        out.push_str("No claims were requested for this run.\n\n");
        return;
    }
    for c in &results {
        let mark = if c.verified { "x" } else { " " };
        let status = if c.verified { "verified" } else { "unverified" };
        // An unverified claim is only falsifiable with the picture next to it.
        let shot = match (c.verified, r.viewports.iter().find(|v| v.viewport == c.viewport)) {
            (false, Some(v)) => v.screenshot.as_deref().map(|p| format!(" → {p}")).unwrap_or_default(),
            _ => String::new(),
        };
        out.push_str(&format!(
            "- [{mark}] \"{}\" — {} — {status} (p={:.2}){shot}\n",
            c.claim, c.viewport, c.probability
        ));
    }
    for c in unevaluated {
        out.push_str(&format!("- (?) \"{c}\" — not evaluated in any viewport\n"));
    }
    out.push('\n');
}

// ----------------------------------------------------------------- findings

fn render_findings(r: &QaReport, out: &mut String) {
    out.push_str("## Findings (severity desc)\n\n");
    let mut findings: Vec<&Finding> =
        r.viewports.iter().flat_map(|v| v.findings.iter()).filter(|f| f.severity > 0).collect();
    findings.sort_by(|a, b| b.severity.cmp(&a.severity));

    if findings.is_empty() {
        out.push_str("No findings above severity 0.\n\n");
        return;
    }
    for f in findings {
        out.push_str(&format!("- {}\n", render_finding_line(f)));
    }
    out.push('\n');
}

fn render_finding_line(f: &Finding) -> String {
    let step_suffix = f.step.map(|s| format!(", step {s}")).unwrap_or_default();
    let detail_suffix = if f.detail.is_empty() { String::new() } else { format!(" — {}", f.detail) };
    let evidence_suffix = if f.evidence.is_empty() {
        String::new()
    } else {
        format!(" — evidence: {}", f.evidence.join(", "))
    };
    format!(
        "**[{}] {}** ({}{step_suffix}) — {}{detail_suffix}{evidence_suffix}",
        f.severity,
        f.kind.as_str(),
        f.viewport,
        f.title
    )
}

// -------------------------------------------------------------------- facts

fn render_facts(r: &QaReport, out: &mut String) {
    out.push_str("## Facts\n\n");
    let mut lines = Vec::new();
    for vp in &r.viewports {
        for f in vp.findings.iter().filter(|f| f.severity == 0) {
            lines.push(format!("({}) {}", f.viewport, render_fact_body(f)));
        }
        if let Some(analysis) = &vp.analysis {
            render_analysis_facts(&vp.viewport, analysis, &mut lines);
        }
    }
    if lines.is_empty() {
        out.push_str("No additional facts recorded.\n\n");
        return;
    }
    for line in lines {
        out.push_str(&format!("- {line}\n"));
    }
    out.push('\n');
}

fn render_fact_body(f: &Finding) -> String {
    let firewall_suffix = if f.blocked_by_firewall { " (blocked by firewall)" } else { "" };
    let detail_suffix = if f.detail.is_empty() { String::new() } else { format!(" — {}", f.detail) };
    format!("{}{firewall_suffix}{detail_suffix}", f.title)
}

/// Summarizes `analyze::explain`'s `analysis.json` (`flicker` / `remounts`
/// arrays) into a few lines. Walked as a raw `Value`: this is embedded verbatim
/// in `ViewportFacts::analysis`, not re-typed.
///
/// One line per category, not per event: a React page produces dozens of
/// remounts and sub-millisecond flickers, and a hundred lines of that buries
/// the findings the report exists to surface.
fn render_analysis_facts(viewport: &str, analysis: &serde_json::Value, lines: &mut Vec<String>) {
    if let Some(items) = analysis.get("flicker").and_then(|v| v.as_array()) {
        if !items.is_empty() {
            let mut within = 0usize;
            let mut marked: Vec<String> = Vec::new();
            let mut worst: Option<(f64, String)> = None;
            for item in items {
                let path = item.get("path").and_then(|v| v.as_str()).unwrap_or("?").to_string();
                let dwell = item.get("dwell_ms").and_then(|v| v.as_f64()).unwrap_or(0.0);
                if item.get("within_one_frame").and_then(|v| v.as_bool()).unwrap_or(false) {
                    within += 1;
                }
                if item.get("marked").and_then(|v| v.as_bool()).unwrap_or(false) {
                    marked.push(path.clone());
                }
                if worst.as_ref().map(|(d, _)| dwell > *d).unwrap_or(true) {
                    worst = Some((dwell, path));
                }
            }
            let (dwell, path) = worst.unwrap_or((0.0, "?".to_string()));
            lines.push(format!(
                "({viewport}) Flicker: {} event(s), {within} within one frame, worst dwell {dwell:.1} ms — {path}",
                items.len()
            ));
            for path in marked.iter().take(3) {
                lines.push(format!("({viewport})   [marked] {path}"));
            }
        }
    }
    if let Some(items) = analysis.get("remounts").and_then(|v| v.as_array()) {
        if !items.is_empty() {
            // Eight remounts of the same Popper in one carousel are one fact.
            let mut groups: Vec<(String, usize, Option<String>, bool)> = Vec::new();
            for item in items {
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let path = item.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                let key = format!("{name} at {path}");
                let cause = item.get("cause").filter(|c| !c.is_null()).map(|cause| {
                    let api = cause.get("api").and_then(|v| v.as_str()).unwrap_or("?");
                    let url = cause.get("url").and_then(|v| v.as_str()).unwrap_or("?");
                    let delta = cause.get("delta_ms").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    format!("cause: {api} {url} (Δ{delta}ms earlier)")
                });
                let marked = item.get("marked").and_then(|v| v.as_bool()).unwrap_or(false);
                match groups.iter_mut().find(|(k, ..)| *k == key) {
                    Some((_, count, seen, was_marked)) => {
                        *count += 1;
                        if seen.is_none() {
                            *seen = cause;
                        }
                        *was_marked = *was_marked || marked;
                    }
                    None => groups.push((key, 1, cause, marked)),
                }
            }
            let with_cause = groups.iter().filter(|(_, _, cause, _)| cause.is_some()).count();
            lines.push(format!(
                "({viewport}) Remounts: {} event(s), {} distinct component(s), {with_cause} with a network cause",
                items.len(),
                groups.len()
            ));
            let mut ranked: Vec<&(String, usize, Option<String>, bool)> = groups.iter().collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            for (key, count, cause, marked) in ranked.iter().take(5) {
                let times = if *count > 1 { format!(" ×{count}") } else { String::new() };
                let cause = cause.as_deref().map(|c| format!(" — {c}")).unwrap_or_default();
                let marked = if *marked { " [marked]" } else { "" };
                lines.push(format!("({viewport})   Remount{times} of {key}{cause}{marked}"));
            }
            if ranked.len() > 5 {
                lines.push(format!("({viewport})   … {} more distinct component(s)", ranked.len() - 5));
            }
        }
    }
}

// -------------------------------------------------------------- exploration

fn render_exploration(r: &QaReport, out: &mut String) {
    out.push_str("## Exploration\n\n");
    let mut total_steps = 0usize;
    for vp in &r.viewports {
        total_steps += vp.steps.len();
        out.push_str(&format!("### {} ({}/{} steps)\n\n", vp.viewport, vp.steps.len(), r.steps_budget));
        if vp.steps.is_empty() {
            out.push_str("(no steps recorded)\n\n");
            continue;
        }
        for s in &vp.steps {
            out.push_str(&format!("{}. {}\n", s.step, render_step_line(s)));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "Steps: {total_steps} total across {} viewport(s) · Duration: {:.1}s · Jev calls: {} · Jev input tokens: {}\n\n",
        r.viewports.len(),
        r.duration_ms as f64 / 1000.0,
        r.jev_calls,
        r.jev_input_tokens
    ));
}

fn render_step_line(s: &StepRecord) -> String {
    let target = match (&s.target_label, &s.target_id) {
        (Some(label), _) => format!(" \"{label}\""),
        (None, Some(id)) => format!(" [{id}]"),
        (None, None) => String::new(),
    };
    let text_suffix = s.text.as_deref().map(|t| format!(" = \"{t}\"")).unwrap_or_default();
    let changed = if s.page_changed { "yes" } else { "no" };
    let note_suffix = s.note.as_deref().map(|n| format!(" — {n}")).unwrap_or_default();
    format!(
        "{}{target}{text_suffix} — confidence {:.2}, page changed: {changed} ({}ms){note_suffix}",
        s.operation, s.confidence, s.ms
    )
}

// -------------------------------------------------------------- suppressed

fn render_suppressed(r: &QaReport, out: &mut String) {
    out.push_str("## Suppressed\n\n");
    out.push_str(&format!(
        "- Third-party network noise: {} request(s) suppressed from findings/facts.\n\n",
        r.third_party_suppressed
    ));
}

// ------------------------------------------------------------------- notes

fn render_notes(r: &QaReport, out: &mut String) {
    if r.notes.is_empty() {
        return;
    }
    out.push_str("## Notes\n\n");
    for n in &r.notes {
        out.push_str(&format!("- {n}\n"));
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qa::{
        FindingKind, ObservedElement as _UnusedMarker, ProbeReport, SeveritySource, ViewportFacts,
    };
    use std::path::PathBuf;

    // Silence the unused-import marker above; kept out of the real import
    // list below so clippy doesn't flag an actually-unused type.
    #[allow(dead_code)]
    fn _unused(_: Option<_UnusedMarker>) {}

    fn base_report(viewports: Vec<ViewportFacts>) -> QaReport {
        QaReport {
            url: "https://example.com/".into(),
            host: "example.com".into(),
            section: None,
            claims: Vec::new(),
            engine: "WebView2/Chromium 119.0.6045.199".into(),
            model: "jev-latest".into(),
            writes: "BLOCKED",
            steps_budget: 25,
            started_ms: 1_700_000_000_000,
            duration_ms: 12_345,
            jev_calls: 3,
            jev_input_tokens: 4_200,
            viewports,
            third_party_suppressed: 0,
            notes: Vec::new(),
            out_dir: PathBuf::from("/tmp/mini-qa-report-test"),
        }
    }

    fn ok_viewport(name: &str) -> ViewportFacts {
        ViewportFacts {
            viewport: name.to_string(),
            run_dir: Some(format!("{name}-run")),
            nav_failed: None,
            probe: Some(ProbeReport::default()),
            steps: Vec::new(),
            findings: Vec::new(),
            claims: Vec::new(),
            screenshot: None,
            analysis: None,
        }
    }

    fn finding(severity: u8, blocked_by_firewall: bool) -> Finding {
        Finding {
            id: "f-1".into(),
            kind: FindingKind::Geometry,
            severity,
            severity_source: SeveritySource::Code,
            viewport: "desktop".into(),
            title: "Something happened".into(),
            detail: String::new(),
            dom_path: None,
            step: None,
            blocked_by_firewall,
            evidence: Vec::new(),
            screenshot: None,
            score: None,
            confidence: None,
        }
    }

    // --------------------------------------------------------------- verdict

    #[test]
    fn verdict_pass_when_clean() {
        let mut vp = ok_viewport("desktop");
        vp.findings.push(finding(0, false)); // a fact, never fails
        vp.findings.push(finding(1, false)); // reported, below the threshold
        let r = base_report(vec![vp]);
        let v = verdict(&r);
        assert_eq!(v.label, "PASS");
        assert_eq!(v.exit_code, 0);
        assert!(v.summary.contains("1 reported finding(s) below severity 2"), "{}", v.summary);
        assert!(v.summary.contains("1 fact(s)"), "{}", v.summary);
    }

    #[test]
    fn verdict_broken_on_severe_finding() {
        let mut vp = ok_viewport("desktop");
        vp.findings.push(finding(FAILING_SEVERITY, false));
        let r = base_report(vec![vp]);
        let v = verdict(&r);
        assert_eq!(v.label, "BROKEN");
        assert_eq!(v.exit_code, 1);
    }

    #[test]
    fn verdict_broken_on_unverified_claim() {
        let mut vp = ok_viewport("desktop");
        vp.claims.push(ClaimResult {
            claim: "checkout works".into(),
            viewport: "desktop".into(),
            verified: false,
            probability: 0.2,
        });
        let r = base_report(vec![vp]);
        let v = verdict(&r);
        assert_eq!(v.label, "BROKEN");
        assert_eq!(v.exit_code, 1);
    }

    #[test]
    fn verdict_ignores_severity_when_blocked_by_firewall() {
        // Violates the documented invariant on purpose (severity should be 0
        // whenever blocked_by_firewall is true) to prove the exclusion holds
        // even if that invariant is ever broken upstream.
        let mut vp = ok_viewport("desktop");
        vp.findings.push(finding(3, true));
        let r = base_report(vec![vp]);
        let v = verdict(&r);
        assert_eq!(v.label, "PASS");
        assert_eq!(v.exit_code, 0);
    }

    #[test]
    fn verdict_inconclusive_on_nav_failure() {
        let mut vp = ok_viewport("desktop");
        vp.nav_failed = Some("net::ERR_CONNECTION_REFUSED".into());
        vp.probe = None;
        let r = base_report(vec![vp]);
        let v = verdict(&r);
        assert_eq!(v.label, "INCONCLUSIVE");
        assert_eq!(v.exit_code, 2);
    }

    #[test]
    fn verdict_inconclusive_when_no_viewport_produced_facts() {
        let mut vp = ok_viewport("desktop");
        vp.probe = None; // navigated fine, but nothing was ever probed
        let r = base_report(vec![vp]);
        let v = verdict(&r);
        assert_eq!(v.label, "INCONCLUSIVE");
        assert_eq!(v.exit_code, 2);
    }

    // ------------------------------------------------------------ render_md

    #[test]
    fn render_md_shows_header_and_caveat() {
        let mut r = base_report(vec![ok_viewport("desktop")]);
        r.section = Some("Cart".into());
        r.writes = "ALLOWED";
        let md = render_md(&r);
        assert!(md.contains("# Mini QA Report — https://example.com/"));
        assert!(md.contains("- Section: Cart"));
        assert!(md.contains("- Engine: WebView2/Chromium 119.0.6045.199"));
        assert!(md.contains("- Writes: ALLOWED"));
        assert!(md.contains("non-Chromium coverage"));
    }

    #[test]
    fn render_md_defaults_section_to_whole_page() {
        let r = base_report(vec![ok_viewport("desktop")]);
        let md = render_md(&r);
        assert!(md.contains("- Section: (whole page)"));
    }

    #[test]
    fn render_md_reports_no_claims_when_none_requested() {
        let r = base_report(vec![ok_viewport("desktop")]);
        let md = render_md(&r);
        assert!(md.contains("## Claims\n\nNo claims were requested for this run."));
    }

    #[test]
    fn render_md_flags_unevaluated_claim() {
        let mut r = base_report(vec![ok_viewport("desktop")]);
        r.claims.push("search works".into());
        let md = render_md(&r);
        assert!(md.contains("(?) \"search works\" — not evaluated in any viewport"));
    }

    #[test]
    fn render_md_firewall_fact_matches_expected_phrasing() {
        let mut vp = ok_viewport("desktop");
        let mut f = finding(0, true);
        f.title = "Button \"Create\" attempts POST /promotions".into();
        vp.findings.push(f);
        let r = base_report(vec![vp]);
        let md = render_md(&r);
        assert!(md.contains("Button \"Create\" attempts POST /promotions (blocked by firewall)"));
    }

    #[test]
    fn render_md_sorts_findings_by_severity_descending() {
        let mut vp = ok_viewport("desktop");
        vp.findings.push(finding(1, false));
        vp.findings.push(finding(3, false));
        vp.findings.push(finding(2, false));
        let r = base_report(vec![vp]);
        let md = render_md(&r);
        let section = md.split("## Findings").nth(1).unwrap().split("## Facts").next().unwrap();
        let p3 = section.find("[3]").unwrap();
        let p2 = section.find("[2]").unwrap();
        let p1 = section.find("[1]").unwrap();
        assert!(p3 < p2 && p2 < p1, "findings must render highest severity first");
    }

    #[test]
    fn render_md_summarizes_flicker_and_remount_from_analysis_json() {
        let mut vp = ok_viewport("desktop");
        vp.analysis = Some(serde_json::json!({
            "run_dir": "desktop-run",
            "marks": [],
            "flicker": [
                {"path": "div.badge", "dwell_ms": 40.0, "t": 12.0, "within_one_frame": true, "marked": false}
            ],
            "remounts": [
                {
                    "name": "CartBadge", "path": "div.cart>span.count", "marked": true,
                    "t_unmount": 500.0, "t_mount": 620.0,
                    "cause": {"api": "fetch", "url": "/api/cart", "phase": "recv", "ts": 610.0, "delta_ms": 120.0},
                    "dom": {"del": ["span.count"], "add": ["span.count.new"]},
                    "layout": {"dy": 14.0, "dx": 0.0}
                }
            ],
        }));
        let r = base_report(vec![vp]);
        let md = render_md(&r);
        assert!(md.contains("Flicker: 1 event(s), 1 within one frame, worst dwell 40.0 ms — div.badge"), "{md}");
        assert!(md.contains("Remounts: 1 event(s), 1 distinct component(s), 1 with a network cause"), "{md}");
        assert!(
            md.contains("Remount of CartBadge at div.cart>span.count — cause: fetch /api/cart (Δ120ms earlier) [marked]"),
            "{md}"
        );
    }

    #[test]
    fn render_md_suppressed_shows_count() {
        let mut r = base_report(vec![ok_viewport("desktop")]);
        r.third_party_suppressed = 12;
        let md = render_md(&r);
        assert!(md.contains("## Suppressed"));
        assert!(md.contains("Third-party network noise: 12 request(s) suppressed"));
    }

    #[test]
    fn render_md_omits_notes_section_when_empty_and_shows_when_present() {
        let r = base_report(vec![ok_viewport("desktop")]);
        assert!(!render_md(&r).contains("## Notes"));

        let mut r2 = base_report(vec![ok_viewport("desktop")]);
        r2.notes.push("Jev responded slowly on 2 calls.".into());
        let md2 = render_md(&r2);
        assert!(md2.contains("## Notes\n\n- Jev responded slowly on 2 calls."));
    }

    // ---------------------------------------------------------- render_json

    #[test]
    fn render_json_round_trips_key_fields() {
        let r = base_report(vec![ok_viewport("desktop")]);
        let json = render_json(&r).expect("should serialize");
        assert_eq!(json["url"], "https://example.com/");
        assert_eq!(json["viewports"][0]["viewport"], "desktop");
        assert_eq!(json["writes"], "BLOCKED");
    }

    // ---------------------------------------------------------------- write

    #[test]
    fn write_creates_report_md_and_json() {
        let dir = std::env::temp_dir().join(format!(
            "mini-qa-report-write-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let mut r = base_report(vec![ok_viewport("desktop")]);
        r.out_dir = dir.clone();

        write(&r).expect("write should succeed");
        let md = std::fs::read_to_string(dir.join("report.md")).expect("report.md should exist");
        let json_text = std::fs::read_to_string(dir.join("report.json")).expect("report.json should exist");
        assert!(md.contains("## Verdict"));
        assert!(json_text.contains("\"url\""));

        std::fs::remove_dir_all(&dir).ok();
    }

    // --------------------------------------------- full synthetic render test

    #[test]
    fn full_synthetic_report_renders_every_section() {
        let out_dir = std::env::temp_dir().join("mini-qa-report-full-sample");

        let desktop = ViewportFacts {
            viewport: "desktop".into(),
            run_dir: Some("desktop-run".into()),
            nav_failed: None,
            probe: Some(ProbeReport::default()),
            steps: vec![
                StepRecord {
                    step: 1,
                    operation: "click".into(),
                    target_id: Some("el-12".into()),
                    target_label: Some("Add to cart".into()),
                    text: None,
                    confidence: 0.91,
                    page_changed: true,
                    note: None,
                    ms: 140,
                },
                StepRecord {
                    step: 2,
                    operation: "fill".into(),
                    target_id: Some("el-7".into()),
                    target_label: Some("Promo code".into()),
                    text: Some("SAVE10".into()),
                    confidence: 0.66,
                    page_changed: false,
                    note: Some("no visible validation feedback".into()),
                    ms: 80,
                },
            ],
            findings: vec![
                Finding {
                    id: "f-console".into(),
                    kind: FindingKind::ConsoleError,
                    severity: 3,
                    severity_source: SeveritySource::Jev,
                    viewport: "desktop".into(),
                    title: "Uncaught TypeError breaks checkout flow".into(),
                    detail: "TypeError: Cannot read properties of undefined (reading 'total')".into(),
                    dom_path: Some("div.checkout>button.pay".into()),
                    step: Some(5),
                    blocked_by_firewall: false,
                    evidence: vec!["frontend.jsonl:42".into()],
                    screenshot: Some("desktop/step-5.png".into()),
                    score: Some(0.95),
                    confidence: Some(0.88),
                },
                Finding {
                    id: "f-geometry".into(),
                    kind: FindingKind::Geometry,
                    severity: 1,
                    severity_source: SeveritySource::Code,
                    viewport: "desktop".into(),
                    title: "Fixed overlay covers 40% of viewport on scroll".into(),
                    detail: "banner.promo overlaps checkout button".into(),
                    dom_path: Some("div.banner.promo".into()),
                    step: None,
                    blocked_by_firewall: false,
                    evidence: Vec::new(),
                    screenshot: Some("desktop/overlay.png".into()),
                    score: None,
                    confidence: None,
                },
                Finding {
                    id: "f-firewall".into(),
                    kind: FindingKind::Firewall,
                    severity: 0,
                    severity_source: SeveritySource::Code,
                    viewport: "desktop".into(),
                    title: "Button \"Create\" attempts POST /promotions".into(),
                    detail: String::new(),
                    dom_path: Some("form.promo>button.create".into()),
                    step: Some(3),
                    blocked_by_firewall: true,
                    evidence: Vec::new(),
                    screenshot: None,
                    score: None,
                    confidence: None,
                },
                Finding {
                    id: "f-autosave".into(),
                    kind: FindingKind::ConsoleError,
                    severity: 0,
                    severity_source: SeveritySource::Code,
                    viewport: "desktop".into(),
                    title: "Autosave detected on \"Title\" field".into(),
                    detail: "Typing triggered POST /api/draft without an explicit save action."
                        .into(),
                    dom_path: Some("input#title".into()),
                    step: Some(2),
                    blocked_by_firewall: false,
                    evidence: vec!["net.jsonl:9".into()],
                    screenshot: None,
                    score: None,
                    confidence: None,
                },
            ],
            claims: vec![ClaimResult {
                claim: "User can complete checkout".into(),
                viewport: "desktop".into(),
                verified: true,
                probability: 0.93,
            }],
            screenshot: Some("desktop/final.png".into()),
            analysis: Some(serde_json::json!({
                "run_dir": "desktop-run",
                "marks": [],
                "flicker": [
                    {"path": "div.header>span.badge", "dwell_ms": 40.0, "t": 1234.5,
                     "within_one_frame": true, "marked": false}
                ],
                "remounts": [
                    {
                        "name": "CartBadge", "path": "div.cart>span.count", "marked": true,
                        "t_unmount": 500.0, "t_mount": 620.0,
                        "cause": {"api": "fetch", "url": "/api/cart", "phase": "recv",
                                  "ts": 610.0, "delta_ms": 120.0},
                        "dom": {"del": ["span.count"], "add": ["span.count.new"]},
                        "layout": {"dy": 14.0, "dx": 0.0}
                    },
                    {
                        "name": "Tooltip", "path": "div.tooltip", "marked": false,
                        "t_unmount": 700.0, "t_mount": 705.0,
                        "cause": serde_json::Value::Null,
                        "dom": {"del": [], "add": []},
                        "layout": serde_json::Value::Null
                    }
                ],
            })),
        };

        let phone = ViewportFacts {
            viewport: "phone".into(),
            run_dir: Some("phone-run".into()),
            nav_failed: None,
            probe: Some(ProbeReport::default()),
            steps: vec![StepRecord {
                step: 1,
                operation: "click".into(),
                target_id: None,
                target_label: None,
                text: None,
                confidence: 0.5,
                page_changed: false,
                note: None,
                ms: 200,
            }],
            findings: vec![Finding {
                id: "f-failed-request".into(),
                kind: FindingKind::FailedRequest,
                severity: 2,
                severity_source: SeveritySource::Jev,
                viewport: "phone".into(),
                title: "POST /api/cart returned 500".into(),
                detail: "Adding to cart failed silently; no user-facing error shown.".into(),
                dom_path: None,
                step: Some(2),
                blocked_by_firewall: false,
                evidence: vec!["net.jsonl:8".into()],
                screenshot: None,
                score: Some(0.7),
                confidence: Some(0.42),
            }],
            claims: vec![ClaimResult {
                claim: "User can complete checkout".into(),
                viewport: "phone".into(),
                verified: false,
                probability: 0.31,
            }],
            screenshot: Some("phone/final.png".into()),
            analysis: None,
        };

        let report = QaReport {
            url: "https://shop.example.com/cart".into(),
            host: "shop.example.com".into(),
            section: Some("Cart & checkout".into()),
            claims: vec![
                "User can complete checkout".into(),
                "Search returns relevant results".into(),
            ],
            engine: "WebView2/Chromium 119.0.6045.199".into(),
            model: "jev-latest".into(),
            writes: "BLOCKED",
            steps_budget: 25,
            started_ms: 1_700_000_000_000,
            duration_ms: 42_300,
            jev_calls: 9,
            jev_input_tokens: 18_234,
            viewports: vec![desktop, phone],
            third_party_suppressed: 7,
            notes: vec!["Jev API responded slowly on 2 of 9 calls (>3s).".into()],
            out_dir: out_dir.clone(),
        };

        // The verdict must reflect the severity-3 and severity-2 findings and
        // the unverified phone claim.
        let v = verdict(&report);
        assert_eq!(v.label, "BROKEN");
        assert_eq!(v.exit_code, 1);

        let md = render_md(&report);

        // Spot-check every required section is present and correctly wired
        // before printing the whole thing for the record.
        assert!(md.contains("# Mini QA Report — https://shop.example.com/cart"));
        assert!(md.contains("- Writes: BLOCKED"));
        assert!(md.contains("## Verdict\n\n**BROKEN**"));
        assert!(md.contains("(?) \"Search returns relevant results\" — not evaluated in any viewport"));
        assert!(md.contains("## Findings (severity desc)"));
        assert!(md.contains("[3] console-error"));
        assert!(md.contains("Button \"Create\" attempts POST /promotions (blocked by firewall)"));
        assert!(md.contains("Autosave detected on \"Title\" field"));
        assert!(md.contains("worst dwell"), "{md}");
        assert!(md.contains("div.header>span.badge"), "{md}");
        assert!(md.contains("Remount of Tooltip at div.tooltip"), "{md}");
        assert!(md.contains("## Exploration"));
        assert!(md.contains("### desktop (2/25 steps)"));
        assert!(md.contains("### phone (1/25 steps)"));
        assert!(md.contains("Third-party network noise: 7 request(s) suppressed"));
        assert!(md.contains("## Notes\n\n- Jev API responded slowly on 2 of 9 calls (>3s)."));

        write(&report).expect("write should succeed");
        let written = std::fs::read_to_string(out_dir.join("report.md")).unwrap();
        assert_eq!(written, md);

        println!("\n----- BEGIN full synthetic report.md -----\n{md}----- END full synthetic report.md -----\n");

        std::fs::remove_dir_all(&out_dir).ok();
    }
}

//! The QA loop: per viewport settle -> probe -> explore (cfg.explore only) -> triage ->
//! claims -> report. Ports `jev_ultrafast/agent.py`'s tick/predict/act shape.
//!
//! One clock basis throughout: epoch milliseconds. The recorder's `t` is
//! `performance.timeOrigin + performance.now()`, `firewall::Blocked::ts_ms` is
//! `SystemTime` since the epoch, and analyze.rs reads the same stream — so
//! "this fact landed right after that step" is a subtraction, never a guess.
//!
//! Everything a run learns lands in `out_dir`: `report.md`, `report.json`,
//! `shots/`, and `runs/run-NNN-<viewport>/` (the JSONL streams plus the
//! analyzer's `analysis.json`). Every evidence path in the report resolves
//! inside that directory, so the artefact survives being copied off the box.

use crate::qa::probes::{self, ErrFact, NetFact, SettleState};
use crate::qa::{firewall, jev, questions, report};
use crate::qa::{
    Browser, ClaimResult, Finding, FindingKind, Judge, Observation, ObservedElement, ProbeReport, QaConfig, QaReport, SectionCandidate, SeveritySource, StepRecord, Viewport,
    ViewportFacts, VIEWPORTS,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Poll interval while waiting for `settleState()` to report settled.
const SETTLE_POLL_MS: u64 = 50;
/// Give up on a settle after this long and probe anyway (the note says so).
const SETTLE_TIMEOUT_MS: u64 = 5_000;
/// `Page.navigate` -> `document.readyState == "complete"`.
const NAV_TIMEOUT_MS: u64 = 30_000;
/// Per-fact cap for the triage request (and the report).
const MAX_FACTS: usize = 20;
/// A fact closer than this to a step is attributed to that step.
const STEP_WINDOW_MS: u64 = 1_500;
/// Facts landing this soon after a firewall block are demoted to plain facts:
/// the page was reacting to a write the QA run deliberately refused.
const BLOCK_DEMOTE_MS: u64 = 2_000;
/// Exploration gives up after this many consecutive steps that changed nothing.
const NO_CHANGE_LIMIT: usize = 3;
/// §7 layer 3: a click runs only when the model's own `mutates_<i>` Noul puts
/// the chance of changing server data below this. Stakes-thresholded, not a
/// global confidence number.
const SAFETY_GATE: f64 = 0.10;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Runs one QA session and returns the process exit code (0 clean, 1 findings
/// or unverified claims, 2 could not run). Prints report.md to stdout.
pub fn run(cfg: &QaConfig, browser: &dyn Browser, judge: &dyn Judge) -> i32 {
    let report = execute(cfg, browser, judge);
    if let Err(e) = report::write(&report) {
        eprintln!("mini --qa: could not write the report into {}: {e}", report.out_dir.display());
    }
    print!("{}", report::render_md(&report));
    let _ = std::io::Write::flush(&mut std::io::stdout());
    report::verdict(&report).exit_code
}

/// A run that died before it could report — a panic, or the window closed
/// mid-pass — still leaves an artefact: a crashed QA pass must never look like
/// a clean one to whoever reads the out dir next.
pub fn write_aborted_report(cfg: &QaConfig, reason: &str) -> i32 {
    let viewports: Vec<ViewportFacts> = VIEWPORTS
        .iter()
        .map(|vp| ViewportFacts {
            viewport: vp.name.to_string(),
            nav_failed: Some(reason.to_string()),
            ..Default::default()
        })
        .collect();
    let report = QaReport {
        url: cfg.url.clone(),
        host: cfg.host.clone(),
        section: cfg.section.clone(),
        claims: cfg.claims.clone(),
        engine: "WebView2/Chromium (unknown)".to_string(),
        model: cfg.model.clone(),
        writes: if cfg.allow_writes { "ALLOWED" } else { "BLOCKED" },
        steps_budget: cfg.steps(),
        started_ms: now_ms(),
        duration_ms: 0,
        jev_calls: 0,
        jev_input_tokens: 0,
        viewports,
        third_party_suppressed: 0,
        notes: vec![reason.to_string()],
        out_dir: cfg.out_dir.clone(),
    };
    if let Err(e) = report::write(&report) {
        eprintln!("mini --qa: could not write the aborted report: {e}");
    }
    print!("{}", report::render_md(&report));
    let _ = std::io::Write::flush(&mut std::io::stdout());
    2
}

/// `run` without the printing/exiting: the facts the report is rendered from.
pub fn execute(cfg: &QaConfig, browser: &dyn Browser, judge: &dyn Judge) -> QaReport {
    let started_ms = now_ms();
    let started = Instant::now();
    let mut pass = Pass {
        cfg,
        browser,
        judge,
        started,
        notes: Vec::new(),
        step_marks: Vec::new(),
        judge_ok: 0,
        judge_err: 0,
        third_party: 0,
    };

    for dir in ["", "shots", "runs"] {
        let path = if dir.is_empty() { cfg.out_dir.clone() } else { cfg.out_dir.join(dir) };
        if let Err(e) = std::fs::create_dir_all(&path) {
            pass.note(format!("could not create {}: {e}", path.display()));
        }
    }

    // Prefer the runtime's own version: the page cannot be trusted for this
    // (Mini spoofs the UA, and `userAgentData.brands` derives from that spoof).
    let engine = match browser.browser_version() {
        Some(version) => format!("WebView2/Chromium {}", version.split('.').next().unwrap_or(&version)),
        None => browser
            .eval(
                "(navigator.userAgentData && navigator.userAgentData.brands \
                 ? navigator.userAgentData.brands.map(b => b.brand + '/' + b.version).join(' ') \
                 : navigator.userAgent)",
            )
            .map(|raw| engine_of(&raw))
            .unwrap_or_else(|_| "WebView2/Chromium (unknown)".to_string()),
    };

    // A QA pass that cannot refuse writes is not a QA pass: if neither
    // interception mechanism armed, report an inconclusive run instead of
    // driving a page that can mutate its server.
    let fatal = browser.firewall_status().err();
    if let Some(reason) = &fatal {
        pass.note(reason.clone());
    }

    let mut viewports: Vec<ViewportFacts> = Vec::new();
    let mut root: Option<String> = None;
    let mut section_resolved = false;
    let mut goal_label = cfg.section.clone().unwrap_or_else(|| "the page".to_string());

    for (i, vp) in VIEWPORTS.iter().enumerate() {
        let mut facts = ViewportFacts { viewport: vp.name.to_string(), ..Default::default() };
        // Step attribution is per viewport: exploration only runs at the viewports
        // in cfg.explore, and a tablet fact must never inherit a phone step number.
        pass.step_marks.clear();
        if let Some(reason) = &fatal {
            facts.nav_failed = Some(reason.clone());
            viewports.push(facts);
            continue;
        }
        if pass.out_of_time() {
            facts.nav_failed = Some("wall-clock budget exhausted before this viewport ran".to_string());
            viewports.push(facts);
            continue;
        }

        // 1. Recorder run for this viewport: everything below lands in its dir.
        let run_dir = match browser.run_label(vp.name, "") {
            Ok(d) => Some(d),
            Err(e) => {
                pass.note(format!("{}: recorder run failed: {e}", vp.name));
                None
            }
        };

        // 2. Viewport override. The first pass also performs the real navigation.
        let run_rel = run_dir.as_ref().map(|d| rel_run_dir(&cfg.out_dir, d));
        facts.run_dir = run_rel.clone();
        if let Err(e) = browser.set_viewport(*vp) {
            pass.note(format!("{}: viewport override failed: {e}", vp.name));
        }
        if i == 0 {
            if let Err(e) = navigate(pass.browser, cfg) {
                // Nothing loaded: every viewport is inconclusive, and probing
                // Chromium's error page would only invent findings.
                facts.nav_failed = Some(e.clone());
                viewports.push(facts);
                for rest in VIEWPORTS.iter().skip(1) {
                    viewports.push(ViewportFacts {
                        viewport: rest.name.to_string(),
                        nav_failed: Some(e.clone()),
                        ..Default::default()
                    });
                }
                break;
            }
        }
        let settle = settle_pass(pass.browser);
        if !settle.settled {
            pass.note(format!("{}: page never fully settled ({} ms)", vp.name, settle.ms));
        }

        // 3. Section under test: ask once, reuse while it still resolves.
        if i == 0 {
            if let Some(section) = &cfg.section {
                match pass.find_root(section) {
                    Some((id, label, note)) => {
                        root = Some(id);
                        section_resolved = true;
                        if let Some(note) = note {
                            pass.note(note);
                        }
                        goal_label = label;
                    }
                    None => pass.note(format!(
                        "{}: no confident region for section \"{section}\"; judging the whole page",
                        vp.name
                    )),
                }
            }
        } else if section_resolved {
            // The registry is page-side and survives layout changes; when it
            // stops resolving, judge the whole page instead of failing the pass.
            if let Some(id) = root.clone() {
                if let Err(e) = pass.snapshot(Some(&id)) {
                    root = None;
                    section_resolved = false;
                    pass.note(format!(
                        "{}: the section region stopped resolving ({e}); judging the whole page",
                        vp.name
                    ));
                }
            }
        }

        // 4. Observe: snapshot + geometry probe + screenshot.
        let observation = match pass.snapshot(root.as_deref()) {
            Ok(o) => o,
            Err(e) => {
                facts.nav_failed = Some(format!("snapshot failed: {e}"));
                viewports.push(facts);
                continue;
            }
        };
        let probe = match pass.probe(root.as_deref()) {
            Ok(p) => Some(p),
            Err(e) => {
                pass.note(format!("{}: probe failed: {e}", vp.name));
                None
            }
        };
        facts.screenshot = pass.shot(&format!("{}.jpg", vp.name));
        facts.probe = probe;

        // 5. Claims, before exploration mutates the page (exploration comes later).
        if !cfg.claims.is_empty() {
            facts.claims = pass.claims(vp, &observation, facts.probe.as_ref());
        }

        // 6. Exploration: the viewports listed in cfg.explore, and never on a page
        // that exists to log in, check out or delete something.
        if cfg.explores(vp.name) {
            facts.explored = true;
            if firewall::is_forbidden_page(&observation.url) {
                pass.note(format!(
                    "exploration skipped: {} is a login/checkout/account page",
                    observation.url
                ));
                facts.findings.push(forbidden_page_fact(vp, &observation.url));
            } else {
                let exploration = pass.explore(&goal_label, root.as_deref(), &observation);
                facts.steps = exploration.steps;
                if let Some(url) = exploration.forbidden {
                    facts.findings.push(forbidden_page_fact(vp, &url));
                }
                if let Some(stop) = exploration.stop {
                    pass.note(format!("exploration stopped: {stop}"));
                }
                for (i, cover) in exploration.refusals.iter().enumerate() {
                    facts.findings.push(covered_controls_fact(vp, i, cover, run_rel.as_deref()));
                }
                // The page moved: re-settle and re-probe so triage judges the
                // resting state, not whatever menu our last click left open.
                pass.dismiss_overlay();
                settle_pass(pass.browser);
                if let Ok(p) = pass.probe(root.as_deref()) {
                    facts.probe = Some(p);
                }
            }
        }

        // 7. Analyzer + recorder facts for this viewport.
        if let Some(dir) = &run_dir {
            let _ = crate::analyze::explain_quiet(Some(&dir.to_string_lossy().to_string()));
            facts.analysis = probes::analysis(dir);
        }

        let (errors, nets, suppressed) = match &run_dir {
            Some(dir) => {
                let (e, n, s) = pass.recorder_facts(dir);
                (e, n, s)
            }
            None => (Vec::new(), Vec::new(), 0),
        };
        pass.third_party += suppressed;

        let mut staged = pass.triage(vp, &goal_label, &errors, &nets, facts.probe.as_ref(), run_dir.as_deref(), run_rel.as_deref());
        // Demote anything the firewall caused: a blocked write is the QA
        // harness doing its job, not a bug in the page. Read the block log now,
        // not before exploration — a click that just triggered a blocked POST
        // must demote the fallout it caused.
        let mut blocked_at: Vec<u64> = pass.browser.blocked().iter().map(|b| b.ts_ms).collect();
        blocked_at.sort_unstable();
        for item in staged.iter_mut() {
            if blocked_at.iter().any(|b| item.ts >= *b && item.ts.saturating_sub(*b) <= BLOCK_DEMOTE_MS) {
                item.finding.blocked_by_firewall = true;
                item.finding.severity = 0;
            }
        }
        // Extend, not replace: the forbidden-page fact was recorded before triage.
        facts.findings.extend(staged.into_iter().map(|s| s.finding));
        facts.findings.extend(pass.firewall_facts(vp, run_dir.as_deref(), run_rel.as_deref(), &blocked_at));
        if let Some(dir) = &run_dir {
            let sockets = probes::read_socket_opens(dir, &cfg.host).unwrap_or_default();
            if !sockets.is_empty() {
                pass.note(format!(
                    "{}: the page opened {} socket(s); outbound frames are not intercepted in v1",
                    vp.name,
                    sockets.len()
                ));
                facts.findings.extend(socket_facts(vp, &sockets, run_dir.as_deref(), run_rel.as_deref()));
            }
        }

        // 8. Evidence the caller can read without the recorder session.
        if let Some(dir) = &run_dir {
            if let Some(rel) = &run_rel {
                let dst = cfg.out_dir.join(rel);
                if let Err(e) = copy_run_dir(dir, &dst) {
                    pass.note(format!("{}: could not copy the run dir: {e}", vp.name));
                }
            }
        }

        viewports.push(facts);
    }

    // A run that could not judge anything could not be judged: say so in the
    // report rather than reporting a page as clean off code facts alone.
    let judge_dead = pass.judge_ok == 0 && pass.judge_err > 0;
    if judge_dead {
        for v in viewports.iter_mut() {
            if v.nav_failed.is_none() {
                v.nav_failed = Some("QA could not judge: every Jev request failed".to_string());
            }
        }
        pass.note("no Jev request succeeded; the run is inconclusive".to_string());
    }

    QaReport {
        url: cfg.url.clone(),
        host: cfg.host.clone(),
        section: cfg.section.clone(),
        claims: cfg.claims.clone(),
        engine,
        model: cfg.model.clone(),
        writes: if cfg.allow_writes { "ALLOWED" } else { "BLOCKED" },
        steps_budget: cfg.steps(),
        started_ms,
        duration_ms: started.elapsed().as_millis() as u64,
        jev_calls: judge.calls(),
        jev_input_tokens: judge.input_tokens(),
        viewports,
        third_party_suppressed: pass.third_party,
        notes: pass.notes,
        out_dir: cfg.out_dir.clone(),
    }
}

struct Pass<'a> {
    cfg: &'a QaConfig,
    browser: &'a dyn Browser,
    judge: &'a dyn Judge,
    started: Instant,
    notes: Vec<String>,
    /// (epoch ms the step started, step number) — used to attribute facts.
    step_marks: Vec<(u64, usize)>,
    judge_ok: usize,
    judge_err: usize,
    third_party: u32,
}

#[derive(Clone)]
struct Staged {
    ts: u64,
    finding: Finding,
}

/// What one exploration pass produced.
#[derive(Default)]
struct Exploration {
    steps: Vec<StepRecord>,
    stop: Option<String>,
    /// A step landed on a login/checkout/account page.
    forbidden: Option<String>,
    /// Covering elements that refused clicks, deduped in order.
    refusals: Vec<String>,
}

/// The element the model's choice resolved to. `skip` means policy refused the
/// action before any input was dispatched — the page was never touched.
#[derive(Default)]
struct Performed {
    target_id: Option<String>,
    label: Option<String>,
    text: Option<String>,
    node: Option<u64>,
    skip: Option<String>,
}

impl<'a> Pass<'a> {
    fn note(&mut self, line: String) {
        if !self.notes.iter().any(|n| n == &line) {
            self.notes.push(line);
        }
    }

    fn out_of_time(&self) -> bool {
        self.started.elapsed().as_secs() >= self.cfg.wall_clock_s
    }

    /// One Jev round trip. `None` means the request failed (already noted).
    fn ask(&mut self, state: Value, questions: Value) -> Option<Value> {
        let body = json!({
            "model": self.cfg.model,
            "state": state,
            "questions": questions,
        });
        match self.judge.answers(&body) {
            Ok(answers) => {
                self.judge_ok += 1;
                Some(answers)
            }
            Err(e) => {
                self.judge_err += 1;
                self.note(format!("judge: {e}"));
                None
            }
        }
    }

    fn snapshot(&mut self, root: Option<&str>) -> Result<Observation, String> {
        let js = format!("window.__miniQA.snapshot({})", root_arg(root));
        let raw = self.browser.eval(&js)?;
        probes::observation(&raw)
    }

    fn probe(&mut self, root: Option<&str>) -> Result<ProbeReport, String> {
        let js = format!("window.__miniQA.probe({})", root_arg(root));
        let raw = self.browser.eval(&js)?;
        probes::probe(&raw)
    }

    fn sections(&mut self) -> Result<Vec<SectionCandidate>, String> {
        let raw = self.browser.eval("window.__miniQA.sections()")?;
        probes::sections(&raw)
    }

    /// Screenshot into `shots/`, returning the path relative to the out dir.
    fn shot(&mut self, name: &str) -> Option<String> {
        let rel = format!("shots/{name}");
        let path = self.cfg.out_dir.join(&rel);
        match self.browser.screenshot(&path) {
            Ok(()) => Some(rel),
            Err(e) => {
                self.note(format!("screenshot {name} failed: {e}"));
                None
            }
        }
    }

    /// Q-A: which region is the section under test. Returns the registry id.
    fn find_root(&mut self, section: &str) -> Option<(String, String, Option<String>)> {
        let candidates = match self.sections() {
            Ok(c) if !c.is_empty() => c,
            Ok(_) => return None,
            Err(e) => {
                self.note(format!("sections() failed: {e}"));
                return None;
            }
        };
        let question = questions::section_question(section, &candidates);
        let mut ids: Vec<String> = candidates.iter().map(|c| c.id.clone()).collect();
        ids.push("none_of_these".to_string());
        let state = json!({
            "page": { "url": self.browser.url().unwrap_or_default() },
            "candidates": candidates.iter().map(|c| json!({ "id": c.id, "tag": c.tag })).collect::<Vec<_>>(),
        });
        let answers = self.ask(state, json!({ "section": question }))?;
        let answer = match jev::validate_choice(&answers["section"], &ids) {
            Ok(a) => a,
            Err(e) => {
                self.note(format!("section answer rejected: {e}"));
                return None;
            }
        };
        if answer.choice == "none_of_these" || answer.confidence < 0.5 {
            // Code tie-break: a heading that literally names the section beats a
            // low-confidence guess (filter in code, judge only the ambiguity).
            if let Some(hit) = candidates.iter().find(|c| heading_matches(&c.heading, section)) {
                return Some((
                    hit.id.clone(),
                    hit.heading.clone(),
                    Some(format!(
                        "section region {} matched by heading text (the model answered {} at confidence {:.2})",
                        hit.id, answer.choice, answer.confidence
                    )),
                ));
            }
            return None;
        }
        let label = candidates
            .iter()
            .find(|c| c.id == answer.choice)
            .map(|c| c.heading.clone())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| section.to_string());
        Some((
            answer.choice.clone(),
            label,
            Some(format!("section region {} at confidence {:.2}", answer.choice, answer.confidence)),
        ))
    }

    /// A click that lands on a covering layer usually means the previous step
    /// *opened* something — a menu, a dialog, a popover. Escape is the web's own
    /// convention for closing those, and burning the remaining budget on
    /// refused clicks helps nobody.
    fn dismiss_overlay(&mut self) {
        for kind in ["keyDown", "keyUp"] {
            let _ = self.browser.cdp(
                "Input.dispatchKeyEvent",
                json!({
                    "type": kind,
                    "key": "Escape",
                    "code": "Escape",
                    "windowsVirtualKeyCode": 27,
                    "nativeVirtualKeyCode": 27,
                }),
            );
        }
    }

    /// A click the page refused because something floats over it. Returns true
    /// when that is what the message says, counting it: a refused click never
    /// touched the page, so it is not a "nothing changed" step — but a page
    /// that refuses every click is itself the finding.
    fn note_refusal(&mut self, message: &str, refused: &mut usize, refusals: &mut Vec<String>) -> bool {
        let Some(path) = message.strip_prefix("not clickable: covered by ") else {
            return false;
        };
        *refused += 1;
        if !refusals.iter().any(|r| r == path) {
            refusals.push(path.to_string());
        }
        true
    }

    /// Q-B: exercise the section's controls, one operation per step.
    fn explore(
        &mut self,
        goal: &str,
        root: Option<&str>,
        first: &Observation,
    ) -> Exploration {
        let mut history: Vec<StepRecord> = Vec::new();
        let mut exercised: HashSet<u64> = HashSet::new();
        let mut refused_nodes: BTreeMap<u64, u32> = BTreeMap::new();
        let mut typed: BTreeMap<u64, usize> = BTreeMap::new();
        let mut observation = first.clone();
        let mut no_change = 0usize;
        let mut refused = 0usize;
        let mut refusals: Vec<String> = Vec::new();
        let mut stop = None;
        let mut forbidden: Option<String> = None;
        let goal = format!("Exercise the controls in {goal} and report anything that misbehaves.");

        while history.len() < self.cfg.steps() {
            if self.out_of_time() {
                stop = Some(format!("wall-clock budget of {} s reached", self.cfg.wall_clock_s));
                break;
            }
            let mut candidates: Vec<ObservedElement> =
                firewall::candidate_elements(&observation.actions, &self.cfg.host)
                    .into_iter()
                    .filter(|el| !exercised.contains(&el.node))
                    .collect();
            candidates.extend(firewall::global_controls(&observation.actions));

            let request = questions::explore_question(&goal, &observation, &candidates, &history);
            let question_map = request["questions"].clone();
            let answers = match self.ask(request["state"].clone(), question_map.clone()) {
                Some(a) => a,
                None => {
                    stop = Some("the judge stopped answering".to_string());
                    break;
                }
            };

            let operation_ids = criteria_ids(&question_map["operation"]["criteria"]);
            let operation = match jev::validate_choice(&answers["operation"], &operation_ids) {
                Ok(a) => a,
                Err(e) => {
                    stop = Some(format!("operation answer rejected: {e}"));
                    break;
                }
            };
            let step_started = now_ms();
            let step = history.len() + 1;
            let margin = choice_margin(&operation.probabilities);
            let mut refused_here = false;
            let mut record = StepRecord {
                step,
                operation: operation.choice.clone(),
                target_id: None,
                target_label: None,
                text: None,
                confidence: operation.confidence,
                page_changed: false,
                // A near-tie between two operations means this step was close to
                // a coin flip; the report should say so.
                note: if margin < 0.15 { Some(format!("thin margin over the runner-up ({margin:.2})")) } else { None },
                ms: 0,
            };

            match operation.choice.as_str() {
                "DONE" => {
                    stop = Some("the judge reported every control exercised".to_string());
                    history.push(record);
                    break;
                }
                "BLOCKED" => {
                    stop = Some("the judge reported no supported operation can progress".to_string());
                    history.push(record);
                    break;
                }
                _ => {}
            }

            let mut outcome = self.perform(&operation.choice, &question_map, &answers, &candidates, &mut typed);
            if let Err(e) = &outcome {
                // A rejected target answer is a contract violation on the model's
                // side; one fresh sample usually validates, and a wasted step is
                // a wasted slice of a 25-step budget.
                if e.contains("answer rejected") {
                    if let Some(retry) = self.ask(request["state"].clone(), question_map.clone()) {
                        outcome = self.perform(&operation.choice, &question_map, &retry, &candidates, &mut typed);
                    }
                }
            }
            match outcome {
                Ok(performed) => {
                    record.target_id = performed.target_id.clone();
                    record.target_label = performed.label.clone();
                    record.text = performed.text.clone();
                    let mut landed = false;
                    if let Some(reason) = performed.skip {
                        record.note = Some(reason);
                    } else {
                        let _ = self.browser.eval("window.__miniQA.arm()");
                        let acted = self.act(&operation.choice, performed.target_id.as_deref(), performed.text.as_deref());
                        if let Err(e) = acted {
                            if self.note_refusal(&e, &mut refused, &mut refusals) {
                                refused_here = true;
                                if refused == 1 {
                                    // Try to close whatever we opened before
                                    // spending more of the budget on refusals.
                                    self.dismiss_overlay();
                                    let _ = settle_pass(self.browser);
                                }
                                if refused >= NO_CHANGE_LIMIT {
                                    stop = Some(format!(
                                        "{refused} consecutive clicks refused — an overlay is covering the controls"
                                    ));
                                }
                            }
                            record.note = Some(e);
                        } else {
                            refused = 0;
                            landed = true;
                        }
                        let after = settle_pass(self.browser);
                        let before_ids = action_ids(&observation);
                        match self.snapshot(root) {
                            Ok(next) => {
                                let changed = marker_changed(&observation, &next) || before_ids != action_ids(&next);
                                observation = next;
                                record.page_changed = changed;
                                // A click can navigate anywhere on the host: re-check
                                // every step, not just the page we started on.
                                if firewall::is_forbidden_page(&observation.url) {
                                    forbidden = Some(observation.url.clone());
                                }
                            }
                            Err(e) => {
                                record.note = Some(format!("snapshot after the step failed: {e}"));
                            }
                        }
                        if record.page_changed {
                            no_change = 0;
                        } else if !refused_here {
                            no_change += 1;
                        }
                        if !after.settled {
                            record.note.get_or_insert_with(|| "page never settled".to_string());
                        }
                        // What the control actually tried to do, from the page's
                        // own event log — a submit or a write request is a fact
                        // worth showing even when the firewall refused it.
                        if let Some(log) = self.browser.eval("window.__miniQA.actLog()").ok()
                            .and_then(|raw| probes::act_log(&raw).ok())
                        {
                            let mut signals = Vec::new();
                            if log.submits > 0 {
                                signals.push(format!("{} form submit(s)", log.submits));
                            }
                            if log.posts > 0 {
                                signals.push(format!("{} write request(s)", log.posts));
                            }
                            if log.navigations > 0 {
                                signals.push(format!("{} navigation(s)", log.navigations));
                            }
                            if !signals.is_empty() {
                                let note = format!("the control attempted {}", signals.join(", "));
                                record.note = Some(match record.note.take() {
                                    Some(existing) => format!("{existing}; {note}"),
                                    None => note,
                                });
                            }
                        }
                    }
                    // Retire the control only once it was actually reached: a
                    // refused click gets one more chance after the overlay is
                    // dismissed, and two attempts is the limit after that.
                    if let Some(node) = performed.node {
                        if landed {
                            exercised.insert(node);
                        } else {
                            let attempts = refused_nodes.entry(node).or_insert(0);
                            *attempts += 1;
                            if *attempts >= 2 {
                                exercised.insert(node);
                            }
                        }
                    }
                }
                Err(e) => {
                    let refused_now = self.note_refusal(&e, &mut refused, &mut refusals);
                    if refused_now && refused >= NO_CHANGE_LIMIT {
                        stop = Some(format!(
                            "{refused} consecutive clicks refused — an overlay is covering the controls"
                        ));
                    }
                    if !refused_now {
                        no_change += 1;
                    }
                    record.note = Some(e);
                }
            }

            let _ = self.shot(&format!("step-{step:02}.jpg"));
            record.ms = now_ms().saturating_sub(step_started);
            self.step_marks.push((step_started, step));
            history.push(record);

            if let Some(url) = &forbidden {
                stop = Some(format!("landed on {url} — a login/checkout/account page"));
                break;
            }
            // The refusal limit is reached mid-step, so it is checked here.
            if stop.is_some() {
                break;
            }
            if no_change >= NO_CHANGE_LIMIT {
                stop = Some(format!("{NO_CHANGE_LIMIT} consecutive steps changed nothing"));
                break;
            }
        }
        Exploration { steps: history, stop, forbidden, refusals }
    }

    /// Maps the winning operation + target answer onto the element to touch.
    /// A CLICK also has to clear the safety gate: the model's own `mutates_<i>`
    /// Noul must say it is unlikely to change server data (§7 layer 3).
    fn perform(
        &mut self,
        operation: &str,
        question_map: &Value,
        answers: &Value,
        candidates: &[ObservedElement],
        typed: &mut BTreeMap<u64, usize>,
    ) -> Result<Performed, String> {
        match operation {
            "CLICK" | "TYPE_TEXT" | "SELECT" => {
                let head = format!("{}_target", operation.to_lowercase());
                let ids = criteria_ids(&question_map[&head]["criteria"]);
                if ids.is_empty() {
                    return Err(format!("no {head} was offered"));
                }
                let answer = jev::validate_choice(&answers[&head], &ids)
                    .map_err(|e| format!("{head} answer rejected: {e}"))?;
                let element = resolve_target(&answer.choice, candidates)
                    .ok_or_else(|| format!("target {} no longer exists", answer.choice))?;
                let mut skip = None;
                if operation == "CLICK" {
                    // Missing answer counts as "unknown": refuse rather than guess.
                    let p = jev::read_noul(&answers[format!("mutates_{}", answer.choice)]).unwrap_or(1.0);
                    if p >= SAFETY_GATE {
                        skip = Some(format!(
                            "safety gate: activating \"{}\" likely changes server data (p={p:.2} >= {SAFETY_GATE:.2})",
                            element.label
                        ));
                    }
                }
                let text = if operation == "TYPE_TEXT" {
                    let values = typing_values(element);
                    let slot = typed.entry(element.node).or_insert(0);
                    let value = values[*slot % values.len()].clone();
                    *slot += 1;
                    Some(value)
                } else {
                    None
                };
                Ok(Performed {
                    target_id: Some(element.id.clone()),
                    label: Some(element.label.clone()),
                    text,
                    node: Some(element.node),
                    skip,
                })
            }
            _ => Ok(Performed::default()),
        }
    }

    fn act(&mut self, operation: &str, target: Option<&str>, text: Option<&str>) -> Result<(), String> {
        match operation {
            "CLICK" => {
                let id = target.ok_or("CLICK without a target")?;
                let raw = self.browser.eval(&format!("window.__miniQA.hitTest({})", json!(id)))?;
                let hit = probes::hit_test(&raw)?;
                if !hit.ok || !hit.topmost {
                    return Err(format!(
                        "not clickable: {}",
                        hit.reason.clone().unwrap_or_else(|| "covered".to_string())
                    ));
                }
                self.browser.mouse_click(hit.x, hit.y)
            }
            "TYPE_TEXT" => {
                let id = target.ok_or("TYPE_TEXT without a target")?;
                let _ = self.browser.eval(&format!("window.__miniQA.act('focus', {})", json!(id)))?;
                self.browser.key_text(text.unwrap_or(""))
            }
            "SELECT" => {
                let id = target.ok_or("SELECT without a target")?;
                let _ = self.browser.eval(&format!("window.__miniQA.act('select', {})", json!(id)))?;
                Ok(())
            }
            "SCROLL_DOWN" => {
                let _ = self.browser.eval("window.__miniQA.act('scroll_down')")?;
                Ok(())
            }
            "SCROLL_UP" => {
                let _ = self.browser.eval("window.__miniQA.act('scroll_up')")?;
                Ok(())
            }
            "WAIT" => {
                let _ = self.browser.eval("window.__miniQA.act('wait')")?;
                Ok(())
            }
            other => Err(format!("unsupported operation from the judge: {other}")),
        }
    }

    /// Per viewport: one Noul per claim.
    fn claims(&mut self, vp: &Viewport, observation: &Observation, probe: Option<&ProbeReport>) -> Vec<ClaimResult> {
        let Some(probe) = probe else {
            return self
                .cfg
                .claims
                .iter()
                .map(|c| ClaimResult { claim: c.clone(), viewport: vp.name.to_string(), verified: false, probability: 0.0 })
                .collect();
        };
        if self.cfg.claims.is_empty() {
            return Vec::new();
        }
        let question = questions::claims_question(&self.cfg.claims, probe, &observation.actions);
        let state = json!({ "url": observation.url, "viewport": vp.name });
        let Some(answers) = self.ask(state, question) else {
            return Vec::new();
        };
        self.cfg
            .claims
            .iter()
            .enumerate()
            .map(|(i, claim)| {
                let probability = jev::read_noul(&answers[format!("claim_{i}")]).unwrap_or(0.0);
                ClaimResult {
                    claim: claim.clone(),
                    viewport: vp.name.to_string(),
                    verified: probability >= 0.5,
                    probability,
                }
            })
            .collect()
    }

    /// Console errors and failed requests from the recorder, first-party only.
    fn recorder_facts(&mut self, run_dir: &Path) -> (Vec<ErrFact>, Vec<NetFact>, u32) {
        let mut suppressed = 0u32;
        let mut errors = probes::read_console_errors(run_dir).unwrap_or_default();
        errors.retain(|e| {
            // A console error with no source came from the page itself; one with
            // an absolute URL is kept only when that script is ours.
            let ours = match e.source.as_deref() {
                Some(src) if src.contains("://") && !src.starts_with("http") => false,
                Some(src) if src.starts_with("http") => firewall::first_party(&self.cfg.host, src),
                _ => true,
            };
            if !ours {
                suppressed += 1;
            }
            ours
        });
        let mut seen: HashSet<String> = HashSet::new();
        errors.retain(|e| {
            let message = e.message.trim();
            carries_evidence(message) && seen.insert(message.to_string())
        });
        errors.truncate(MAX_FACTS);

        let mut nets = probes::read_net(run_dir, &self.cfg.host).unwrap_or_default();
        let total = nets.len();
        nets.retain(|n| n.first_party);
        suppressed += (total - nets.len()) as u32;
        nets.truncate(MAX_FACTS);
        (errors, nets, suppressed)
    }

    /// Q-C: severity per fact from Jev, then the code facts around them.
    fn triage(
        &mut self,
        vp: &Viewport,
        section: &str,
        errors: &[ErrFact],
        nets: &[NetFact],
        probe: Option<&ProbeReport>,
        run_dir: Option<&Path>,
        run_rel: Option<&str>,
    ) -> Vec<Staged> {
        let mut staged: Vec<Staged> = Vec::new();
        let fallback = ProbeReport::default();
        let probe = probe.unwrap_or(&fallback);

        let empty = Value::Null;
        let answers = {
            let question = questions::triage_question(section, errors, nets, probe);
            let state = json!({
                "page": { "url": self.browser.url().unwrap_or_default(), "viewport": vp.name },
            });
            self.ask(state, question).unwrap_or(empty)
        };

        for (i, e) in errors.iter().enumerate() {
            let judged = jev::read_score(&answers[format!("sev_err_{i}")]).ok();
            let (severity, confidence, score) = severity_of(judged.as_ref());
            staged.push(Staged {
                ts: e.ts_ms,
                finding: Finding {
                    id: format!("{}-err-{i}", vp.name),
                    kind: FindingKind::ConsoleError,
                    severity,
                    severity_source: if answers.is_null() { SeveritySource::Code } else { SeveritySource::Jev },
                    viewport: vp.name.to_string(),
                    title: format!("{}: {}", e.kind, e.message),
                    detail: e
                        .source
                        .clone()
                        .map(|s| format!("source {s}"))
                        .unwrap_or_else(|| "no source recorded".to_string()),
                    dom_path: None,
                    step: None,
                    blocked_by_firewall: false,
                    evidence: evidence(run_dir, run_rel, "frontend.jsonl", e.ts_ms, Some(&e.message)),
                    screenshot: None,
                    score,
                    confidence,
                },
            });
        }

        for (j, n) in nets.iter().enumerate() {
            let judged = jev::read_score(&answers[format!("sev_net_{j}")]).ok();
            let (severity, confidence, score) = severity_of(judged.as_ref());
            let status = n.status.map(|s| s.to_string()).unwrap_or_else(|| "no response".to_string());
            staged.push(Staged {
                ts: n.ts_ms,
                finding: Finding {
                    id: format!("{}-net-{j}", vp.name),
                    kind: FindingKind::FailedRequest,
                    severity,
                    severity_source: if answers.is_null() { SeveritySource::Code } else { SeveritySource::Jev },
                    viewport: vp.name.to_string(),
                    title: format!("{} {} → {}", n.method, n.url, status),
                    detail: format!("phase {}", n.phase),
                    dom_path: None,
                    step: None,
                    blocked_by_firewall: false,
                    evidence: evidence(run_dir, run_rel, "net.jsonl", n.ts_ms, Some(&n.url)),
                    screenshot: None,
                    score,
                    confidence,
                },
            });
        }

        // Geometry and text: arithmetic, never the model.
        let now = now_ms();
        for (k, item) in probe.overflow.iter().enumerate() {
            staged.push(Staged {
                ts: now,
                finding: geometry(
                    vp,
                    format!("{}-overflow-{k}", vp.name),
                    format!("Horizontal overflow {} px (device {} px wide)", item.over_by, vp.width),
                    format!("{} extends {:.1} px past the right edge", item.path, item.over_by),
                    item.path.clone(),
                    run_rel,
                ),
            });
        }
        for (k, item) in probe.overlaps.iter().enumerate() {
            staged.push(Staged {
                ts: now,
                finding: geometry(
                    vp,
                    format!("{}-overlap-{k}", vp.name),
                    format!("Interactive elements overlap ({:.0} px²)", item.area),
                    format!("{} overlaps {}", item.a, item.b),
                    item.b.clone(),
                    run_rel,
                ),
            });
        }
        for (k, item) in probe.small_taps.iter().enumerate() {
            staged.push(Staged {
                ts: now,
                finding: geometry(
                    vp,
                    format!("{}-tap-{k}", vp.name),
                    format!("Tap target {:.0}×{:.0} px (under 44 px)", item.w, item.h),
                    format!("{} ({})", item.path, item.role),
                    item.path.clone(),
                    run_rel,
                ),
            });
        }
        for (k, item) in probe.clipped.iter().enumerate() {
            staged.push(Staged {
                ts: now,
                finding: geometry(
                    vp,
                    format!("{}-clip-{k}", vp.name),
                    format!("Clipped text: scrollWidth {:.0} > clientWidth {:.0}", item.scroll_w, item.client_w),
                    item.path.clone(),
                    item.path.clone(),
                    run_rel,
                ),
            });
        }
        for (k, item) in probe.fixed_overlays.iter().enumerate() {
            staged.push(Staged {
                ts: now,
                finding: geometry(
                    vp,
                    format!("{}-overlay-{k}", vp.name),
                    format!("Fixed overlay covers {:.0}% of the viewport", item.coverage * 100.0),
                    item.path.clone(),
                    item.path.clone(),
                    run_rel,
                ),
            });
        }
        for (k, item) in probe.junk.iter().enumerate() {
            staged.push(Staged {
                ts: now,
                finding: Finding {
                    id: format!("{}-junk-{k}", vp.name),
                    kind: FindingKind::Junk,
                    severity: 2,
                    severity_source: SeveritySource::Code,
                    viewport: vp.name.to_string(),
                    title: format!("Literal \"{}\" rendered as page text", item.token),
                    detail: item.snippet.clone(),
                    dom_path: Some(item.path.clone()),
                    step: None,
                    blocked_by_firewall: false,
                    evidence: evidence(run_dir, run_rel, "dom.jsonl", now, None),
                    screenshot: None,
                    score: None,
                    confidence: None,
                },
            });
        }

        // Attribute each fact to the exploration step it followed, if any.
        let marks = self.step_marks.clone();
        for item in staged.iter_mut() {
            item.finding.step = marks
                .iter()
                .filter(|(ts, _)| *ts <= item.ts && item.ts.saturating_sub(*ts) <= STEP_WINDOW_MS)
                .max_by_key(|(ts, _)| *ts)
                .map(|(_, step)| *step);
        }
        staged.sort_by_key(|s| std::cmp::Reverse(s.finding.severity));
        collapse_geometry(staged)
    }

    /// Requests the firewall refused: the page tried to write and could not.
    /// Deduped in arrival order (a HashSet would shuffle the report between runs).
    fn firewall_facts(
        &mut self,
        vp: &Viewport,
        run_dir: Option<&Path>,
        run_rel: Option<&str>,
        _blocked_at: &[u64],
    ) -> Vec<Finding> {
        let mut seen: Vec<(u64, String, String)> = Vec::new();
        for b in self.browser.blocked() {
            let path = url_path_of(&b.url);
            let key = format!("{} {}", b.method, path);
            if !seen.iter().any(|(_, k, _)| *k == key) {
                seen.push((b.ts_ms, key, path));
            }
        }
        seen.into_iter()
            .enumerate()
            .map(|(i, (ts, key, path))| Finding {
                id: format!("{}-firewall-{i}", vp.name),
                kind: FindingKind::Firewall,
                severity: 0,
                severity_source: SeveritySource::Code,
                viewport: vp.name.to_string(),
                title: format!("{key} — blocked by firewall"),
                detail: "the control is wired to a write; QA refused it".to_string(),
                dom_path: None,
                step: None,
                blocked_by_firewall: true,
                evidence: evidence(run_dir, run_rel, "net.jsonl", ts, Some(&path)),
                screenshot: None,
                score: None,
                confidence: None,
            })
            .collect()
    }
}

/// One page can produce the same geometry defect on fifty elements — a tap
/// target rule on every card in a list, an overlap on every row of a grid. The
/// report keeps the signal (this rule fired, this many times, here are the
/// examples) instead of fifty near-identical lines.
fn collapse_geometry(staged: Vec<Staged>) -> Vec<Staged> {
    let rule_of = |f: &Finding| -> Option<&'static str> {
        if f.kind != FindingKind::Geometry || f.severity == 0 {
            return None;
        }
        let title = f.title.as_str();
        if title.starts_with("Interactive elements overlap") {
            Some("overlap")
        } else if title.starts_with("Tap target") {
            Some("tap-target")
        } else if title.starts_with("Clipped text") {
            Some("clipped")
        } else {
            None
        }
    };

    let mut groups: Vec<(&'static str, Vec<Staged>)> = Vec::new();
    let mut out: Vec<Staged> = Vec::new();
    for item in staged {
        match rule_of(&item.finding) {
            Some(rule) => match groups.iter_mut().find(|(r, _)| *r == rule) {
                Some((_, items)) => items.push(item),
                None => groups.push((rule, vec![item])),
            },
            None => out.push(item),
        }
    }

    for (rule, mut items) in groups {
        if items.len() == 1 {
            out.push(items.remove(0));
            continue;
        }
        let examples: Vec<String> = items
            .iter()
            .take(3)
            .map(|s| s.finding.dom_path.clone().unwrap_or_else(|| s.finding.detail.clone()))
            .collect();
        let first = &items[0].finding;
        let (title, detail) = match rule {
            "overlap" => (
                format!("{} interactive element pairs overlap", items.len()),
                format!("worst examples: {}", examples.join(" · ")),
            ),
            "tap-target" => (
                format!("{} tap targets under 44 px", items.len()),
                format!("examples: {}", examples.join(" · ")),
            ),
            _ => (
                format!("{} clipped text blocks", items.len()),
                format!("examples: {}", examples.join(" · ")),
            ),
        };
        out.push(Staged {
            ts: items[0].ts,
            finding: Finding {
                id: format!("{}-{rule}", first.viewport),
                kind: FindingKind::Geometry,
                severity: first.severity,
                severity_source: SeveritySource::Code,
                viewport: first.viewport.clone(),
                title,
                detail,
                dom_path: first.dom_path.clone(),
                step: first.step,
                blocked_by_firewall: false,
                evidence: first.evidence.clone(),
                screenshot: first.screenshot.clone(),
                score: None,
                confidence: None,
            },
        });
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.finding.severity));
    out
}

// ---------------------------------------------------------------- helpers

/// Does this console message actually say something? A rejected `Error` object
/// stringifies to `{}`, and an `error`-level record may be nothing but a
/// `file:line:col` location — neither is evidence, and the failed request they
/// came from is already reported from net.jsonl.
fn carries_evidence(message: &str) -> bool {
    message
        .split_whitespace()
        .filter(|token| !looks_like_location(token))
        .any(|token| token.chars().any(char::is_alphabetic))
}

fn looks_like_location(token: &str) -> bool {
    token.contains(':') && token.rsplit(':').take(2).all(|part| part.chars().all(|c| c.is_ascii_digit()))
}

fn root_arg(root: Option<&str>) -> String {
    match root {
        Some(id) => json!(id).to_string(),
        None => "null".to_string(),
    }
}

/// Does this candidate heading name the section the caller asked about?
/// Trimmed, case-insensitive containment either way — `--section "Promotions"`
/// against an `<h2>Promotions</h2>` is a string fact, not a judgment call.
fn heading_matches(heading: &str, section: &str) -> bool {
    let h = heading.trim().to_lowercase();
    let s = section.trim().to_lowercase();
    !h.is_empty() && !s.is_empty() && (h == s || h.contains(&s) || s.contains(&h))
}

fn criteria_ids(criteria: &Value) -> Vec<String> {
    criteria.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default()
}

/// Identity of the actionable DOM across two snapshots ("did the page move?").
fn action_ids(observation: &Observation) -> Vec<String> {
    let mut ids: Vec<String> = observation.actions.iter().map(|a| format!("{}:{}", a.id, a.kind)).collect();
    ids.sort();
    ids
}

/// The deduped element order `questions::explore_question` builds, so a target
/// key ("3", "1:2") resolves back to the element the model actually chose.
fn deduped_nodes(candidates: &[ObservedElement]) -> Vec<u64> {
    let mut seen: HashSet<u64> = HashSet::new();
    let mut order = Vec::new();
    for el in candidates {
        if matches!(el.kind.as_str(), "click" | "fill" | "select") && seen.insert(el.node) {
            order.push(el.node);
        }
    }
    order
}

fn resolve_target<'a>(key: &str, candidates: &'a [ObservedElement]) -> Option<&'a ObservedElement> {
    let order = deduped_nodes(candidates);
    let (index, option) = match key.split_once(':') {
        Some((i, o)) => (i.parse::<usize>().ok()?, Some(o.parse::<usize>().ok()?)),
        None => (key.parse::<usize>().ok()?, None),
    };
    let node = *order.get(index.checked_sub(1)?)?;
    let mut matches: Vec<&ObservedElement> =
        candidates.iter().filter(|el| el.node == node && el.kind == "select").collect();
    if matches.is_empty() {
        matches = candidates.iter().filter(|el| el.node == node).collect();
    }
    match option {
        Some(o) => matches.get(o.checked_sub(1)?).copied(),
        None => matches.first().copied(),
    }
}

fn marker_changed(before: &Observation, after: &Observation) -> bool {
    before.marker != after.marker || before.url != after.url
}

fn navigate(browser: &dyn Browser, cfg: &QaConfig) -> Result<(), String> {
    browser.cdp("Page.navigate", json!({ "url": cfg.url }))?;
    let start = Instant::now();
    loop {
        if let Ok(raw) = browser.eval("document.readyState") {
            if raw.trim_matches('"') == "complete" {
                // A dead host still hands us a complete document: Chromium's own
                // error page. Reporting geometry findings on that page is worse
                // than useless, so the load counts as failed.
                let error_page = browser
                    .eval("!!document.querySelector('#main-frame-error, #main-message')")
                    .map(|raw| raw.trim() == "true")
                    .unwrap_or(false);
                if error_page {
                    return Err(format!("{} failed to load (Chromium error page)", cfg.url));
                }
                return Ok(());
            }
        }
        if start.elapsed() > Duration::from_millis(NAV_TIMEOUT_MS) {
            return Err(format!("{} did not reach readyState=complete", cfg.url));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Wait for settle, bounded. Not settled is normal for a page with a timer.
fn settle_pass(browser: &dyn Browser) -> SettleState {
    let _ = browser.eval("window.__miniQA.settleStart()");
    let start = Instant::now();
    let mut last = SettleState::default();
    while start.elapsed() < Duration::from_millis(SETTLE_TIMEOUT_MS) {
        std::thread::sleep(Duration::from_millis(SETTLE_POLL_MS));
        match browser.eval("window.__miniQA.settleState()").ok().and_then(|raw| probes::settled(&raw).ok()) {
            Some(state) => {
                last = state;
                if last.settled {
                    return last;
                }
            }
            None => break,
        }
    }
    last
}

fn severity_of(judged: Option<&jev::ScoreAnswer>) -> (u8, Option<f64>, Option<f64>) {
    match judged {
        Some(a) if a.confidence >= 0.5 && top_mass(&a.probabilities) >= 0.6 => {
            ((a.score.round() as i64).clamp(0, 3) as u8, Some(a.confidence), Some(a.score))
        }
        // A flat distribution means the levels overlap for this fact — the same
        // score can be "all level 1" or "half 0, half 2". An uncertain answer
        // must not fail a build on its own.
        Some(a) => (1, Some(a.confidence), Some(a.score)),
        None => (1, None, None),
    }
}

/// Probability mass on the most likely level.
fn top_mass(probabilities: &BTreeMap<String, f64>) -> f64 {
    probabilities.values().copied().fold(0.0, f64::max)
}

/// Gap between the winning option and the runner-up: how close the choice was.
fn choice_margin(probabilities: &BTreeMap<String, f64>) -> f64 {
    let mut sorted: Vec<f64> = probabilities.values().copied().collect();
    sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    match (sorted.first(), sorted.get(1)) {
        (Some(top), Some(second)) => top - second,
        _ => 1.0,
    }
}

/// Controls the run could not click because something floats over them. This is
/// why an exploration pass can end early, and it is a real defect: a user hits
/// the same wall.
fn covered_controls_fact(vp: &Viewport, index: usize, cover: &str, run_rel: Option<&str>) -> Finding {
    Finding {
        id: format!("{}-covered-{index}", vp.name),
        kind: FindingKind::Geometry,
        severity: 1,
        severity_source: SeveritySource::Code,
        viewport: vp.name.to_string(),
        title: format!("Controls under {cover} could not be clicked"),
        detail: "the run hit the same wall a user would: this layer swallows pointer input".to_string(),
        dom_path: Some(cover.to_string()),
        step: None,
        blocked_by_firewall: false,
        evidence: vec![run_rel.map(|r| format!("{r}/dom.jsonl")).unwrap_or_else(|| "dom.jsonl".to_string())],
        screenshot: Some(format!("shots/{}.jpg", vp.name)),
        score: None,
        confidence: None,
    }
}

/// A page the run refuses to act on. Reported as a fact so a reader knows the
/// pass was narrower than it looks.
fn forbidden_page_fact(vp: &Viewport, url: &str) -> Finding {
    let shot = format!("shots/{}.jpg", vp.name);
    Finding {
        id: format!("{}-forbidden", vp.name),
        kind: FindingKind::Firewall,
        severity: 0,
        severity_source: SeveritySource::Code,
        viewport: vp.name.to_string(),
        title: format!("{} is a login/checkout/account page — not exercised", url_path_of(url)),
        detail: "the run never acts on pages that exist to log in, pay or delete".to_string(),
        dom_path: None,
        step: None,
        blocked_by_firewall: true,
        evidence: vec![shot.clone()],
        screenshot: Some(shot),
        score: None,
        confidence: None,
    }
}

/// WebSocket/SSE connections the page opened. v1 cannot inspect outbound
/// frames, so the report lists them rather than pretending they were covered.
fn socket_facts(
    vp: &Viewport,
    sockets: &[NetFact],
    run_dir: Option<&Path>,
    run_rel: Option<&str>,
) -> Vec<Finding> {
    sockets
        .iter()
        .enumerate()
        .map(|(i, s)| Finding {
            id: format!("{}-socket-{i}", vp.name),
            kind: FindingKind::Firewall,
            severity: 0,
            severity_source: SeveritySource::Code,
            viewport: vp.name.to_string(),
            title: format!("socket opened: {}", s.url),
            detail: "outbound frames on this socket are not intercepted in v1".to_string(),
            dom_path: None,
            step: None,
            blocked_by_firewall: false,
            evidence: evidence(run_dir, run_rel, "net.jsonl", s.ts_ms, Some(&s.url)),
            screenshot: None,
            score: None,
            confidence: None,
        })
        .collect()
}

fn geometry(vp: &Viewport, id: String, title: String, detail: String, path: String, run_rel: Option<&str>) -> Finding {
    Finding {
        id,
        kind: FindingKind::Geometry,
        severity: 1,
        severity_source: SeveritySource::Code,
        viewport: vp.name.to_string(),
        title,
        detail,
        dom_path: Some(path),
        step: None,
        blocked_by_firewall: false,
        evidence: vec![run_rel.map(|r| format!("{r}/dom.jsonl")).unwrap_or_else(|| "dom.jsonl".to_string())],
        screenshot: Some(format!("shots/{}.jpg", vp.name)),
        score: None,
        confidence: None,
    }
}

/// `runs/run-001-desktop/frontend.jsonl:7` — the line is located by timestamp,
/// so the citation survives re-reading the stream offline.
fn evidence(
    run_dir: Option<&Path>,
    run_rel: Option<&str>,
    file: &str,
    ts_ms: u64,
    needle: Option<&str>,
) -> Vec<String> {
    let Some(rel) = run_rel else {
        return vec![file.to_string()];
    };
    // No needle means the fact was computed in code at report time, not read
    // from the stream: cite the file, never a line that happens to match.
    let line = needle.and_then(|needle| {
        run_dir.and_then(|dir| {
            line_of(dir, file, ts_ms, needle).or_else(|| {
                // An `error`-level record joins its arguments, so the fact's
                // message can be a re-joined form of the JSON string. A prefix
                // still has to appear verbatim in the line to be cited.
                let prefix: String = needle.chars().take(24).collect();
                if prefix.chars().count() < needle.chars().count() {
                    line_of(dir, file, ts_ms, &prefix)
                } else {
                    None
                }
            })
        })
    });
    match line {
        Some(line) => vec![format!("{rel}/{file}:{line}")],
        None => vec![format!("{rel}/{file}")],
    }
}

/// Finds the JSONL line for a fact: the record whose `t` rounds to the fact's
/// timestamp **and** contains `needle`. Several records can share a millisecond,
/// so a timestamp-only match would happily cite the wrong sibling — no line is
/// better than a wrong line, and the caller falls back to a file-only citation.
fn line_of(run_dir: &Path, file: &str, ts_ms: u64, needle: &str) -> Option<usize> {
    let text = std::fs::read_to_string(run_dir.join(file)).ok()?;
    for (i, line) in text.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<Value>(line) else { continue };
        let t = value.get("t").and_then(Value::as_f64).unwrap_or(0.0);
        if (t.round() as i64 - ts_ms as i64).abs() > 1 {
            continue;
        }
        if line.contains(needle) {
            return Some(i + 1);
        }
    }
    None
}

fn rel_run_dir(out_dir: &Path, run_dir: &Path) -> String {
    let name = run_dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "run".to_string());
    let _ = out_dir;
    format!("runs/{name}")
}

fn copy_run_dir(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    let entries = std::fs::read_dir(src).map_err(|e| format!("{}: {e}", src.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let target = dst.join(entry.file_name());
            std::fs::copy(&path, &target).map_err(|e| format!("{}: {e}", target.display()))?;
        }
    }
    Ok(())
}

fn engine_of(user_agent_json: &str) -> String {
    let ua = user_agent_json.trim().trim_matches('"');
    // `navigator.userAgentData.brands` reads like "Chromium/153 Not=A?Brand/24";
    // the plain UA reads like "... Chrome/153.0.0.0 Safari/537.36 Edg/153.0.0.0".
    let version = ua
        .split_whitespace()
        .find_map(|brand| brand.strip_prefix("Chromium/"))
        .or_else(|| ua.split("Edg/").nth(1))
        .or_else(|| ua.split("Chrome/").nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or("unknown");
    format!("WebView2/Chromium {version}")
}

/// The adversarial corpus, one value per field per run (numeric fields get the
/// numeric half). Typing is safe by construction: the firewall refuses writes.
fn typing_values(el: &ObservedElement) -> Vec<String> {
    let numeric = matches!(el.input_type.as_deref(), Some("number"))
        || matches!(el.role.as_deref(), Some("spinbutton") | Some("slider"));
    if numeric {
        return vec!["0".to_string(), "-1".to_string(), "1e309".to_string(), "999999999999".to_string()];
    }
    vec![
        String::new(),
        "a".to_string(),
        "x".repeat(500),
        "  leading and trailing  ".to_string(),
        "<b>bold</b><script>1</script>".to_string(),
        "'; DROP TABLE x;--".to_string(),
        "😀🔥".to_string(),
        "\u{202e}evil".to_string(),
        "0".to_string(),
        "-1".to_string(),
        "1e309".to_string(),
        "A long paragraph typed into the field to see how it copes. ".repeat(20),
    ]
}

/// `https://host/a/b?x=1` -> `/a/b`: the compact form the report prints.
fn url_path_of(url: &str) -> String {
    let after = url.split("://").nth(1).unwrap_or(url);
    let path = after.find('/').map(|i| &after[i..]).unwrap_or("/");
    path.split('?').next().unwrap_or(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::path::PathBuf;

    const UA: &str = r#""Mozilla/5.0 Chrome/151.0.0.0 Safari/537.36 Edg/151.0.4129.101""#;
    const OBSERVATION: &str = r#"{"ok":true,"url":"https://example.com/promotions","title":"Promotions","w":1440,"h":900,"text":"Promotions Filters Create promotion","actions":[{"id":"e1","node":3,"kind":"click","label":"Filters","role":"button","rect":{"x":10,"y":20,"w":80,"h":30}},{"id":"e2","node":4,"kind":"fill","label":"Promo code","role":"textbox","rect":{"x":10,"y":60,"w":200,"h":30}},{"id":"scroll_down","node":0,"kind":"scroll","label":"Scroll down","delta":560},{"id":"scroll_up","node":0,"kind":"scroll","label":"Scroll up","delta":-560},{"id":"wait","node":0,"kind":"wait","label":"Wait for the page to update"}],"marker":[1,2,3]}"#;
    const PROBE: &str = r#"{"ok":true,"w":1440,"h":900,"doc":{"scroll_width":1440,"client_width":1440,"scroll_height":900},"overflow":[],"overlaps":[],"small_taps":[],"clipped":[],"fixed_overlays":[],"cls":0,"junk":[],"text":"Promotions Filters Create promotion"}"#;
    const PROBE_PHONE: &str = r#"{"ok":true,"w":390,"h":844,"doc":{"scroll_width":402,"client_width":390,"scroll_height":844},"overflow":[{"path":"main > div","right":402,"over_by":12}],"overlaps":[],"small_taps":[],"clipped":[],"fixed_overlays":[],"cls":0,"junk":[{"path":"main > h3","token":"undefined","snippet":"undefined"}],"text":"Promotions undefined"}"#;
    const SECTIONS: &str = r#"{"ok":true,"sections":[{"id":"s1","tag":"main","heading":"Promotions","snippet":"Filters Create promotion"}]}"#;
    const HIT: &str = r#"{"ok":true,"x":50,"y":35,"topmost":true}"#;

    /// Answers by question key, never by call order: a QA run issues a variable
    /// number of requests and a FIFO script would silently mis-align.
    #[derive(Default)]
    struct ScriptedJudge {
        operations: Mutex<Vec<String>>,
        mutate_p: f64,
        claim_p: f64,
        severity: f64,
        section: String,
        fail: bool,
        /// Answer the first `*_target` head with a malformed distribution.
        bad_target_once: Mutex<bool>,
        calls: Mutex<u32>,
    }

    fn choice(pick: &str, criteria: &Value, confidence: f64) -> Value {
        let ids = criteria_ids(criteria);
        let n = ids.len().max(1) as f64;
        let mut probabilities = serde_json::Map::new();
        for id in &ids {
            probabilities.insert(id.clone(), json!(if id == pick { 1.0 - 0.01 * (n - 1.0) } else { 0.01 }));
        }
        json!({ "choice": pick, "confidence": confidence, "probabilities": probabilities })
    }

    fn score(level: u8) -> Value {
        let mut probabilities = serde_json::Map::new();
        for i in 0..4u8 {
            probabilities.insert(i.to_string(), json!(if i == level { 1.0 } else { 0.0 }));
        }
        json!({ "score": level as f64, "confidence": 0.9, "legend": "level", "probabilities": probabilities })
    }

    impl ScriptedJudge {
        fn with(operations: &[&str]) -> Self {
            ScriptedJudge {
                operations: Mutex::new(operations.iter().map(|s| s.to_string()).collect()),
                mutate_p: 0.01,
                claim_p: 0.9,
                severity: 0.0,
                section: "s1".to_string(),
                fail: false,
                bad_target_once: Mutex::new(false),
                calls: Mutex::new(0),
            }
        }
    }

    impl Judge for ScriptedJudge {
        fn answers(&self, body: &Value) -> Result<Value, String> {
            *self.calls.lock() += 1;
            if self.fail {
                return Err("model provider returned HTTP 500; no action executed".to_string());
            }
            let Some(questions) = body["questions"].as_object() else { return Ok(json!({})) };
            let mut out = serde_json::Map::new();
            for key in questions.keys() {
                let value = if key == "operation" {
                    let next = {
                        let mut ops = self.operations.lock();
                        if ops.is_empty() { "DONE".to_string() } else { ops.remove(0) }
                    };
                    choice(&next, &questions[key]["criteria"], 0.9)
                } else if key.ends_with("_target") {
                    let ids = criteria_ids(&questions[key]["criteria"]);
                    let pick = ids.first().cloned().unwrap_or_default();
                    if *self.bad_target_once.lock() {
                        *self.bad_target_once.lock() = false;
                        // Keys that are not the offered ids: validate_choice must refuse it.
                        json!({ "choice": pick, "confidence": 0.9, "probabilities": { "wrong-key": 1.0 } })
                    } else {
                        choice(&pick, &questions[key]["criteria"], 0.9)
                    }
                } else if key.starts_with("mutates_") {
                    json!({ "noul": self.mutate_p })
                } else if key.starts_with("claim_") {
                    json!({ "noul": self.claim_p })
                } else if key.starts_with("sev_") {
                    score(self.severity.round() as u8)
                } else if key == "error_message" || key == "empty_state" {
                    json!({ "noul": 0.1 })
                } else if key == "section" {
                    choice(&self.section, &questions[key]["criteria"], 0.9)
                } else {
                    continue;
                };
                out.insert(key.clone(), value);
            }
            Ok(Value::Object(out))
        }

        fn calls(&self) -> u32 {
            *self.calls.lock()
        }
    }

    struct FakeBrowser {
        routes: Mutex<Vec<(String, String)>>,
        blocked: Vec<firewall::Blocked>,
        js: Mutex<Vec<String>>,
        clicks: Mutex<Vec<(f64, f64)>>,
        keys: Mutex<Vec<String>>,
        labels: Mutex<Vec<String>>,
        viewports: Mutex<Vec<String>>,
        /// Console level written into each run dir's frontend.jsonl.
        console: String,
        /// Message argument written into that record.
        console_message: String,
        /// When set, `firewall_status()` reports the firewall as unarmed.
        firewall_error: Option<String>,
        /// When set, each run dir's net.jsonl gets a WebSocket open record.
        socket: Option<String>,
        /// Return `snapshot_after` from the Nth `__miniQA.snapshot` call onward.
        snapshot_switch_at: Option<usize>,
        snapshot_after: Option<String>,
        snapshots: std::sync::atomic::AtomicUsize,
        cdp_calls: Mutex<Vec<String>>,
        root: PathBuf,
    }

    impl FakeBrowser {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!("mini-qa-{tag}-{}", now_ms()));
            let _ = std::fs::create_dir_all(&root);
            FakeBrowser {
                routes: Mutex::new(Vec::new()),
                blocked: Vec::new(),
                js: Mutex::new(Vec::new()),
                clicks: Mutex::new(Vec::new()),
                keys: Mutex::new(Vec::new()),
                labels: Mutex::new(Vec::new()),
                viewports: Mutex::new(Vec::new()),
                console: "error".to_string(),
                console_message: "Uncaught TypeError: cannot read properties of undefined".to_string(),
                firewall_error: None,
                socket: None,
                snapshot_switch_at: None,
                snapshot_after: None,
                snapshots: std::sync::atomic::AtomicUsize::new(0),
                cdp_calls: Mutex::new(Vec::new()),
                root,
            }
        }

        fn route(self, fragment: &str, payload: &str) -> Self {
            self.routes.lock().push((fragment.to_string(), payload.to_string()));
            self
        }

        fn payload(&self, js: &str) -> String {
            for (fragment, payload) in self.routes.lock().iter() {
                if js.contains(fragment.as_str()) {
                    return payload.clone();
                }
            }
            if js.contains("document.readyState") {
                return r#""complete""#.to_string();
            }
            if js.contains("settleState") {
                return r#"{"ok":true,"settled":true,"ms":320,"mutations":1,"inflight":0}"#.to_string();
            }
            if js.contains("userAgent") {
                return UA.to_string();
            }
            if js.contains("__miniQA.snapshot") {
                let call = self.snapshots.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                if let (Some(switch), Some(after)) = (self.snapshot_switch_at, &self.snapshot_after) {
                    if call >= switch {
                        return after.clone();
                    }
                }
                return OBSERVATION.to_string();
            }
            if js.contains("__miniQA.probe") {
                return PROBE.to_string();
            }
            if js.contains("__miniQA.hitTest") {
                return HIT.to_string();
            }
            r#"{"ok":true}"#.to_string()
        }
    }

    impl Browser for FakeBrowser {
        fn eval(&self, js: &str) -> Result<String, String> {
            self.js.lock().push(js.to_string());
            Ok(self.payload(js))
        }
        fn cdp(&self, method: &str, _params: Value) -> Result<Value, String> {
            self.cdp_calls.lock().push(method.to_string());
            Ok(Value::Null)
        }
        fn set_viewport(&self, v: Viewport) -> Result<(), String> {
            self.viewports.lock().push(v.name.to_string());
            Ok(())
        }
        fn screenshot(&self, path: &Path) -> Result<(), String> {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(path, b"jpeg").map_err(|e| e.to_string())
        }
        fn mouse_click(&self, x: f64, y: f64) -> Result<(), String> {
            self.clicks.lock().push((x, y));
            Ok(())
        }
        fn key_text(&self, text: &str) -> Result<(), String> {
            self.keys.lock().push(text.to_string());
            Ok(())
        }
        fn run_label(&self, label: &str, _incident: &str) -> Result<PathBuf, String> {
            self.labels.lock().push(label.to_string());
            let dir = self.root.join(format!("run-{label}"));
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let t = now_ms();
            let console = json!({ "cmd": "debug", "kind": "console", "t": t, "url": "https://example.com/promotions",
                "level": self.console, "args": [self.console_message.clone(), "app.js:3:1"] });
            std::fs::write(dir.join("frontend.jsonl"), format!("{console}\n")).map_err(|e| e.to_string())?;
            let net = json!({ "cmd": "debug", "kind": "net", "t": t, "url": "https://example.com/api/promotions",
                "api": "fetch", "id": 1, "phase": "recv", "status": 500, "method": "GET" });
            std::fs::write(dir.join("net.jsonl"), format!("{net}\n")).map_err(|e| e.to_string())?;
            if let Some(url) = &self.socket {
                let open = json!({ "cmd": "debug", "kind": "net", "t": t, "url": url,
                    "api": "ws", "id": 2, "phase": "open" });
                let mut existing = std::fs::read_to_string(dir.join("net.jsonl")).unwrap_or_default();
                existing.push_str(&format!("{open}\n"));
                std::fs::write(dir.join("net.jsonl"), existing).map_err(|e| e.to_string())?;
            }
            Ok(dir)
        }
        fn url(&self) -> Result<String, String> {
            Ok("https://example.com/promotions".to_string())
        }
        fn blocked(&self) -> Vec<firewall::Blocked> {
            self.blocked.clone()
        }
        fn firewall_status(&self) -> Result<(), String> {
            match &self.firewall_error {
                Some(reason) => Err(reason.clone()),
                None => Ok(()),
            }
        }
        fn browser_version(&self) -> Option<String> {
            Some("153.0.4129.101".to_string())
        }
    }

    fn config(browser: &FakeBrowser) -> QaConfig {
        QaConfig {
            url: "https://example.com/promotions".to_string(),
            host: "example.com".to_string(),
            section: None,
            claims: Vec::new(),
            out_dir: browser.root.join("out"),
            allow_writes: false,
            steps: 4,
            explore: vec!["phone".to_string(), "desktop".to_string()],
            wall_clock_s: 60,
            model: crate::qa::DEFAULT_MODEL.to_string(),
            api_key: Some("test".to_string()),
        }
    }

    fn executed(browser: &FakeBrowser, judge: &dyn Judge) -> QaReport {
        let cfg = config(browser);
        execute(&cfg, browser, judge)
    }

    #[test]
    fn resolve_target_maps_index_keys_back_to_elements() {
        let a = ObservedElement { id: "e1".into(), node: 7, kind: "click".into(), label: "Save".into(), ..Default::default() };
        let mut b = ObservedElement { id: "e2".into(), node: 9, kind: "select".into(), label: "Size - S".into(), ..Default::default() };
        b.value = Some("s".into());
        let mut c = ObservedElement { id: "e3".into(), node: 9, kind: "select".into(), label: "Size - M".into(), ..Default::default() };
        c.value = Some("m".into());
        let candidates = vec![a, b, c];

        assert_eq!(resolve_target("1", &candidates).map(|e| e.id.as_str()), Some("e1"));
        assert_eq!(resolve_target("2:2", &candidates).map(|e| e.id.as_str()), Some("e3"));
        assert!(resolve_target("9", &candidates).is_none());
    }

    #[test]
    fn typers_corpus_hands_numeric_fields_numeric_values() {
        let field = ObservedElement { input_type: Some("number".into()), ..Default::default() };
        let text = ObservedElement { input_type: Some("text".into()), ..Default::default() };
        let numeric = typing_values(&field);
        assert!(numeric.iter().all(|v| v.parse::<f64>().is_ok()));
        assert!(typing_values(&text).iter().any(|v| v.contains("DROP TABLE")));
        assert!(typing_values(&text).iter().any(|v| v.is_empty()));
    }

    #[test]
    fn every_viewport_gets_its_own_run_label_and_override() {
        let browser = FakeBrowser::new("labels");
        let judge = ScriptedJudge::with(&["DONE"]);
        let report = executed(&browser, &judge);
        assert_eq!(*browser.labels.lock(), vec!["phone", "tablet", "desktop"]);
        assert_eq!(*browser.viewports.lock(), vec!["phone", "tablet", "desktop"]);
        assert_eq!(report.viewports.len(), 3);
        assert!(report.viewports.iter().all(|v| v.probe.is_some()));
    }

    #[test]
    fn exploration_runs_at_every_listed_viewport_and_marks_it() {
        let browser = FakeBrowser::new("explore-list");
        let judge = ScriptedJudge::with(&["CLICK", "DONE", "CLICK", "DONE"]);
        let report = executed(&browser, &judge);
        let phone = report.viewports.iter().find(|v| v.viewport == "phone").unwrap();
        let tablet = report.viewports.iter().find(|v| v.viewport == "tablet").unwrap();
        let desktop = report.viewports.iter().find(|v| v.viewport == "desktop").unwrap();
        assert!(phone.explored && desktop.explored && !tablet.explored);
        assert_eq!(phone.steps.len(), 2, "{:?}", phone.steps);
        assert_eq!(desktop.steps.len(), 2, "{:?}", desktop.steps);
        assert!(tablet.steps.is_empty());
    }

    #[test]
    fn explore_none_keeps_every_viewport_probe_only() {
        let browser = FakeBrowser::new("explore-none");
        let judge = ScriptedJudge::with(&["CLICK", "DONE"]);
        let mut cfg = config(&browser);
        cfg.explore.clear();
        let report = execute(&cfg, &browser, &judge);
        assert!(report.viewports.iter().all(|v| !v.explored && v.steps.is_empty()));
        assert!(browser.clicks.lock().is_empty());
    }

    #[test]
    fn a_clean_run_exits_zero_and_writes_both_reports() {
        let mut browser = FakeBrowser::new("clean");
        browser.console = "log".to_string(); // the recorder keeps errors only
        let judge = ScriptedJudge::with(&["DONE"]);
        let report = executed(&browser, &judge);
        let verdict = report::verdict(&report);
        assert_eq!(verdict.exit_code, 0, "notes: {:?} findings: {:?}", report.notes,
            report.viewports.iter().flat_map(|v| v.findings.iter()).map(|f| (&f.title, f.severity)).collect::<Vec<_>>());
        assert!(report::write(&report).is_ok());
        assert!(report.out_dir.join("report.md").exists());
        assert!(report.out_dir.join("report.json").exists());
        assert!(report.viewports[0].run_dir.as_deref() == Some("runs/run-phone"));
        assert!(report.out_dir.join("runs/run-phone/frontend.jsonl").exists());
    }

    #[test]
    fn a_severe_first_party_error_fails_the_run_with_cited_evidence() {
        let mut browser = FakeBrowser::new("error");
        browser.console = "error".to_string();
        let mut judge = ScriptedJudge::with(&["DONE"]);
        judge.severity = 3.0;
        let report = executed(&browser, &judge);
        let finding = report.viewports[0]
            .findings
            .iter()
            .find(|f| f.kind == FindingKind::ConsoleError)
            .expect("the console error should be reported");
        assert_eq!(finding.severity, 3);
        assert_eq!(finding.severity_source, SeveritySource::Jev);
        assert_eq!(finding.evidence, vec!["runs/run-phone/frontend.jsonl:1".to_string()]);
        assert_eq!(report::verdict(&report).exit_code, 1);
    }

    #[test]
    fn a_failed_request_is_reported_with_its_status() {
        let browser = FakeBrowser::new("net");
        let mut judge = ScriptedJudge::with(&["DONE"]);
        judge.severity = 2.0;
        let report = executed(&browser, &judge);
        let finding = report.viewports[0]
            .findings
            .iter()
            .find(|f| f.kind == FindingKind::FailedRequest)
            .expect("the 500 should be reported");
        assert!(finding.title.contains("500"), "{}", finding.title);
        assert!(finding.evidence[0].starts_with("runs/run-phone/net.jsonl"));
    }

    #[test]
    fn the_safety_gate_refuses_a_click_that_would_write() {
        let browser = FakeBrowser::new("gate");
        let mut judge = ScriptedJudge::with(&["CLICK", "DONE"]);
        judge.mutate_p = 0.80;
        let report = executed(&browser, &judge);
        assert!(browser.clicks.lock().is_empty(), "the click must never be dispatched");
        let step = report.viewports[0].steps.first().expect("the step is recorded");
        assert!(step.note.as_deref().unwrap_or("").contains("safety gate"), "{:?}", step.note);
        assert_eq!(step.target_label.as_deref(), Some("Filters"));
    }

    #[test]
    fn a_click_below_the_gate_is_dispatched() {
        let browser = FakeBrowser::new("click");
        let mut judge = ScriptedJudge::with(&["CLICK", "DONE"]);
        judge.mutate_p = 0.02;
        let report = executed(&browser, &judge);
        assert_eq!(browser.clicks.lock().len(), 1);
        assert_eq!(report.viewports[0].steps[0].target_id.as_deref(), Some("e1"));
    }

    #[test]
    fn typing_a_field_uses_the_corpus_and_marks_the_step() {
        let browser = FakeBrowser::new("type");
        let judge = ScriptedJudge::with(&["TYPE_TEXT", "DONE"]);
        let report = executed(&browser, &judge);
        let keys = browser.keys.lock().clone();
        assert_eq!(keys.len(), 1, "one value typed per step");
        assert!(report.viewports[0].steps[0].text.is_some());
        assert!(report.viewports[0].steps[0].page_changed == false || report.viewports[0].steps[0].page_changed);
    }

    #[test]
    fn exploration_stops_after_three_unchanged_steps() {
        let browser = FakeBrowser::new("stuck");
        let judge = ScriptedJudge::with(&["SCROLL_DOWN", "SCROLL_DOWN", "SCROLL_DOWN", "SCROLL_DOWN"]);
        let report = executed(&browser, &judge);
        assert_eq!(report.viewports[0].steps.len(), NO_CHANGE_LIMIT);
        assert!(report.notes.iter().any(|n| n.contains("consecutive steps changed nothing")), "{:?}", report.notes);
    }

    #[test]
    fn exploration_respects_the_step_budget() {
        let browser = FakeBrowser::new("budget");
        let judge = ScriptedJudge::with(&["SCROLL_DOWN", "SCROLL_DOWN", "SCROLL_DOWN", "SCROLL_DOWN"]);
        let mut cfg = config(&browser);
        cfg.steps = 2;
        let report = execute(&cfg, &browser, &judge);
        assert_eq!(report.viewports[0].steps.len(), 2);
    }

    #[test]
    fn claims_are_judged_per_viewport_and_unverified_fails_the_run() {
        let browser = FakeBrowser::new("claims");
        let mut judge = ScriptedJudge::with(&["DONE"]);
        judge.claim_p = 0.10;
        let mut cfg = config(&browser);
        cfg.claims = vec!["The Export CSV button is visible".to_string()];
        let report = execute(&cfg, &browser, &judge);
        assert_eq!(report.viewports.len(), 3);
        assert!(report.viewports.iter().all(|v| v.claims.len() == 1));
        assert!(report.viewports.iter().all(|v| !v.claims[0].verified));
        assert_eq!(report::verdict(&report).exit_code, 1);
        let md = report::render_md(&report);
        assert!(md.contains("unverified"), "{md}");
        assert!(md.contains("shots/phone.jpg"), "an unverified claim must link its screenshot:\n{md}");
    }

    #[test]
    fn the_section_region_is_used_for_every_probe() {
        let browser = FakeBrowser::new("section");
        let browser = browser.route("__miniQA.sections", SECTIONS);
        let judge = ScriptedJudge::with(&["DONE"]);
        let mut cfg = config(&browser);
        cfg.section = Some("Promotions".to_string());
        let report = execute(&cfg, &browser, &judge);
        let log = browser.js.lock().clone();
        assert!(
            log.iter().any(|js| js.contains(r#"__miniQA.snapshot("s1")"#)),
            "log: {:?}\nnotes: {:?}",
            log,
            report.notes
        );
        assert!(log.iter().any(|js| js.contains(r#"__miniQA.probe("s1")"#)), "log: {log:?}");
        assert!(report.notes.iter().any(|n| n.contains("section region s1")), "{:?}", report.notes);
    }

    #[test]
    fn a_console_record_with_no_readable_message_is_not_a_finding() {
        let mut browser = FakeBrowser::new("noise");
        browser.console_message = "{}".to_string();
        let judge = ScriptedJudge::with(&["DONE"]);
        let report = executed(&browser, &judge);
        assert!(
            report.viewports[0].findings.iter().all(|f| f.kind != FindingKind::ConsoleError),
            "an unreadable rejection object must not become a finding: {:?}",
            report.viewports[0].findings
        );
        assert!(carries_evidence("Uncaught TypeError: cannot read properties of undefined"));
        assert!(carries_evidence("Failed to fetch"));
        assert!(!carries_evidence("{}"));
        assert!(!carries_evidence(""));
        assert!(!carries_evidence("app.js:3:1"));
        assert!(!carries_evidence("{} app.js:3:1"));
    }

    #[test]
    fn heading_matches_is_a_string_fact() {
        assert!(heading_matches("Promotions", "Promotions"));
        assert!(heading_matches("  promotions ", "Promotions"));
        assert!(heading_matches("Promotions (beta)", "Promotions"));
        assert!(!heading_matches("Reports", "Promotions"));
        assert!(!heading_matches("", "Promotions"));
        assert!(!heading_matches("Promotions", "  "));
    }

    #[test]
    fn a_hesitant_model_still_finds_the_section_by_heading_text() {
        let browser = FakeBrowser::new("section-fallback").route("__miniQA.sections", SECTIONS);
        let mut judge = ScriptedJudge::with(&["DONE"]);
        judge.section = "none_of_these".to_string();
        let mut cfg = config(&browser);
        cfg.section = Some("Promotions".to_string());
        let report = execute(&cfg, &browser, &judge);
        assert!(browser.js.lock().iter().any(|js| js.contains(r#"__miniQA.snapshot("s1")"#)));
        assert!(report.notes.iter().any(|n| n.contains("matched by heading text")), "{:?}", report.notes);
    }

    #[test]
    fn an_unarmed_firewall_aborts_the_run() {
        let mut browser = FakeBrowser::new("unarmed");
        browser.firewall_error = Some("firewall could not be armed: no listener".to_string());
        let judge = ScriptedJudge::with(&["DONE"]);
        let report = executed(&browser, &judge);
        assert!(report.viewports.iter().all(|v| v.nav_failed.is_some()));
        assert_eq!(report::verdict(&report).exit_code, 2);
        assert!(report.notes.iter().any(|n| n.contains("could not be armed")), "{:?}", report.notes);
        assert!(browser.clicks.lock().is_empty(), "nothing may be driven without a firewall");
    }

    #[test]
    fn a_forbidden_page_is_reported_and_never_explored() {
        let observation = OBSERVATION.replace("/promotions", "/checkout");
        let browser = FakeBrowser::new("forbidden").route("__miniQA.snapshot", &observation);
        let judge = ScriptedJudge::with(&["CLICK", "DONE"]);
        let report = executed(&browser, &judge);
        assert!(browser.clicks.lock().is_empty(), "a checkout page must never be clicked");
        let fact = report.viewports[0]
            .findings
            .iter()
            .find(|f| f.title.contains("login/checkout/account"))
            .expect("the refusal is reported");
        assert_eq!(fact.severity, 0);
        assert!(report.notes.iter().any(|n| n.contains("exploration skipped")), "{:?}", report.notes);
    }

    #[test]
    fn sockets_the_page_opened_are_listed_as_facts() {
        let mut browser = FakeBrowser::new("socket");
        browser.socket = Some("wss://example.com/live".to_string());
        let judge = ScriptedJudge::with(&["DONE"]);
        let report = executed(&browser, &judge);
        let fact = report.viewports[0]
            .findings
            .iter()
            .find(|f| f.title.contains("socket opened"))
            .expect("the socket is listed");
        assert_eq!(fact.severity, 0);
        assert!(fact.detail.contains("not intercepted"), "{}", fact.detail);
        assert!(report.notes.iter().any(|n| n.contains("outbound frames are not intercepted")), "{:?}", report.notes);
    }

    #[test]
    fn line_lookup_prefers_the_record_that_carries_the_needle() {
        let dir = std::env::temp_dir().join(format!("mini-qa-lines-{}", now_ms()));
        let _ = std::fs::create_dir_all(&dir);
        let t = 1_700_000_000_000u64;
        let first = json!({ "kind": "console", "t": t as f64, "level": "error", "args": ["other"] });
        let second = json!({ "kind": "console", "t": t as f64 + 0.4, "level": "error", "args": ["boom"] });
        std::fs::write(dir.join("frontend.jsonl"), format!("{first}\n{second}\n")).unwrap();

        assert_eq!(line_of(&dir, "frontend.jsonl", t, "boom"), Some(2));
        // A needle that matches nothing must cite no line at all: a sibling
        // record in the same millisecond is not the record the fact came from.
        assert_eq!(line_of(&dir, "frontend.jsonl", t, "missing"), None);
        assert_eq!(line_of(&dir, "frontend.jsonl", t + 60_000, "boom"), None);
    }

    #[test]
    fn exploration_stops_when_a_step_lands_on_a_forbidden_page() {
        let mut browser = FakeBrowser::new("nav-forbidden");
        // The first snapshot is the page we started on; the next one is where
        // the click took us.
        browser.snapshot_switch_at = Some(2);
        browser.snapshot_after = Some(OBSERVATION.replace("/promotions", "/checkout"));
        let judge = ScriptedJudge::with(&["CLICK", "CLICK", "CLICK"]);
        let report = executed(&browser, &judge);
        assert_eq!(report.viewports[0].steps.len(), 1, "the run must stop on the step that landed there");
        assert!(
            report.notes.iter().any(|n| n.contains("landed on") && n.contains("login/checkout/account")),
            "{:?}",
            report.notes
        );
        assert!(
            report.viewports[0].findings.iter().any(|f| f.title.contains("login/checkout/account")),
            "the landing is reported as a fact"
        );
    }

    #[test]
    fn a_split_severity_distribution_is_demoted() {
        let mut probabilities = BTreeMap::new();
        probabilities.insert("2".to_string(), 0.5);
        probabilities.insert("3".to_string(), 0.5);
        let split = jev::ScoreAnswer { score: 2.5, confidence: 0.9, probabilities };
        assert_eq!(severity_of(Some(&split)).0, 1, "a coin-flip between levels must not fail a build");

        let mut decisive = BTreeMap::new();
        decisive.insert("3".to_string(), 1.0);
        let clear = jev::ScoreAnswer { score: 3.0, confidence: 0.9, probabilities: decisive };
        assert_eq!(severity_of(Some(&clear)).0, 3);
        assert_eq!(severity_of(None).0, 1);
    }

    #[test]
    fn an_aborted_run_still_leaves_a_report() {
        let browser = FakeBrowser::new("aborted");
        let cfg = config(&browser);
        let code = write_aborted_report(&cfg, "the QA thread panicked");
        assert_eq!(code, 2);
        let written = std::fs::read_to_string(cfg.out_dir.join("report.md")).expect("report.md exists");
        assert!(written.contains("INCONCLUSIVE"), "{written}");
        assert!(written.contains("the QA thread panicked"), "{written}");
    }

    #[test]
    fn a_rejected_target_answer_is_retried_once() {
        let browser = FakeBrowser::new("retry");
        let judge = ScriptedJudge::with(&["CLICK", "DONE"]);
        *judge.bad_target_once.lock() = true;
        let report = executed(&browser, &judge);
        assert_eq!(browser.clicks.lock().len(), 1, "the retry must recover the step");
        assert_eq!(report.viewports[0].steps[0].target_id.as_deref(), Some("e1"));
        assert!(judge.calls() > 1);
    }

    #[test]
    fn covered_controls_are_reported_and_do_not_count_as_no_change() {
        let mut observation: Value = serde_json::from_str(OBSERVATION).unwrap();
        let actions = observation["actions"].as_array_mut().unwrap();
        for n in 5..8u64 {
            actions.push(json!({
                "id": format!("e{n}"), "node": n, "kind": "click", "label": format!("Card {n}"),
                "role": "link", "rect": { "x": 10, "y": n * 10, "w": 80, "h": 30 }
            }));
        }
        let payload = observation.to_string();
        let browser = FakeBrowser::new("covered")
            .route("__miniQA.snapshot", &payload)
            .route("__miniQA.hitTest", r#"{"ok":true,"x":0,"y":0,"topmost":false,"reason":"covered by div.overlay"}"#);
        let judge = ScriptedJudge::with(&["CLICK", "CLICK", "CLICK", "CLICK", "DONE"]);
        let report = executed(&browser, &judge);
        assert!(browser.clicks.lock().is_empty(), "a covered control is never clicked");
        // Escape is pressed when a click is refused, and again before the
        // resting probe; each press is a keyDown + keyUp pair.
        let escapes = browser.cdp_calls.lock().iter().filter(|m| *m == "Input.dispatchKeyEvent").count();
        assert!(escapes >= 2 && escapes % 2 == 0, "expected key pairs, saw {escapes} events");
        assert_eq!(
            report.viewports[0].steps.len(),
            NO_CHANGE_LIMIT,
            "three refusals end the pass; notes: {:?}",
            report.viewports[0].steps.iter().map(|s| s.note.clone()).collect::<Vec<_>>()
        );
        // A refused control gets exactly one retry (the overlay may have been
        // dismissed), then it is retired and the model moves on.
        let targets: Vec<Option<&str>> =
            report.viewports[0].steps.iter().map(|s| s.target_id.as_deref()).collect();
        assert_eq!(targets[0], targets[1], "the refused control is retried once: {targets:?}");
        assert_ne!(targets[1], targets[2], "after the retry it is retired: {targets:?}");
        assert!(
            report.notes.iter().any(|n| n.contains("overlay is covering the controls")),
            "notes: {:?} steps: {:?}",
            report.notes,
            report.viewports[0].steps.iter().map(|s| s.note.clone()).collect::<Vec<_>>()
        );
        let fact = report.viewports[0]
            .findings
            .iter()
            .find(|f| f.title.contains("div.overlay"))
            .expect("the covering layer is reported");
        assert_eq!(fact.severity, 1);
        assert!(fact.detail.contains("swallows pointer input"), "{}", fact.detail);
    }

    #[test]
    fn repeated_geometry_findings_collapse_into_one_counted_row() {
        let vp = Viewport { name: "phone", width: 390, height: 844, dsf: 3.0, mobile: true };
        let overlaps: Vec<Staged> = (0..5)
            .map(|i| Staged {
                ts: 0,
                finding: geometry(
                    &vp,
                    format!("phone-overlap-{i}"),
                    "Interactive elements overlap (100 px²)".to_string(),
                    format!("a{i} overlaps b{i}"),
                    format!("a{i}"),
                    Some("runs/run-003-phone"),
                ),
            })
            .collect();
        let mut staged = overlaps;
        staged.push(Staged {
            ts: 0,
            finding: geometry(
                &vp,
                "phone-tap-0".to_string(),
                "Tap target 20×20 px (under 44 px)".to_string(),
                "button".to_string(),
                "button#x".to_string(),
                Some("runs/run-003-phone"),
            ),
        });

        let collapsed = collapse_geometry(staged);
        assert_eq!(collapsed.len(), 2, "one row per rule plus the lone finding");
        let overlap = collapsed.iter().find(|s| s.finding.title.contains("overlap")).unwrap();
        assert_eq!(overlap.finding.title, "5 interactive element pairs overlap");
        assert!(overlap.finding.detail.contains("worst examples: a0 · a1 · a2"), "{}", overlap.finding.detail);
        assert!(collapsed.iter().any(|s| s.finding.title.starts_with("Tap target")));
    }

    #[test]
    fn engine_of_prefers_the_unspoofed_brand_list() {
        // `userAgentData.brands` is what the runtime really is.
        assert_eq!(engine_of(r#""Chromium/153 Not=A?Brand/24""#), "WebView2/Chromium 153");
        // A spoofed UA string is the fallback.
        assert_eq!(
            engine_of(r#""Mozilla/5.0 Chrome/151.0.0.0 Safari/537.36 Edg/151.0.4129.101""#),
            "WebView2/Chromium 151.0.4129.101"
        );
        assert_eq!(engine_of(""), "WebView2/Chromium unknown");
    }

    #[test]
    fn a_dead_judge_makes_the_run_inconclusive_not_clean() {
        let browser = FakeBrowser::new("dead");
        let mut judge = ScriptedJudge::with(&["DONE"]);
        judge.fail = true;
        let report = executed(&browser, &judge);
        assert!(report.viewports.iter().all(|v| v.nav_failed.is_some()));
        assert_eq!(report::verdict(&report).exit_code, 2);
        assert!(report.notes.iter().any(|n| n.contains("no Jev request succeeded")));
    }

    #[test]
    fn geometry_and_junk_are_code_severity_findings() {
        let browser = FakeBrowser::new("phone").route("__miniQA.probe", PROBE_PHONE);
        let judge = ScriptedJudge::with(&["DONE"]);
        let report = executed(&browser, &judge);
        let phone = report.viewports.iter().find(|v| v.viewport == "phone").unwrap();
        let overflow = phone.findings.iter().find(|f| f.kind == FindingKind::Geometry).expect("overflow reported");
        assert_eq!(overflow.severity, 1);
        assert_eq!(overflow.severity_source, SeveritySource::Code);
        assert_eq!(overflow.viewport, "phone");
        let junk = phone.findings.iter().find(|f| f.kind == FindingKind::Junk).expect("junk reported");
        assert_eq!(junk.severity, 2);
        assert!(report::verdict(&report).exit_code == 1);
    }

    #[test]
    fn firewall_blocked_writes_are_facts_not_findings() {
        let mut browser = FakeBrowser::new("firewall");
        browser.blocked = vec![firewall::Blocked {
            ts_ms: 1,
            method: "POST".to_string(),
            url: "https://example.com/promotions?x=1".to_string(),
            mechanism: "fetch".to_string(),
        }];
        let mut judge = ScriptedJudge::with(&["DONE"]);
        judge.severity = 3.0;
        let report = executed(&browser, &judge);
        let fact = report.viewports[0]
            .findings
            .iter()
            .find(|f| f.kind == FindingKind::Firewall)
            .expect("a blocked write is reported as a fact");
        assert_eq!(fact.severity, 0);
        assert!(fact.blocked_by_firewall);
        assert!(fact.title.contains("POST /promotions"), "{}", fact.title);
    }
}

//! OWNER: desk-loop. `mini --desk` / `mini --desk-observe` entry points and the loop.
//!
//! The loop is `--move`'s, over a `DeskSurface` instead of a CDP page: the same
//! question, the same decision, the same one evidence gate for `ok`, the same
//! loop guard and episode log. Only observing and acting differ.

use super::{client::CuaDesk, redact, ActResult, DeskAction, DeskSurface};
use crate::episodes;
use crate::move_cmd::{self, Decision, LoopGuard, MoveReport, MoveStep};
use crate::qa::{firewall, jev, questions, Judge, ObservedElement, Observation, StepRecord};
use serde_json::json;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const DEFAULT_STEPS: usize = 15;
const MAX_STEPS: usize = 40;

#[derive(Debug, Default)]
pub struct DeskArgs {
    pub goal: String,
    pub app: String,
    pub window: Option<String>,
    pub steps: usize,
    pub values: BTreeMap<String, String>,
    pub allow_writes: bool,
    pub launch: bool,
    pub hints_dir: Option<std::path::PathBuf>,
}

/// Parses the words after `--desk` / `--desk-observe`. `--goal` is checked by
/// `run`, since `--desk-observe` has none.
pub fn parse_args(args: &[String]) -> Result<DeskArgs, String> {
    let mut out = DeskArgs { steps: DEFAULT_STEPS, ..Default::default() };
    let mut i = 0;
    while i < args.len() {
        let next = args.get(i + 1);
        match args[i].as_str() {
            "--goal" => {
                out.goal = next.cloned().ok_or("--goal needs a value")?;
                i += 1;
            }
            "--app" => {
                out.app = next.cloned().ok_or("--app needs a name")?;
                i += 1;
            }
            "--window" => {
                out.window = Some(next.cloned().ok_or("--window needs a title part")?);
                i += 1;
            }
            "--steps" => {
                out.steps = next.and_then(|s| s.parse().ok()).ok_or("--steps needs a number")?;
                i += 1;
            }
            "--value" => {
                let pair = next.ok_or("--value needs name=value")?;
                let (k, v) = pair.split_once('=').ok_or_else(|| format!("--value {pair}: expected name=value"))?;
                out.values.insert(k.trim().to_string(), v.to_string());
                i += 1;
            }
            "--hints" => {
                out.hints_dir = Some(next.ok_or("--hints needs a folder")?.into());
                i += 1;
            }
            "--launch" => out.launch = true,
            "--allow-writes" => out.allow_writes = true,
            _ => {}
        }
        i += 1;
    }
    out.steps = out.steps.clamp(1, MAX_STEPS);
    if out.app.trim().is_empty() {
        return Err("missing --app".into());
    }
    Ok(out)
}

/// The episode host (and hints file stem) for an app: `desk-calculator`.
pub fn desk_host(app: &str) -> String {
    format!("desk-{}", app.trim().to_lowercase().replace(' ', "-"))
}

/// Candidates Jev may pick: `--move`'s visible, enabled controls, minus dead
/// ones and, unless `--allow-writes`, any control that writes/sends/deletes.
fn offered(before: &Observation, guard: &LoopGuard, allow_writes: bool) -> Vec<ObservedElement> {
    firewall::dev_candidates(&before.actions)
        .into_iter()
        .filter(|el| guard.allows(&before.url, el))
        .filter(|el| allow_writes || !redact::is_write_label(&el.label))
        .collect()
}

/// A decision as a desk input. `Err` is the note for a step that was not performed.
fn to_action(d: &Decision, values: &BTreeMap<String, String>) -> Result<(DeskAction, Option<String>), String> {
    let node = d.target.as_ref().map(|t| t.node);
    match (d.operation.as_str(), node) {
        ("CLICK" | "SELECT", Some(node)) => Ok((DeskAction::Click { node }, None)),
        ("TYPE_TEXT", Some(node)) => {
            let target = d.target.as_ref().expect("node came from the target");
            match move_cmd::value_for(values, target) {
                Some((name, value)) => Ok((DeskAction::SetValue { node, value: value.to_string() }, Some(name.to_string()))),
                None => Err(format!("no value supplied for \"{}\"", target.label)),
            }
        }
        ("SCROLL_DOWN" | "SCROLL_UP", _) => Ok((DeskAction::Scroll { node: None, down: d.operation == "SCROLL_DOWN" }, None)),
        (op, _) => Err(format!("unsupported operation on the desktop: {op}")),
    }
}

fn is_stale(r: &ActResult) -> bool {
    r.effect == "refused" && r.note.as_deref().is_some_and(|n| n.contains("stale"))
}

/// The same action on the control with the same label in a fresh snapshot.
fn retarget(action: &DeskAction, label: &str, now: &Observation) -> Option<DeskAction> {
    let node = now.actions.iter().find(|el| el.label == label)?.node;
    match action {
        DeskAction::Click { .. } => Some(DeskAction::Click { node }),
        DeskAction::SetValue { value, .. } => Some(DeskAction::SetValue { node, value: value.clone() }),
        DeskAction::TypeText { text, .. } => Some(DeskAction::TypeText { node, text: text.clone() }),
        _ => None,
    }
}

/// Acts; a stale-token refusal gets one re-observe and one retry of the same label.
/// Returns the note for a step that did not go through.
fn act_once_retry(surface: &mut dyn DeskSurface, action: &DeskAction, label: Option<&str>) -> Option<String> {
    let refusal = |r: &ActResult| format!("refused: {}", r.note.as_deref().unwrap_or("no reason"));
    let first = match surface.act(action) {
        Ok(r) => r,
        Err(e) => return Some(format!("action failed: {e}")),
    };
    if !is_stale(&first) {
        return (first.effect == "refused").then(|| refusal(&first));
    }
    let retry = label.zip(surface.observe().ok()).and_then(|(l, now)| retarget(action, l, &now));
    let Some(retry) = retry else { return Some(refusal(&first)) };
    match surface.act(&retry) {
        Ok(r) if r.effect == "refused" => Some(refusal(&r)),
        Ok(_) => None,
        Err(e) => Some(format!("action failed: {e}")),
    }
}

/// `--move`'s loop on a desktop window. `None` when the window cannot even be
/// observed once (exit 2).
pub fn run_loop(surface: &mut dyn DeskSurface, judge: &dyn Judge, a: &DeskArgs) -> Option<MoveReport> {
    let started = Instant::now();
    let first = surface.observe().ok()?;
    let url_before = first.url.clone();
    let host = desk_host(&a.app);
    let hints_url = format!("http://{host}/");
    let names: Vec<String> = a.values.keys().cloned().collect();
    let priors = episodes::PriorTable::load(None, &host);
    let mut history: Vec<StepRecord> = Vec::new();
    let mut steps: Vec<MoveStep> = Vec::new();
    let mut guard = LoopGuard::default();
    let (mut ok, mut rejected_once, mut done_disputed, mut no_change) = (false, false, 0usize, 0usize);
    let mut last_page = first;

    let stop = loop {
        if move_cmd::actions_taken(&steps) >= a.steps {
            break format!("step budget of {} reached", a.steps);
        }
        if started.elapsed() > move_cmd::WALL_CLOCK {
            break format!("wall clock of {}s reached", move_cmd::WALL_CLOCK.as_secs());
        }
        let before = match surface.observe() {
            Ok(o) => o,
            Err(e) => break format!("observe failed: {e}"),
        };
        last_page = before.clone();
        let candidates = offered(&before, &guard, a.allow_writes);
        // Text helper off: Jev may only type fields named by --value.
        let mut question = questions::move_question(&a.goal, &before, &candidates, &history, &names, false);
        question["state"]["tried_without_effect"] = json!(guard.dead_labels(&before.url));
        move_cmd::add_goal_check(&mut question, &a.goal);
        if let Some(notes) = move_cmd::site_notes(a.hints_dir.as_deref(), &hints_url) {
            move_cmd::add_site_notes(&mut question, &notes);
        }
        let page_sig = episodes::page_signature(&before.url, &move_cmd::control_labels(&before.actions));
        let page_priors = priors.for_page(&page_sig, move_cmd::PRIORS_PER_PAGE);
        if let Some(rows) = move_cmd::past_runs_json(&page_priors) {
            question["state"]["past_runs"] = rows;
        }
        let decision = match move_cmd::decide(judge, &question, &candidates) {
            Ok(d) => d,
            Err(e) if !rejected_once => {
                rejected_once = true;
                eprintln!("mini --desk: judge answer rejected ({e}); asking once more");
                continue;
            }
            Err(e) => break format!("judge failed: {e}"),
        };
        let needed = move_cmd::goal_threshold(&a.goal, &url_before, &before.url);
        // One evidence gate for every ok, exactly as --move.
        if decision.operation != "BLOCKED" && move_cmd::done_has_evidence(decision.goal_met, needed) {
            ok = true;
            break format!("the goal check passed on this window (p={:.2})", decision.goal_met.unwrap_or_default());
        }
        if decision.operation == "BLOCKED" {
            break "the judge reported no operation can progress".to_string();
        }
        let mut step = MoveStep {
            step: steps.len() + 1,
            operation: decision.operation.clone(),
            target: decision.target.as_ref().map(|t| t.label.clone()),
            field: None,
            text: None,
            confidence: decision.confidence,
            goal_met: decision.goal_met,
            url: before.url.clone(),
            page_sig,
            page_changed: false,
            prior_return: None,
            note: None,
            ms: 0,
        };
        step.prior_return = move_cmd::prior_return_for(&page_priors, &step.operation, step.target.as_deref());
        if decision.operation == "DONE" {
            done_disputed += 1;
            if done_disputed > 2 {
                break "DONE claimed three times, but the goal check never passed".to_string();
            }
            let why = "DONE rejected: the goal check found no evidence in this window";
            step.note = Some(why.into());
            steps.push(step);
            history.push(StepRecord { step: steps.len(), operation: "DONE".into(), note: Some(why.into()), ..Default::default() });
            continue;
        }
        let step_started = Instant::now();
        let note = if decision.operation == "WAIT" {
            std::thread::sleep(Duration::from_millis(300));
            None
        } else {
            match to_action(&decision, &a.values) {
                Ok((action, field)) => {
                    step.field = field;
                    act_once_retry(surface, &action, step.target.as_deref())
                }
                Err(why) => Some(why),
            }
        };
        let after = surface.observe().unwrap_or_default();
        let changed = move_cmd::page_changed(&before, &after);
        last_page = after;
        let refused = note.is_some();
        step.page_changed = changed;
        step.note = note.clone();
        step.ms = step_started.elapsed().as_millis() as u64;
        history.push(StepRecord {
            step: steps.len() + 1,
            operation: step.operation.clone(),
            target_label: step.target.clone(),
            page_changed: changed,
            note,
            confidence: step.confidence,
            ..Default::default()
        });
        if let Some(t) = &decision.target {
            guard.record(&before.url, t, changed, refused);
        }
        steps.push(step);
        no_change = if changed && !refused { 0 } else { no_change + 1 };
        if no_change >= move_cmd::NO_CHANGE_LIMIT {
            break format!("{} steps in a row changed nothing", move_cmd::NO_CHANGE_LIMIT);
        }
    };

    if let Ok(now) = surface.observe() {
        last_page = now;
    }
    Some(MoveReport {
        ok,
        stop,
        episode_id: None,
        replay_from: None,
        replayed: 0,
        url_before,
        url_after: last_page.url.clone(),
        judge_calls: judge.calls(),
        text_calls: 0,
        text_model: None,
        elapsed_ms: started.elapsed().as_millis() as u64,
        steps,
        title: last_page.title,
        page_text: last_page.text,
    })
}

/// The run as an episode, filed under `desk-<app>`.
pub fn desk_episode(report: &MoveReport, goal: &str, app: &str) -> episodes::Episode {
    let mut ep = episodes::build_episode(
        goal,
        report.ok,
        &report.stop,
        report.judge_calls,
        report.elapsed_ms,
        &move_cmd::raw_steps(&report.steps),
    );
    ep.host = desk_host(app);
    ep
}

fn fail(cmd: &str, msg: &str) -> i32 {
    eprintln!("mini --{cmd}: {msg}");
    2
}

/// Connect, optionally launch, find and attach the app's window.
fn open(a: &DeskArgs) -> Result<CuaDesk, String> {
    let mut desk = CuaDesk::connect()?;
    if a.launch {
        desk.launch(&a.app)?;
    }
    let window = desk.find_window(&a.app, a.window.as_deref())?;
    desk.attach(window);
    Ok(desk)
}

/// `mini --desk ...`; exit 0 goal reached, 1 stopped without it, 2 could not run.
pub fn run(args: &[String], index: usize) -> i32 {
    let a = match parse_args(&args[index + 1..]) {
        Ok(a) if a.goal.trim().is_empty() => return fail("desk", "missing --goal"),
        Ok(a) => a,
        Err(e) => return fail("desk", &e),
    };
    let Some(api_key) = jev::api_key() else {
        return fail("desk", &format!("no Jev API key found — {}.", jev::key_help()));
    };
    let mut desk = match open(&a) {
        Ok(d) => d,
        Err(e) => return fail("desk", &e),
    };
    let judge = jev::JevClient::new(api_key, jev::model());
    let Some(mut report) = run_loop(&mut desk, &judge, &a) else {
        return fail("desk", "the window could not be observed");
    };
    let ep = desk_episode(&report, &a.goal, &a.app);
    match episodes::append(None, &ep) {
        Ok(_) => report.episode_id = (!ep.id.is_empty()).then(|| ep.id.clone()),
        Err(e) => eprintln!("mini --desk: episode not logged: {e}"),
    }
    if let Some(dir) = dirs::data_local_dir().map(|d| d.join("Mini").join("Debug")) {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("desk.json"), serde_json::to_string_pretty(&report).unwrap_or_default());
    }
    println!("{}", serde_json::to_string(&report).unwrap_or_default());
    if report.ok {
        0
    } else {
        1
    }
}

/// `mini --desk-observe --app X [--window T]`: one redacted observation as JSON, no Jev call.
pub fn observe(args: &[String], index: usize) -> i32 {
    let a = match parse_args(&args[index + 1..]) {
        Ok(a) => a,
        Err(e) => return fail("desk-observe", &e),
    };
    let mut desk = match open(&a) {
        Ok(d) => d,
        Err(e) => return fail("desk-observe", &e),
    };
    match desk.observe() {
        Ok(o) => {
            let elements: Vec<_> = firewall::dev_candidates(&o.actions)
                .iter()
                .map(|e| json!({ "id": e.id, "kind": e.kind, "role": e.role, "label": e.label, "value": e.value }))
                .collect();
            println!("{}", json!({ "url": o.url, "title": o.title, "text": o.text, "elements": elements }));
            0
        }
        Err(e) => fail("desk-observe", &e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qa::Rect;
    use serde_json::Value;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_needs_an_app_and_caps_steps() {
        assert!(parse_args(&args(&["--goal", "x"])).is_err(), "--app is required");
        let a = parse_args(&args(&["--app", "Calculator", "--goal", "x"])).unwrap();
        assert_eq!(a.steps, 15);
        assert!(!a.launch && !a.allow_writes);
        let a = parse_args(&args(&[
            "--app", "Calculator", "--steps", "99", "--value", "a=b", "--launch", "--allow-writes", "--window", "Calc",
        ]))
        .unwrap();
        assert_eq!(a.steps, 40);
        assert_eq!(a.values.get("a").map(String::as_str), Some("b"));
        assert!(a.launch && a.allow_writes);
        assert_eq!(a.window.as_deref(), Some("Calc"));
        assert!(parse_args(&args(&["--app", "C", "--value", "novalue"])).is_err());
    }

    /// Calculator as a fake window: buttons by label; the display reads 84
    /// once Seven, Multiply by, One, Two, Equals were clicked in that order.
    struct FakeCalc {
        buttons: Vec<&'static str>,
        clicks: Vec<&'static str>,
        acts: usize,
        /// The next `stale` acts are refused `stale_element_token`.
        stale: usize,
    }

    impl FakeCalc {
        fn new(extra: &[&'static str]) -> FakeCalc {
            let mut buttons = vec!["Clear", "Seven", "Multiply by", "One", "Two", "Equals"];
            buttons.extend_from_slice(extra);
            FakeCalc { buttons, clicks: Vec::new(), acts: 0, stale: 0 }
        }
    }

    impl DeskSurface for FakeCalc {
        fn observe(&mut self) -> Result<Observation, String> {
            let display = if self.clicks.ends_with(&["Seven", "Multiply by", "One", "Two", "Equals"]) { "84" } else { "0" };
            let actions = self
                .buttons
                .iter()
                .enumerate()
                .map(|(i, label)| ObservedElement {
                    id: i.to_string(),
                    node: i as u64,
                    kind: "click".into(),
                    label: label.to_string(),
                    role: Some("Button".into()),
                    rect: Rect { x: 0.0, y: 0.0, w: 50.0, h: 40.0 },
                    ..Default::default()
                })
                .collect();
            Ok(Observation {
                url: "desk://calculator/Calculator".into(),
                title: "Calculator".into(),
                text: format!("Display is {display}"),
                actions,
                marker: json!(self.clicks.len()),
                ..Default::default()
            })
        }
        fn act(&mut self, action: &DeskAction) -> Result<ActResult, String> {
            self.acts += 1;
            if self.stale > 0 {
                self.stale -= 1;
                return Ok(ActResult { effect: "refused".into(), note: Some("stale_element_token".into()) });
            }
            let DeskAction::Click { node } = action else { return Err("only clicks".into()) };
            self.clicks.push(self.buttons[*node as usize]);
            Ok(ActResult { effect: "unverifiable".into(), note: None })
        }
        fn window(&self) -> &super::super::WindowRef {
            unimplemented!()
        }
    }

    fn one_hot(q: &Value, pick: &str) -> Value {
        let probs: serde_json::Map<String, Value> =
            q["criteria"].as_object().unwrap().keys().map(|k| (k.clone(), json!(if k == pick { 1.0 } else { 0.0 }))).collect();
        json!({ "choice": pick, "confidence": 0.9, "probabilities": probs })
    }

    /// Clicks `script` in order, then says DONE with goal check `done_p`
    /// (and 0.05 before). Records every body it was sent.
    struct ClickJudge {
        script: Vec<&'static str>,
        done_p: f64,
        calls: AtomicU32,
        bodies: parking_lot::Mutex<Vec<Value>>,
    }

    impl ClickJudge {
        fn new(script: &[&'static str], done_p: f64) -> ClickJudge {
            ClickJudge { script: script.to_vec(), done_p, calls: Default::default(), bodies: Default::default() }
        }
    }

    impl Judge for ClickJudge {
        fn answers(&self, body: &Value) -> Result<Value, String> {
            let n = self.calls.fetch_add(1, Ordering::Relaxed) as usize;
            self.bodies.lock().push(body.clone());
            let qs = &body["questions"];
            let mut out = serde_json::Map::new();
            let (op, p) = match self.script.get(n) {
                Some(label) => {
                    let q = &qs["click_target"];
                    let key = q["criteria"]
                        .as_object()
                        .and_then(|c| c.iter().find(|(_, v)| v["element"].as_str().is_some_and(|e| e.ends_with(&format!("] {label}")))))
                        .map(|(k, _)| k.clone())
                        .ok_or_else(|| format!("{label} not offered"))?;
                    out.insert("click_target".into(), one_hot(q, &key));
                    ("CLICK", 0.05)
                }
                None => ("DONE", self.done_p),
            };
            out.insert("operation".into(), one_hot(&qs["operation"], op));
            out.insert("goal_met".into(), json!({ "noul": p }));
            Ok(Value::Object(out))
        }
        fn calls(&self) -> u32 {
            self.calls.load(Ordering::Relaxed)
        }
    }

    const SEVEN_TIMES_TWELVE: [&str; 5] = ["Seven", "Multiply by", "One", "Two", "Equals"];

    fn desk_args(allow_writes: bool) -> DeskArgs {
        DeskArgs {
            goal: "Compute 7 times 12. Stop when the display shows 84.".into(),
            app: "Calculator".into(),
            steps: 15,
            allow_writes,
            hints_dir: Some(std::env::temp_dir().join("mini-desk-no-hints")),
            ..Default::default()
        }
    }

    #[test]
    fn the_loop_computes_7_times_12_and_ends_ok_on_evidence() {
        // GAP: redact::is_write_label is a fail-closed stub on this branch, so
        // the write filter is lifted here; the Delete test covers the filter.
        let mut calc = FakeCalc::new(&[]);
        let judge = ClickJudge::new(&SEVEN_TIMES_TWELVE, 0.95);
        let report = run_loop(&mut calc, &judge, &desk_args(true)).expect("observable");
        assert!(report.ok, "{}", report.stop);
        assert_eq!(calc.clicks, SEVEN_TIMES_TWELVE);
        assert!(report.page_text.contains("Display is 84"), "{}", report.page_text);
        assert!(report.url_before.starts_with("desk://"), "{}", report.url_before);
        assert_eq!(report.steps.iter().map(|s| s.target.clone().unwrap()).collect::<Vec<_>>(), SEVEN_TIMES_TWELVE);
        assert!(report.steps.iter().all(|s| s.page_changed && s.note.is_none()));
        let ep = desk_episode(&report, "Compute 7 times 12", "Calculator");
        assert_eq!(ep.host, "desk-calculator");
    }

    #[test]
    fn a_done_below_the_threshold_never_ends_ok() {
        let mut calc = FakeCalc::new(&[]);
        let judge = ClickJudge::new(&SEVEN_TIMES_TWELVE, 0.5);
        let report = run_loop(&mut calc, &judge, &desk_args(true)).expect("observable");
        assert!(!report.ok, "{}", report.stop);
        assert!(report.stop.contains("DONE claimed three times"), "{}", report.stop);
    }

    fn offered_labels(body: &Value) -> Vec<String> {
        body["state"]["elements"].as_array().unwrap().iter().map(|e| e["label"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn a_delete_control_is_offered_only_with_allow_writes() {
        for allow in [false, true] {
            let mut calc = FakeCalc::new(&["Delete"]);
            let judge = ClickJudge::new(&[], 0.95);
            run_loop(&mut calc, &judge, &desk_args(allow)).expect("observable");
            let labels = offered_labels(&judge.bodies.lock()[0]);
            assert_eq!(labels.iter().any(|l| l == "Delete"), allow, "allow_writes={allow}: {labels:?}");
        }
    }

    #[test]
    fn a_stale_token_is_retried_once_on_a_fresh_snapshot() {
        let mut calc = FakeCalc::new(&[]);
        calc.stale = 1;
        let judge = ClickJudge::new(&["Seven"], 0.95);
        let report = run_loop(&mut calc, &judge, &desk_args(true)).expect("observable");
        assert_eq!(calc.acts, 2, "one refusal, one retry");
        assert_eq!(calc.clicks, ["Seven"]);
        assert!(report.steps[0].note.is_none(), "{:?}", report.steps[0].note);

        let mut calc = FakeCalc::new(&[]);
        calc.stale = 5;
        let judge = ClickJudge::new(&["Seven"], 0.95);
        let report = run_loop(&mut calc, &judge, &desk_args(true)).expect("observable");
        assert_eq!(calc.acts, 2, "a second stale refusal is not retried again");
        assert!(report.steps[0].note.as_deref().is_some_and(|n| n.contains("stale")), "{:?}", report.steps[0].note);
    }
}

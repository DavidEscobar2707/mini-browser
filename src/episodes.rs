//! RL phase 1: every `mini --move` run is an episode with a code-computed
//! reward, stored per host as JSONL, and a human label (`--feedback`).
//! Contract: plans/004-episode-log-reward-feedback.md.
// `dead_code` until the wiring branch calls these from `move_cmd`/`main`.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Feedback {
    pub label: String,
    pub reason: Option<String>,
    pub ts_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct StepSample {
    pub step: usize,
    pub operation: String,
    pub target: Option<String>,
    pub page_sig: String,
    pub url: String,
    pub page_changed: bool,
    pub goal_before: Option<f64>,
    pub goal_after: Option<f64>,
    pub ms: u64,
    pub reward: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Episode {
    pub id: String,
    pub ts_ms: u64,
    pub host: String,
    pub goal: String,
    pub goal_key: String,
    pub ok: bool,
    pub stop: String,
    pub judge_calls: u32,
    pub elapsed_ms: u64,
    pub steps: Vec<StepSample>,
    pub episode_reward: f64,
    pub feedback: Option<Feedback>,
}

impl Episode {
    /// Reward after the human label: `bad` pins it to -2.0, `good` floors it
    /// at 1.0, no label leaves the code-computed reward untouched.
    pub fn final_reward(&self) -> f64 {
        match self.feedback.as_ref() {
            Some(f) if f.label.eq_ignore_ascii_case("bad") => -2.0,
            Some(f) if f.label.eq_ignore_ascii_case("good") => self.episode_reward.max(1.0),
            _ => self.episode_reward,
        }
    }
}

/// One step as `run_loop` saw it, before rewards are filled in.
#[derive(Clone, Debug, Default)]
pub struct RawStep {
    pub step: usize,
    pub operation: String,
    pub target: Option<String>,
    pub page_sig: String,
    pub url: String,
    pub page_changed: bool,
    pub goal_met: Option<f64>,
    pub ms: u64,
}

pub fn root_dir(dir: Option<&Path>) -> PathBuf {
    match dir {
        Some(d) => d.to_path_buf(),
        None => dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("Mini").join("episodes"),
    }
}

/// `"<host><path-pattern>#<hash8>"`: host without `www.`, id-like path
/// segments folded to `:id`, and a hash of the page's control labels so two
/// runs of the same page with different showtimes/prices share one signature.
pub fn page_signature(url: &str, labels: &[String]) -> String {
    format!("{}{}#{}", host_of(url), path_pattern(url), label_hash(labels))
}

pub fn normalize_goal(goal: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for c in goal.to_lowercase().chars() {
        if c.is_whitespace() {
            pending_space = true;
        } else if c.is_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(c);
        }
    }
    out
}

/// Delta in goal satisfaction, minus a 0.05 cost per step, minus 0.3 when the
/// step did not even move the page. Missing goal values count as 0.0.
pub fn step_reward(goal_before: Option<f64>, goal_after: Option<f64>, page_changed: bool) -> f64 {
    let mut reward = goal_after.unwrap_or(0.0) - goal_before.unwrap_or(0.0) - 0.05;
    if !page_changed {
        reward -= 0.3;
    }
    reward
}

pub fn episode_reward(ok: bool, steps: &[StepSample]) -> f64 {
    let mut reward = if ok { 1.0 } else { -1.0 };
    for step in steps {
        reward += step.reward;
    }
    reward.clamp(-3.0, 3.0)
}

pub fn build_episode(goal: &str, ok: bool, stop: &str, judge_calls: u32, elapsed_ms: u64, raw: &[RawStep]) -> Episode {
    let ts_ms = now_ms();
    let steps: Vec<StepSample> = raw
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let goal_before = r.goal_met;
            // The next step's check is this step's outcome; the run's own
            // verdict closes the last step.
            let goal_after = match raw.get(i + 1) {
                Some(next) => next.goal_met,
                None if ok => Some(1.0),
                None => goal_before,
            };
            StepSample {
                step: r.step,
                operation: r.operation.clone(),
                target: r.target.clone(),
                page_sig: r.page_sig.clone(),
                url: r.url.clone(),
                page_changed: r.page_changed,
                goal_before,
                goal_after,
                ms: r.ms,
                reward: step_reward(goal_before, goal_after, r.page_changed),
            }
        })
        .collect();
    Episode {
        id: format!("{ts_ms}-{}", std::process::id()),
        ts_ms,
        host: raw.first().map(|r| host_of(&r.url)).unwrap_or_default(),
        goal: goal.to_string(),
        goal_key: normalize_goal(goal),
        ok,
        stop: stop.to_string(),
        judge_calls,
        elapsed_ms,
        episode_reward: episode_reward(ok, &steps),
        steps,
        feedback: None,
    }
}

/// Appends one line to `<host>.jsonl` and points `last.json` at it.
pub fn append(dir: Option<&Path>, ep: &Episode) -> Result<PathBuf, String> {
    let dir = root_dir(dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = dir.join(format!("{}.jsonl", file_stem_for(&ep.host)));
    let line = serde_json::to_string(ep).map_err(|e| format!("encode episode: {e}"))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    writeln!(file, "{line}").map_err(|e| format!("append {}: {e}", path.display()))?;
    let last = serde_json::json!({ "host": ep.host, "id": ep.id });
    write_atomic(&dir.join("last.json"), last.to_string().as_bytes())?;
    Ok(path)
}

/// Labels one stored episode (`None` = the newest one) and rewrites that line
/// in place, leaving every other line byte-identical.
pub fn set_feedback(dir: Option<&Path>, id: Option<&str>, label: &str, reason: Option<&str>) -> Result<Episode, String> {
    let label = label.trim().to_ascii_lowercase();
    if label != "good" && label != "bad" {
        return Err(format!("feedback label must be 'good' or 'bad', got '{label}'"));
    }
    let dir = root_dir(dir);
    let (path, want_id) = match id.map(str::trim).filter(|i| !i.is_empty()) {
        Some(id) => (file_with_id(&dir, id)?, id.to_string()),
        None => {
            let (host, id) = read_last(&dir)?;
            (dir.join(format!("{}.jsonl", file_stem_for(&host))), id)
        }
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("no episode to label: {} ({e})", path.display()))?;
    let mut rewritten = String::with_capacity(text.len());
    let mut updated: Option<Episode> = None;
    for line in text.lines() {
        if updated.is_none() {
            if let Ok(mut ep) = serde_json::from_str::<Episode>(line.trim()) {
                if ep.id == want_id {
                    ep.feedback = Some(Feedback {
                        label: label.clone(),
                        reason: reason.map(str::trim).filter(|r| !r.is_empty()).map(str::to_string),
                        ts_ms: now_ms(),
                    });
                    let encoded = serde_json::to_string(&ep).map_err(|e| format!("encode episode: {e}"))?;
                    rewritten.push_str(&encoded);
                    rewritten.push('\n');
                    updated = Some(ep);
                    continue;
                }
            }
        }
        rewritten.push_str(line);
        rewritten.push('\n');
    }
    let updated = updated.ok_or_else(|| format!("episode '{want_id}' not found in {}", path.display()))?;
    write_atomic(&path, rewritten.as_bytes())?;
    Ok(updated)
}

/// Aggregate over every stored episode (or one host's file): runs, reward
/// means, and the per-action table the learner will read.
pub fn summary(dir: Option<&Path>, host: Option<&str>) -> Result<serde_json::Value, String> {
    let dir = root_dir(dir);
    let mut episodes: Vec<Episode> = Vec::new();
    let mut corrupt_lines = 0u64;
    for path in host_files(&dir, host)? {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            // A missing file for an explicitly requested host is an empty log.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("reading {}: {e}", path.display())),
        };
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Episode>(line) {
                Ok(ep) => episodes.push(ep),
                Err(_) => corrupt_lines += 1,
            }
        }
    }

    let n = episodes.len() as f64;
    let mut ok_count = 0u64;
    let mut reward_sum = 0.0;
    let mut judge_sum = 0.0;
    let mut elapsed_sum = 0.0;
    let mut actions: Vec<(String, Option<String>, String, u64, f64)> = Vec::new();
    for ep in &episodes {
        if ep.ok {
            ok_count += 1;
        }
        reward_sum += ep.final_reward();
        judge_sum += ep.judge_calls as f64;
        elapsed_sum += ep.elapsed_ms as f64;
        for step in &ep.steps {
            match actions
                .iter_mut()
                .find(|a| a.0 == step.page_sig && a.1 == step.target && a.2 == step.operation)
            {
                Some(a) => {
                    a.3 += 1;
                    a.4 += step.reward;
                }
                None => actions.push((step.page_sig.clone(), step.target.clone(), step.operation.clone(), 1, step.reward)),
            }
        }
    }
    let mut rows: Vec<serde_json::Value> = actions
        .iter()
        .map(|(page_sig, target, operation, count, sum)| {
            serde_json::json!({
                "page_sig": page_sig,
                "target": target,
                "operation": operation,
                "n": count,
                "mean_reward": sum / *count as f64,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        let (ar, br) = (a["mean_reward"].as_f64().unwrap_or(0.0), b["mean_reward"].as_f64().unwrap_or(0.0));
        br.total_cmp(&ar)
            .then(b["n"].as_u64().unwrap_or(0).cmp(&a["n"].as_u64().unwrap_or(0)))
            .then(a["page_sig"].as_str().unwrap_or("").cmp(b["page_sig"].as_str().unwrap_or("")))
            .then(a["target"].as_str().unwrap_or("").cmp(b["target"].as_str().unwrap_or("")))
            .then(a["operation"].as_str().unwrap_or("").cmp(b["operation"].as_str().unwrap_or("")))
    });
    rows.truncate(50);

    Ok(serde_json::json!({
        "episodes": episodes.len(),
        "ok_rate": if episodes.is_empty() { 0.0 } else { ok_count as f64 / n },
        "mean_final_reward": if episodes.is_empty() { 0.0 } else { reward_sum / n },
        "mean_judge_calls": if episodes.is_empty() { 0.0 } else { judge_sum / n },
        "mean_elapsed_ms": if episodes.is_empty() { 0.0 } else { elapsed_sum / n },
        "corrupt_lines": corrupt_lines,
        "actions": rows,
    }))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Lowercased host with `www.` and any port removed (`host_only` already
/// drops the port, so `%LOCALAPPDATA%\Mini\episodes` matches `--hints`).
fn host_of(url: &str) -> String {
    let host = crate::qa::firewall::host_only(url);
    host.strip_prefix("www.").unwrap_or(&host).to_string()
}

/// `/search/12345?q=x#top` -> `/search/:id`: digits-only, uuid-like and
/// over-long segments folded to `:id`, query/fragment and trailing `/` gone.
fn path_pattern(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let path = match after_scheme.find('/') {
        Some(i) => &after_scheme[i..],
        None => "",
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    let mut out = String::new();
    for segment in path.trim_end_matches('/').split('/').filter(|s| !s.is_empty()) {
        out.push('/');
        out.push_str(if is_id_like(segment) { ":id" } else { segment });
    }
    out
}

fn is_id_like(segment: &str) -> bool {
    if segment.chars().count() > 24 {
        return true;
    }
    if !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    is_uuid_like(segment)
}

/// 8-4-4-4-12 hyphenated UUID, or a hex run long enough to be one.
fn is_uuid_like(segment: &str) -> bool {
    let hex = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit());
    let groups: Vec<&str> = segment.split('-').collect();
    if groups.len() == 5 && [8usize, 4, 4, 4, 12].iter().zip(&groups).all(|(len, g)| g.len() == *len && hex(g)) {
        return true;
    }
    segment.len() >= 16 && hex(segment)
}

/// FNV-1a 64 over the first 40 labels, each lowercased, digits stripped,
/// trimmed, deduped and sorted — so a changed showtime or price keeps the
/// signature and a changed control set breaks it.
fn label_hash(labels: &[String]) -> String {
    let mut normalized: Vec<String> = Vec::new();
    for label in labels.iter().take(40) {
        let cleaned: String = label.to_lowercase().chars().filter(|c| !c.is_ascii_digit()).collect();
        let cleaned = cleaned.trim();
        if cleaned.is_empty() || normalized.iter().any(|n| n == cleaned) {
            continue;
        }
        normalized.push(cleaned.to_string());
    }
    normalized.sort();
    format!("{:016x}", fnv1a64(normalized.join("\n").as_bytes()))[..8].to_string()
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// `<host>.jsonl`; an empty or filesystem-hostile host never escapes the dir.
fn file_stem_for(host: &str) -> String {
    let cleaned: String = host
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if !cleaned.chars().any(|c| c.is_ascii_alphanumeric()) {
        "unknown".to_string()
    } else {
        cleaned
    }
}

fn host_files(dir: &Path, host: Option<&str>) -> Result<Vec<PathBuf>, String> {
    if let Some(host) = host {
        let path = dir.join(format!("{}.jsonl", file_stem_for(&host_of(host))));
        return Ok(if path.is_file() { vec![path] } else { Vec::new() });
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // A host that was never recorded has no folder yet: an empty log.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("reading {}: {e}", dir.display())),
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "jsonl").unwrap_or(false))
        .collect();
    paths.sort();
    Ok(paths)
}

fn read_last(dir: &Path) -> Result<(String, String), String> {
    let path = dir.join("last.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("no episode to label: {} ({e})", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let id = value.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    if id.is_empty() {
        return Err(format!("{} has no episode id", path.display()));
    }
    let host = value.get("host").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    Ok((host, id))
}

fn file_with_id(dir: &Path, id: &str) -> Result<PathBuf, String> {
    for path in host_files(dir, None)? {
        let text = std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        if text.lines().any(|l| serde_json::from_str::<Episode>(l.trim()).map(|ep| ep.id == id).unwrap_or(false)) {
            return Ok(path);
        }
    }
    Err(format!("episode '{id}' not found in {}", dir.display()))
}

/// Write-tmp-then-rename: a reader never sees a half-written log line.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename {}: {e}", path.display())
    })
}


// ---- RL phase 2 (plans/005-action-value-priors.md). STUB: worker p2-core implements. ----

pub const GAMMA: f64 = 0.9;

pub fn terminal_reward(ep: &Episode) -> f64 {
    0.0
}

pub fn discounted_returns(ep: &Episode, gamma: f64) -> Vec<f64> {
    vec![0.0; ep.steps.len()]
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Prior {
    pub operation: String,
    pub target: Option<String>,
    pub n: u32,
    pub mean_return: f64,
    pub exact: bool,
}

#[derive(Clone, Debug, Default)]
pub struct PriorTable {
    rows: Vec<(String, Prior)>,
}

impl PriorTable {
    pub fn load(dir: Option<&Path>, host: &str) -> PriorTable {
        PriorTable::default()
    }
    pub fn from_episodes(eps: &[Episode]) -> PriorTable {
        PriorTable::default()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn for_page(&self, page_sig: &str, top: usize) -> Vec<Prior> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mini-episodes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn labels(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn step(step: usize, reward: f64) -> StepSample {
        StepSample { step, reward, ..Default::default() }
    }

    fn raw(step: usize, url: &str, goal_met: Option<f64>, page_changed: bool) -> RawStep {
        RawStep {
            step,
            operation: "CLICK".into(),
            target: Some(format!("button {step}")),
            page_sig: "example.com/search#e67d8c7f".into(),
            url: url.into(),
            page_changed,
            goal_met,
            ms: 100,
        }
    }

    /// The contract's transform strips digits only, so the two runs must differ
    /// in digits alone (times and prices), not in punctuation.
    #[test]
    fn signature_is_stable_when_digits_change() {
        let evening = labels(&["Log In", "Showtimes 7:30 PM", "Search", "showtimes 9:45 pm", "Prices $12.50"]);
        let matinee = labels(&["log in", "SHOWTIMES 2:15 PM", "search", "Showtimes 6:40 pm", "Prices $9.90"]);
        let sig = page_signature("https://www.cinema.example/showtimes", &evening);
        assert_eq!(sig, page_signature("https://www.cinema.example/showtimes", &matinee), "digits in labels must not move the signature");
        // Golden value locks the FNV-1a 64 prefix: changing it invalidates every
        // stored page_sig, so it must be a deliberate contract change.
        assert_eq!(sig, "cinema.example/showtimes#6db8ff34");
        assert_ne!(sig, page_signature("https://www.cinema.example/showtimes", &labels(&["Log In", "Search"])), "a different control set must move the signature");
    }

    #[test]
    fn signature_folds_id_like_segments() {
        assert_eq!(
            page_signature("https://example.com/movies/12345/play", &[]),
            "example.com/movies/:id/play#cbf29ce4"
        );
        assert_eq!(
            page_signature("https://example.com/m/9f8e7d6c5b4a39281706f5e4d3c2b1a0", &[]),
            "example.com/m/:id#cbf29ce4"
        );
        assert_eq!(
            page_signature("https://example.com/m/123e4567-e89b-12d3-a456-426614174000", &[]),
            "example.com/m/:id#cbf29ce4"
        );
        assert_eq!(
            page_signature("https://example.com/m/a-very-long-slug-that-exceeds-the-limit", &[]),
            "example.com/m/:id#cbf29ce4"
        );
        assert_eq!(page_signature("https://example.com/m/486-cinema", &[]), "example.com/m/486-cinema#cbf29ce4");
    }

    #[test]
    fn signature_drops_www_query_fragment_and_trailing_slash() {
        let plain = page_signature("https://example.com/search", &labels(&["Search"]));
        assert_eq!(page_signature("https://www.example.com/search/", &labels(&["Search"])), plain);
        assert_eq!(page_signature("http://example.com:8080/search?page=2#results", &labels(&["Search"])), plain);
    }

    #[test]
    fn signature_hashes_only_the_first_40_labels() {
        let forty: Vec<String> = (0..40)
            .map(|i| format!("ctrl {}{}", (b'a' + (i / 10) as u8) as char, (b'a' + (i % 10) as u8) as char))
            .collect();
        let mut forty_one = forty.clone();
        forty_one.push("extra ctrl".into());
        assert_eq!(page_signature("https://example.com/", &forty), page_signature("https://example.com/", &forty_one));
        assert_ne!(
            page_signature("https://example.com/", &forty),
            page_signature("https://example.com/", &forty[..39]),
            "the fortieth label is inside the window"
        );
    }

    #[test]
    fn normalize_goal_keeps_letters_digits_accents() {
        assert_eq!(normalize_goal("  Book   the 2 PM \"Dune\" screening! "), "book the 2 pm dune screening");
        assert_eq!(normalize_goal("Café\tGéant"), "café géant");
        assert_eq!(normalize_goal("!!!"), "");
    }

    #[test]
    fn step_reward_numbers() {
        assert!((step_reward(Some(0.4), Some(0.9), true) - 0.45).abs() < 1e-9);
        assert!((step_reward(None, Some(1.0), true) - 0.95).abs() < 1e-9, "missing goal_before counts as 0.0");
        assert!((step_reward(Some(1.0), None, true) + 1.05).abs() < 1e-9, "missing goal_after counts as 0.0");
        assert!((step_reward(Some(0.5), Some(0.5), true) + 0.05).abs() < 1e-9);
        assert!((step_reward(Some(0.5), Some(0.5), false) + 0.35).abs() < 1e-9, "an unchanged page costs 0.3 more");
        assert!((step_reward(None, None, false) + 0.35).abs() < 1e-9);
    }

    #[test]
    fn build_episode_chains_goal_checks_and_closes_last_step() {
        let raw = [
            raw(1, "https://www.example.com/search", None, true),
            raw(2, "https://www.example.com/booking", Some(0.6), true),
        ];
        let ep = build_episode("Book Dune tickets", true, "DONE", 3, 4210, &raw);
        assert_eq!(ep.host, "example.com", "host comes from the first step, without www.");
        assert_eq!(ep.goal_key, "book dune tickets");
        assert_eq!((ep.stop.as_str(), ep.judge_calls, ep.elapsed_ms, ep.ok), ("DONE", 3, 4210, true));
        assert!(ep.feedback.is_none());
        assert!(ep.ts_ms > 0);
        assert_eq!(ep.id, format!("{}-{}", ep.ts_ms, std::process::id()));

        // goal_after of step i is goal_met of step i+1; the ok verdict closes the last.
        assert_eq!(ep.steps[0].goal_before, None);
        assert_eq!(ep.steps[0].goal_after, Some(0.6));
        assert!((ep.steps[0].reward - 0.55).abs() < 1e-9);
        assert_eq!(ep.steps[1].goal_before, Some(0.6));
        assert_eq!(ep.steps[1].goal_after, Some(1.0));
        assert!((ep.steps[1].reward - 0.35).abs() < 1e-9);

        // episode_reward = +1.0 for ok + the step rewards.
        assert!((ep.episode_reward - 1.9).abs() < 1e-9);
        assert!((ep.final_reward() - 1.9).abs() < 1e-9);
    }

    #[test]
    fn build_episode_failed_run_keeps_last_goal_before() {
        let raw = [
            raw(1, "https://example.com/a", Some(0.2), true),
            raw(2, "https://example.com/b", Some(0.2), false),
        ];
        let ep = build_episode("Book tickets", false, "STUCK", 5, 900, &raw);
        assert_eq!(ep.steps[1].goal_before, Some(0.2));
        assert_eq!(ep.steps[1].goal_after, Some(0.2), "a failed run does not claim progress on the last step");
        assert!((ep.steps[1].reward + 0.35).abs() < 1e-9);
        assert!((ep.episode_reward + 1.4).abs() < 1e-9, "-1.0 for the failed run plus -0.05 and -0.35");
    }

    #[test]
    fn build_episode_without_steps_has_no_host() {
        let ep = build_episode("Book tickets", false, "", 0, 10, &[]);
        assert_eq!(ep.host, "");
        assert!(ep.steps.is_empty());
        assert_eq!(ep.episode_reward, -1.0);
    }

    #[test]
    fn episode_reward_is_clamped() {
        let big = vec![step(1, 5.0), step(2, 5.0)];
        assert_eq!(episode_reward(true, &big), 3.0, "the run verdict is added before the clamp");
        assert_eq!(episode_reward(false, &big), 3.0, "a run full of progress is not dragged down by a bad verdict");
        let bad = vec![step(1, -5.0), step(2, -5.0)];
        assert_eq!(episode_reward(false, &bad), -3.0);
        assert_eq!(episode_reward(true, &bad), -3.0, "a failed verdict cannot be rescued above the floor");
        assert_eq!(episode_reward(false, &[step(1, 0.5)]), -0.5, "no clamp inside the range");
    }

    #[test]
    fn final_reward_follows_feedback() {
        let mut ep = Episode { episode_reward: 0.4, ..Default::default() };
        assert!((ep.final_reward() - 0.4).abs() < 1e-9);
        ep.feedback = Some(Feedback { label: "bad".into(), reason: Some("wrong movie".into()), ts_ms: 1 });
        assert!((ep.final_reward() + 2.0).abs() < 1e-9, "a human 'bad' pins the reward at -2.0");
        ep.feedback = Some(Feedback { label: "good".into(), reason: None, ts_ms: 1 });
        assert!((ep.final_reward() - 1.0).abs() < 1e-9, "a human 'good' floors the reward at 1.0");
        ep.episode_reward = 2.5;
        assert!((ep.final_reward() - 2.5).abs() < 1e-9, "a 'good' label never lowers a higher code reward");
        ep.feedback = Some(Feedback { label: "BAD".into(), reason: None, ts_ms: 1 });
        assert!((ep.final_reward() + 2.0).abs() < 1e-9);
    }

    #[test]
    fn append_and_set_feedback_round_trip() {
        let dir = tmp_dir("roundtrip");
        let first = build_episode("Book Dune", false, "STUCK", 4, 800, &[raw(1, "https://www.example.com/a", Some(0.1), false)]);
        let mut second = build_episode("Book Dune", true, "DONE", 2, 1200, &[raw(1, "https://www.example.com/b", Some(0.9), true)]);
        second.ts_ms += 1; // distinct id, same host -> same file
        second.id = format!("{}-{}", second.ts_ms, std::process::id());

        let path = append(Some(&dir), &first).unwrap();
        assert_eq!(path, dir.join("example.com.jsonl"));
        append(Some(&dir), &second).unwrap();
        let lines: Vec<String> = std::fs::read_to_string(&path).unwrap().lines().map(str::to_string).collect();
        assert_eq!(lines.len(), 2, "append is append-only");
        assert_eq!(serde_json::from_str::<Episode>(&lines[0]).unwrap().id, first.id);

        let last: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("last.json")).unwrap()).unwrap();
        assert_eq!(last["host"], "example.com");
        assert_eq!(last["id"], second.id.as_str());

        let updated = set_feedback(Some(&dir), None, "bad", Some(" booked the wrong showing ")).unwrap();
        assert_eq!(updated.id, second.id, "no id means the newest episode");
        assert_eq!(updated.feedback.as_ref().unwrap().label, "bad");
        assert_eq!(updated.feedback.as_ref().unwrap().reason.as_deref(), Some("booked the wrong showing"));
        assert!(updated.feedback.as_ref().unwrap().ts_ms > 0);
        assert!((updated.final_reward() + 2.0).abs() < 1e-9);

        let lines_after: Vec<String> = std::fs::read_to_string(&path).unwrap().lines().map(str::to_string).collect();
        assert_eq!(lines_after.len(), 2, "only the one line is rewritten");
        assert_eq!(lines_after[0], lines[0], "the other episode stays byte-identical");
        let reread: Episode = serde_json::from_str(&lines_after[1]).unwrap();
        assert_eq!(reread.feedback.unwrap().label, "bad");

        let by_id = set_feedback(Some(&dir), Some(&first.id), "GOOD", None).unwrap();
        assert_eq!(by_id.id, first.id);
        assert_eq!(by_id.feedback.as_ref().unwrap().label, "good");
        assert!(by_id.feedback.as_ref().unwrap().reason.is_none(), "an empty reason is stored as none");
        assert_eq!(by_id.final_reward(), 1.0, "a 'good' episode that failed still floors at 1.0");
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);

        assert!(set_feedback(Some(&dir), None, "great", None).is_err(), "only good|bad are valid labels");
        assert!(set_feedback(Some(&dir), Some("nope-1"), "good", None).is_err(), "unknown id must fail loudly");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_creates_the_folder_and_keeps_hosts_apart() {
        let dir = tmp_dir("hosts").join("episodes");
        let ep = build_episode("Book Dune", true, "DONE", 1, 10, &[raw(1, "https://www.cinema.example/x", Some(1.0), true)]);
        let path = append(Some(&dir), &ep).unwrap();
        assert_eq!(path, dir.join("cinema.example.jsonl"));
        assert!(path.is_file());
        assert!(dir.join("last.json").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summary_skips_corrupt_lines_and_ranks_actions() {
        let dir = tmp_dir("summary");
        let mut good = build_episode("Book Dune", true, "DONE", 2, 1000, &[
            RawStep { step: 1, operation: "CLICK".into(), target: Some("Search".into()), page_sig: "example.com/#aaaaaaaa".into(), url: "https://example.com/".into(), page_changed: true, goal_met: Some(0.5), ms: 50 },
            RawStep { step: 2, operation: "CLICK".into(), target: Some("Book".into()), page_sig: "example.com/booking#bbbbbbbb".into(), url: "https://example.com/booking".into(), page_changed: true, goal_met: Some(1.0), ms: 50 },
        ]);
        good.id = "100-1".into();
        good.ts_ms = 100;
        let mut bad = build_episode("Book Dune", false, "STUCK", 4, 500, &[
            RawStep { step: 1, operation: "CLICK".into(), target: Some("Search".into()), page_sig: "example.com/#aaaaaaaa".into(), url: "https://example.com/".into(), page_changed: false, goal_met: Some(0.5), ms: 50 },
            RawStep { step: 2, operation: "TYPE".into(), target: Some("Search".into()), page_sig: "example.com/#aaaaaaaa".into(), url: "https://example.com/".into(), page_changed: true, goal_met: Some(0.5), ms: 50 },
        ]);
        bad.id = "200-1".into();
        bad.ts_ms = 200;
        assert_eq!(append(Some(&dir), &good).unwrap(), append(Some(&dir), &bad).unwrap());
        set_feedback(Some(&dir), Some("200-1"), "bad", None).unwrap();

        let file = dir.join("example.com.jsonl");
        let mut text = std::fs::read_to_string(&file).unwrap();
        text.push_str("{ this is not json }\n\n");
        std::fs::write(&file, text).unwrap();

        let s = summary(Some(&dir), None).unwrap();
        assert_eq!(s["episodes"], 2, "the corrupt line is skipped, the blank one ignored");
        assert_eq!(s["corrupt_lines"], 1);
        assert_eq!(s["ok_rate"], 0.5);
        // Code rewards: ok run (+1.0 + 0.45 - 0.05) = 1.4; stuck run
        // (-1.0 - 0.35 - 0.05) = -1.4 but the human label pins it at -2.0.
        assert!((s["mean_final_reward"].as_f64().unwrap() - (-0.3)).abs() < 1e-9, "{s}");
        assert_eq!(s["mean_judge_calls"], 3.0);
        assert_eq!(s["mean_elapsed_ms"], 750.0);

        let actions = s["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 3, "one row per page_sig+target+operation");
        // Search/CLICK ran twice: 0.45 from the ok run, -0.35 from the failed one.
        let search_click = actions.iter().find(|a| a["target"] == "Search" && a["operation"] == "CLICK").unwrap();
        assert_eq!(search_click["page_sig"], "example.com/#aaaaaaaa");
        assert_eq!(search_click["n"], 2);
        assert!((search_click["mean_reward"].as_f64().unwrap() - 0.05).abs() < 1e-9);
        let book = actions.iter().find(|a| a["target"] == "Book").unwrap();
        assert_eq!(book["page_sig"], "example.com/booking#bbbbbbbb");
        assert_eq!(book["n"], 1);
        assert!((book["mean_reward"].as_f64().unwrap() + 0.05).abs() < 1e-9);
        let typed = actions.iter().find(|a| a["operation"] == "TYPE").unwrap();
        assert_eq!(typed["n"], 1);
        assert!((typed["mean_reward"].as_f64().unwrap() + 0.05).abs() < 1e-9);

        assert_eq!(actions[0]["target"], "Search", "the best action is first");
        assert_eq!(actions[0]["operation"], "CLICK");
        let means: Vec<f64> = actions.iter().map(|a| a["mean_reward"].as_f64().unwrap()).collect();
        assert!(means.windows(2).all(|w| w[0] >= w[1]), "rows are sorted by mean_reward desc: {means:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summary_filters_one_host_and_tolerates_a_missing_log() {
        let dir = tmp_dir("summary-host");
        let a = build_episode("Book Dune", true, "DONE", 1, 10, &[raw(1, "https://www.example.com/a", Some(1.0), true)]);
        let b = build_episode("Find coffee", true, "DONE", 3, 20, &[raw(1, "https://other.test/b", Some(1.0), true)]);
        append(Some(&dir), &a).unwrap();
        append(Some(&dir), &b).unwrap();

        let all = summary(Some(&dir), None).unwrap();
        assert_eq!(all["episodes"], 2);
        let one = summary(Some(&dir), Some("www.example.com")).unwrap();
        assert_eq!(one["episodes"], 1, "a www. host arg still finds the file");
        assert_eq!(one["mean_judge_calls"], 1.0);
        let missing = summary(Some(&dir), Some("nope.test")).unwrap();
        assert_eq!(missing["episodes"], 0);
        assert_eq!(missing["ok_rate"], 0.0);
        assert_eq!(missing["actions"].as_array().unwrap().len(), 0);
        let empty = summary(Some(&dir.join("nothing-here")), None).unwrap();
        assert_eq!(empty["episodes"], 0);
        assert_eq!(empty["corrupt_lines"], 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

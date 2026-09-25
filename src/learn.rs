//! RL phase 4: `mini --learn` turns the episode log into the per-site lesson
//! block that `--move` sends to Jev as `site_notes`, so the hand-written
//! lessons become an output of the data.
//! Contract: plans/006-route-replay-and-learn.md.

use crate::episodes::{self, Episode, Prior, PriorTable, Route};
use std::path::{Path, PathBuf};

/// The learned block's markers. Everything between them belongs to this
/// command; every other line of the file is the operator's.
pub const LEARNED_START: &str = "<!-- mini:learned -->";
pub const LEARNED_END: &str = "<!-- /mini:learned -->";

/// Bullet cap and hard section cap: site notes are cut at 1500 chars
/// (see `move_cmd::SITE_NOTES`) and Jev pays per token, so the block stays
/// small enough to leave room for the operator's own lines.
const MAX_BULLETS: usize = 8;
const MAX_SECTION: usize = 700;

/// `n` tries before an action counts as learned.
const MIN_TRIES: u32 = 2;
/// Mean return at which an action helps / hurts.
const HELPS_AT: f64 = 0.5;
const AVOID_AT: f64 = -0.2;

/// `mini --learn [host] [--write] [--hints DIR]`, the words after the flag.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LearnArgs {
    /// Host to learn from (`www.` dropped, port dropped); `None` = every
    /// `*.jsonl` host in the episodes folder.
    pub host: Option<String>,
    /// Write the hints file; without it this is a dry run that prints the
    /// section.
    pub write: bool,
    /// Hints folder (`<host>.md`), default `%LOCALAPPDATA%\Mini\hints`.
    pub hints_dir: Option<PathBuf>,
}

/// Parses `--learn [host] [--write] [--hints DIR]`. A host or a full URL is
/// accepted.
pub fn parse_args(args: &[String]) -> Result<LearnArgs, String> {
    const USAGE: &str = "usage: mini --learn [host] [--write] [--hints DIR]";
    let mut out = LearnArgs::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--write" => out.write = true,
            "--hints" => {
                out.hints_dir = Some(args.get(i + 1).ok_or("--hints needs a folder")?.into());
                i += 1;
            }
            flag if flag.starts_with("--") => return Err(format!("unknown flag {flag}; {USAGE}")),
            word => {
                if out.host.is_some() {
                    return Err(format!("--learn takes one host; got an extra {word}"));
                }
                let host = crate::move_cmd::episode_host(word);
                if host.is_empty() {
                    return Err("--learn needs a host or a URL".into());
                }
                out.host = Some(host);
            }
        }
        i += 1;
    }
    Ok(out)
}

/// `mini --learn ...`: read the episode log, build the learned section, print
/// it (dry run) or write it into the hints file. Exit 0 when something was
/// printed/written, 1 when a host has no episodes, 2 on a bad command line.
pub fn run(args: &[String], index: usize) -> i32 {
    let eps_dir = episodes::root_dir(None);
    run_in(args, index, &eps_dir)
}

/// `run` against an explicit episodes folder, so the tests can drive the whole
/// command without reading `%LOCALAPPDATA%\Mini`.
pub fn run_in(args: &[String], index: usize, eps_dir: &Path) -> i32 {
    let a = match parse_args(&args[index + 1..]) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("mini --learn: {e}");
            return 2;
        }
    };
    let hosts = match a.host.clone() {
        Some(host) => vec![host],
        None => hosts_in(eps_dir),
    };
    if hosts.is_empty() {
        eprintln!("mini --learn: no episode logs in {}", eps_dir.display());
        return 1;
    }
    let mut failed = false;
    for host in hosts {
        match learn_host(&host, Some(eps_dir), a.hints_dir.as_deref(), a.write) {
            Ok(out) => println!("{out}"),
            Err(e) => {
                eprintln!("mini --learn: {e}");
                failed = true;
            }
        }
    }
    i32::from(failed)
}

/// One host: its episodes in, the section (dry run) or the hints path out.
/// Directories are arguments so a test never reads or writes
/// `%LOCALAPPDATA%\Mini`.
pub fn learn_host(host: &str, eps_dir: Option<&Path>, hints_dir: Option<&Path>, write: bool) -> Result<String, String> {
    let given = host.trim();
    // Same guard as `site_notes`: a host must never escape the hints folder.
    if given.is_empty() || given.contains("..") || given.contains(['/', '\\', ':']) {
        return Err(format!("not a usable host: {given}"));
    }
    // The hints file must be the one `site_notes` looks for: lowercase, no
    // `www.`, no port.
    let host = crate::move_cmd::episode_host(given);
    if host.is_empty() {
        return Err(format!("not a usable host: {given}"));
    }
    let eps = episodes::load_episodes(eps_dir, &host);
    if eps.is_empty() {
        return Err(format!("no episodes for {host} in {}", episodes::root_dir(eps_dir).display()));
    }
    let section = learned_section(&eps);
    if !write {
        return Ok(section);
    }
    let dir = hints_dir
        .map(Path::to_path_buf)
        .or_else(|| dirs::data_local_dir().map(|d| d.join("Mini").join("hints")))
        .ok_or("no hints folder (%LOCALAPPDATA% is unavailable)")?;
    let path = dir.join(format!("{host}.md"));
    let existing = std::fs::read_to_string(&path).ok();
    let text = upsert_learned(existing.as_deref(), &host, &section, eps.len(), &today());
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    write_atomic(&path, text.as_bytes())?;
    Ok(path.display().to_string())
}

/// The `*.jsonl` host names in the episodes folder, sorted; a missing folder
/// is no hosts.
pub fn hosts_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut hosts: Vec<String> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "jsonl").unwrap_or(false))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
        .collect();
    hosts.sort();
    hosts
}

// ---- the section ----

/// `Learned from <N> episodes (ok rate <p>%).` plus the bullets worth
/// repeating: actions with `n >= 2` that clearly helped, then actions with
/// `n >= 2` that clearly wasted a step, then the route for the most common
/// goal. Bullets are plain lines (no `#`) so `move_cmd::note_body` keeps them
/// when the file is sent as site notes.
pub fn learned_section(eps: &[Episode]) -> String {
    let merged = merge_rows(PriorTable::from_episodes(eps).rows());
    let mut bullets = action_bullets(&merged);
    // `best_route` is the p3-replay worker's; while it is a stub no episode
    // has a route and this bullet is simply absent.
    if let Some(route) = most_common_goal(eps).and_then(|key| episodes::best_route(eps, &key)) {
        bullets.push(route_bullet(&route));
    }
    section_from(&header(eps), &bullets)
}

fn header(eps: &[Episode]) -> String {
    let ok = eps.iter().filter(|e| e.ok).count();
    let rate = (ok as f64 * 100.0 / eps.len() as f64).round() as usize;
    format!("Learned from {} episodes (ok rate {rate}%).", eps.len())
}

/// Header plus as many bullets as fit: at most [`MAX_BULLETS`] lines and at
/// most [`MAX_SECTION`] characters, so the file stays a note and not a log.
fn section_from(header: &str, bullets: &[String]) -> String {
    let mut out = header.to_string();
    let mut used = out.chars().count();
    for bullet in bullets.iter().take(MAX_BULLETS) {
        let len = bullet.chars().count();
        if used + 1 + len > MAX_SECTION {
            break;
        }
        out.push('\n');
        out.push_str(bullet);
        used += 1 + len;
    }
    out
}

/// One action on one page: the page path (no hash), the operation, its target
/// when it has one, how many tries, and the mean discounted return.
#[derive(Clone, Debug, Default, PartialEq)]
struct Merged {
    path: String,
    operation: String,
    target: Option<String>,
    n: u32,
    mean: f64,
}

/// Folds the prior rows' page hashes away: the hash moves when a drop-down
/// opens, the path does not (the same reason `PriorTable::for_page` falls back
/// to the path). Same path + operation + target merge with a weighted mean.
fn merge_rows(rows: Vec<(String, Prior)>) -> Vec<Merged> {
    let mut out: Vec<Merged> = Vec::new();
    for (sig, prior) in rows {
        let path = episodes::path_of_sig(&sig).to_string();
        match out
            .iter_mut()
            .find(|m| m.path == path && m.operation == prior.operation && m.target == prior.target)
        {
            Some(seen) => {
                let n = seen.n.saturating_add(prior.n);
                if n > 0 {
                    seen.mean = (seen.mean * seen.n as f64 + prior.mean_return * prior.n as f64) / n as f64;
                }
                seen.n = n;
            }
            None => out.push(Merged {
                path,
                operation: prior.operation,
                target: prior.target,
                n: prior.n,
                mean: prior.mean_return,
            }),
        }
    }
    out
}

/// Ops that never count as "helps": WAIT and the manual scrolls carry the
/// goal progress of the step that follows them, so a win on one of them is a
/// return leaking backward, not a lesson (plans/007, the 006 cinemark
/// episodes). They can still be an "avoid": a scroll that wasted a step cost
/// one.
const NO_HELP_OPS: [&str; 3] = ["WAIT", "SCROLL_DOWN", "SCROLL_UP"];

/// Wins first (best return first), then losses (worst first) — a line that
/// stops a wasted click is worth keeping even when there are many wins.
fn action_bullets(rows: &[Merged]) -> Vec<String> {
    let mut helps: Vec<&Merged> = rows
        .iter()
        .filter(|r| r.n >= MIN_TRIES && r.mean >= HELPS_AT && !never_helps(&r.operation))
        .collect();
    let mut avoids: Vec<&Merged> = rows.iter().filter(|r| r.n >= MIN_TRIES && r.mean <= AVOID_AT).collect();
    helps.sort_by(|a, b| b.mean.total_cmp(&a.mean).then(tie(a, b)));
    avoids.sort_by(|a, b| a.mean.total_cmp(&b.mean).then(tie(a, b)));
    helps.iter().chain(avoids.iter()).map(|r| bullet(r, r.mean >= HELPS_AT)).collect()
}

fn never_helps(operation: &str) -> bool {
    NO_HELP_OPS.iter().any(|op| operation.eq_ignore_ascii_case(op))
}

/// Deterministic order behind equal means: more tries first, then the names.
fn tie(a: &Merged, b: &Merged) -> std::cmp::Ordering {
    b.n.cmp(&a.n)
        .then(a.path.cmp(&b.path))
        .then(a.operation.cmp(&b.operation))
        .then(a.target.cmp(&b.target))
}

fn bullet(row: &Merged, helps: bool) -> String {
    let what = match &row.target {
        Some(target) => format!("{} \"{target}\"", row.operation),
        None => row.operation.clone(),
    };
    match helps {
        true => format!("- On {}: {what} helps (return {:+.2}, n={}).", row.path, row.mean, row.n),
        false => format!("- On {}: avoid {what} (return {:+.2}, n={}).", row.path, row.mean, row.n),
    }
}

fn route_bullet(route: &Route) -> String {
    let steps: Vec<String> = route
        .steps
        .iter()
        .map(|s| match &s.target {
            Some(target) => format!("{} \"{target}\"", s.operation),
            None => s.operation.clone(),
        })
        .collect();
    format!("- Known route ({} steps): {}.", route.steps.len(), steps.join(" > "))
}

/// The goal most episodes were run for, as `normalize_goal` writes it — the
/// key `best_route` and a live `--move` goal share — so an un-normalized log
/// still counts as one goal. Equal counts -> the smallest key, so the section
/// is stable run to run.
fn most_common_goal(eps: &[Episode]) -> Option<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for key in eps.iter().map(|e| episodes::normalize_goal(&e.goal_key)).filter(|k| !k.is_empty()) {
        match counts.iter_mut().find(|(seen, _)| *seen == key) {
            Some((_, n)) => *n += 1,
            None => counts.push((key, 1)),
        }
    }
    counts.into_iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0))).map(|(key, _)| key)
}

// ---- the hints file ----

/// The block as it sits in `<hints>/<host>.md`.
pub fn learned_block(section: &str) -> String {
    format!("{LEARNED_START}\n{section}\n{LEARNED_END}")
}

/// The hints file after a learn run: the front matter re-stamped, then the
/// learned block replaced in place, appended when it is missing. An absent
/// file becomes a front-mattered vault note. Every other line is left alone.
pub fn upsert_learned(existing: Option<&str>, host: &str, section: &str, runs: usize, date: &str) -> String {
    let text = match existing {
        Some(text) => update_front_matter(text, runs, date),
        None => format!("---\ntype: reference\nsite: {host}\nupdated: {date}\nruns: {runs}\n---\n# {host}\n"),
    };
    replace_block(&text, &learned_block(section))
}

/// `updated:` and `runs:` inside the leading front matter only — a body line
/// that happens to start with those words is the operator's text.
fn update_front_matter(text: &str, runs: usize, date: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let end = match lines.first().map(|l| l.trim()) {
        Some("---") => lines.iter().skip(1).position(|l| l.trim() == "---").map(|i| i + 1),
        _ => None,
    };
    let Some(end) = end else {
        return text.to_string();
    };
    for line in lines.iter_mut().take(end).skip(1) {
        match line.split(':').next().unwrap_or("").trim() {
            "updated" => *line = format!("updated: {date}"),
            "runs" => *line = format!("runs: {runs}"),
            _ => {}
        }
    }
    let mut out = lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn replace_block(text: &str, block: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|l| l.trim() == LEARNED_START);
    let end = lines.iter().rposition(|l| l.trim() == LEARNED_END);
    match (start, end) {
        (Some(start), Some(end)) if end >= start => {
            let mut out = String::new();
            for line in &lines[..start] {
                out.push_str(line);
                out.push('\n');
            }
            out.push_str(block);
            out.push('\n');
            for line in &lines[end + 1..] {
                out.push_str(line);
                out.push('\n');
            }
            out
        }
        // A half-open block (one marker only) must never cost the file its
        // other lines: append instead of replacing.
        _ if !text.trim().is_empty() => format!("{}\n\n{block}\n", text.trim_end()),
        _ => format!("{block}\n"),
    }
}

/// `YYYY-MM-DD` (UTC) for the front matter, out of the same hand-rolled civil
/// date `main` stamps QA directories with.
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stamp = crate::civil_timestamp(secs); // yyyymmdd-hhmmss
    match (stamp.get(0..4), stamp.get(4..6), stamp.get(6..8)) {
        (Some(y), Some(m), Some(d)) => format!("{y}-{m}-{d}"),
        _ => stamp,
    }
}

/// Write-tmp-then-rename: a reader never sees a half-written hints file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename {}: {e}", path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::episodes::{Feedback, RouteStep, StepSample};

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mini-learn-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// One-step episode on `sig` with the given verdict, labelled `bad` when
    /// the run failed so the terminal reward (and the return) is -1.0.
    fn ep(goal_key: &str, ok: bool, operation: &str, target: Option<&str>, sig: &str) -> Episode {
        Episode {
            id: format!("{sig}-{operation}-{}", if ok { "ok" } else { "bad" }),
            host: "example.com".into(),
            goal_key: goal_key.into(),
            ok,
            steps: vec![StepSample {
                operation: operation.into(),
                target: target.map(str::to_string),
                page_sig: sig.into(),
                url: format!("https://{sig}"),
                page_changed: true,
                ..Default::default()
            }],
            feedback: (!ok).then(|| Feedback {
                label: "bad".into(),
                reason: None,
                ts_ms: 0,
            }),
            ..Default::default()
        }
    }

    fn merged(path: &str, operation: &str, target: Option<&str>, n: u32, mean: f64) -> Merged {
        Merged {
            path: path.into(),
            operation: operation.into(),
            target: target.map(str::to_string),
            n,
            mean,
        }
    }

    fn prior_row(sig: &str, operation: &str, target: Option<&str>, n: u32, mean_return: f64) -> (String, Prior) {
        (
            sig.into(),
            Prior {
                operation: operation.into(),
                target: target.map(str::to_string),
                n,
                mean_return,
                exact: true,
            },
        )
    }

    #[test]
    fn section_marks_repeated_wins_and_losses() {
        let eps = vec![
            ep("book a table", true, "CLICK", Some("Sign in"), "example.com/login#a1b2c3d4"),
            ep("book a table", true, "CLICK", Some("Sign in"), "example.com/login#a1b2c3d4"),
            ep("book a table", false, "CLICK", Some("Buy now"), "example.com/cart#e5f6a7b8"),
            ep("book a table", false, "CLICK", Some("Buy now"), "example.com/cart#e5f6a7b8"),
        ];
        let section = learned_section(&eps);
        assert_eq!(section.lines().next().unwrap(), "Learned from 4 episodes (ok rate 50%).");
        assert!(
            section.contains("- On example.com/login: CLICK \"Sign in\" helps (return +1.00, n=2)."),
            "{section}"
        );
        assert!(
            section.contains("- On example.com/cart: avoid CLICK \"Buy now\" (return -1.00, n=2)."),
            "{section}"
        );
        // No route while `best_route` is a stub.
        assert!(!section.contains("Known route"), "{section}");
    }

    #[test]
    fn one_off_tries_and_weak_returns_are_not_learned() {
        let rows = vec![
            merged("example.com/a", "CLICK", Some("Once"), 1, 1.0),
            merged("example.com/a", "PRESS_ENTER", None, 3, 0.3),
            merged("example.com/a", "SCROLL_DOWN", None, 3, -0.19),
            merged("example.com/a", "CLICK", Some("Book"), 2, 0.5),
            merged("example.com/a", "CLICK", Some("Trap"), 2, -0.21),
        ];
        assert_eq!(
            action_bullets(&rows),
            vec![
                "- On example.com/a: CLICK \"Book\" helps (return +0.50, n=2).",
                "- On example.com/a: avoid CLICK \"Trap\" (return -0.21, n=2).",
            ]
        );
    }

    #[test]
    fn wait_and_the_manual_scrolls_never_help() {
        let rows = vec![
            merged("example.com/a", "WAIT", None, 4, 0.80),
            merged("example.com/a", "SCROLL_DOWN", None, 3, 0.60),
            merged("example.com/a", "SCROLL_UP", None, 5, 0.55),
            merged("example.com/a", "CLICK", Some("Book"), 2, 0.50),
            merged("example.com/a", "SCROLL_DOWN", None, 2, -0.90),
        ];
        assert_eq!(
            action_bullets(&rows),
            vec![
                "- On example.com/a: CLICK \"Book\" helps (return +0.50, n=2).",
                "- On example.com/a: avoid SCROLL_DOWN (return -0.90, n=2).",
            ],
            "the return of a wait or a scroll is the next step's, so only the loss is real"
        );
    }

    #[test]
    fn wins_come_first_and_the_worst_loss_first() {
        let rows = vec![
            merged("example.com/a", "CLICK", Some("Trap"), 2, -0.30),
            merged("example.com/a", "CLICK", Some("Book"), 3, 0.60),
            merged("example.com/a", "SCROLL_DOWN", None, 4, -0.90),
            merged("example.com/a", "PRESS_ENTER", None, 2, 0.95),
        ];
        let bullets = action_bullets(&rows);
        assert_eq!(
            bullets,
            vec![
                "- On example.com/a: PRESS_ENTER helps (return +0.95, n=2).",
                "- On example.com/a: CLICK \"Book\" helps (return +0.60, n=3).",
                "- On example.com/a: avoid SCROLL_DOWN (return -0.90, n=4).",
                "- On example.com/a: avoid CLICK \"Trap\" (return -0.30, n=2).",
            ]
        );
    }

    #[test]
    fn merge_folds_the_page_hash_with_a_weighted_mean() {
        let rows = vec![
            prior_row("example.com/search#aaaa1111", "CLICK", Some("Next"), 1, 1.0),
            prior_row("example.com/search#bbbb2222", "CLICK", Some("Next"), 3, -1.0),
            prior_row("example.com/search#bbbb2222", "CLICK", Some("Other"), 1, 1.0),
        ];
        assert_eq!(
            merge_rows(rows),
            vec![
                merged("example.com/search", "CLICK", Some("Next"), 4, -0.5),
                merged("example.com/search", "CLICK", Some("Other"), 1, 1.0),
            ]
        );
    }

    #[test]
    fn the_goal_key_is_normalized_before_it_is_counted() {
        let eps = vec![
            ep("Book  A Table", true, "CLICK", Some("Sign in"), "example.com/#a1"),
            ep("book a table", true, "CLICK", Some("Sign in"), "example.com/#a1"),
            ep("search flights", true, "CLICK", Some("Sign in"), "example.com/#a1"),
        ];
        assert_eq!(most_common_goal(&eps), Some("book a table".to_string()));

        // No goal at all (a log from before goals were stored) -> no route.
        let blank = vec![ep("", true, "CLICK", Some("Sign in"), "example.com/#a1")];
        assert_eq!(most_common_goal(&blank), None);
        assert!(!learned_section(&blank).contains("Known route"));
    }

    #[test]
    fn route_bullet_names_every_step() {
        let route = Route {
            episode_id: "ep-1".into(),
            goal_key: "book a table".into(),
            final_reward: 1.0,
            steps: vec![
                RouteStep {
                    operation: "CLICK".into(),
                    target: Some("Sign in".into()),
                    page_sig: "example.com/#a1".into(),
                    url: "https://example.com/".into(),
                },
                RouteStep {
                    operation: "SCROLL_DOWN".into(),
                    target: None,
                    page_sig: "example.com/list#b2".into(),
                    url: "https://example.com/list".into(),
                },
                RouteStep {
                    operation: "PRESS_ENTER".into(),
                    target: None,
                    page_sig: "example.com/list#b2".into(),
                    url: "https://example.com/list".into(),
                },
            ],
        };
        assert_eq!(
            route_bullet(&route),
            "- Known route (3 steps): CLICK \"Sign in\" > SCROLL_DOWN > PRESS_ENTER."
        );
    }

    #[test]
    fn section_caps_the_bullet_count() {
        let bullets: Vec<String> = (0..20).map(|i| format!("- On example.com/p{i}: CLICK helps (return +1.00, n=2).")).collect();
        let section = section_from("Learned from 40 episodes (ok rate 50%).", &bullets);
        assert_eq!(section.lines().count(), MAX_BULLETS + 1);
        assert!(section.chars().count() <= MAX_SECTION, "{}", section.chars().count());
    }

    #[test]
    fn section_caps_the_character_count() {
        let long = format!("- On example.com/{}: CLICK helps (return +1.00, n=2).", "x".repeat(400));
        let section = section_from("Learned from 40 episodes (ok rate 50%).", &[long.clone(), long]);
        assert_eq!(section.lines().count(), 2, "{section}");
        assert!(section.chars().count() <= MAX_SECTION, "{}", section.chars().count());
    }

    #[test]
    fn a_new_hints_file_gets_front_matter_and_the_block() {
        let text = upsert_learned(None, "example.com", "Learned from 4 episodes (ok rate 50%).\n- On /: X.", 4, "2026-09-24");
        assert!(
            text.starts_with("---\ntype: reference\nsite: example.com\nupdated: 2026-09-24\nruns: 4\n---\n# example.com\n"),
            "{text}"
        );
        assert!(
            text.contains("<!-- mini:learned -->\nLearned from 4 episodes (ok rate 50%).\n- On /: X.\n<!-- /mini:learned -->"),
            "{text}"
        );
    }

    #[test]
    fn only_the_marked_block_and_the_front_matter_change() {
        let existing = "---\ntype: reference\nsite: example.com\nupdated: 2020-01-01\nruns: 1\n---\n\
                        # example.com\n\nHand written line.\n\n\
                        <!-- mini:learned -->\nold section\n<!-- /mini:learned -->\n\nAfter line.\n";
        let text = upsert_learned(Some(existing), "example.com", "new section", 7, "2026-09-24");
        assert!(text.contains("Hand written line."), "{text}");
        assert!(text.contains("After line."), "{text}");
        assert!(!text.contains("old section"), "{text}");
        assert!(text.contains("updated: 2026-09-24"), "{text}");
        assert!(text.contains("runs: 7"), "{text}");
        assert!(!text.contains("runs: 1"), "{text}");
        let body = text.find("Hand written line.").unwrap();
        let block = text.find(LEARNED_START).unwrap();
        let after = text.find("After line.").unwrap();
        assert!(body < block && block < after, "{text}");
        assert_eq!(text.matches(LEARNED_START).count(), 1, "{text}");
        assert_eq!(text.matches(LEARNED_END).count(), 1, "{text}");
    }

    #[test]
    fn a_missing_block_is_appended_at_the_end() {
        let existing = "---\ntype: reference\nsite: example.com\nupdated: 2020-01-01\nruns: 1\n---\n# example.com\n\nHand written line.\n";
        let text = upsert_learned(Some(existing), "example.com", "new section", 2, "2026-09-24");
        assert!(text.starts_with("---\ntype: reference\nsite: example.com\nupdated: 2026-09-24\nruns: 2\n---\n"), "{text}");
        assert!(text.contains("Hand written line.\n\n<!-- mini:learned -->\nnew section\n<!-- /mini:learned -->\n"), "{text}");
        assert!(text.ends_with(LEARNED_END) || text.ends_with(&format!("{LEARNED_END}\n")), "{text}");
    }

    #[test]
    fn words_like_runs_outside_the_front_matter_are_left_alone() {
        let existing = "# example.com\n\nruns: 99 for this site.\nupdated: never.\n";
        let text = upsert_learned(Some(existing), "example.com", "new section", 3, "2026-09-24");
        assert!(text.contains("runs: 99 for this site."), "{text}");
        assert!(text.contains("updated: never."), "{text}");
        assert!(!text.contains("runs: 3"), "{text}");
    }

    #[test]
    fn an_empty_existing_file_still_gets_the_block() {
        let text = upsert_learned(Some(""), "example.com", "new section", 1, "2026-09-24");
        assert_eq!(text, "<!-- mini:learned -->\nnew section\n<!-- /mini:learned -->\n");
    }

    #[test]
    fn learn_host_dry_runs_then_writes_once() {
        let eps_dir = tmp("drive-episodes");
        let hints = tmp("drive-hints");
        let eps = vec![
            ep("book a table", true, "CLICK", Some("Sign in"), "example.com/login#a1b2c3d4"),
            ep("book a table", true, "CLICK", Some("Sign in"), "example.com/login#a1b2c3d4"),
        ];
        let log: String = eps.iter().map(|e| format!("{}\n", serde_json::to_string(e).unwrap())).collect();
        std::fs::write(eps_dir.join("example.com.jsonl"), log).unwrap();

        let dry = learn_host("www.example.com", Some(&eps_dir), Some(&hints), false).unwrap();
        assert!(dry.starts_with("Learned from 2 episodes (ok rate 100%)."), "{dry}");
        assert!(!hints.join("example.com.md").exists(), "dry run wrote the file");

        let path = learn_host("example.com", Some(&eps_dir), Some(&hints), true).unwrap();
        assert_eq!(PathBuf::from(&path), hints.join("example.com.md"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("runs: 2"), "{text}");
        assert!(text.contains(&format!("updated: {}", today())), "{text}");
        assert!(text.contains(&dry), "{text}");

        // A second write replaces the block instead of stacking a new one.
        learn_host("example.com", Some(&eps_dir), Some(&hints), true).unwrap();
        let again = std::fs::read_to_string(&path).unwrap();
        assert_eq!(again, text);
        assert_eq!(again.matches(LEARNED_START).count(), 1, "{again}");
    }

    #[test]
    fn learn_host_keeps_the_operators_lines_when_rewriting() {
        let eps_dir = tmp("keep-episodes");
        let hints = tmp("keep-hints");
        let log = format!(
            "{}\n",
            serde_json::to_string(&ep("book a table", true, "CLICK", Some("Sign in"), "example.com/login#a1")).unwrap()
        );
        std::fs::write(eps_dir.join("example.com.jsonl"), log).unwrap();
        let hand = "---\ntype: reference\nsite: example.com\nupdated: 2020-01-01\nruns: 0\n---\n# example.com\n\n- Press Enter after typing.\n";
        std::fs::write(hints.join("example.com.md"), hand).unwrap();

        learn_host("example.com", Some(&eps_dir), Some(&hints), true).unwrap();
        let text = std::fs::read_to_string(hints.join("example.com.md")).unwrap();
        assert!(text.contains("- Press Enter after typing."), "{text}");
        assert!(text.contains("runs: 1"), "{text}");
        assert!(text.contains(LEARNED_START), "{text}");
    }

    #[test]
    fn a_host_without_episodes_is_an_error() {
        let eps_dir = tmp("empty-episodes");
        let hints = tmp("empty-hints");
        let err = learn_host("example.com", Some(&eps_dir), Some(&hints), true).unwrap_err();
        assert!(err.starts_with("no episodes for example.com"), "{err}");
        assert!(err.contains(&eps_dir.display().to_string()), "{err}");
        assert!(!hints.join("example.com.md").exists());
    }

    #[test]
    fn a_host_that_would_escape_the_hints_folder_is_rejected() {
        let eps_dir = tmp("escape-episodes");
        let hints = tmp("escape-hints");
        for host in ["..", "../evil", "a/b", "c\\d", "e:f", "  "] {
            let err = learn_host(host, Some(&eps_dir), Some(&hints), true).unwrap_err();
            assert!(err.starts_with("not a usable host:"), "{host}: {err}");
        }
        assert!(!hints.parent().unwrap().join("evil.md").exists());
    }

    #[test]
    fn hosts_come_from_the_jsonl_stems_in_order() {
        let dir = tmp("hosts");
        for name in ["b.com.jsonl", "a.com.jsonl", "notes.txt"] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        assert_eq!(hosts_in(&dir), vec!["a.com".to_string(), "b.com".to_string()]);
        assert_eq!(hosts_in(&dir.join("missing")), Vec::<String>::new());
    }

    #[test]
    fn parse_reads_host_write_and_hints() {
        let a = parse_args(&["https://www.github.com/search".to_string()]).unwrap();
        assert_eq!(a.host.as_deref(), Some("github.com"));
        assert!(!a.write);
        assert_eq!(a.hints_dir, None);

        let a = parse_args(&["localhost:3000".to_string(), "--write".to_string(), "--hints".to_string(), "H".to_string()]).unwrap();
        assert_eq!(a.host.as_deref(), Some("localhost"));
        assert!(a.write);
        assert_eq!(a.hints_dir, Some(PathBuf::from("H")));

        let a = parse_args(&[]).unwrap();
        assert_eq!(a, LearnArgs::default());
    }

    #[test]
    fn parse_rejects_bad_command_lines() {
        let err = parse_args(&["--nope".to_string()]).unwrap_err();
        assert!(err.contains("unknown flag --nope"), "{err}");
        let err = parse_args(&["a.com".to_string(), "b.com".to_string()]).unwrap_err();
        assert!(err.contains("takes one host"), "{err}");
        let err = parse_args(&["--hints".to_string()]).unwrap_err();
        assert_eq!(err, "--hints needs a folder");
    }

    #[test]
    fn today_is_an_iso_day() {
        let day = today();
        assert_eq!(day.len(), 10, "{day}");
        assert_eq!(day.chars().filter(|c| *c == '-').count(), 2, "{day}");
        assert!(day.chars().all(|c| c.is_ascii_digit() || c == '-'), "{day}");
    }

    fn log_line(eps: &[Episode]) -> String {
        eps.iter().map(|e| format!("{}\n", serde_json::to_string(e).unwrap())).collect()
    }

    #[test]
    fn the_command_writes_every_host_it_finds() {
        let eps_dir = tmp("cmd-episodes");
        let hints = tmp("cmd-hints");
        std::fs::write(
            eps_dir.join("a.com.jsonl"),
            log_line(&[ep("book", true, "CLICK", Some("Sign in"), "a.com/login#a1")]),
        )
        .unwrap();
        std::fs::write(
            eps_dir.join("b.com.jsonl"),
            log_line(&[ep("book", true, "CLICK", Some("Search"), "b.com/#b1")]),
        )
        .unwrap();

        let args: Vec<String> = ["--learn", "--write", "--hints", hints.to_str().unwrap()]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(run_in(&args, 0, &eps_dir), 0);
        for host in ["a.com", "b.com"] {
            let text = std::fs::read_to_string(hints.join(format!("{host}.md"))).unwrap();
            assert!(text.contains(LEARNED_START), "{host}: {text}");
            assert!(text.contains("runs: 1"), "{host}: {text}");
        }
    }

    #[test]
    fn the_command_exits_one_when_there_is_nothing_to_learn() {
        let eps_dir = tmp("cmd-empty-episodes");
        let hints = tmp("cmd-empty-hints");
        let args: Vec<String> = ["--learn", "--write", "--hints", hints.to_str().unwrap()]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // No `*.jsonl` at all, then a log whose only host has no episodes.
        assert_eq!(run_in(&args, 0, &eps_dir), 1);
        std::fs::write(eps_dir.join("c.com.jsonl"), "").unwrap();
        assert_eq!(run_in(&args, 0, &eps_dir), 1);
        assert!(!hints.join("c.com.md").exists());
    }

    #[test]
    fn the_command_exits_two_on_a_bad_flag() {
        let eps_dir = tmp("cmd-bad-episodes");
        let args: Vec<String> = vec!["--learn".into(), "--wat".into()];
        assert_eq!(run_in(&args, 0, &eps_dir), 2);
    }
}

//! Offline analysis of debug runs: flicker, remount, cause, diff.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const FLICKER_WINDOW_MS: f64 = 150.0;
const REMOUNT_WINDOW_MS: f64 = 80.0;
const CAUSE_WINDOW_MS: f64 = 500.0;

struct Run {
    dir: PathBuf,
    manifest: Value,
    console: Vec<Value>,
    dom_events: Vec<Value>,
    net: Vec<Value>,
    react: Vec<Value>,
    backend: Vec<Value>,
    frames: Vec<f64>,
    marks: Vec<Value>,
}

struct Flicker {
    path: String,
    dwell_ms: f64,
    t: f64,
    within_one_frame: bool,
    rect: Option<[f64; 4]>,
}

fn rect_of(ev: &Value) -> Option<[f64; 4]> {
    let r = ev.get("r")?;
    Some([
        r.get("x").and_then(num_to_f64)?,
        r.get("y").and_then(num_to_f64)?,
        r.get("w").and_then(num_to_f64)?,
        r.get("h").and_then(num_to_f64)?,
    ])
}

/// Inclusive on edges: empty containers report zero-height rects
/// (`getBoundingClientRect` of a div with no laid-out children), and a
/// strict comparison would make such marks unmatchable.
fn rects_overlap(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] <= b[0] + b[2] && b[0] <= a[0] + a[2] && a[1] <= b[1] + b[3] && b[1] <= a[1] + a[3]
}

/// A finding is "marked" when its path shares a subtree with a mark's path
/// (substring either way — `path()` collapses at the nearest #id, so this
/// alone misses later-created children) or when the finding's recorded rect
/// overlaps the marked element's rect.
fn is_marked(marks: &[Value], path: &str, rect: Option<[f64; 4]>) -> bool {
    marks.iter().any(|m| {
        let mp = str_field(m, "path");
        if !mp.is_empty() && !path.is_empty() && (path.contains(&mp) || mp.contains(path)) {
            return true;
        }
        match (rect_of(m), rect) {
            (Some(mr), Some(fr)) => rects_overlap(mr, fr),
            _ => false,
        }
    })
}

struct Remount {
    name: String,
    path: String,
    t_unmount: f64,
    t_mount: f64,
}

struct Cause {
    api: String,
    url: String,
    phase: String,
    ts: f64,
    delta_ms: f64,
}

fn num_to_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_u64().map(|n| n as f64))
        .or_else(|| v.as_i64().map(|n| n as f64))
}

fn ts(v: &Value) -> f64 {
    v.get("ts")
        .and_then(num_to_f64)
        .or_else(|| v.get("t").and_then(num_to_f64))
        .unwrap_or(0.0)
}

fn str_field(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn path_list(v: &Value, k: &str) -> Vec<String> {
    v.get(k)
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn kind_of(ev: &Value) -> &str {
    ev.get("k").and_then(|x| x.as_str()).unwrap_or("")
}

fn read_jsonl(dir: &Path, name: &str) -> Vec<Value> {
    let Ok(text) = fs::read_to_string(dir.join(name)) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn flatten_dom(lines: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for line in lines {
        let env_t = line.get("t").cloned().unwrap_or(json!(0));
        let Some(events) = line.get("events").and_then(|e| e.as_array()) else {
            continue;
        };
        for ev in events {
            let mut obj = ev.clone();
            if obj.get("ts").is_none() {
                if let Some(m) = obj.as_object_mut() {
                    m.insert("ts".into(), env_t.clone());
                }
            }
            out.push(obj);
        }
    }
    out
}

fn flatten_frames(lines: &[Value]) -> Vec<f64> {
    let mut out = Vec::new();
    for line in lines {
        let Some(arr) = line.get("frames").and_then(|f| f.as_array()) else {
            continue;
        };
        for x in arr {
            if let Some(n) = num_to_f64(x) {
                out.push(n);
            }
        }
    }
    out
}

fn load_run(dir: &Path) -> Option<Run> {
    if !dir.is_dir() {
        return None;
    }
    let manifest = fs::read_to_string(dir.join("run.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    let dom_lines = read_jsonl(dir, "dom.jsonl");
    let frame_lines = read_jsonl(dir, "frames.jsonl");
    Some(Run {
        dir: dir.to_path_buf(),
        manifest,
        console: read_jsonl(dir, "frontend.jsonl"),
        dom_events: flatten_dom(&dom_lines),
        net: read_jsonl(dir, "net.jsonl"),
        react: read_jsonl(dir, "react.jsonl"),
        backend: read_jsonl(dir, "backend.jsonl"),
        frames: flatten_frames(&frame_lines),
        marks: read_jsonl(dir, "marks.jsonl"),
    })
}

fn within_one_frame(frames: &[f64], t0: f64, t1: f64) -> bool {
    !frames.iter().any(|&f| f > t0 && f < t1)
}

fn flicker(run: &Run) -> Vec<Flicker> {
    let mut hits = Vec::new();
    let events = &run.dom_events;
    for (i, ev) in events.iter().enumerate() {
        let t_add = ts(ev);
        if kind_of(ev) == "children" {
            for p in path_list(ev, "add") {
                let mut best: Option<f64> = None;
                for later in events.iter().skip(i + 1) {
                    if kind_of(later) != "children" {
                        continue;
                    }
                    if !path_list(later, "del").iter().any(|d| d == &p) {
                        continue;
                    }
                    let t_del = ts(later);
                    let dt = t_del - t_add;
                    if dt <= 0.0 || dt > FLICKER_WINDOW_MS {
                        continue;
                    }
                    best = Some(match best {
                        Some(b) if b <= t_del => b,
                        _ => t_del,
                    });
                }
                if let Some(t_del) = best {
                    hits.push(Flicker {
                        path: p,
                        dwell_ms: t_del - t_add,
                        t: t_add,
                        within_one_frame: within_one_frame(&run.frames, t_add, t_del),
                        rect: rect_of(ev),
                    });
                }
            }
        } else if kind_of(ev) == "attr" {
            let p = str_field(ev, "p");
            let a = str_field(ev, "a");
            let Some(old) = ev.get("from").cloned() else {
                continue;
            };
            let mut best: Option<f64> = None;
            for later in events.iter().skip(i + 1) {
                if kind_of(later) != "attr" {
                    continue;
                }
                if str_field(later, "p") != p || str_field(later, "a") != a {
                    continue;
                }
                if later.get("to") != Some(&old) {
                    continue;
                }
                let t_del = ts(later);
                let dt = t_del - t_add;
                if dt <= 0.0 || dt > FLICKER_WINDOW_MS {
                    continue;
                }
                best = Some(match best {
                    Some(b) if b <= t_del => b,
                    _ => t_del,
                });
            }
            if let Some(t_del) = best {
                hits.push(Flicker {
                    path: p,
                    dwell_ms: t_del - t_add,
                    t: t_add,
                    within_one_frame: within_one_frame(&run.frames, t_add, t_del),
                    rect: rect_of(ev),
                });
            }
        }
    }
    hits
}

fn remounts(run: &Run) -> Vec<Remount> {
    if !run.react.is_empty() {
        let mut used = vec![false; run.react.len()];
        let mut out = Vec::new();
        for ev in &run.react {
            if str_field(ev, "op") != "unmount" {
                continue;
            }
            let name = str_field(ev, "name");
            let t_un = ts(ev);
            let mut pair: Option<(usize, f64)> = None;
            for (j, m) in run.react.iter().enumerate() {
                if used[j] || str_field(m, "op") != "mount" {
                    continue;
                }
                if str_field(m, "name") != name {
                    continue;
                }
                let t_m = ts(m);
                let dt = t_m - t_un;
                if dt <= 0.0 || dt > REMOUNT_WINDOW_MS {
                    continue;
                }
                pair = Some(match pair {
                    Some((pj, pt)) if pt <= t_m => (pj, pt),
                    _ => (j, t_m),
                });
            }
            if let Some((j, t_m)) = pair {
                used[j] = true;
                let mp = str_field(&run.react[j], "path");
                let path = if mp.is_empty() {
                    str_field(ev, "path")
                } else {
                    mp
                };
                out.push(Remount {
                    name,
                    path,
                    t_unmount: t_un,
                    t_mount: t_m,
                });
            }
        }
        return out;
    }

    let mut used = vec![false; run.dom_events.len()];
    let mut out = Vec::new();
    for (i, ev) in run.dom_events.iter().enumerate() {
        if kind_of(ev) != "children" {
            continue;
        }
        let t_un = ts(ev);
        for p in path_list(ev, "del") {
            let mut pair: Option<(usize, f64)> = None;
            for (j, later) in run.dom_events.iter().enumerate() {
                if j == i || used[j] || kind_of(later) != "children" {
                    continue;
                }
                if !path_list(later, "add").iter().any(|a| a == &p) {
                    continue;
                }
                let t_m = ts(later);
                let dt = t_m - t_un;
                if dt <= 0.0 || dt > REMOUNT_WINDOW_MS {
                    continue;
                }
                pair = Some(match pair {
                    Some((pj, pt)) if pt <= t_m => (pj, pt),
                    _ => (j, t_m),
                });
            }
            if let Some((j, t_m)) = pair {
                used[j] = true;
                out.push(Remount {
                    name: "<dom>".into(),
                    path: p,
                    t_unmount: t_un,
                    t_mount: t_m,
                });
            }
        }
    }
    out
}

fn cause_of(run: &Run, t: f64) -> Option<Cause> {
    let mut best: Option<(f64, &Value)> = None;
    for ev in &run.net {
        let nts = ts(ev);
        if nts <= t && t - nts <= CAUSE_WINDOW_MS {
            best = Some(match best {
                Some((bts, bev)) if bts >= nts => (bts, bev),
                _ => (nts, ev),
            });
        }
    }
    best.map(|(nts, ev)| Cause {
        api: str_field(ev, "api"),
        url: str_field(ev, "url"),
        phase: str_field(ev, "phase"),
        ts: nts,
        delta_ms: t - nts,
    })
}

fn rect_xy(ev: &Value) -> Option<(f64, f64)> {
    let r = ev.get("r")?;
    Some((r.get("x").and_then(num_to_f64)?, r.get("y").and_then(num_to_f64)?))
}

fn layout_delta(run: &Run, path: &str, t: f64) -> Option<(f64, f64)> {
    let mut del: Option<(f64, f64, f64)> = None;
    let mut add: Option<(f64, f64, f64)> = None;
    for ev in &run.dom_events {
        if kind_of(ev) != "children" {
            continue;
        }
        let Some((x, y)) = rect_xy(ev) else {
            continue;
        };
        let dist = (ts(ev) - t).abs();
        let in_del =
            path_list(ev, "del").iter().any(|d| d == path) || str_field(ev, "p") == path;
        let in_add =
            path_list(ev, "add").iter().any(|d| d == path) || str_field(ev, "p") == path;
        if in_del && !path_list(ev, "del").is_empty() && del.map(|d| dist < d.0).unwrap_or(true) {
            del = Some((dist, x, y));
        }
        if in_add && !path_list(ev, "add").is_empty() && add.map(|d| dist < d.0).unwrap_or(true) {
            add = Some((dist, x, y));
        }
    }
    match (add, del) {
        (Some((_, ax, ay)), Some((_, dx, dy))) => Some((ay - dy, ax - dx)),
        _ => None,
    }
}

fn dom_around(run: &Run, t0: f64, t1: f64) -> (Vec<String>, Vec<String>) {
    let mut del = Vec::new();
    let mut add = Vec::new();
    for ev in &run.dom_events {
        if kind_of(ev) != "children" {
            continue;
        }
        let t = ts(ev);
        if t < t0 || t > t1 {
            continue;
        }
        del.extend(path_list(ev, "del"));
        add.extend(path_list(ev, "add"));
    }
    (del, add)
}

fn label_of(run: &Run) -> String {
    run.manifest
        .get("label")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn write_pair(dir: &Path, stem: &str, json_body: &Value, txt: &str) -> Result<(), std::io::Error> {
    fs::write(dir.join(format!("{stem}.json")), json_body.to_string())?;
    fs::write(dir.join(format!("{stem}.txt")), txt)?;
    Ok(())
}

pub fn explain(run_dir: Option<&String>) -> i32 {
    let (code, txt) = explain_impl(run_dir);
    if code == 0 {
        print!("{txt}");
    }
    code
}

/// Same analysis without taking over stdout: `--qa` owns stdout for report.md.
pub fn explain_quiet(run_dir: Option<&String>) -> i32 {
    explain_impl(run_dir).0
}

fn explain_impl(run_dir: Option<&String>) -> (i32, String) {
    let Some(dir_s) = run_dir else {
        eprintln!("usage: mini --debug-explain <run_dir>");
        return (2, String::new());
    };
    let dir = PathBuf::from(dir_s);
    let Some(run) = load_run(&dir) else {
        eprintln!("missing run dir: {dir_s}");
        return (2, String::new());
    };

    let flickers = flicker(&run);
    let mounts = remounts(&run);

    let flicker_json: Vec<Value> = flickers
        .iter()
        .map(|f| {
            json!({
                "path": f.path,
                "dwell_ms": f.dwell_ms,
                "t": f.t,
                "within_one_frame": f.within_one_frame,
                "marked": is_marked(&run.marks, &f.path, f.rect),
            })
        })
        .collect();

    let remount_json: Vec<Value> = mounts
        .iter()
        .map(|r| {
            let cause = cause_of(&run, r.t_mount);
            let layout = layout_delta(&run, &r.path, r.t_mount);
            let (dels, adds) = dom_around(&run, r.t_unmount, r.t_mount);
            json!({
                "name": r.name,
                "path": r.path,
                "marked": is_marked(&run.marks, &r.path, None),
                "t_unmount": r.t_unmount,
                "t_mount": r.t_mount,
                "cause": cause.as_ref().map(|c| json!({
                    "api": c.api,
                    "url": c.url,
                    "phase": c.phase,
                    "ts": c.ts,
                    "delta_ms": c.delta_ms,
                })),
                "dom": { "del": dels, "add": adds },
                "layout": layout.map(|(dy, dx)| json!({"dy": dy, "dx": dx})),
            })
        })
        .collect();

    let analysis = json!({
        "run_dir": run.dir.to_string_lossy(),
        "marks": run.marks,
        "flicker": flicker_json,
        "remounts": remount_json,
    });

    let mut lines: Vec<String> = Vec::new();
    for m in &run.marks {
        lines.push(format!(
            "Mark {} — {} (via {}).",
            str_field(m, "path"),
            {
                let n = str_field(m, "note");
                if n.is_empty() { str_field(m, "text") } else { n }
            },
            str_field(m, "via")
        ));
    }
    for r in &mounts {
        let cause = cause_of(&run, r.t_mount);
        let cause_s = match &cause {
            Some(c) => format!("{} {} (Δ{}ms earlier)", c.api, c.url, c.delta_ms),
            None => "none".into(),
        };
        let layout = layout_delta(&run, &r.path, r.t_mount);
        let dy_s = match layout {
            Some((dy, _)) => format!("{dy}"),
            None => "none".into(),
        };
        let tag = if is_marked(&run.marks, &r.path, None) { "[MARKED] " } else { "" };
        lines.push(format!(
            "{}Remount of {} at t={} — cause: {}; DOM del {} + add {}; layout Δy={}px.",
            tag, r.name, r.t_mount, cause_s, r.path, r.path, dy_s
        ));
    }
    for f in &flickers {
        let tag = if is_marked(&run.marks, &f.path, f.rect) { "[MARKED] " } else { "" };
        lines.push(format!(
            "{}Flicker {} dwell {}ms (within one frame: {}).",
            tag, f.path, f.dwell_ms, f.within_one_frame
        ));
    }
    let txt = if lines.is_empty() {
        String::new()
    } else {
        let mut s = lines.join("\n");
        s.push('\n');
        s
    };

    if write_pair(&run.dir, "analysis", &analysis, &txt).is_err() {
        eprintln!("failed to write analysis files");
        return (2, String::new());
    }
    (0, txt)
}

fn keyed_items(rows: &[Value], key: impl Fn(&Value) -> (String, Value)) -> Vec<(String, Value)> {
    rows.iter().map(key).collect()
}

fn counts_of(items: &[(String, Value)]) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    for (k, _) in items {
        *m.entry(k.clone()).or_insert(0) += 1;
    }
    m
}

fn only_items(items: &[(String, Value)], other: &HashMap<String, usize>) -> Vec<Value> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::new();
    for (k, v) in items {
        let n = seen.entry(k.clone()).or_insert(0);
        *n += 1;
        if *n > other.get(k).copied().unwrap_or(0) {
            out.push(v.clone());
        }
    }
    out
}

fn remount_counts(run: &Run) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    for r in remounts(run) {
        *m.entry(r.name).or_insert(0) += 1;
    }
    m
}

fn remount_only(a: &HashMap<String, usize>, b: &HashMap<String, usize>) -> Vec<Value> {
    let mut names: Vec<_> = a.keys().cloned().collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|n| {
            let ca = *a.get(&n).unwrap_or(&0);
            let cb = *b.get(&n).unwrap_or(&0);
            if ca > cb {
                Some(json!({"name": n, "count": ca - cb}))
            } else {
                None
            }
        })
        .collect()
}

fn flicker_map(run: &Run) -> HashMap<String, f64> {
    let mut m = HashMap::new();
    for f in flicker(run) {
        m.entry(f.path).or_insert(f.dwell_ms);
    }
    m
}

fn flicker_only(a: &HashMap<String, f64>, b: &HashMap<String, f64>) -> Vec<Value> {
    let mut paths: Vec<_> = a.keys().cloned().collect();
    paths.sort();
    paths
        .into_iter()
        .filter(|p| !b.contains_key(p))
        .map(|p| json!({"path": p, "dwell_ms": a[&p]}))
        .collect()
}

pub fn diff(a: Option<&String>, b: Option<&String>) -> i32 {
    let (Some(a_s), Some(b_s)) = (a, b) else {
        eprintln!("usage: mini --debug-diff <run_a_dir> <run_b_dir>");
        return 2;
    };
    let Some(run_a) = load_run(Path::new(a_s)) else {
        eprintln!("missing run dir: {a_s}");
        return 2;
    };
    let Some(run_b) = load_run(Path::new(b_s)) else {
        eprintln!("missing run dir: {b_s}");
        return 2;
    };

    let backend_a = keyed_items(&run_a.backend, |v| {
        (
            format!("{}\0{}", str_field(v, "event"), str_field(v, "detail")),
            json!({"event": str_field(v, "event"), "detail": str_field(v, "detail")}),
        )
    });
    let backend_b = keyed_items(&run_b.backend, |v| {
        (
            format!("{}\0{}", str_field(v, "event"), str_field(v, "detail")),
            json!({"event": str_field(v, "event"), "detail": str_field(v, "detail")}),
        )
    });
    let console_a = keyed_items(&run_a.console, |v| {
        let args = v
            .get("args")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        (
            format!("{}\0{}", str_field(v, "level"), args),
            json!({"level": str_field(v, "level"), "args": args}),
        )
    });
    let console_b = keyed_items(&run_b.console, |v| {
        let args = v
            .get("args")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        (
            format!("{}\0{}", str_field(v, "level"), args),
            json!({"level": str_field(v, "level"), "args": args}),
        )
    });
    let net_a = keyed_items(&run_a.net, |v| {
        (
            format!(
                "{}\0{}\0{}\0{}",
                str_field(v, "api"),
                str_field(v, "method"),
                str_field(v, "url"),
                str_field(v, "phase")
            ),
            json!({
                "api": str_field(v, "api"),
                "method": str_field(v, "method"),
                "url": str_field(v, "url"),
                "phase": str_field(v, "phase"),
            }),
        )
    });
    let net_b = keyed_items(&run_b.net, |v| {
        (
            format!(
                "{}\0{}\0{}\0{}",
                str_field(v, "api"),
                str_field(v, "method"),
                str_field(v, "url"),
                str_field(v, "phase")
            ),
            json!({
                "api": str_field(v, "api"),
                "method": str_field(v, "method"),
                "url": str_field(v, "url"),
                "phase": str_field(v, "phase"),
            }),
        )
    });

    let bc_a = counts_of(&backend_a);
    let bc_b = counts_of(&backend_b);
    let cc_a = counts_of(&console_a);
    let cc_b = counts_of(&console_b);
    let nc_a = counts_of(&net_a);
    let nc_b = counts_of(&net_b);

    let rem_a = remount_counts(&run_a);
    let rem_b = remount_counts(&run_b);
    let fl_a = flicker_map(&run_a);
    let fl_b = flicker_map(&run_b);

    let rem_only_a = remount_only(&rem_a, &rem_b);
    let rem_only_b = remount_only(&rem_b, &rem_a);
    let fl_only_a = flicker_only(&fl_a, &fl_b);
    let fl_only_b = flicker_only(&fl_b, &fl_a);

    let label_a = label_of(&run_a);
    let label_b = label_of(&run_b);
    let la = if label_a.is_empty() { "a" } else { &label_a };
    let lb = if label_b.is_empty() { "b" } else { &label_b };

    let body = json!({
        "a": {"dir": run_a.dir.to_string_lossy(), "label": label_a},
        "b": {"dir": run_b.dir.to_string_lossy(), "label": label_b},
        "backend": {"only_a": only_items(&backend_a, &bc_b), "only_b": only_items(&backend_b, &bc_a)},
        "console": {"only_a": only_items(&console_a, &cc_b), "only_b": only_items(&console_b, &cc_a)},
        "net": {"only_a": only_items(&net_a, &nc_b), "only_b": only_items(&net_b, &nc_a)},
        "remount_delta": {"only_a": rem_only_a, "only_b": rem_only_b},
        "flicker": {"only_a": fl_only_a, "only_b": fl_only_b},
    });

    let mut parts: Vec<String> = Vec::new();
    if let Some(arr) = body["remount_delta"]["only_a"].as_array() {
        for it in arr {
            parts.push(format!(
                "{}-only remount: {} ×{}",
                la,
                it["name"].as_str().unwrap_or(""),
                it["count"]
            ));
        }
    }
    if let Some(arr) = body["flicker"]["only_a"].as_array() {
        for it in arr {
            parts.push(format!(
                "{}-only flicker: {} dwell {}ms",
                la,
                it["path"].as_str().unwrap_or(""),
                it["dwell_ms"]
            ));
        }
    }
    let b_empty = body["remount_delta"]["only_b"]
        .as_array()
        .map(|a| a.is_empty())
        .unwrap_or(true)
        && body["flicker"]["only_b"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true);
    if b_empty {
        parts.push(format!("{lb} adds none."));
    } else {
        if let Some(arr) = body["remount_delta"]["only_b"].as_array() {
            for it in arr {
                parts.push(format!(
                    "{}-only remount: {} ×{}",
                    lb,
                    it["name"].as_str().unwrap_or(""),
                    it["count"]
                ));
            }
        }
        if let Some(arr) = body["flicker"]["only_b"].as_array() {
            for it in arr {
                parts.push(format!(
                    "{}-only flicker: {} dwell {}ms",
                    lb,
                    it["path"].as_str().unwrap_or(""),
                    it["dwell_ms"]
                ));
            }
        }
    }
    let txt = if parts.is_empty() {
        "no differences.\n".into()
    } else {
        let mut s = parts.join("; ");
        s.push('\n');
        s
    };

    if write_pair(&run_b.dir, "diff", &body, &txt).is_err() {
        eprintln!("failed to write diff files");
        return 2;
    }
    print!("{txt}");
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn empty_run() -> Run {
        Run {
            dir: PathBuf::new(),
            manifest: json!({}),
            console: vec![],
            dom_events: vec![],
            net: vec![],
            react: vec![],
            backend: vec![],
            frames: vec![],
            marks: vec![],
        }
    }

    #[test]
    fn flicker_detects_4ms_dwell() {
        let mut run = empty_run();
        run.dom_events = vec![
            json!({"k":"children","p":"div#box","add":["div#r"],"del":[],"ts":100.0}),
            json!({"k":"children","p":"div#box","add":[],"del":["div#r"],"ts":104.0}),
        ];
        let hits = flicker(&run);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].dwell_ms, 4.0);
    }

    #[test]
    fn marked_flag_uses_rect_overlap_and_path() {
        let mut run = empty_run();
        run.dom_events = vec![
            json!({"k":"children","p":"div#box","add":["div#r"],"del":[],"ts":100.0,
                   "r":{"x":8.0,"y":8.0,"w":100.0,"h":20.0}}),
            json!({"k":"children","p":"div#box","add":[],"del":["div#r"],"ts":104.0}),
        ];
        // Mark whose path shares nothing with div#r but whose rect covers it.
        run.marks = vec![json!({"kind":"mark","path":"section#cal","r":{"x":0.0,"y":0.0,"w":200.0,"h":50.0}})];
        let hits = flicker(&run);
        assert_eq!(hits.len(), 1);
        assert!(is_marked(&run.marks, &hits[0].path, hits[0].rect));

        // Far-away mark: neither path nor rect matches.
        run.marks = vec![json!({"kind":"mark","path":"div#footer","r":{"x":900.0,"y":900.0,"w":10.0,"h":10.0}})];
        assert!(!is_marked(&run.marks, &hits[0].path, hits[0].rect));

        // Path containment without rects.
        run.marks = vec![json!({"kind":"mark","path":"div#r"})];
        assert!(is_marked(&run.marks, "div#r", None));
    }

    #[test]
    fn remount_pairs_unmount_then_mount() {
        let mut run = empty_run();
        run.react = vec![
            json!({"op":"unmount","name":"Row","t":200.0,"path":"div#row"}),
            json!({"op":"mount","name":"Row","t":220.0,"path":"div#row"}),
        ];
        let rs = remounts(&run);
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].name, "Row");
    }

    #[test]
    fn cause_picks_nearest_preceding_net_in_window() {
        let mut run = empty_run();
        run.net = vec![json!({"api":"fetch","url":"/x","phase":"send","t":150.0})];
        let c = cause_of(&run, 220.0).expect("cause");
        assert_eq!(c.delta_ms, 70.0);

        let mut run2 = empty_run();
        run2.net = vec![json!({"api":"fetch","url":"/x","phase":"send","t":0.0})];
        assert!(cause_of(&run2, 700.0).is_none());
    }

    #[test]
    fn remount_falls_back_to_dom_when_react_empty() {
        let mut run = empty_run();
        run.dom_events = vec![
            json!({"k":"children","p":"div#list","add":[],"del":["div#row"],"ts":100.0}),
            json!({"k":"children","p":"div#list","add":["div#row"],"del":[],"ts":150.0}),
        ];
        let rs = remounts(&run);
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].name, "<dom>");
    }
}

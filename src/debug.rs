//! Opt-in agent debug mode: CDP port + JSONL session recording.
//! Active with --debug, --qa, --qa-spike, or a non-empty MINI_DEBUG env var.
//! `--qa`/`--qa-spike` turn the recorder on but must NOT add the CDP port
//! (main.rs gates `--remote-debugging-port` on an explicit --debug only).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

pub fn enabled() -> bool {
    std::env::args().any(|a| a == "--debug" || a == "--qa" || a == "--qa-spike")
        || std::env::var_os("MINI_DEBUG").is_some_and(|v| !v.is_empty())
}

pub fn port() -> u16 {
    std::env::var("MINI_DEBUG_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(9222)
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn debug_root() -> PathBuf {
    dirs::data_local_dir().expect("%LOCALAPPDATA% is unavailable").join("Mini").join("Debug")
}

/// True when this process should become the short-lived detached parent of
/// an interactive `--debug` window: not already the re-exec'd child, some
/// arg asks for `--debug` (or `MINI_DEBUG` is set), and none of the
/// one-shot/non-interactive modes (`--qa`, `--qa-spike`, `--debug-explain`,
/// `--debug-diff`) are present. Pure so `cargo test` can call it directly
/// instead of touching process environment variables (tests run threads in
/// parallel).
pub fn should_detach(args: &[String], env_child: Option<&str>, env_mini_debug: Option<&str>) -> bool {
    if env_child.is_some_and(|v| !v.is_empty()) {
        return false;
    }
    let one_shot = args
        .iter()
        .any(|a| a == "--qa" || a == "--qa-spike" || a == "--debug-explain" || a == "--debug-diff");
    if one_shot {
        return false;
    }
    args.iter().any(|a| a == "--debug") || env_mini_debug.is_some_and(|v| !v.is_empty())
}

/// Builds the object written to `current.json`. Covers every key the file
/// has always had (`cdp`, `session_dir`, `pid`, `started_ms`,
/// `active_run_dir`, `run_label`, `run_index`) plus `ready`. `session_dir` /
/// `active_run_dir` / `run_label` / `run_index` are `None` (-> JSON `null`)
/// when there is no live `DebugRecorder` session (see
/// `write_unrecorded_ready`). Pure so it is testable without a live session
/// directory.
#[allow(clippy::too_many_arguments)]
fn status_json(
    cdp: &str,
    pid: u32,
    started_ms: u64,
    session_dir: Option<&str>,
    active_run_dir: Option<&str>,
    run_label: Option<&str>,
    run_index: Option<u32>,
    ready: bool,
) -> serde_json::Value {
    serde_json::json!({
        "cdp": cdp,
        "session_dir": session_dir,
        "pid": pid,
        "started_ms": started_ms,
        "active_run_dir": active_run_dir,
        "run_label": run_label,
        "run_index": run_index,
        "ready": ready,
    })
}

/// Polls `127.0.0.1:port` until a TCP connect succeeds or `timeout_ms`
/// elapses. Used by the re-exec'd child to learn when the content webview's
/// CDP endpoint is actually accepting connections before it tells the
/// detached parent it is safe to exit.
pub fn wait_for_port(port: u16, timeout_ms: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        if std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).is_ok() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Writes `current.json` with `ready: true` when there is no `DebugRecorder`
/// (recording setup failed; fail-soft). Session fields are `null`; the
/// detached parent only needs `pid` / `cdp` / `ready` to stop waiting.
/// Creates the debug root if missing. Never panics (fail-soft, same as
/// `DebugRecorder::start`).
pub fn write_unrecorded_ready() {
    let root = debug_root();
    if std::fs::create_dir_all(&root).is_err() {
        return;
    }
    let meta = status_json(
        &format!("http://127.0.0.1:{}", port()),
        std::process::id(),
        now_ms() as u64,
        None,
        None,
        None,
        None,
        true,
    );
    let _ = std::fs::write(root.join("current.json"), meta.to_string());
}

/// Polls `current.json` until it parses, `ready == true`, and `pid` matches
/// `child_pid`, or `timeout_ms` elapses. Returns the file's own compact JSON
/// text so the detached parent can print exactly what the child published.
pub fn await_child_ready(child_pid: u32, timeout_ms: u64) -> Option<String> {
    let path = debug_root().join("current.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                let ready = value.get("ready").and_then(|v| v.as_bool()).unwrap_or(false);
                let pid = value.get("pid").and_then(|v| v.as_u64());
                if ready && pid == Some(u64::from(child_pid)) {
                    return Some(value.to_string());
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Re-execs the current binary as a fully detached process: no console, its
/// own process group, and (when the enclosing job allows it) broken away
/// from that job, so closing the launching console/job does not deliver
/// `CTRL_CLOSE_EVENT` to it. `args` is the parent's full argv (argv[0] is
/// skipped: `Command::new` already names the executable); the child gets
/// `MINI_DEBUG_CHILD=1` added, everything else inherited.
pub fn spawn_detached_child(args: &[String]) -> Result<std::process::Child, i32> {
    use std::os::windows::process::CommandExt;

    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("mini: failed to locate the running executable: {e}");
            return Err(2);
        }
    };
    let build = || {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(args.iter().skip(1));
        cmd.env("MINI_DEBUG_CHILD", "1");
        // A truly detached process must not hold the launcher's console or
        // any redirected stdout/stderr pipe open — otherwise a caller that
        // captures the parent's output (common in agent tooling) blocks
        // forever waiting for EOF that never comes while this child runs.
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        cmd
    };

    let mut with_breakaway = build();
    with_breakaway.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
    if let Ok(child) = with_breakaway.spawn() {
        return Ok(child);
    }

    eprintln!("mini: job breakaway was refused; detached without breakaway");
    let mut without_breakaway = build();
    without_breakaway.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    match without_breakaway.spawn() {
        Ok(child) => Ok(child),
        Err(e) => {
            eprintln!("mini: failed to spawn the detached debug browser: {e}");
            Err(2)
        }
    }
}

struct RunWriters {
    dir: PathBuf,
    index: u32,
    label: String,
    frontend: BufWriter<File>,
    dom: BufWriter<File>,
    backend: BufWriter<File>,
    net: BufWriter<File>,
    react: BufWriter<File>,
    frames: BufWriter<File>,
    marks: BufWriter<File>,
}

pub struct DebugRecorder {
    session_dir: PathBuf,
    run_index: u32, // next run number to assign
    run: RunWriters,
    started_ms: u64,
}

fn sanitize(label: &str) -> String {
    let s: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "run".into()
    } else {
        s
    }
}

fn env_label() -> String {
    std::env::var("MINI_DEBUG_LABEL").unwrap_or_else(|_| "default".into())
}

fn env_incident() -> String {
    std::env::var("MINI_DEBUG_INCIDENT").unwrap_or_default()
}

fn open_run(session_dir: &Path, index: u32, label: &str, incident: &str) -> Option<RunWriters> {
    let dir = session_dir.join(format!("run-{index:03}-{}", sanitize(label)));
    std::fs::create_dir_all(&dir).ok()?;
    let open = |name: &str| File::create(dir.join(name)).map(BufWriter::new).ok();
    let run = RunWriters {
        dir: dir.clone(),
        index,
        label: label.to_string(),
        frontend: open("frontend.jsonl")?,
        dom: open("dom.jsonl")?,
        backend: open("backend.jsonl")?,
        net: open("net.jsonl")?,
        react: open("react.jsonl")?,
        frames: open("frames.jsonl")?,
        marks: open("marks.jsonl")?,
    };
    let meta = serde_json::json!({
        "index": index,
        "label": label,
        "incident": incident,
        "started_ms": now_ms() as u64,
    });
    std::fs::write(dir.join("run.json"), meta.to_string()).ok()?;
    Some(run)
}

impl DebugRecorder {
    /// Creates %LOCALAPPDATA%\Mini\Debug\session-<unix_ms>\run-000-<label>\
    /// with six JSONL files, and writes ..\Debug\current.json pointing agents
    /// at this session + active run. Any I/O failure -> None (debug mode
    /// silently off; browser must still run).
    pub fn start() -> Option<DebugRecorder> {
        let session_dir = debug_root().join(format!("session-{}", now_ms()));
        std::fs::create_dir_all(&session_dir).ok()?;
        let run = open_run(&session_dir, 0, &env_label(), &env_incident())?;
        let rec = DebugRecorder {
            session_dir,
            run_index: 1,
            run,
            started_ms: now_ms() as u64,
        };
        rec.write_current();
        Some(rec)
    }

    /// The active run's directory. Used by the QA bridge's `run_label` to
    /// hand the loop a path to stash per-viewport evidence under.
    pub fn run_dir(&self) -> PathBuf {
        self.run.dir.clone()
    }

    fn write_status(&self, ready: bool) {
        let meta = status_json(
            &format!("http://127.0.0.1:{}", port()),
            std::process::id(),
            self.started_ms,
            Some(self.session_dir.to_string_lossy().as_ref()),
            Some(self.run.dir.to_string_lossy().as_ref()),
            Some(self.run.label.as_str()),
            Some(self.run.index),
            ready,
        );
        let _ = std::fs::write(debug_root().join("current.json"), meta.to_string());
    }

    fn write_current(&self) {
        self.write_status(false);
    }

    /// Rewrites `current.json` with `ready: true`. Callers invoke this only
    /// after a TCP connect to `127.0.0.1:port()` has actually succeeded
    /// (`wait_for_port`) — this is the signal the detached parent blocks on.
    pub fn mark_ready(&self) {
        self.write_status(true);
    }

    pub fn start_run(&mut self, label: &str, incident: &str) {
        match open_run(&self.session_dir, self.run_index, label, incident) {
            Some(r) => {
                self.run = r;
                self.run_index += 1;
                self.write_current();
            }
            None => {
                self.backend("run-error", label);
            }
        }
    }

    /// body is the raw JSON line already produced by the injected JS (has its own `t`).
    pub fn frontend_line(&mut self, body: &str) {
        let _ = writeln!(self.run.frontend, "{body}");
        let _ = self.run.frontend.flush();
    }

    pub fn dom_line(&mut self, body: &str) {
        let _ = writeln!(self.run.dom, "{body}");
        let _ = self.run.dom.flush();
    }

    pub fn net_line(&mut self, body: &str) {
        let _ = writeln!(self.run.net, "{body}");
        let _ = self.run.net.flush();
    }

    pub fn react_line(&mut self, body: &str) {
        let _ = writeln!(self.run.react, "{body}");
        let _ = self.run.react.flush();
    }

    pub fn frames_line(&mut self, body: &str) {
        let _ = writeln!(self.run.frames, "{body}");
        let _ = self.run.frames.flush();
    }

    pub fn marks_line(&mut self, body: &str) {
        let _ = writeln!(self.run.marks, "{body}");
        let _ = self.run.marks.flush();
    }

    pub fn backend(&mut self, event: &str, detail: &str) {
        let line = serde_json::json!({ "t": now_ms() as u64, "event": event, "detail": detail });
        let _ = writeln!(self.run.backend, "{line}");
        let _ = self.run.backend.flush();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn debug_root_suffix() {
        let p = super::debug_root().to_string_lossy().replace('/', "\\");
        assert!(p.ends_with("\\Mini\\Debug"), "got: {p}");
    }

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn should_detach_true_for_plain_debug_flag() {
        assert!(super::should_detach(&args(&["mini", "--debug"]), None, None));
    }

    #[test]
    fn should_detach_false_when_already_the_child() {
        assert!(!super::should_detach(&args(&["mini", "--debug"]), Some("1"), None));
    }

    #[test]
    fn should_detach_false_for_qa_even_with_debug() {
        assert!(!super::should_detach(&args(&["mini", "--debug", "--qa"]), None, None));
    }

    #[test]
    fn should_detach_false_for_debug_explain_even_with_debug() {
        assert!(!super::should_detach(&args(&["mini", "--debug", "--debug-explain"]), None, None));
    }

    #[test]
    fn should_detach_true_from_mini_debug_env_var() {
        assert!(super::should_detach(&args(&["mini"]), None, Some("1")));
    }

    #[test]
    fn should_detach_false_for_qa_from_env_var() {
        assert!(!super::should_detach(&args(&["mini", "--qa"]), None, Some("1")));
    }

    #[test]
    fn status_json_ready_flag_flips_pid_and_cdp_stay_equal() {
        let not_ready = super::status_json("http://127.0.0.1:9222", 4242, 0, None, None, None, None, false);
        let ready = super::status_json("http://127.0.0.1:9222", 4242, 0, None, None, None, None, true);
        assert_eq!(not_ready["ready"], false);
        assert_eq!(ready["ready"], true);
        assert_eq!(not_ready["pid"], ready["pid"]);
        assert_eq!(not_ready["cdp"], ready["cdp"]);
    }
}

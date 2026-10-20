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

/// Debug mode only. Canonical source: tools/recorder.js (also injected into
/// external Chromiums by tools/record.js, and over CDP into Chrome for Android
/// by `qa::remote`). Its `post` prefers the __miniPost CDP binding and falls
/// back to window.ipc: inside Mini only window.ipc exists, so everything ships
/// as {"cmd":"debug","kind":...} lines; through the binding each call carries
/// one such line.
pub const RECORDER_SCRIPT: &str = include_str!("../tools/recorder.js");

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

/// Starts the interactive `--debug` browser detached (same spawn and
/// readiness wait as `mini --debug`) and blocks until it publishes `ready`.
/// `url` = start page; `None` opens Mini's start page.
pub fn launch_detached(url: Option<&str>, timeout_ms: u64) -> Result<(), String> {
    let mut args = vec!["mini".to_string(), "--debug".to_string()];
    args.extend(url.map(str::to_string));
    // CreateProcess inherits every inheritable handle, so without this the
    // long-lived browser keeps the caller's stdout/stderr pipe open and a
    // shell capturing `mini --move` output never sees EOF.
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{SetHandleInformation, HANDLE, HANDLE_FLAGS, HANDLE_FLAG_INHERIT};
        for h in [std::io::stdin().as_raw_handle(), std::io::stdout().as_raw_handle(), std::io::stderr().as_raw_handle()] {
            unsafe {
                let _ = SetHandleInformation(HANDLE(h), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));
            }
        }
    }
    let mut child = spawn_detached_child(&args).map_err(|_| "could not start the debug browser".to_string())?;
    if await_child_ready(child.id(), timeout_ms).is_some() {
        return Ok(());
    }
    let _ = child.kill();
    Err(format!("debug browser did not open 127.0.0.1:{} within {}s", port(), timeout_ms / 1000))
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
    /// false for `start_private`: `current.json` is never written or tombstoned.
    publish: bool,
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
            publish: true,
        };
        rec.write_current();
        Some(rec)
    }

    /// A recorder that writes runs under `session_dir` and never touches
    /// `current.json` (the device lane must not clobber a live debug window).
    pub fn start_private(session_dir: PathBuf) -> Option<DebugRecorder> {
        std::fs::create_dir_all(&session_dir).ok()?;
        let run = open_run(&session_dir, 0, "device", "")?;
        Some(DebugRecorder { session_dir, run_index: 1, run, started_ms: now_ms() as u64, publish: false })
    }

    /// The active run's directory. Used by the QA bridge's `run_label` to
    /// hand the loop a path to stash per-viewport evidence under.
    pub fn run_dir(&self) -> PathBuf {
        self.run.dir.clone()
    }

    /// Where this recorder publishes `current.json`; `None` for a private one.
    fn status_path(&self) -> Option<PathBuf> {
        self.publish.then(current_path)
    }

    fn write_status(&self, ready: bool) {
        let Some(path) = self.status_path() else { return };
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
        let _ = std::fs::write(path, meta.to_string());
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

/// One recorder line as the page posts it: `{"cmd":"debug","kind":...}`, or a
/// `{"cmd":"debug-batch","lines":[...]}` wrapping several of them.
#[derive(serde::Deserialize)]
struct RecorderLine {
    cmd: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    incident: Option<String>,
    /// `debug-batch`: raw recorder lines, each a `{"cmd":"debug",...}` object.
    #[serde(default)]
    lines: Vec<String>,
}

/// Files one recorder line (or every `debug` line of a `debug-batch`) into its
/// stream: `kind` dom, net, react, frame or mark has its own file, `run` rotates
/// the run, anything else is frontend. Other commands and lines that do not
/// parse are ignored.
pub fn route_line(rec: &mut DebugRecorder, line: &str) {
    if let Ok(c) = serde_json::from_str::<RecorderLine>(line) {
        if c.cmd == "debug" || c.cmd == "debug-batch" {
            route_parsed(rec, &c, line);
        }
    }
}

fn route_parsed(rec: &mut DebugRecorder, c: &RecorderLine, body: &str) {
    if c.cmd == "debug-batch" {
        for line in &c.lines {
            if let Ok(inner) = serde_json::from_str::<RecorderLine>(line) {
                if inner.cmd == "debug" {
                    route_parsed(rec, &inner, line);
                }
            }
        }
        return;
    }
    match c.kind.as_deref() {
        Some("dom") => rec.dom_line(body),
        Some("net") => rec.net_line(body),
        Some("react") => rec.react_line(body),
        Some("frame") => rec.frames_line(body),
        Some("mark") => rec.marks_line(body),
        Some("run") => rec.start_run(c.label.as_deref().unwrap_or("run"), c.incident.as_deref().unwrap_or("")),
        _ => rec.frontend_line(body),
    }
}

fn current_path() -> PathBuf {
    debug_root().join("current.json")
}

/// Parsed `current.json`, or `None` when the file is missing or invalid.
pub fn read_status() -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(current_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// The dead-session form of a status record: `ready`/`alive` false and no
/// `cdp`. Only when the record belongs to `pid`; a newer session's record is
/// returned unchanged so an old process can never clobber it.
fn tombstone(mut status: serde_json::Value, pid: u64) -> serde_json::Value {
    if status.get("pid").and_then(|v| v.as_u64()) != Some(pid) {
        return status;
    }
    if let Some(obj) = status.as_object_mut() {
        obj.insert("ready".into(), serde_json::Value::Bool(false));
        obj.insert("alive".into(), serde_json::Value::Bool(false));
        obj.insert("cdp".into(), serde_json::Value::Null);
    }
    status
}

/// Rewrites `current.json` as dead when it still names `pid`. Never panics.
fn tombstone_pid(pid: u32) {
    if let Some(status) = read_status() {
        let dead = tombstone(status.clone(), u64::from(pid));
        if dead != status {
            let _ = std::fs::write(current_path(), dead.to_string());
        }
    }
}

impl Drop for DebugRecorder {
    fn drop(&mut self) {
        // A private recorder shares the pid of a live debug window in this process:
        // it must not tombstone a record it never wrote.
        if self.publish {
            tombstone_pid(std::process::id());
        }
    }
}

/// Tombstones this process's record on a normal exit. The interactive loop
/// never returns (`event_loop.run` exits the process), so `Drop` alone does
/// not run there; `main` calls this on `ControlFlow::Exit`.
pub fn tombstone_self() {
    tombstone_pid(std::process::id());
}

/// Why a recorded debug browser is not usable, or `None` when it is.
/// Pure: the Windows checks are passed in so tests need no live process.
fn status_problem(
    status: Option<&serde_json::Value>,
    alive: impl Fn(u32) -> bool,
    image_ok: impl Fn(u32) -> bool,
    port_open: impl Fn(&str) -> bool,
) -> Option<&'static str> {
    let Some(s) = status else { return Some("missing file") };
    if !s.get("ready").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Some("not ready");
    }
    if s.get("alive").and_then(|v| v.as_bool()) == Some(false) {
        return Some("not ready");
    }
    let Some(pid) = s.get("pid").and_then(|v| v.as_u64()).and_then(|p| u32::try_from(p).ok()) else {
        return Some("missing file");
    };
    if !alive(pid) {
        return Some("pid dead");
    }
    if !image_ok(pid) {
        return Some("pid is not mini.exe");
    }
    let cdp = s.get("cdp").and_then(|v| v.as_str()).unwrap_or("");
    if !port_open(cdp) {
        return Some("port closed");
    }
    None
}

/// `127.0.0.1:9222` from `http://127.0.0.1:9222`.
fn cdp_socket_addr(cdp: &str) -> Option<std::net::SocketAddr> {
    let authority = cdp.strip_prefix("http://")?.trim_end_matches('/');
    authority.parse().ok()
}

fn port_accepts(cdp: &str) -> bool {
    match cdp_socket_addr(cdp) {
        Some(addr) => {
            std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(1)).is_ok()
        }
        None => false,
    }
}

const STILL_ACTIVE: u32 = 259;

pub fn process_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code).is_ok();
        let _ = CloseHandle(handle);
        ok && code == STILL_ACTIVE
    }
}

pub fn process_image_is_mini(pid: u32) -> bool {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return false;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/']).next().is_some_and(|name| name.eq_ignore_ascii_case("mini.exe"))
    }
}

/// `mini --debug-status`: 0 when the recorded debug browser is up and its CDP
/// port accepts connections; otherwise one stderr line naming the failed check, 2.
pub fn status_code() -> i32 {
    let status = read_status();
    match status_problem(status.as_ref(), process_alive, process_image_is_mini, port_accepts) {
        None => {
            if let Some(s) = status {
                println!("{s}");
            }
            0
        }
        Some(problem) => {
            eprintln!("mini: debug browser is not usable: {problem}");
            2
        }
    }
}

/// Same checks as [`status_code`], without printing. For callers that
/// report their own error (`--move`, `--observe`).
pub fn status_code_quiet() -> i32 {
    let status = read_status();
    match status_problem(status.as_ref(), process_alive, process_image_is_mini, port_accepts) {
        None => 0,
        Some(_) => 2,
    }
}

/// The CDP port the live session published (it may differ from this
/// process's `MINI_DEBUG_PORT`).
pub fn recorded_port() -> Option<u16> {
    let status = read_status()?;
    cdp_socket_addr(status.get("cdp")?.as_str()?).map(|a| a.port())
}

/// `mini --debug-stop`: terminates the recorded debug browser (only when the
/// pid is still a `mini.exe`) and tombstones `current.json`.
pub fn stop_running() -> i32 {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    let Some(pid) = read_status()
        .and_then(|s| s.get("pid").and_then(|v| v.as_u64()))
        .and_then(|p| u32::try_from(p).ok())
    else {
        eprintln!("mini: no debug browser recorded");
        return 0;
    };
    if !process_alive(pid) {
        tombstone_pid(pid);
        return 0;
    }
    if !process_image_is_mini(pid) {
        eprintln!("mini: pid {pid} is not mini.exe; not killing it");
        return 2;
    }
    let killed = unsafe {
        match OpenProcess(PROCESS_TERMINATE, false, pid) {
            Ok(handle) => {
                let ok = TerminateProcess(handle, 0).is_ok();
                let _ = CloseHandle(handle);
                ok
            }
            Err(_) => false,
        }
    };
    if !killed {
        eprintln!("mini: could not terminate pid {pid}");
        return 2;
    }
    tombstone_pid(pid);
    0
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
    fn ready_status() -> serde_json::Value {
        super::status_json("http://127.0.0.1:9222", 4242, 0, Some("C:\\s"), None, None, None, true)
    }

    #[test]
    fn tombstone_clears_cdp_and_keeps_session_dir() {
        let dead = super::tombstone(ready_status(), 4242);
        assert_eq!(dead["ready"], false);
        assert_eq!(dead["alive"], false);
        assert!(dead["cdp"].is_null());
        assert_eq!(dead["session_dir"], ready_status()["session_dir"]);
    }

    #[test]
    fn tombstone_leaves_another_sessions_record_alone() {
        assert_eq!(super::tombstone(ready_status(), 1), ready_status());
    }

    #[test]
    fn status_problem_is_none_only_when_every_check_passes() {
        let s = ready_status();
        let yes = |_: u32| true;
        let no = |_: u32| false;
        let open = |_: &str| true;
        let closed = |_: &str| false;
        assert_eq!(super::status_problem(Some(&s), yes, yes, open), None);
        assert_eq!(super::status_problem(None, yes, yes, open), Some("missing file"));
        assert_eq!(super::status_problem(Some(&s), no, yes, open), Some("pid dead"));
        assert_eq!(super::status_problem(Some(&s), yes, no, open), Some("pid is not mini.exe"));
        assert_eq!(super::status_problem(Some(&s), yes, yes, closed), Some("port closed"));
        let dead = super::tombstone(s.clone(), 4242);
        assert_eq!(super::status_problem(Some(&dead), yes, yes, open), Some("not ready"));
    }

    #[test]
    fn cdp_socket_addr_parses_the_published_url() {
        assert_eq!(super::cdp_socket_addr("http://127.0.0.1:9222").map(|a| a.port()), Some(9222));
        assert!(super::cdp_socket_addr("").is_none());
    }

    // ---- route_line / start_private: only ever under a temp dir, never the real debug root ----

    const STREAMS: [&str; 6] = ["frontend.jsonl", "dom.jsonl", "net.jsonl", "react.jsonl", "frames.jsonl", "marks.jsonl"];

    /// A private recorder over a fresh temp dir. The caller removes `root`.
    fn private_recorder(tag: &str) -> (super::DebugRecorder, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("mini-debug-{tag}-{}-{}", std::process::id(), super::now_ms()));
        let rec = super::DebugRecorder::start_private(root.join("session")).expect("private recorder starts");
        (rec, root)
    }

    fn stream(dir: &std::path::Path, file: &str) -> Vec<String> {
        std::fs::read_to_string(dir.join(file)).unwrap_or_default().lines().map(str::to_string).collect()
    }

    fn dir_name(dir: &std::path::Path) -> String {
        dir.file_name().unwrap().to_string_lossy().into_owned()
    }

    #[test]
    fn route_line_splits_a_batch_into_its_streams() {
        let (mut rec, root) = private_recorder("batch");
        let net = r#"{"cmd":"debug","kind":"net","t":1}"#;
        let dom = r#"{"cmd":"debug","kind":"dom","t":2}"#;
        let batch = serde_json::json!({ "cmd": "debug-batch", "lines": [net, dom] }).to_string();
        super::route_line(&mut rec, &batch);
        let run = rec.run_dir();
        assert_eq!(stream(&run, "net.jsonl"), [net]);
        assert_eq!(stream(&run, "dom.jsonl"), [dom]);
        for other in ["frontend.jsonl", "react.jsonl", "frames.jsonl", "marks.jsonl"] {
            assert!(stream(&run, other).is_empty(), "{other} should stay empty");
        }
        assert!(!root.join("current.json").exists(), "a private recorder publishes nothing");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn route_line_files_each_kind_in_its_own_stream() {
        let (mut rec, root) = private_recorder("kinds");
        let cases = [
            ("dom", "dom.jsonl"),
            ("net", "net.jsonl"),
            ("react", "react.jsonl"),
            ("frame", "frames.jsonl"),
            ("mark", "marks.jsonl"),
            ("console", "frontend.jsonl"),
        ];
        for (kind, _) in cases {
            super::route_line(&mut rec, &format!(r#"{{"cmd":"debug","kind":"{kind}"}}"#));
        }
        super::route_line(&mut rec, r#"{"cmd":"debug"}"#); // no kind: frontend as well
        let run = rec.run_dir();
        for (kind, file) in cases {
            let want = format!(r#"{{"cmd":"debug","kind":"{kind}"}}"#);
            assert!(stream(&run, file).contains(&want), "{kind} belongs in {file}");
        }
        for file in ["dom.jsonl", "net.jsonl", "react.jsonl", "frames.jsonl", "marks.jsonl"] {
            assert_eq!(stream(&run, file).len(), 1, "{file}");
        }
        assert_eq!(stream(&run, "frontend.jsonl").len(), 2, "console + the kind-less line");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn route_line_ignores_other_commands_and_garbage() {
        let (mut rec, root) = private_recorder("ignored");
        for line in [r#"{"cmd":"navigate","text":"https://example.com"}"#, r#"{"cmd":"focus-url"}"#, r#"{"kind":"net"}"#, "not json", ""] {
            super::route_line(&mut rec, line);
        }
        // A batch only carries `debug` lines: no other command, no nested batch.
        let nested = r#"{"cmd":"debug-batch","lines":["{\"cmd\":\"debug\",\"kind\":\"net\"}"]}"#;
        let batch = serde_json::json!({ "cmd": "debug-batch", "lines": [r#"{"cmd":"navigate"}"#, "junk", nested] }).to_string();
        super::route_line(&mut rec, &batch);
        let run = rec.run_dir();
        for file in STREAMS {
            assert!(stream(&run, file).is_empty(), "{file} should stay empty");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_run_line_rotates_to_a_new_run_with_its_label_and_incident() {
        let (mut rec, root) = private_recorder("run");
        super::route_line(&mut rec, r#"{"cmd":"debug","kind":"run","label":"phone view","incident":"INC-7"}"#);
        let run = rec.run_dir();
        assert_eq!(dir_name(&run), "run-001-phone_view");
        let meta: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(run.join("run.json")).unwrap()).unwrap();
        assert_eq!(meta["label"], "phone view");
        assert_eq!(meta["incident"], "INC-7");
        super::route_line(&mut rec, r#"{"cmd":"debug","kind":"run"}"#); // no label: "run"
        assert_eq!(dir_name(&rec.run_dir()), "run-002-run");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_private_recorder_opens_run_000_and_never_publishes_current_json() {
        let (mut rec, root) = private_recorder("private");
        assert_eq!(dir_name(&rec.run_dir()), "run-000-device");
        rec.start_run("second", "");
        rec.mark_ready();
        assert!(rec.status_path().is_none(), "a private recorder has nowhere to publish");
        assert!(!root.join("current.json").exists());
        assert!(!root.join("session").join("current.json").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}

# Plan 001: Detach the debug browser and publish current.json only after CDP is listening

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 5501909..HEAD -- src/main.rs src/debug.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: direction
- **Planned at**: commit `5501909`, 2026-09-22

## Why this matters

A coding agent launches `target\debug\mini.exe --debug` from a command that then exits. Debug builds are console-subsystem processes (`src/main.rs` line 1 is `windows` subsystem only when `debug_assertions` is off). Closing that console delivers `CTRL_CLOSE_EVENT` to Mini, so the process dies with the command. `current.json` is written before the content webview exists and before port 9222 listens, so the next command sees a healthy-looking file and gets "connection refused".

After this plan, the interactive `--debug` launch re-executes itself detached from the parent console, and the parent prints `current.json` and exits only once `127.0.0.1:<port>` accepts a TCP connection. The child keeps running.

## Current state

- `src/main.rs` — process entry, interactive window, and the QA shell. Debug CDP is enabled only on the content webview.
- `src/debug.rs` — session directory and `current.json`.
- `AGENTS.md` — agents are told to launch `target\debug\mini.exe --debug` and read `current.json`. Do not rewrite that file in this plan (plan 003 does).

`src/main.rs` line 1:

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
```

Interactive debug setup (`src/main.rs`, the `main` function, after the `--qa` early exit):

```rust
let debug_on = debug::enabled();
let mut recorder = if debug_on { debug::DebugRecorder::start() } else { None };
// ...
let content_args = if debug_on {
    format!("{CONTENT_BROWSER_ARGS} --remote-debugging-port={}", debug::port())
} else {
    CONTENT_BROWSER_ARGS.to_string()
};
```

`debug::enabled()` is true for `--debug`, `--qa`, `--qa-spike`, or a non-empty `MINI_DEBUG`. The port comes from `MINI_DEBUG_PORT`, default `9222` (`src/debug.rs` `port()`).

`DebugRecorder::start` creates the session and immediately writes `%LOCALAPPDATA%\Mini\Debug\current.json` with `cdp`, `session_dir`, `pid`, `started_ms`, `active_run_dir`, `run_label`, `run_index` (`src/debug.rs`, `write_current`). Nothing waits for the port. Nothing clears the file on exit (that is plan 002; do not add the tombstone here).

`--qa` / `--qa-spike` must not open `--remote-debugging-port`. `build_qa_shell` passes `CONTENT_BROWSER_ARGS` only. Leave that path alone.

Repo conventions:

- Rust edition 2021. New helpers get a short module doc comment, the same style as the top of `src/debug.rs`.
- Fail-soft debug: if recording setup fails, the browser still runs (`DebugRecorder::start` returns `None`). Keep that. A failed *port* on an explicit debug launch is different: the parent must exit 2 and must not leave the child running.
- Build and test with the repo wrapper, from the repo root. `cargo` is not on `PATH`. A running `mini.exe` locks `target\debug\mini.exe` (`os error 5`).
- Commit messages look like `fix(windows): hide console on release builds`.

`docs/SPEC.md` §1 still applies: one window, no tabs. This plan does not add UI.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Test | `cmd.exe /c "tools\win-cargo.cmd" test` | exit 0 |
| Build | `cmd.exe /c "tools\win-cargo.cmd" build` | exit 0, produces `target\debug\mini.exe` |
| Kill a stale lock | `taskkill /IM mini.exe /F` | exit 0, or exit 128 when no such process |

Run the kill only when a build fails with `os error 5`. Then rebuild.

## Scope

**In scope** (the only files you should modify):

- `src/debug.rs`
- `src/main.rs`
- `plans/README.md` (status row only, after the plan is done)

**Out of scope** (do NOT touch, even though they look related):

- `src/qa/**` — the QA loop, firewall, and Jev client stay as they are.
- `src/main.rs` function `build_qa_shell` and `run_qa` / `run_qa_spike` — no detach, no debug port.
- `tools/recorder.js`, `tools/qa-*.js` — not part of process lifetime.
- `AGENTS.md`, `README.md`, `docs/SPEC.md` — plan 003 documents the agent-facing command.
- Tombstone, `--debug-status`, `--debug-stop` — plan 002.
- Any Jev / `--move` work — plan 003.

## Git workflow

- Branch: `advisor/001-detach-debug-browser`
- One commit when the steps pass: `fix(debug): detach the debug browser and publish CDP only when it listens`
- Do NOT push or open a PR.

## Steps

### Step 1: Add a detach decision and a ready file writer

In `src/debug.rs`:

1. Add `pub fn should_detach(args: &[String], env_child: Option<&str>, env_mini_debug: Option<&str>) -> bool`. Keep it pure so tests do not write process environment variables (`cargo test` runs threads in parallel). Return true only when this process should become the short-lived parent of an interactive debug window:
   - `env_child` is `None` or empty, AND
   - some arg is `--debug`, or `env_mini_debug` is `Some` and non-empty, AND
   - no arg is `--qa`, `--qa-spike`, `--debug-explain`, or `--debug-diff`.
   The caller in `main` passes `std::env::var("MINI_DEBUG_CHILD").ok()` and `std::env::var("MINI_DEBUG").ok()`.
2. Add `pub struct DebugStatus` with the fields today's `current.json` already has, plus `ready: bool`. Serialize it with `serde_json`.
3. Change `write_current` so the object includes `"ready": false`. Do not remove existing keys.
4. Add `pub fn mark_ready(&self)` which sets `ready` to true and rewrites `current.json`. Callers invoke it only after a TCP connect to `127.0.0.1:port()` succeeds.
5. Add `pub fn wait_for_port(port: u16, timeout_ms: u64) -> bool`. Loop `std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))` every 100 ms until success or the timeout. Return true on connect.

Unit tests in the existing `debug::tests` module:

- `should_detach(["mini", "--debug"], None, None)` is true.
- `should_detach(["mini", "--debug"], Some("1"), None)` is false.
- `should_detach` is false when the args contain `--qa` or `--debug-explain`, even with `--debug`.
- `should_detach(["mini"], None, Some("1"))` is true. `should_detach(["mini", "--qa"], None, Some("1"))` is false.
- A JSON value written by a small test helper (extract the object-building expression if needed) contains `"ready": false` before `mark_ready` and `"ready": true` after. If `mark_ready` needs a live session directory, build that object in a pure function `fn status_json(...) -> serde_json::Value` and test the function. Do not require `%LOCALAPPDATA%` for the assertion beyond the existing `debug_root_suffix` test.

**Verify**: `cmd.exe /c "tools\win-cargo.cmd" test debug::` → exit 0. If the filter matches nothing, the wrapper still exits 0 only when those new tests ran. Check the test output names `should_detach` and `ready`. If the filter is wrong, run `cmd.exe /c "tools\win-cargo.cmd" test` and confirm the new tests passed by name.

### Step 2: Re-exec the interactive debug launch

In `src/main.rs`, in `main`, after the `--debug-diff`, `--debug-explain`, `--qa-spike`, and `--qa` early exits, and before `profile::ensure_dirs()`:

- If `debug::should_detach(&args, std::env::var("MINI_DEBUG_CHILD").ok().as_deref(), std::env::var("MINI_DEBUG").ok().as_deref())` is true, call a new `debug::spawn_detached_child()` and then wait.

`spawn_detached_child` (in `src/debug.rs`), Windows-only (`std::os::windows::process::CommandExt`):

- Spawn `std::env::current_exe()` with the same arguments the parent received, plus env `MINI_DEBUG_CHILD=1`. Inherit the other environment (so `MINI_DEBUG`, `MINI_DEBUG_PORT`, `TYPESAFE_API_KEY` still flow).
- Creation flags, first attempt: `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB` (`0x00000008 | 0x00000200 | 0x01000000`).
- If that spawn returns an error, retry once with `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` only. Print one stderr line: `mini: job breakaway was refused; detached without breakaway`.
- If both spawns fail, print the error and return a code the parent will exit with (2). Do not open a window in the parent.

Parent wait, in `main`, after a successful spawn:

- Poll `%LOCALAPPDATA%\Mini\Debug\current.json` every 100 ms for up to 20 seconds.
- Success: the file parses as JSON, `ready == true`, and `pid` equals the child process id. Print that JSON as a single line to stdout. Exit 0. Do not open a window in the parent.
- Failure: stderr `mini: debug browser did not open 127.0.0.1:<port> within 20s`, `child.kill()` if the child is still running, exit 2.

Child path (`MINI_DEBUG_CHILD=1`, so `should_detach` is false): the existing interactive `main` runs. After the content webview is built (the `die_if_webview_missing(... "content webview")` call) and only when `debug_on` is true:

- Call `debug::wait_for_port(debug::port(), 15_000)`.
- On true, call `recorder.mark_ready()` if `recorder` is `Some`. If `recorder` is `None`, still write a `current.json` with `ready: true`, `pid` = this process, `cdp` = `http://127.0.0.1:<port>`, and null session fields, so the parent can return. Use the same `status_json` helper. Do not panic.
- On false, exit 2 with stderr `mini: CDP port <port> never accepted a connection`. Do not enter the event loop. This is the one case where a debug launch must not continue: the parent is blocked on this signal.

Do not call `mark_ready` from `run_qa_event_loop`. QA must not publish `ready: true`.

**Verify**: `cmd.exe /c "tools\win-cargo.cmd" build` → exit 0.

### Step 3: Smoke the detach

Stop any running Mini first (`taskkill /IM mini.exe /F`). Then from the repo root in PowerShell:

```powershell
$out = Join-Path $env:TEMP 'mini-detach-out.txt'
$err = Join-Path $env:TEMP 'mini-detach-err.txt'
$p = Start-Process -FilePath 'target\debug\mini.exe' -ArgumentList '--debug','http://127.0.0.1:9' -RedirectStandardOutput $out -RedirectStandardError $err -PassThru -NoNewWindow
if (-not $p.WaitForExit(25000)) { Stop-Process -Id $p.Id -Force; throw 'parent did not exit within 25s' }
if ($p.ExitCode -ne 0) { Get-Content $err; throw "parent exit $($p.ExitCode)" }
$meta = Get-Content (Join-Path $env:LOCALAPPDATA 'Mini\Debug\current.json') -Raw | ConvertFrom-Json
if (-not $meta.ready) { throw 'ready is false' }
if (-not (Get-Process -Id $meta.pid -ErrorAction SilentlyContinue)) { throw 'child pid is dead' }
$tcp = New-Object System.Net.Sockets.TcpClient
$tcp.Connect('127.0.0.1', ([uri]$meta.cdp).Port)
$tcp.Close()
Stop-Process -Id $meta.pid -Force
```

Expected: the parent process exits 0 within 25 seconds, `ready` is true, `pid` is a live process that is not the parent, TCP connect to the `cdp` port succeeds, then that pid is killed. `http://127.0.0.1:9` will not load a page. That is fine. This step checks the process and the port, not navigation.

**Verify**: the PowerShell block finishes without `throw`, and `Get-Process mini -ErrorAction SilentlyContinue` prints nothing afterwards.

## Test plan

- Extend `src/debug.rs` `mod tests`. Pattern: the existing `debug_root_suffix` test in that module (assert on a string, no browser).
- Cases: detach decision (parent, child, `--qa`, `--debug-explain`), and `status_json` ready false then true with the same pid and cdp URL.
- No new browser test in `cargo test`. The smoke in step 3 is the process-lifetime check.
- Verification: `cmd.exe /c "tools\win-cargo.cmd" test` → exit 0, and the step 3 smoke passes.

## Done criteria

ALL must hold:

- [ ] `cmd.exe /c "tools\win-cargo.cmd" test` exits 0
- [ ] `cmd.exe /c "tools\win-cargo.cmd" build` exits 0
- [ ] Step 3 smoke passes, and no `mini.exe` is left running
- [ ] `git grep -n "remote-debugging-port" src/main.rs` shows the flag only on the interactive content-webview args, never inside `build_qa_shell`
- [ ] No files outside the in-scope list are modified (`git status`)
- [ ] `plans/README.md` status row for 001 is `DONE`

## STOP conditions

Stop and report back (do not improvise) if:

- The excerpts in "Current state" do not match the live files.
- `should_detach` cannot see `MINI_DEBUG` because `enabled()` treats `--qa` and `--debug` as one predicate and splitting them breaks `cargo test`. Report the test name.
- Both `CreateProcess` flag sets fail on this machine. Paste the `std::io::Error` kind and raw os error. Do not fall back to running the window in the parent.
- Step 3's parent is still alive at 25 seconds, or the child pid equals the parent pid. That means detach did not happen. Do not weaken the smoke.
- The fix appears to require editing `src/qa/**` or `build_qa_shell`.

## Maintenance notes

- Plan 002 tombstones this file on exit and adds `--debug-status` / `--debug-stop`. Leave `ready: false` as the value `start()` writes. Do not delete `current.json` on drop in this plan.
- A reviewer should check that the parent never builds a `WebView`, and that the child is the process whose pid is in `current.json`.
- `CREATE_BREAKAWAY_FROM_JOB` fails when the agent job does not allow breakaway. The retry without that flag is intentional. If the smoke still dies when the parent exits, the remaining cause is the job object, and that is a STOP, not a second retry loop.
- Deferred: Jev movement (`plans/003-jev-move-loopback.md`) and the stale-file tombstone (`plans/002-debug-status.md`).

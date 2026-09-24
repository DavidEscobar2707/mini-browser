# Plan 002: Make current.json false when the process is dead, and add status/stop

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 5501909..HEAD -- src/main.rs src/debug.rs Cargo.toml`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition. Plan 001 is expected to have changed
> `src/debug.rs` and `src/main.rs`. Read those changes. Stop only when the
> ready/pid behavior from plan 001 is absent, not merely because the diff is non-empty.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/001-detach-debug-browser.md
- **Category**: direction
- **Planned at**: commit `5501909`, 2026-09-22

## Why this matters

`%LOCALAPPDATA%\Mini\Debug\current.json` is how an agent finds the debug browser. Today it is written once in `DebugRecorder::start` and never updated when the process exits. The next agent trusts `cdp` and `pid` from a dead session and gets "connection refused", or worse, talks to a recycled pid.

After this plan, a normal exit rewrites the file so `ready` is false, `mini --debug-status` exits 0 only when that pid is alive and the CDP port accepts a connection, and `mini --debug-stop` terminates that Mini process.

## Current state

Depends on plan 001. After that plan:

- `current.json` includes `ready`, `pid`, and `cdp` (`http://127.0.0.1:<port>`).
- `DebugRecorder::write_current` writes `ready: false`. `mark_ready` sets `ready: true` after the port listens.
- There is still no `Drop`, no `--debug-status`, and no `--debug-stop`. Confirm that before editing. If plan 001 is not in the tree (no `should_detach`, no `ready` field), STOP. Do not reimplement plan 001 here.

`Cargo.toml` windows features today:

```toml
windows = { version = "0.61", features = ["Win32_Foundation", "Win32_System_Com", "Win32_System_Com_StructuredStorage"] }
```

Process-lifetime checks need `Win32_System_Threading`. Add that feature. Do not bump the crate version.

Early exits in `main` (`src/main.rs`) already handle `--debug-diff`, `--debug-explain`, `--qa-spike`, and `--qa` before the interactive window. `--debug-status` and `--debug-stop` must join that list, ahead of `should_detach`, so a status check does not spawn a browser.

Repo test style: `#[cfg(test)] mod tests` at the bottom of `src/debug.rs`, assertions on strings and JSON, no webview. Match that.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Test | `cmd.exe /c "tools\win-cargo.cmd" test` | exit 0 |
| Build | `cmd.exe /c "tools\win-cargo.cmd" build` | exit 0 |
| Kill a stale lock | `taskkill /IM mini.exe /F` | exit 0, or exit 128 when no such process |

## Scope

**In scope**:

- `src/debug.rs`
- `src/main.rs` (only the new early-exit arms and the call that tombstones on `ControlFlow::Exit` if `Drop` is not enough)
- `Cargo.toml` (add the `Win32_System_Threading` feature only)
- `plans/README.md` (status row only, when done)

**Out of scope**:

- `src/qa/**`, `tools/**`, `AGENTS.md`, `README.md`, `docs/SPEC.md`
- The detach / re-exec logic from plan 001. Call it. Do not rewrite it.
- `--move` and Jev. Plan 003.

## Git workflow

- Branch: `advisor/002-debug-status`
- One commit: `fix(debug): tombstone current.json and add debug-status and debug-stop`
- Do NOT push or open a PR.

## Steps

### Step 1: Tombstone on drop

In `src/debug.rs`:

- Implement `Drop` for `DebugRecorder`. Read `current.json`. If it parses and `pid` equals `std::process::id()`, rewrite the file to the same session fields with `"ready": false` and `"alive": false`, and omit `cdp` (set it to JSON `null`). If the pid does not match, leave the file alone: a newer session owns it.
- `Drop` must not panic. Ignore I/O errors.
- Add `pub fn read_status() -> Option<serde_json::Value>` that reads and parses `current.json`, or returns `None` when the file is missing or invalid.

Unit test: build a status JSON via the plan 001 helper, run it through a pure function `fn tombstone(status: Value, pid: u64) -> Value`, and assert `ready == false`, `alive == false`, `cdp` is null, and `session_dir` is unchanged. A second call with a different pid returns the input unchanged.

**Verify**: `cmd.exe /c "tools\win-cargo.cmd" test debug::` → exit 0, and the output names the tombstone test.

### Step 2: Alive check, status, and stop

Add `Win32_System_Threading` to the `windows` features in `Cargo.toml`.

In `src/debug.rs`, add:

- `pub fn process_alive(pid: u32) -> bool`. `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)`. On failure, return false. On success, `GetExitCodeProcess`; alive means the code equals `259` (`STILL_ACTIVE`). Close the handle.
- `pub fn process_image_is_mini(pid: u32) -> bool`. `OpenProcess` with query rights, `QueryFullProcessImageNameW`, and return true only when the file name (the last path component) is `mini.exe`, case-insensitive. Close the handle. Return false when the process cannot be opened.
- `pub fn status_code() -> i32`. Exit 0 when `read_status()` yields `ready == true`, `alive` is not false, `process_alive(pid)`, `process_image_is_mini(pid)`, and `TcpStream::connect` to the `cdp` host/port succeeds within 1 second. Otherwise print one stderr line saying which check failed (`missing file`, `not ready`, `pid dead`, `pid is not mini.exe`, `port closed`) and return 2.
- `pub fn stop_running() -> i32`. Read the file. If there is no pid, print `mini: no debug browser recorded` and return 0. If `process_alive` is false, call the same tombstone rewrite (pid in the file, not this process — add a `tombstone_pid` that does not require `Drop`) and return 0. If the image is not `mini.exe`, print `mini: pid <pid> is not mini.exe; not killing it` and return 2. Otherwise `OpenProcess(PROCESS_TERMINATE)`, `TerminateProcess` with exit code 0, tombstone the file, return 0.

Wire `src/main.rs` `main`, next to the other flag early-exits, before `should_detach`:

```rust
if args.iter().any(|a| a == "--debug-status") {
    std::process::exit(debug::status_code());
}
if args.iter().any(|a| a == "--debug-stop") {
    std::process::exit(debug::stop_running());
}
```

`--debug-status` and `--debug-stop` must not spawn a window and must not call `should_detach`.

**Verify**: `cmd.exe /c "tools\win-cargo.cmd" build` → exit 0.

### Step 3: Smoke status and stop

```powershell
taskkill /IM mini.exe /F 2>$null
$out = Join-Path $env:TEMP 'mini-status-out.txt'
$err = Join-Path $env:TEMP 'mini-status-err.txt'
$p = Start-Process -FilePath 'target\debug\mini.exe' -ArgumentList '--debug','http://127.0.0.1:9' -RedirectStandardOutput $out -RedirectStandardError $err -PassThru -NoNewWindow
if (-not $p.WaitForExit(25000)) { Stop-Process -Id $p.Id -Force; throw 'parent did not exit' }
& 'target\debug\mini.exe' --debug-status
if ($LASTEXITCODE -ne 0) { throw "status exit $LASTEXITCODE" }
& 'target\debug\mini.exe' --debug-stop
if ($LASTEXITCODE -ne 0) { throw "stop exit $LASTEXITCODE" }
Start-Sleep -Milliseconds 500
& 'target\debug\mini.exe' --debug-status
if ($LASTEXITCODE -eq 0) { throw 'status still healthy after stop' }
```

Expected: first `--debug-status` exits 0, `--debug-stop` exits 0, second `--debug-status` exits 2, and `Get-Process mini -ErrorAction SilentlyContinue` prints nothing.

**Verify**: the block does not `throw`.

## Test plan

- Pure tests in `src/debug.rs`: tombstone keeps `session_dir` and clears `cdp`; tombstone with the wrong pid is a no-op.
- `process_alive` / `process_image_is_mini` need a real process. Do not assert them inside `cargo test` against Mini itself (the test process is not `mini.exe` in a stable way). The step 3 smoke covers them.
- You may unit-test `status_code`'s decision if you extract `fn status_decision(file: Option<Value>, alive: bool, image_ok: bool, port_open: bool) -> i32` and call the Windows APIs only from the wrapper. Test: ready+alive+image+port → 0; each single failure → 2.
- Verification: `cmd.exe /c "tools\win-cargo.cmd" test` → exit 0, plus the step 3 smoke.

## Done criteria

ALL must hold:

- [ ] `cmd.exe /c "tools\win-cargo.cmd" test` exits 0
- [ ] `cmd.exe /c "tools\win-cargo.cmd" build` exits 0
- [ ] Step 3 smoke passes, and no `mini.exe` is left running
- [ ] `--debug-stop` refuses a pid whose image is not `mini.exe` (covered by `process_image_is_mini` used in `stop_running`; the decision test has a false `image_ok` case)
- [ ] No files outside the in-scope list are modified
- [ ] `plans/README.md` status row for 002 is `DONE`

## STOP conditions

Stop and report back (do not improvise) if:

- Plan 001's `ready` field or `should_detach` is not in the tree.
- `OpenProcess` / `GetExitCodeProcess` are not available from `windows` 0.61 with `Win32_System_Threading`. Report the compile error. Do not add another crate.
- `--debug-status` starts opening a window (the early exit is in the wrong place).
- A STOP from plan 001's smoke still reproduces. Fixing detach is not this plan.

## Maintenance notes

- Plan 003's `--move` should call `status_code()` (or the same decision) and exit 2 when it is not 0, before any CDP or Jev call.
- A reviewer should check `Drop` does not clobber a newer session's `current.json` (pid mismatch).
- Killing the process from the outside (`taskkill`) skips `Drop`. `--debug-status` still returns 2 because `process_alive` fails. That is the point of the live check. Do not rely on `Drop` alone.
- Deferred: Jev goal steps, plan 003.

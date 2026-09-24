# Plan 003: Let Jev take goal steps in the live loopback debug window

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 5501909..HEAD -- src/main.rs src/debug.rs src/qa/mod.rs src/qa/agent.rs src/qa/questions.rs src/qa/firewall.rs src/qa/bridge.rs tools/qa-snapshot.js Cargo.toml AGENTS.md README.md`
> Plan 001 and plan 002 are expected to have changed `src/debug.rs` and `src/main.rs`. Stop if `should_detach` or `--debug-status` is missing. Stop if the "Current state" excerpts of the QA files no longer match; those files are not supposed to have changed for 001/002.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: MED
- **Depends on**: plans/001-detach-debug-browser.md, plans/002-debug-status.md
- **Category**: direction
- **Planned at**: commit `5501909`, 2026-09-22

## Why this matters

Jev already chooses every exploration step inside `mini --qa`. `Pass::explore` in `src/qa/agent.rs` calls `questions::explore_question`, then `Judge::answers`, then `jev::validate_choice`, then `mouse_click` / `key_text`. The interactive debug window does none of that. A coding agent attaches with puppeteer and sends its own click. On a page with two submits, that click hits the first one (the search bar), not the submit that belongs to the email and password fields.

`mini --qa` cannot be reused for this. It wipes `%LOCALAPPDATA%\Mini\QA\profile` (`profile::reset_qa_profile`), refuses login URLs (`firewall::is_forbidden_page`), drops controls whose label matches `sign in` / `log in` (`WRITE_VERBS` inside `candidate_elements`), blocks non-GET requests, and applies the click safety gate in `Pass::perform` (`SAFETY_GATE` 0.10). A local sign-in fails all of those on purpose.

After this plan, the coding agent leaves `mini --debug` running (plans 001 and 002) and runs `mini --move` with a goal plus field values. `--move` attaches to the live content CDP port, asks Jev which control serves the goal, and performs that one step with the same CDP input events the QA bridge already uses. Loopback hosts only. `--qa` is unchanged.

## Current state

QA movement (do not call this function from `--move`; copy the shape, not the policy):

```rust
// src/qa/agent.rs, Pass::explore
let mut candidates: Vec<ObservedElement> =
    firewall::candidate_elements(&observation.actions, &self.cfg.host)
        .into_iter()
        .filter(|el| !exercised.contains(&el.node))
        .collect();
let request = questions::explore_question(&goal, &observation, &candidates, &history);
// ask Jev, jev::validate_choice on "operation", then perform()
```

`Pass::perform` refuses a `CLICK` when `mutates_<i>` is missing or `>= 0.10`. `TYPE_TEXT` uses `typing_values(element)`, not operator-supplied text. `--move` must not call `perform`.

`questions::explore_question` (`src/qa/questions.rs`) builds the `/v1/systemone` state. Each element sent to Jev has `index`, `label`, `role`, `value`, aria state, and `operations`. It does not include a form id. The instructions (`QA_NEXT_ACTION`) say to exercise every control and never complete a transaction. The file header says frozen signatures. Add a new function. Do not edit `explore_question`, `QA_NEXT_ACTION`, `CLICK_LABEL`, or the other existing builders.

`firewall::candidate_elements` (`src/qa/firewall.rs`) keeps `click` / `fill` / `select` elements that are visible and not disabled, then drops write-like elements, off-site links, and forbidden hrefs. `WRITE_VERBS` includes `"log in"` and `"sign in"`. Do not change this function.

`ObservedElement` (`src/qa/mod.rs`) already has `name`, `input_type`, `form_method`, `form_action`, all `#[serde(default)]`. It has no `form_id`. Adding a field with `#[serde(default)]` is compatible with old JSON.

The snapshot script (`tools/qa-snapshot.js`, `buildActions`) sets `form_method` and `form_action` from `e.closest('form')` and does not assign a stable form index. `querySelectorAll` does not cross shadow roots. Do not try to fix shadow DOM in this plan.

Input events the QA bridge already sends (`src/qa/bridge.rs`):

```rust
// mouse_click
Input.dispatchMouseEvent { type: mousePressed, x, y, button: "left", buttons: 1, clickCount: 1 }
Input.dispatchMouseEvent { type: mouseReleased, ...same }
// key_text
Input.insertText { text }
```

`--move` speaks CDP to the debug port instead of `UiBridge`. Do not retarget `UiBridge`'s `EventLoopProxy<QaEvent>`. The interactive loop uses `Msg`. A second proxy type inside the bridge is out of scope.

`qa::PAGE_SCRIPTS` (`src/qa/mod.rs`) is the five init scripts in order: `qa-settle.js`, `qa-sections.js`, `qa-snapshot.js`, `qa-act.js`, `qa-probe.js`. They are included in the binary. The interactive content webview does not inject them. `--move` evaluates them over CDP on the already-open page. They guard on `window.__miniQA` / `QA.__settle` / `QA.__dom`, so evaluating twice on one document is safe.

Jev client: `qa::jev::JevClient::new(api_key, model)` implements `Judge`. Missing `TYPESAFE_API_KEY` makes `--qa` exit 2 before a window opens (`src/main.rs` `run_qa`). Use the same env var and the same `qa::DEFAULT_MODEL` / `TYPESAFE_MODEL` override. Never write the key to stdout, stderr, `move.json`, or a test.

Debug port: only the interactive content webview gets `--remote-debugging-port` (`src/main.rs` `content_args`). `GET http://127.0.0.1:<port>/json/list` returns the page targets. Plans 001 and 002 make `mini --debug-status` exit 0 only when that port is live.

`Cargo.toml` dependencies today include `ureq` (HTTP, JSON) and `serde_json`. There is no WebSocket crate. CDP attach needs one. Add `tungstenite` with no extra features and use the blocking `tungstenite::connect`. Do not add `tokio`.

Build/test command and lock rule: `AGENTS.md` "Build/test". Commit style: `feat(debug): ...`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Test | `cmd.exe /c "tools\win-cargo.cmd" test` | exit 0 |
| Build | `cmd.exe /c "tools\win-cargo.cmd" build` | exit 0 |
| Kill a stale lock | `taskkill /IM mini.exe /F` | exit 0, or exit 128 when no process |

A live Jev call is an `#[ignore]` test, same as `qa::jev::jev_live_connects`. Do not require `TYPESAFE_API_KEY` for the default `cargo test`.

## Scope

**In scope**:

- `Cargo.toml` / `Cargo.lock` (the `tungstenite` line, and the lockfile update `cargo` produces)
- `src/main.rs` (early exit for `--move` only)
- `src/cdp.rs` (new)
- `src/move_cmd.rs` (new)
- `src/qa/mod.rs` (`form_id` field only)
- `src/qa/questions.rs` (new `move_question` function only)
- `src/qa/firewall.rs` (new `dev_candidates` function only)
- `tools/qa-snapshot.js` (`form_id` on each action)
- `AGENTS.md` (one new section, "Moving on a local page")
- `README.md` (one short paragraph under "Debug mode")
- `plans/README.md` (status row when done)

**Out of scope**:

- `Pass::explore`, `Pass::perform`, `Pass::act`, `explore_question`, `candidate_elements`, `is_forbidden_page`, `WRITE_VERBS`, `SAFETY_GATE` — QA policy stays
- `src/qa/bridge.rs`, `src/qa/agent.rs`, `src/qa/jev.rs` (call `JevClient`; do not edit it)
- `profile::reset_qa_profile` and the QA profile path
- Arming the Fetch firewall during `--move`. A loopback sign-in POST must reach the local server
- Shadow DOM, file inputs, hover menus, source maps, response-body recording
- Navigating from `--move`. The debug window is already on the page (`mini --debug http://127.0.0.1:3000` or the omnibox)
- `tools/spec-audit-*.js` — one-off scripts, not the product
- Tabs, bookmarks, extensions

## Git workflow

- Branch: `advisor/003-jev-move-loopback`
- One commit: `feat(debug): ask Jev before each loopback move step`
- Do NOT push or open a PR.
- Do not commit a real API key, a filled `goal.json`, or `move.json` from a local run.

## Steps

### Step 1: Form identity on snapshot actions

In `tools/qa-snapshot.js`, inside the `QA.__dom` setup, keep a `WeakMap` from `<form>` element to a 1-based integer, plus a counter. In `buildActions`, when `e.closest('form')` returns a form, assign the next id if the map does not have it, and set `form_id` on `base` to that number. When there is no form, set `form_id` to `null`.

In `ObservedElement` (`src/qa/mod.rs`) add:

```rust
/// 1-based id of the enclosing `<form>`, stable for the page lifetime. `None` when the control is not in a form.
pub form_id: Option<u64>,
```

The struct already has `#[serde(default)]`, so missing `form_id` deserializes as `None`. Do not rename or remove any existing field.

**Verify**: `cmd.exe /c "tools\win-cargo.cmd" test` → exit 0. Existing QA tests that parse snapshot JSON must still pass. If a test asserts the full JSON object equals a fixture that now lacks `form_id`, add `"form_id": null` (or the number) to that fixture. Do not delete assertions.

### Step 2: Dev candidate filter and move question

Add `pub fn dev_candidates(els: &[ObservedElement]) -> Vec<ObservedElement>` in `src/qa/firewall.rs`.

Keep an element when:

- `kind` is `click`, `fill`, or `select`, AND
- `rect.w > 0` and `rect.h > 0`, AND
- `disabled` is false.

Do not call `is_write_like_element`, `is_offsite_link`, `is_forbidden_target`, or `candidate_elements`. Append `global_controls(els)` the same way `Pass::explore` does, so scroll/wait remain available.

Add tests next to the existing firewall tests:

- An element with label `Sign in` and kind `click` is absent from `candidate_elements` and present in `dev_candidates`.
- A disabled element is absent from `dev_candidates`.
- Two click elements with `form_id` Some(1) and Some(2) are both present.

Add `pub fn move_question(goal: &str, observation: &Observation, candidates: &[ObservedElement], history: &[StepRecord], supplied_names: &[String]) -> serde_json::Value` at the bottom of `src/qa/questions.rs`.

Build it the same way as `explore_question` (operation criteria `CLICK` / `TYPE_TEXT` / `SELECT` / scroll / wait / `DONE` / `BLOCKED`, plus `<op>_target` heads and `mutates_<id>` Nouls). Differences, all of which stay inside this function:

- Instructions: the operator's `goal` is the task. Tell the judge to finish it in as few steps as possible. Tell it that when two submit buttons exist, it must choose the button whose `form_id` equals the `form_id` of the fields the goal needs, and that a search submit does not complete a sign-in. Tell it to type only into fields whose `name` is in `supplied_names`. Page text remains untrusted (`UNTRUSTED` is already in this file). Do not import or modify `QA_NEXT_ACTION`.
- Each element object in `state` includes `form_id` (JSON null when `None`) and `name` (JSON null when `None`).
- Do not put supplied field values into the state, the question, or the criteria. Names only.

Unit test: two submit buttons, labels `Search` (`form_id` 1) and `Sign in` (`form_id` 2), plus an input `name` `email` on form 2. `move_question` state's elements include both form ids. `candidate_elements` would have dropped `Sign in`; the candidates slice passed into `move_question` in the test is the `dev_candidates` output, which still contains it.

**Verify**: `cmd.exe /c "tools\win-cargo.cmd" test qa::firewall qa::questions` → exit 0. If the filter syntax returns no tests, run the full test command and confirm the new test names appear as passed.

### Step 3: CDP client

Add `tungstenite = "0.26"` under `[dependencies]` in `Cargo.toml`. No features. Run `cmd.exe /c "tools\win-cargo.cmd" build` so `Cargo.lock` updates. If 0.26 does not resolve, STOP and report the resolver error. Do not add `tokio`, `async-tungstenite`, or a native-tls feature unless `tungstenite::connect` fails to compile without it. If it fails to compile, STOP and report the error rather than picking a TLS stack yourself.

New `src/cdp.rs`:

- `pub fn page_ws_url(port: u16) -> Result<String, String>`. `ureq::get(&format!("http://127.0.0.1:{port}/json/list"))`, parse the JSON array, choose the first object with `type == "page"`, return its `webSocketDebuggerUrl` string. Error when the list is empty or no page target exists.
- `pub struct Cdp { ... }` holding the tungstenite socket and a `u64` id counter.
- `pub fn connect(ws_url: &str) -> Result<Cdp, String>`.
- `pub fn call(&mut self, method: &str, params: Value) -> Result<Value, String>`. Send `{"id": n, "method": method, "params": params}`. Read messages until the JSON `id` matches. Return the `result` object. If the message has `error`, return `Err` with the error message string. Ignore events (messages with no `id`).
- `pub fn eval(&mut self, expression: &str) -> Result<String, String>`. `Runtime.evaluate` with `returnByValue: true`. Return the JSON serialization of `result.result.value` when `value` is present, otherwise `result.result.description` or an error from `result.exceptionDetails`. Match the QA bridge's habit of handing the caller the raw text `probes::observation` already parses.
- `mouse_click` and `key_text` send the same CDP payloads as `UiBridge::mouse_click` and `UiBridge::key_text` in `src/qa/bridge.rs` (quoted in Current state). Prefer `Input.insertText`. On error, fall back to per-character `keyDown` / `keyUp` the same way `key_text` does.

`mod cdp;` in `src/main.rs`.

No live browser in `cargo test`. Unit-test only a pure helper if you extract one (for example picking the page target from a JSON array: first `type=="page"`, error on empty). The smoke in step 5 covers the socket.

**Verify**: `cmd.exe /c "tools\win-cargo.cmd" build` → exit 0.

### Step 4: `mini --move`

New `src/move_cmd.rs`. `mod move_cmd;` in `src/main.rs`.

CLI, parsed in this file, early-exit from `main` before `should_detach`:

```text
mini --move --goal "<text>" [--steps N] [--value <name> ...]
```

Also accept a single JSON object on stdin when stdin is not a terminal and the first bytes are `{`. Shape: `{"goal": "...", "steps": 8, "values": {"email": "...", "password": "..."}}`. If both stdin and `--goal` exist, the goal string from stdin wins when it is non-empty. `--value name` without `=` is an error (exit 2). `--value name=value` may be repeated. Names are matched later against `ObservedElement.name`, then `input_type`, then the label head (text before ` → `).

Value handling:

- Keep values only in memory.
- Never print them, log them, or write them to `move.json`.
- `TYPE_TEXT` looks up a value for the chosen element. If none was supplied, skip the keystrokes, record a note `no value supplied for <name>`, and continue. Do not call `typing_values`. Do not invent an email or a password.

Preconditions, in order, each failure exits 2 with one stderr line and no Jev call:

1. `debug::status_code()` is not 0. Print `mini --move: debug browser is not up (`mini --debug-status` failed)`. (Calling `status_code` prints its own reason; that is fine.)
2. `TYPESAFE_API_KEY` is missing or empty. Same sentence `run_qa` already prints.
3. CDP `page_ws_url` / `connect` fails.
4. After injecting scripts and evaluating `window.__miniQA.snapshot(null)`, the observation URL's host is not `localhost`, `127.0.0.1`, or `::1` (use `crate::qa::firewall::host_only`, strip a trailing port the same way that function already does). Print `mini --move: <host> is not loopback; use mini --qa for non-local pages`.
5. The URL is empty or starts with `data:` or `about:`. Print `mini --move: navigate the debug window to the local page first`.

Injection: for each script in `qa::PAGE_SCRIPTS`, `Runtime.evaluate` the source. Then `Page.addScriptToEvaluateOnNewDocument` with the same sources so a navigation during the goal keeps the helpers. Ignore a second-add error if the method is rejected; the evaluate path is the one that matters for the current document.

Loop, default 8 steps, clamp 1..=20, wall clock 60 seconds:

1. `snapshot(null)`. Parse with `probes::observation`.
2. `dev_candidates`.
3. `move_question` with the goal, observation, candidates, history, and the supplied value names (not the values).
4. `JevClient::answers` with `model`, `state`, `questions`, the same body shape as `Pass::ask`.
5. `jev::validate_choice` on `operation`. On `DONE`, stop success. On `BLOCKED`, stop failure. On a rejected answer, try one fresh `answers` call, then stop failure.
6. Resolve the target the same way `Pass::perform` does (`<op>_target`, `validate_choice`, match the candidate) but do not read `mutates_*` and do not skip the click. The safety gate stays inside `Pass::perform` for `--qa` only.
7. Perform:
   - `CLICK`: `eval` of `window.__miniQA.hitTest(<id>)`, parse with `probes::hit_test`. If `ok` and `topmost` are not both true, record the reason and do not click. Otherwise `mouse_click` at `hit.x`, `hit.y`.
   - `TYPE_TEXT`: `window.__miniQA.act('focus', <id>)`, then `key_text` with the supplied value.
   - `SELECT`: `window.__miniQA.act('select', <id>)`.
   - `SCROLL_DOWN` / `SCROLL_UP` / `WAIT`: the matching `window.__miniQA.act(...)` call used in `Pass::act`.
8. Eval `window.__miniQA.settleState()` once after the action (do not port the full QA settle poll). Snapshot again. `page_changed` is true when `probes` observation `marker` or the action id list differs. You can compare `observation.marker` and the `id`s of `actions`. Do not import private `Pass` helpers; duplicate the small compare in `move_cmd.rs`.

Stop the loop on `DONE`, `BLOCKED`, step budget, wall clock, or three steps in a row with `page_changed == false` and no refusal.

Write `%LOCALAPPDATA%\Mini\Debug\move.json` (pretty or compact, one object) and print the same object as one line on stdout:

```json
{
  "ok": true,
  "stop": "the judge reported the goal is done",
  "url_before": "http://127.0.0.1:3000/login",
  "url_after": "http://127.0.0.1:3000/",
  "steps": [
    {"step": 1, "operation": "TYPE_TEXT", "target": "Email", "name": "email", "confidence": 0.9, "page_changed": true, "note": null}
  ]
}
```

`ok` is true only when the stop reason is the judge's `DONE`. Budget exhausted, `BLOCKED`, a rejected answer, and three no-change steps are `ok: false`. Exit code: 0 when `ok`, 1 when the loop ran but `ok` is false, 2 when a precondition failed. Do not include typed text in `steps`.

`main` arm, before `should_detach`:

```rust
if let Some(i) = args.iter().position(|a| a == "--move") {
    std::process::exit(move_cmd::run(&args, i));
}
```

**Verify**: `cmd.exe /c "tools\win-cargo.cmd" test` → exit 0, and `cmd.exe /c "tools\win-cargo.cmd" build` → exit 0.

### Step 5: Document the command and smoke it without Jev

`AGENTS.md`: add a section "Moving on a local page" after the debug-mode steps. State:

- Launch with `mini --debug` (plan 001's parent exits when the port is up; the child stays).
- Check with `mini --debug-status`. Stop with `mini --debug-stop`.
- To act on the page, run `mini --move`. Do not `page.click` the first submit. Pass the goal and field values on stdin as a JSON object so secrets are not in the command line. `--move` refuses a non-loopback host.
- Reading JSONL, `--debug-explain`, and `--debug-diff` stay as they are.

`README.md`, under "Debug mode (for agents)", add three lines: `--move` asks Jev which control to use on the page already open in that debug window; it only runs against localhost; it needs `TYPESAFE_API_KEY`.

Smoke, no Jev key required for the refusal path:

```powershell
taskkill /IM mini.exe /F 2>$null
$env:TYPESAFE_API_KEY = ''
& 'target\debug\mini.exe' --move --goal 'sign in'
if ($LASTEXITCODE -ne 2) { throw "expected exit 2 when no debug browser, got $LASTEXITCODE" }
```

Then start a debug browser on `http://127.0.0.1:9` the way plan 001's smoke does, and run:

```powershell
$env:TYPESAFE_API_KEY = ''
& 'target\debug\mini.exe' --move --goal 'sign in'
# exit 2, stderr mentions TYPESAFE_API_KEY
& 'target\debug\mini.exe' --debug-stop
```

Expected: both `--move` invocations exit 2. The first fails because no browser is up. The second fails because the key is empty, and it must fail before any click (the page is port 9; there is nothing to sign in to). Confirm stderr for the second run contains `TYPESAFE_API_KEY` and does not contain a CDP click attempt. Then `--debug-stop` leaves no `mini.exe`.

Do not point this smoke at a real application login. Do not put a password in the command.

**Verify**: both invocations exit 2, `Get-Process mini -ErrorAction SilentlyContinue` prints nothing, `git status` does not show `move.json` or a goal file.

## Test plan

- `src/qa/firewall.rs` tests: `Sign in` survives `dev_candidates` and is removed by `candidate_elements`; disabled controls are dropped; distinct `form_id`s both pass.
- `src/qa/questions.rs` tests: `move_question` state contains `form_id` 1 and 2 and the supplied name `email`, and does not contain a sample password string. Assert the serialized JSON of the question does not contain the characters of a value you deliberately pass only as a value (the function never receives values; the test documents that by taking names `&["email"]` and asserting the string `s3cr3t` is absent).
- `src/cdp.rs` or `src/move_cmd.rs` tests: page-target picker; value lookup by `name`, then `input_type`, then label; `move.json` serialization omits the value; loopback host check accepts `127.0.0.1`, `localhost`, `::1` and rejects `example.com`.
- Pattern: existing `#[cfg(test)]` modules in `src/qa/firewall.rs` and `src/debug.rs`. No webview. No live Jev call in the default test run.
- Verification: `cmd.exe /c "tools\win-cargo.cmd" test` → exit 0, including the new tests, plus the step 5 smoke.

## Done criteria

ALL must hold:

- [ ] `cmd.exe /c "tools\win-cargo.cmd" test` exits 0
- [ ] `cmd.exe /c "tools\win-cargo.cmd" build` exits 0
- [ ] `candidate_elements` still drops a `Sign in` control (the new firewall test fails if QA policy was loosened)
- [ ] `--move` does not call `Pass::perform`, `Pass::explore`, or `firewall::candidate_elements` (grep `src/move_cmd.rs` for those names returns no matches)
- [ ] Step 5 smoke: `--move` exits 2 with no browser, and exits 2 with an empty `TYPESAFE_API_KEY` without clicking
- [ ] `git grep -n "TYPESAFE_API_KEY" src/move_cmd.rs` shows only the env read and the error string, never a literal key
- [ ] No files outside the in-scope list are modified
- [ ] `plans/README.md` status row for 003 is `DONE`

## STOP conditions

Stop and report back (do not improvise) if:

- Plan 001 or 002 is not in the tree (`should_detach`, `--debug-status` missing).
- The only way to drive the page is to change `UiBridge` to post `Msg` instead of `QaEvent`. That refactor is not this plan. `--move` uses the debug CDP port.
- `tungstenite` 0.26 does not resolve or `tungstenite::connect` does not compile on this toolchain. Report the error.
- `probes::observation` rejects snapshots that include `form_id`. Report the serde error. Do not strip `form_id` back out.
- A step seems to require weakening `candidate_elements`, `is_forbidden_page`, or `SAFETY_GATE` so that `--qa` will click `Sign in`. Do not. `--move` has its own candidate list.
- The debug `/json/list` response has no `type: page` target after plan 001's smoke has `ready: true`. Report one redacted target object (type and url only).

## Maintenance notes

- Future QA snapshot fields can follow `form_id`: optional, `#[serde(default)]`, ignored by the QA loop until a question builder reads them.
- A reviewer should check `--qa` tests still drop write-like controls, and that `move.json` / stdout cannot contain the operator's password (the step record has `name`, not `text`).
- `--move` on a loopback host can submit a login and change the local database. That is the point of the dev command. It must keep refusing non-loopback hosts.
- The click safety gate and the login blocklist remain the `--qa` contract. If a later change shares one `perform` function between QA and `--move`, the gate has to stay on the QA call path.
- Deferred on purpose: shadow DOM, file inputs, navigating from `--move`, response bodies, source maps, and any change to the QA write firewall.

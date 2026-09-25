# QA runs: `mini --qa <url>`

One-shot self-driving QA pass: three viewports, geometry/text facts computed in
code, Jev (api.typesafe.ai) for severity and claims, and a report that makes an
"it's done" claim falsifiable.

```
target\debug\mini.exe --qa <url> [--section "<name>"] [--claim "<text>"]... [--out <dir>] [--steps N] [--allow-writes]
```

- Exit 0 clean · 1 findings at severity ≥ 2 or an unverified claim · 2 could not
  run. `report.md` is printed to stdout; the full artefact lands in
  `%LOCALAPPDATA%\Mini\QA\<stamp>-<host>\`: `report.md`, `report.json`, `shots/`
  (per viewport + per exploration step), and `runs/run-NNN-<viewport>/` holding
  the seven JSONL streams plus `analysis.json`. Every evidence path in the
  report resolves inside that directory.
- Writes are refused by default, in three layers: every non-GET/HEAD/OPTIONS
  request fails at the network layer (`Fetch.failRequest`); submit buttons,
  post-form descendants, write-verb labels and offsite links are never offered
  to the model; and a CLICK runs only when the model's own `mutates_*` Noul puts
  the chance of changing server data below 0.10. `--allow-writes` is for a
  disposable local database — it is refused on a `*.vercel.app` host unless you
  also pass `--i-know-this-is-prod`.
- Needs a Jev key: `TYPESAFE_API_KEY` in the environment, else the first
  `TYPESAFE_API_KEY=` line in `%LOCALAPPDATA%\Mini\.env` or
  `<Desktop>\jev-ultrafast\.env` (`qa::jev::api_key`). Never print it. A missing
  key exits 2 before any window opens. No CDP port is opened, and no new content IPC exists — the page
  is driven host→page through `window.__miniQA.*` (injected from `tools/qa-*.js`)
  and in-process WebView2 CDP.
- One engine only (WebView2/Chromium): visual-only regressions and non-Chromium
  differences are out of scope. Screenshots are attached so a human can look.
- Diagnostic: `--qa-spike <url>` exercises the five transport capabilities
  (eval, viewport, screenshot, click, firewall) and prints one JSON line each.

# Debugging Mini pages: use the built-in debug mode, not Playwright

Mini is a WebView2 (wry) browser. Playwright cannot launch it, and spawning a
separate Chromium to reproduce a page bug loses everything the recorder
captures. For any in-page investigation (console errors, DOM flicker, React
remounts, network/stream causation), MUST use Mini's own debug mode:

1. **Launch:** `target\debug\mini.exe --debug` (or set `MINI_DEBUG=1`).
   Fail-safe: if recording setup fails the browser still runs, just unrecorded.
2. **Discover:** read `%LOCALAPPDATA%\Mini\Debug\current.json` →
   `{cdp, session_dir, active_run_dir, run_label, run_index, pid, started_ms}`.
3. **Attach:** puppeteer-core is vendored at `tools\node_modules` (fallback:
   `%TEMP%\mini-cdp`). Scripts in `tools\` resolve it with a plain
   `require('puppeteer-core')`; drivers elsewhere use
   `require('C:\\Users\\USUARIO\\Desktop\\mini-browser\\tools\\node_modules\\puppeteer-core')`.
   then `puppeteer.connect({ browserURL: current.cdp })`. Do NOT
   `puppeteer.launch` or use Playwright — the recorder script is only injected
   into Mini's own content webview. Run driver scripts from the repo root with
   a relative path (`node tools\x.js`); absolute Windows paths get mangled by
   the MSYS shell.
4. **Runs are the comparable unit.** Before each reproduction call
   `page.evaluate(() => window.__miniDebug.run('<label>', '<incident>'))`.
   This rotates `active_run_dir` to `run-NNN-<label>/` with six streams:
   `frontend.jsonl` (console/errors), `dom.jsonl` (per-event `ts` + rects),
   `net.jsonl` (fetch/xhr/ws/sse), `react.jsonl` (mount/unmount),
   `frames.jsonl` (rAF paint timeline), `backend.jsonl`. Env knobs for the
   initial run: `MINI_DEBUG_LABEL`, `MINI_DEBUG_INCIDENT`.
5. **Analyze offline — do not eyeball JSONL:**
   - `mini.exe --debug-explain <run_dir>` → `analysis.json`/`analysis.txt`
     (flicker dwell + within-one-frame, remounts with net cause, layout Δ).
   - `mini.exe --debug-diff <run_a> <run_b>` → `diff.json`/`diff.txt` in run B
     (faulty-vs-fixed deltas: backend/console/net/remounts/flicker).
6. **Visual evidence (optional):** `node tools\screencast.js` writes
   `<active_run_dir>\frames\<seq>.jpg` + `screencast.jsonl`; stop with SIGINT.
   Images are supplementary; the JSONL timeline is the proof.

# Moving on a page: `mini --move` (Jev picks the control)

1. Launch `target\debug\mini.exe --debug <url>`. The parent exits once CDP
   listens; the window stays. `--debug-status` (exit 0 = usable) and
   `--debug-stop` manage it.
2. `mini --observe` prints `{url, title, text, elements[]}` (ids, labels,
   `name`, `form_id`) with no model call. Use it to choose field names.
3. To act, run `mini --move`. Do NOT `page.click` the first submit — on a
   page with a search box and a login form that hits the wrong form. Pass the
   goal and values as stdin JSON (`{"goal", "values": {name: value}, "steps",
   "url"}`) so secrets stay off the command line. Values match on the field's
   `name`, then input type, then label. Password fields are offered only in
   `--move`; their contents are never read.
4. Result: one JSON line on stdout, same object in
   `%LOCALAPPDATA%\Mini\Debug\move.json` (`ok`, `stop`, `steps[]` with the
   field NAME typed, never the value, plus `episode_id` for the log in 6).
   `ok` is the model's DONE — verify.
5. Loopback only unless `--allow-remote`. `--qa` policy is unchanged and is
   not used by `--move`.
6. Every run is also appended to the episode log
   `%LOCALAPPDATA%\Mini\episodes\<host>.jsonl` (host without `www.`, one
   `Episode` JSON per line, append only; the newest id is in
   `episodes\last.json`), with a reward computed in code, never by the model.
   `steps[].page_sig` is the page fingerprint of the page the step started on
   (`<host><path-pattern>#<hash8>`): same page across runs, digits/prices/
   showtimes ignored. Logging is best effort — a failure prints one line to
   stderr and leaves the run's exit code alone.
7. Label the episode once you have read the result; the operator is the
   reward signal. Neither command opens a window or calls Jev:
   - `mini --feedback good|bad ["reason"] [--id ID]` → the updated episode
     JSON, exit 0; bad input exit 2. No `--id` = the newest episode.
   - `mini --episodes [host]` → the summary JSON (`episodes`, `ok_rate`,
     `mean_final_reward`, `mean_judge_calls`, `mean_elapsed_ms`, `actions[]`),
     every host when none is named.
8. Live E2E without Jev: serve `tools\fixtures` on :8000, launch
   `mini --debug http://127.0.0.1:8000/login.html`, then
   `cmd.exe /c "tools\win-cargo.cmd" test live_move -- --ignored`.

Navigation: `file:` URLs are blocked; serve fixtures over
`http://127.0.0.1:8000` (`python -m http.server` in `tools\fixtures`) or use a
`data:text/html` URL.

Build/test: `cargo` is not on PATH — use the repo wrapper, which sets up the VS
2022 Build Tools environment and puts rustup's `bin` on PATH so build scripts
can find a bare `rustc`:

```powershell
cmd.exe /c "tools\win-cargo.cmd" build
cmd.exe /c "tools\win-cargo.cmd" test
```

Rebuild before relaunching — a running `mini.exe` holds the target file lock and
`cargo build` fails with os error 5 (kill stale `mini.exe` first). `cargo test`
must pass after any change to `src/debug.rs`, `src/analyze.rs`, `src/qa/**`, or
any of the injected JS (`DEBUG_RECORDER_SCRIPT`, `tools/qa-*.js`).

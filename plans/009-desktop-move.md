# 009 - Desktop move: Jev picks the control in native apps (`mini --desk`)

Status: TODO. Depends on 008 (one evidence gate). Owner files: new `src/desk.rs`, `src/main.rs`
(flag only), reuse `src/qa/questions.rs::move_question`, `src/move_cmd.rs` (decide, goal gate,
LoopGuard), `src/episodes.rs`.

## Why
Mini already makes good web decisions: Jev picks each control, a goal check gates `ok`, episodes
give rewards and priors. Hermes `computer_use` has none of that: the chat model guesses each
click from a screenshot. The same Jev loop must drive native Windows apps.

## Proven (spike 2026-09-24, Hermes session)
- `cua-driver call <tool> '<json>'` (v0.28.3, daemon on `\\.\pipe\cua-driver`) works from a
  subprocess. `launch_app {"name":"notepad"}` (no focus steal) 7 s; `list_windows` gives pid +
  window_id; `get_window_state {pid, window_id, include_screenshot:false}` returned 65 elements
  in ~1 s. Each element: `element_index, role, label, value, enabled, selected, actions`
  (`invoke|select|set_value|text|scroll|toggle|expand`), `frame {x,y,w,h}` (null when
  off-screen), `parent_index, depth`.
- The structured side maps 1:1 onto `ObservedElement`: role -> role, label -> label,
  value -> value, frame -> rect, element_index -> node. kind: `invoke|toggle|expand|select`
  -> click, `set_value|text` on Edit/Document/ComboBox-edit -> fill, ComboBox/List with
  items -> select, `scroll` -> scroll.
- DANGER found: Notepad tab titles (restored unsaved notes) exposed parts of API keys and passwords
  in UIA labels. Desktop trees carry secrets web pages rarely do.

## Required changes
A. Transport `DeskPage` (subprocess to `cua-driver call`, JSON in/out, 30 s timeout):
   observe(pid, window_id) -> Observation {url: "desk://<app>/<window title>", title, text
   (joined Text/Document labels, capped), elements}. Act: click -> `click {pid, window_id,
   element_index}`; fill -> `set_value` (fallback `type_text`); select -> `click` on the item;
   PRESS_ENTER -> `press_key return`; scroll -> `scroll`. Re-observe after every action (indices
   die with each snapshot - cua-driver invariant).
B. Redaction firewall BEFORE any Jev request, the move.json, and the episode log (pure fn + tests):
   mask tokens that look like secrets (sk-, sk_live, AIza, ghp_, xox, JWT eyJ.., 20+ char
   base64/hex runs, `password|pass|key|token|secret` followed by a value) -> `[redacted]`.
   Drop every element whose role is a password field (UIA IsPassword) entirely.
   Test with fixture labels shaped like the spike ones (fake values only).
C. Loop: reuse move_question + decide + done_has_evidence + goal_threshold + LoopGuard
   unchanged. Only the Page layer differs. If that forces a trait split, add
   `trait Surface { observe, act }` with `CdpSurface` and `DeskSurface`; do not fork the loop.
D. Safety (defaults, same spirit as --qa): refuse windows of password managers, banking, and the
   terminal running Hermes; refuse controls labeled delete/send/pay/purchase/sign/submit order
   unless `--allow-writes`; never type into a field the operator did not supply a value for
   unless `--text-helper`. Desktop input goes background first (cua-driver default).
E. CLI: `mini --desk --app <name|pid> [--window <title substring>] --goal "..." [--steps N]
   [--value name=value] [--allow-writes]`. Also `mini --desk-observe --app X` (no Jev call).
   Same one-line JSON report as --move (`ok, stop, steps[], title, page_text`), written to
   `%LOCALAPPDATA%\Mini\Debug\desk.json`. Exit 0/1/2 like --move.
F. Episodes: log to `episodes/desk-<app>.jsonl`, same reward code, so priors and replay work per app.
   Hints: `sites/desk-<app>.md` is sent as site_notes (the lesson loop works unchanged).

## Tests (keep all existing tests green)
- Unit: UIA element -> ObservedElement mapping (fixture JSON copied from a SAFE app, e.g.
  Calculator/Settings, never from Notepad sessions).
- Unit: redaction fixtures; password-field drop; refused-window list.
- Loop test with fake judge + fake DeskPage: goal met after 2 clicks; DONE without evidence rejected.
- Live check (manual, safe app only): Calculator "compute 12 x 7, stop when the display shows 84";
  Settings "open Bluetooth & devices page". Report ok, steps, Jev calls, ms.

## STOP conditions
- Never run live checks on Notepad, a browser with personal tabs, mail, banking, or any window
  whose title matches the refuse list.
- If cua-driver `call` changes its JSON shape, stop and report; do not scrape the markdown tree.

Final line: `DONE <branch> <sha> <N tests passed>` or `BLOCKED: <reason>`.

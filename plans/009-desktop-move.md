# 009 - Desktop move: Jev picks the control in native apps (`mini --desk`)

Status: DONE. Merged desk/core + desk/loop; 278 tests. Live: Calculator 7x12 ok (5 Jev clicks, 17 s), Settings Bluetooth ok, Notepad and PowerShell refused (exit 2, nothing printed).
Baseline on main before this plan: 254 passed, 2 ignored (`tools\win-cargo.cmd test`).

## Why
Mini makes good web decisions: Jev picks each control, one evidence gate decides `ok`, episodes
give rewards, priors and replay. Hermes `computer_use` has none of that; the chat model guesses
each click from a screenshot. `mini --desk` runs the same loop on native Windows apps.

## Proven facts (spike 2026-09-24, cua-driver 0.28.3, Calculator)
1. Use ONE long-lived `cua-driver mcp` stdio process (JSON-RPC 2.0, one JSON object per line).
   The one-shot `cua-driver call` CLI returns tokens that are already stale on the next call.
   Handshake: `{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"mini","version":"<pkg>"}}}`
   then `{"jsonrpc":"2.0","method":"notifications/initialized"}`. Tools: `tools/call` with
   `{"name":..., "arguments":{...}}`; read `result.structuredContent` (fall back to `result`).
   Skip notification lines (no `id`) while waiting for a reply.
2. `get_window_state {pid, window_id, include_screenshot:false}` -> ~150 ms, keys: `snapshot_id`,
   `window_title`, `app_name`, `window_bounds`, `elements[]`, `truncated`. Element keys:
   `element_index, element_token ("s00000002:0"), role, label, value, enabled, selected,
   actions[], frame {x,y,w,h} | null, depth, parent_index`.
3. Actions MUST pass `element_token` (a bare `element_index` is refused `snapshot_id_required`).
   A token from an older snapshot is refused `stale_element_token`: re-observe, then retry once.
4. `click {pid, window_id, element_token}` -> `{"delivery":{"mode":"background"},"effect":
   "unverifiable","route":"accessibility","summary":"..."}` in 0.2-3 s. Clear,7,x,1,2,= gave
   "Display is 84". Refusals: `{"status":"refused","refusal":{"code","message"}}`.
5. `set_value {pid, window_id, element_token, value}`, `type_text {pid, window_id, element_token, text}`,
   `press_key {pid, window_id, key}`, `scroll {pid, window_id, direction, amount, element_token?}`,
   `launch_app {name}`, `list_windows {}` -> `_legacy_windows[] {pid, window_id, title, is_on_screen, minimized}`.
6. Fixtures: `tests/fixtures/desk/calculator_state.json` (before), `calculator_after_7x12.json`
   (display "Display is 84"). Calculator: 32 Button, 5 Text, 2 TabItem, 1 MenuItem, 1 Window.
7. DANGER: Notepad's UIA tree exposed API keys and passwords in tab labels. Never observe
   Notepad or editors in tests or live checks.

## Contract (src/desk/mod.rs, committed stub - do not change shapes; raise gaps in your pane)
`WindowRef`, `DeskAction`, `ActResult`, `trait DeskSurface { observe, act, window }`.
ObservedElement mapping (map.rs), every field's source:
- `node` = element_index; `id` = element_index as string; `label` = redact(label), else
  redact(value) when label is empty; `role` = role; `value` = redact(value);
  `rect` = frame (skip element when frame is null or w*h == 0); `disabled` = !enabled;
  `selected` = "true"/"false" from selected when present.
- `kind`: actions contains invoke|toggle|expand -> "click"; TabItem/ListItem/MenuItem with
  select -> "click"; set_value on Edit/Document/ComboBox -> "fill" (input_type "text");
  role Window, Pane, ScrollBar, Text, Group, TitleBar -> no action (text only);
  anything else -> no action. `path` = "desk:" + element_token (tokens map only, never logged).
- Secret fields (redact::is_secret_field) are removed entirely, value never read.
- Observation: `url` = "desk://" + lowercase app + "/" + redact(window_title) (spaces -> "-");
  `title` = redact(window_title); `text` = redacted labels+values of Text/Document/Edit
  elements joined by "\n", capped 8000 chars; `w,h` = window_bounds; `marker` =
  JSON array of [label,value] of all kept elements (page_changed compares it).
Redaction (redact.rs), pure, unit-tested with FAKE values only:
- mask: `sk-`/`sk_live_`/`sk_test_`/`rk_live_`/`pk_live_` + 8+ chars, `AIza` + 20+, `ghp_`/`gho_`/`github_pat_`,
  `xox[abpr]-`, `eyJ` JWT (3 dot parts), `AKIA` + 16, any 24+ char run of [A-Za-z0-9_-]
  containing both letters and digits, and the word after `password|passwd|pass|pwd|secret|token|api key|apikey|key` + `:`/`=`/space.
- is_secret_field: label/role mentions password/contraseña/PIN/CVV/secret.
- refused_window (app or title, case-insensitive): notepad, bloc de notas, 1password, bitwarden,
  keepass, lastpass, "bank"/"banco", outlook, mail, whatsapp, telegram, "hermes", windows terminal,
  powershell, cmd.exe, herdr, and any browser (chrome, edge, firefox, brave) -> web goes to `--move`.
- is_write_label: delete, remove, send, pay, purchase, buy, sign, submit, publish, post, transfer,
  format, uninstall, eliminar, borrar, enviar, pagar, comprar, firmar, publicar.

Loop (run.rs): reuse, do not fork, move_cmd's `questions::move_question`, `decide`,
`done_has_evidence`, `goal_threshold`, `LoopGuard`, `episodes::*`, `site_notes`. Make those
`pub(crate)` in move_cmd.rs (visibility only, no logic change). Per step: observe -> drop
write-label controls unless `--allow-writes` -> move_question (text helper OFF: TYPE_TEXT only
for `--value` names) -> decide -> act -> re-observe -> page_changed -> record. A `refused`
stale token: re-observe once and retry the same label. `ok` only through done_has_evidence.
Report: `MoveReport` (url_before/url_after = desk:// urls), printed as one JSON line and written
to `%LOCALAPPDATA%\Mini\Debug\desk.json`. Episodes: host = "desk-" + lowercase app.
Hints: `sites/desk-<app>.md` via site_notes. CLI:
`mini --desk --app <name> [--window <title part>] [--launch] --goal "..." [--steps N (default 15, max 40)]
[--value name=value]... [--allow-writes] [--hints DIR]` and `mini --desk-observe --app X [--window T]`.
Exit 0/1/2 like --move.

## Workers
| Worker | Branch | Owns (may write) | Must not touch |
|---|---|---|---|
| desk-core | desk/core | src/desk/client.rs, map.rs, redact.rs, tests for them | mod.rs, run.rs, main.rs, move_cmd.rs |
| desk-loop | desk/loop | src/desk/run.rs, src/main.rs (flag dispatch), src/move_cmd.rs (pub(crate) only), AGENTS.md section, README line | client.rs, map.rs, redact.rs, mod.rs |
desk-loop tests with a FakeSurface built from the fixtures + a fake Judge (move_cmd tests have one).

## Done means
- `tools\win-cargo.cmd test` green on the merge: 254 + new tests, 0 failed.
- Live (orchestrator only): `mini --desk --app Calculator --launch --goal "Compute 7 times 12. Stop when the display shows 84."`
  exits 0, `desk.json` steps are Jev-chosen, `page_text` contains "Display is 84".
  `mini --desk-observe --app Notepad` refuses with exit 2 and prints no element.

## STOP (print `BLOCKED <reason>`)
- A contract shape is missing a value you need: say which field; do not invent one.
- Workers never run cua-driver, mini.exe live, or any network call; the orchestrator verifies live.

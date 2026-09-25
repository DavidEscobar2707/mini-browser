# 007 - Search reliability on non-agent-first sites + no scroll oscillation

Status: IN PROGRESS. Workers: `p7-afford`, `p7-loop` (herdr, OMP, deepseek-v4.1-flash). Depends on 006.

## Evidence
Benchmark `tools/bench/run_search.py` (12 real sites; a run passes only if the query is in the final URL).
Baseline 2026-09-24: PASS 7/12 (the model said ok on 8/12: 1 false DONE).

| site | failure | first wrong step / cause |
|---|---|---|
| exito | BLOCKED, 0 steps | the search box is a fake textbox (`kind=click`, role textbox, label "Open search"/"search") that opens an overlay; Jev said nothing could progress |
| ktronix | BLOCKED, 0 steps | the same pattern: "¿Qué buscas hoy?" is a click-only textbox |
| alkosto | pass, but 8 calls, 2 no-change clicks | the same pattern (fake textbox -> overlay) |
| homecenter | exit 2 before any step | `--url` navigation: `wait_for_load` 20 s timeout is fatal ("page did not finish loading") on a heavy page that is usable |
| booking | DONE disputed 3x | TYPE_TEXT then PRESS_ENTER refused "no text field has focus" (the autocomplete stole focus); the destination needs a suggestion to be picked |
| rottentomatoes | FALSE ok | DONE was accepted with no passing goal check; the URL never changed |
| (David, all sites) | scroll oscillation | Mini itself runs SCROLL_THROUGH (bottom, then `scrollTo(0,0)`) on every new URL, on reloads, and again for the report -> down/up/down |
| (006) cinemark replay | 2/3 replays broke | exact label match; drop-down labels contain the current selection |
| (006) learn | WAIT listed as "helps" | the return leaks backward to WAIT/SCROLL steps |

## Contract: new `src/affordance.rs` (stub on main; p7-afford implements)
```
pub fn is_search_like(el: &ObservedElement) -> bool
pub fn is_fake_search_trigger(el: &ObservedElement) -> bool
pub fn goal_wants_search(goal: &str) -> bool
pub fn label_similarity(a: &str, b: &str) -> f64
pub fn needs_scroll_through(visible_text: &str, full_text: &str, goal_met: Option<f64>) -> bool
```
- `is_search_like`: role in {searchbox, combobox, textbox} or input_type == "search", OR label/name
  (lowercased, accents removed) contains one of: search, buscar, busca, buscas, find, "qué buscas",
  "que buscas", lupa, "search amazon" etc. (token match on search|buscar|busca|buscas|find|query|lupa).
  Must be false for plain links whose label is only a category name.
- `is_fake_search_trigger`: is_search_like AND kind == "click" AND role in {textbox, searchbox, combobox}
  OR (kind=="click" && role=="button" && label matches /open search|abrir b[uú]squeda|buscar/).
- `goal_wants_search`: goal (lowercased, accents removed) contains search|busca|buscar|find|look up.
- `label_similarity`: token Jaccard on lowercased, accent-free alphanumeric tokens, with tokens "icon",
  "down", "up", "button" dropped; 1.0 for equal strings; 0.0 if either is empty.
- `needs_scroll_through`: true only if (a) the visible text contains a loading word line
  (cargando/loading, same set as move_cmd::drop_loading_lines) OR (b) full_text is less than 1.2x
  visible_text in length (so lazy content likely did not render) AND goal_met is below 0.5 or None.
  Otherwise false.
- Tests with hand-made ObservedElement values for: Exito ("search" fill; "Open search" click textbox),
  Ktronix ("¿Qué buscas hoy?" click textbox), Amazon ("Search Amazon" fill), a category link "Celulares" (false),
  and similarity: "Seleccionar Teatro Todos los teatros icon: down" vs "Seleccionar Teatro Arkadia icon: down" >= 0.4.

p7-afford also owns `src/learn.rs`: exclude WAIT, SCROLL_DOWN, SCROLL_UP from the "helps" bullets
(they can still be "avoid"). Add a test.

## p7-loop (owns `src/move_cmd.rs`, `src/qa/questions.rs`, `src/cdp.rs`, `tools/qa-act.js`)
1. No oscillation: replace the automatic `load_whole_page` on the first visit of a URL with
   `if affordance::needs_scroll_through(visible, full, None) { load_whole_page }`. In SCROLL_THROUGH,
   save `scrollY` first and restore THAT position at the end (not 0). The report still reads the
   whole-page text through `full_text` (DOM innerText does not need scrolling); run load_whole_page for
   the report only if needs_scroll_through says so. Every automatic scroll-through adds a MoveStep with
   operation "AUTO_SCROLL", note "load lazy content", page_changed from the snapshot compare. It does NOT
   count against the step budget and is NOT sent to Jev in history. Never scroll-through one URL twice
   (keep the existing set).
2. Fake search triggers: each candidate sent to Jev gets `search_like: true` when is_search_like.
   When `goal_wants_search(goal)` and Jev answers BLOCKED (or DONE with goal_met < GOAL_MET_AT) while an
   untried `is_fake_search_trigger` candidate exists: Mini clicks it itself (a MoveStep with
   note "opened search"), re-observes, and continues. At most 2 times per run. Also add to the
   --move instructions: "A control with search_like true that is a textbox you cannot type into opens
   the real search field: CLICK it first, then TYPE_TEXT into the field that appears."
3. Soft load: in `run`, when `--url` navigation returns the "page did not finish loading" error,
   continue if `document.readyState` is "interactive" or "complete" or body innerText length > 200
   (print one stderr line); otherwise fail as now.
4. PRESS_ENTER refocus: remember the element last typed into (the selector/node that TYPE_TEXT used). On
   PRESS_ENTER, if no text field has focus, focus that element first and then press Enter. The refusal
   stays only if the element is gone.
5. DONE needs evidence: accept DONE only when goal_met >= 0.5. Below that, handle it like the
   current disputed path (counts toward the 3-strike stop, which is a failure, not ok).
6. Replay fuzzy match: in `clicked_for_step`, if no exact label head match, pick the candidate with the
   highest `affordance::label_similarity >= 0.5` and the same role; ties -> the first in DOM order.
   `replay stopped` only if none.
7. Unit tests for every pure piece (the refocus decision, the DONE threshold, fuzzy pick, AUTO_SCROLL
   not counted against the budget). Existing tests stay green.

## Worker split
| Worker | Branch | Owns |
|---|---|---|
| p7-afford | `rl/p7-afford` | `src/affordance.rs`, `src/learn.rs` |
| p7-loop | `rl/p7-loop` | `src/move_cmd.rs`, `src/qa/questions.rs`, `src/cdp.rs`, `tools/qa-act.js` |

## Done / STOP
The same as plans 004-006. The orchestrator re-runs the benchmark after the merge. Target: >= 10/12, with 0 false ok.

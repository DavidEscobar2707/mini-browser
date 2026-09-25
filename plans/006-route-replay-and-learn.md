# 006 - Route replay + learned site notes (RL phases 3 and 4)

Status: DONE (226 tests; live on Cinemark). Workers: `p3-replay`, `p4-learn` (herdr, OMP, deepseek-v4.1-flash). Depends on 005.

## Why
Phase 2 showed that priors alone do not cut Jev calls on an easy task (5 vs 5). The cost is one
Jev call per step. Phase 3 replays a known-good route with no Jev calls and checks each page
before it acts. Phase 4 turns the episode log into the per-site lesson file that is sent as
`site_notes`, so the hand-written lessons become an output of the data.

## Shared contract (in `src/episodes.rs` on main; fixed)
```
pub struct RouteStep { operation, target: Option<String>, page_sig, url }
pub struct Route { episode_id, goal_key, final_reward: f64, steps: Vec<RouteStep> }
pub fn load_episodes(dir, host) -> Vec<Episode>          // implemented
impl PriorTable { pub fn rows(&self) -> Vec<(String, Prior)> }   // implemented
pub fn best_route(eps: &[Episode], goal_key: &str) -> Option<Route>   // STUB -> p3-replay
pub fn path_of_sig(sig: &str) -> &str                     // implemented: text before '#'
```

### best_route (p3-replay)
- Candidates: episodes with `goal_key == goal_key`, `ok == true`, feedback not `bad`, and
  `final_reward() >= 0.5`.
- Route steps = the episode's steps that are CLICK, SCROLL_DOWN, or PRESS_ENTER AND had
  `page_changed == true` (WAIT, DONE, no-effect steps dropped). If ANY step of the episode is
  TYPE_TEXT, that episode is not a candidate (typed values are not stored).
- Pick the fewest route steps; tie -> higher final_reward; tie -> newest ts_ms. Empty route -> None.

### Replay in `--move` (p3-replay, src/move_cmd.rs)
- New flag `--no-replay` (MoveArgs.no_replay). Replay is also skipped when `no_priors` is set.
- Before the Jev loop: load episodes for the start host, `best_route(.., normalize_goal(goal))`.
- For each route step: snapshot the page (same load/settle rules as the loop); require
  `path_of_sig(current sig) == path_of_sig(step.page_sig)`; for CLICK find the candidate whose
  label equals the step target exactly (after the same label normalization the resolver uses);
  perform it with the existing perform path; require page_changed. Any mismatch/failure -> stop
  replay, note `replay stopped at step k: <why>`, and continue with the normal Jev loop from
  the current page (steps keep counting, same step budget).
- Replayed steps are MoveStep with `note: Some("replay")`, `goal_met: None`, correct page_sig.
- After the route ends, the normal loop runs; its first Jev call is the goal check (it stops at
  p >= GOAL_MET_AT), so a full replay costs 1 Jev call.
- `MoveReport` gains `pub replayed: usize` (steps executed by replay) and
  `#[serde(skip_serializing_if = "Option::is_none")] pub replay_from: Option<String>` (episode id).
- Replayed steps go into the episode log like any other step (so good routes are reinforced and a
  broken route gets negative rewards).
- Tests: best_route rules (bad label excluded, TYPE_TEXT excluded, fewest steps wins, no-effect
  and WAIT dropped); arg parsing of --no-replay; a pure `route_step_matches(current_sig, label_found, step)`
  helper.

### `mini --learn [host] [--write] [--hints DIR]` (p4-learn, new src/learn.rs + dispatch in src/main.rs)
- Reads `load_episodes` for one host (or every `*.jsonl` host in the episodes folder).
- Builds a learned section from `PriorTable::from_episodes(..).rows()` grouped by `path_of_sig`
  (merge rows with same operation+target, weighted mean):
  - helps: n >= 2 and mean_return >= 0.5 -> `- On <path>: <OP> "<target>" helps (return +0.80, n=3).`
  - avoid: n >= 2 and mean_return <= -0.2 -> `- On <path>: avoid <OP> "<target>" (return -0.40, n=2).`
  - route: best route over the most common goal_key -> `- Known route (<k> steps): A > B > C.`
  - header line `Learned from <N> episodes (ok rate <p>%).` ; at most 8 bullet lines; section <= 700 chars.
- The section sits between `<!-- mini:learned -->` and `<!-- /mini:learned -->` in
  `<hints>/<host>.md` (hints default `%LOCALAPPDATA%\Mini\hints`, same as site_notes). Replace
  only that block; append it at the end if missing; create the file with front matter
  (`---\ntype: reference\nsite: <host>\nupdated: <YYYY-MM-DD>\nruns: <N>\n---\n# <host>\n`) if missing.
  Update `updated:` and `runs:` in existing front matter. Never touch other lines.
- Without `--write`: print the new section to stdout (dry run), exit 0. With `--write`: write
  atomically (tmp + rename), print the path. No episodes -> exit 1 with a message.
- Pure functions + tests: section builder (hand-made episodes), block replace/append/create,
  front matter update, 700-char cap.

## Worker split
| Worker | Branch | Owns | Must not touch |
|---|---|---|---|
| p3-replay | `rl/p3-replay` | `src/move_cmd.rs`, `src/episodes.rs` (only `best_route` body + its tests) | everything else |
| p4-learn | `rl/p4-learn` | new `src/learn.rs`, `src/main.rs` (mod + `--learn` dispatch) | everything else, incl. episodes.rs |
Docs (AGENTS.md/README.md) are done by the orchestrator after merge.

## Done / STOP
Same as plans 004/005: tests green, commit on own branch, no merge/push/secrets/network/mini.exe,
final line `DONE <branch> <sha> <N tests passed>` or `BLOCKED <reason>`.

## Live results (Cinemark Arkadia, alternated)

| mode | Jev calls | ms | replayed |
|---|---|---|---|
| replay (full route) | 1 | 8,067 | 4 |
| replay (broke at step 3) | 4 | 11,646 | 2 |
| replay (broke at step 1) | 7 | 12,350 | 0 |
| no replay | 6 / 9 | 11,678 / 13,556 | 0 |

A full replay cuts Jev calls by 85% and time by 35%. The problem: 2 of 3 replays broke on an EXACT
label match. Drop-down labels contain the current selection ("Seleccionar Teatro Todos los teatros
icon: down"), and the page starts in a different state (cookie, pop-up). Next: fuzzy match the
target (token overlap, same role), and on a mismatch ask Jev for ONE step with the route step as a
hint, then resume the replay. Also: `--learn` lists WAIT as "helps" (the return leaks backward).
Exclude WAIT/SCROLL from the "helps" bullets.

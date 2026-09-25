# 004 - Episode log, reward, and human feedback (RL phase 1)

Status: DONE (merged e33117d, 180 tests). Orchestrator: Hermes. Workers: `ep-core`, `ep-wire` (herdr, OMP, deepseek-v4.1-flash).

## Why

`mini --move` forgets every run. Mini cannot learn which clicks help on a site,
and a human who sees a wrong result has no way to tell it. Phase 1 records every
run as an RL episode with a reward computed in code, and adds a human label.
Later phases (not in this plan): 2 = action-value priors sent to Jev,
3 = route replay with signature checks, 4 = `mini --learn` distills episodes into
`sites/<host>.md`.

## Contract (the stub in `src/episodes.rs` on `main` is the interface; do not change signatures without the orchestrator)

Storage: `%LOCALAPPDATA%\Mini\episodes\<host>.jsonl` (host without `www.`), one
`Episode` JSON per line, append only, except `--feedback` which rewrites one line.
`%LOCALAPPDATA%\Mini\episodes\last.json` = `{"host": "...", "id": "..."}` of the newest episode.
`episodes::root_dir(None)` returns the default folder; tests pass `Some(dir)`.

### Page signature
`page_signature(url, labels) -> String` = `"<host><path-pattern>#<hash8>"`.
- host without `www.`; path segments that are all digits, UUID-like, or longer
  than 24 chars become `:id`; query and fragment dropped; trailing `/` dropped.
- hash8 = first 8 hex chars of FNV-1a 64 over the first 40 control labels,
  each lowercased, digits removed, trimmed, deduped, sorted, joined by `\n`.
- Must be stable across runs of the same page and ignore showtimes/prices (digits).

### Goal key
`normalize_goal(goal)` = lowercase, keep letters/digits/spaces (Unicode letters kept,
accents kept), collapse whitespace, trim.

### Reward (computed in code, never by the model)
- `step_reward(goal_before, goal_after, page_changed)`:
  `(goal_after - goal_before)` (missing value = 0.0) `- 0.05`, and `- 0.3` more if `!page_changed`.
- `goal_after` of step i = `goal_met` of step i+1. For the last step: 1.0 if the run
  ended ok, else the step's own `goal_before`.
- `episode_reward(ok, steps)` = `(+1.0 if ok else -1.0) + sum(step rewards)`, clamped to [-3, 3].
- `Episode::final_reward()` = feedback `bad` -> -1.0 minus 1.0 (i.e. -2.0);
  feedback `good` -> max(episode_reward, 1.0); none -> episode_reward.

### Types (serde, all fields serialized)
```
Feedback { label: String /* "good" | "bad" */, reason: Option<String>, ts_ms: u64 }
StepSample { step: usize, operation: String, target: Option<String>, page_sig: String,
             url: String, page_changed: bool, goal_before: Option<f64>, goal_after: Option<f64>,
             ms: u64, reward: f64 }
Episode { id: String, ts_ms: u64, host: String, goal: String, goal_key: String, ok: bool,
          stop: String, judge_calls: u32, elapsed_ms: u64, steps: Vec<StepSample>,
          episode_reward: f64, feedback: Option<Feedback> }
```
`id` = `"<ts_ms>-<pid>"`.

### Functions
- `build_episode(goal, ok, stop, judge_calls, elapsed_ms, raw: &[RawStep]) -> Episode`
  where `RawStep { step, operation, target, page_sig, url, page_changed, goal_met, ms }`
  (goal_met = Jev's check of the page the step started on). Fills goal_after + rewards.
- `append(dir: Option<&Path>, ep: &Episode) -> Result<PathBuf, String>` (creates folder, writes last.json).
- `set_feedback(dir, id: Option<&str>, label, reason) -> Result<Episode, String>`
  (None = last episode; label must be good|bad; rewrites the one line atomically: write tmp, rename).
- `summary(dir, host: Option<&str>) -> Result<serde_json::Value, String>`:
  `{episodes, ok_rate, mean_final_reward, mean_judge_calls, mean_elapsed_ms,
    actions: [{page_sig, target, operation, n, mean_reward}]}` sorted by mean_reward desc,
  at most 50 rows. Skip corrupt lines (count them in `corrupt_lines`).

## Worker split (no shared files -> no merge conflicts)

| Worker | Branch | Owns | Must not touch |
|---|---|---|---|
| ep-core | `rl/ep-core` | `src/episodes.rs` (replace stub bodies, add `#[cfg(test)]` tests) | every other file |
| ep-wire | `rl/ep-wire` | `src/move_cmd.rs`, `src/main.rs`, `AGENTS.md` (+ `README.md` usage lines) | `src/episodes.rs` |

ep-wire:
1. In `run_loop`, compute `episodes::page_signature(&before.url, labels of before.actions)` per step;
   add `pub page_sig: String` to `MoveStep`.
2. After the report is built in `run`, build + `append` the episode (goal = `a.goal`;
   host from `url_before`). Add `pub episode_id: Option<String>` to `MoveReport`.
   An append error prints one line to stderr and never changes the exit code.
3. `mini --feedback good|bad ["reason"] [--id ID]` -> prints the updated episode JSON, exit 0; bad input exit 2.
4. `mini --episodes [host]` -> prints `summary` JSON.
5. Unit tests for arg parsing of the two new commands. Existing tests stay green.
   The stub returns defaults, so wiring compiles and tests run before ep-core lands.

ep-core: unit tests for every rule above (signature stable when digits change,
`:id` segments, `www.` dropped, reward numbers, last-step goal_after, clamp,
final_reward with feedback, append+set_feedback round trip in a temp dir,
summary skips a corrupt line).

## Done means
- `cmd.exe /c "tools\win-cargo.cmd" test` green on the worker branch.
- One commit (or a few) on the worker branch. No merge, no push. No secrets. No edits outside owned files.
- Final message in the pane: `DONE <branch> <commit sha>` + test count, or `BLOCKED <reason>`.

## STOP conditions
- Needing to change a signature in the contract: stop and write `BLOCKED` with the change you need.
- Build tools missing / cargo fails for reasons outside owned files: `BLOCKED`.

## Findings from the first live episode (input for phase 2)

- Credit assignment: goal_met stays ~0.02 on every page before the answer page, so the
  per-step delta is sparse and the LAST step takes all the credit (a WAIT got +0.89;
  the useful clicks got ~-0.05). Phase 2 must score actions by discounted return
  (G_t = r_t + 0.9 * G_{t+1}), not by the raw step reward.
- The no-effect penalty works: the dead "Close" click scored -0.36.
- Human feedback overrides a model "ok": ok=true, final_reward=-2.0 after `--feedback bad`.

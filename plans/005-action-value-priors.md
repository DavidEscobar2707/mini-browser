# 005 - Action-value priors from past episodes (RL phase 2)

Status: IN PROGRESS. Orchestrator: Hermes. Workers: `p2-core`, `p2-wire` (herdr, OMP, deepseek-v4.1-flash).
Depends on: 004 (episode log).

## Why

Phase 1 records episodes but `--move` does not use them. Phase 2 turns the log into
per-page action values and shows them to Jev on every step, so a known-good path is
preferred and a known-wasted click (e.g. Cinemark "Close") is avoided.
004's live finding: the step reward is sparse, and the last step takes all the credit.
So the values use the discounted return, not the raw step reward.

## Contract (additions in `src/episodes.rs`; the stub on `main` fixes the signatures)

```
pub const GAMMA: f64 = 0.9;
pub fn terminal_reward(ep: &Episode) -> f64
pub fn discounted_returns(ep: &Episode, gamma: f64) -> Vec<f64>
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Prior { pub operation: String, pub target: Option<String>, pub n: u32, pub mean_return: f64, pub exact: bool }
#[derive(Clone, Debug, Default)]
pub struct PriorTable { /* private fields */ }
impl PriorTable {
    pub fn load(dir: Option<&Path>, host: &str) -> PriorTable   // never fails: empty table on any error
    pub fn from_episodes(eps: &[Episode]) -> PriorTable
    pub fn is_empty(&self) -> bool
    pub fn for_page(&self, page_sig: &str, top: usize) -> Vec<Prior>
}
```

Rules:
- `terminal_reward`: feedback `bad` -> -1.0; feedback `good` -> +1.0; no feedback -> +1.0 if ok, else -1.0.
- `discounted_returns`: r'_t = steps[t].reward, plus terminal_reward on the LAST step only.
  G_last = r'_last; G_t = r'_t + gamma * G_{t+1}. Empty steps -> empty vec.
- `from_episodes`: aggregate G_t over every step of every episode by key
  (page_sig, operation, target). Store n and mean.
- `for_page(sig, top)`: rows whose page_sig == sig (exact=true). If there are none, use rows whose
  path part (text before `#`) equals the sig's path part (exact=false). The hash changes when a
  drop-down opens, and the path does not. In fallback mode, merge rows with the same (operation, target)
  (weighted mean, summed n). Sort by mean_return desc, ties by n desc. Keep at most `top`, but
  keep every row with mean_return < -0.2 (known-bad actions must be visible) - so the result can be
  longer than `top` only by those rows.
- `load`: reads `<root>/<host>.jsonl` (host without `www.`, same stem rule as `append`). Skips corrupt lines.
- `summary` action rows gain `mean_return` (from discounted returns). `mean_reward` stays.

## Wiring (p2-wire)

1. In `run_loop` (src/move_cmd.rs): load `PriorTable` once, before the loop, for the host of the
   start URL. Skip loading when the new flag `--no-priors` is set (field `no_priors` in MoveArgs; parse it).
2. For each step, when `for_page(&page_sig, 12)` is not empty, set `question["state"]["past_runs"]` to
   that list (JSON array of Prior).
3. Add `#[serde(skip_serializing_if = "Option::is_none")] pub prior_return: Option<f64>` to `MoveStep`:
   the mean_return of the chosen (operation, target) in that list, if present.
4. In `src/qa/questions.rs`, append to the --move instructions (after the site_notes sentence):
   "past_runs, when present, lists actions taken on this same page in earlier runs: n = times tried,
   mean_return = discounted reward (above 0 helped reach the goal; below 0 wasted steps or led to a
   wrong result). Prefer an offered action with a high mean_return. Avoid actions with a negative
   mean_return unless nothing else can progress."
5. Loading placeholders (a finding from the 004 live run, labeled bad): before building the report's `page_text`,
   wait until the whole-page text is stable (same length in two reads 400 ms apart, max 5 s), then
   drop lines whose trimmed, lowercased text is only a loading word: `cargando`, `loading`,
   `cargando...`, `loading...`, `cargando…`, `loading…`. Put the filter in a pure function with a unit test.
6. `AGENTS.md` + `README.md`: document `past_runs`, `prior_return`, `--no-priors`.

## Worker split

| Worker | Branch | Owns | Must not touch |
|---|---|---|---|
| p2-core | `rl/p2-core` | `src/episodes.rs` | every other file |
| p2-wire | `rl/p2-wire` | `src/move_cmd.rs`, `src/qa/questions.rs`, `src/main.rs`, `AGENTS.md`, `README.md` | `src/episodes.rs` |

p2-core tests: terminal_reward for all 4 cases; returns on a 3-step episode with hand-computed numbers;
the bad label makes earlier returns lower than the same episode without a label; exact match beats fallback;
fallback merge (weighted mean); a known-bad row survives the `top` cut; load of a missing file = empty.

## Done means / STOP
Same as plan 004: tests green, commit on own branch, no merge/push/secrets, final line
`DONE <branch> <sha> <N tests passed>` or `BLOCKED <reason>`.

# 008 - No false success, opener that fires, fair goal check

Status: IN PROGRESS. Worker: `p8-fix` (herdr, OMP, deepseek-v4.1-flash). Depends on 007.

## Evidence (bench p007b: `%LOCALAPPDATA%\Mini\bench\p007b\<site>.json` has full move reports - READ them first)
1. ktronix: `stop = the goal check passed on this page (p=0.72)` but url_after == start URL and the
   last two clicks were "Submit"/"Open Submit" with no submit. FALSE ok.
2. rottentomatoes: steps end with a disputed DONE (goal_met 0.32), then the run stopped with
   `the judge reported the goal is done` -> ok=true. A DONE with no evidence was accepted by some other path. FALSE ok.
3. exito: Jev answered BLOCKED with 0 steps. Candidates include a fake trigger (kind click, role textbox,
   label "Open search") and a fill "search". The 007 opener (`search_recovery_wanted` + `opener.next`) did not fire.

## Required changes (owner: src/move_cmd.rs, src/affordance.rs, src/qa/questions.rs)
A. Find the exact code path for 2 and 3 by reading `run_loop`. Every path that sets `ok = true`
   from a DONE must go through `done_has_evidence`. Write a unit test that reproduces the
   rottentomatoes sequence with a fake judge (the loop already runs against a fake Judge in tests - find it).
B. Opener: make the BLOCKED/DONE-without-evidence recovery run BEFORE any other BLOCKED handling
   (stale check, reload), and treat `kind == "fill"` search-like fields that Jev did not use as a
   reason to retry once with a note in `history` ("the page has a search field: <label>"). Unit
   test with a fake judge: BLOCKED first, then the loop must click the fake trigger.
C. Fair goal check (generic, not per site): when `affordance::goal_wants_search(goal)` and the
   current URL (without fragment) equals the run's start URL, the goal check needs p >= 0.9 (not
   GOAL_MET_AT) to end the run. Pure helper `goal_threshold(goal, start_url, url) -> f64` + tests.
D. Keep all 246 tests green; add tests for A, B, C.

## Done / STOP
Same rules as plans 004-007. Final line `DONE rl/p8-fix <sha> <N tests passed>` or `BLOCKED: <reason>`.

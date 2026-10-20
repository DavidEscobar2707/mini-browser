# Mini

A tiny Windows 11 browser. One window, one page, no tabs, no Chrome cookies.

Download `mini.exe` from [Releases](https://github.com/DavidEscobar2707/mini-browser/releases). Pin it to the taskbar. That's the installer.

Needs the [Evergreen WebView2 Runtime](https://go.microsoft.com/fwlink/p/?LinkId=2124703) (Windows 11 already has it).

Cookies and cache live only under `%LOCALAPPDATA%\Mini\User Data`. Not Chrome. Not Edge.

## Run from source

```powershell
cargo run
```

MSVC toolchain (`stable-x86_64-pc-windows-msvc`). Binary: `target/debug/mini.exe`.

## Debug mode (for agents)

```powershell
cargo run -- --debug    # or set MINI_DEBUG=1; port override: MINI_DEBUG_PORT
```

Exposes CDP for the content page on `http://127.0.0.1:9222` (attach with
`puppeteer.connect({ browserURL: 'http://127.0.0.1:9222' })`) and records the
session to `%LOCALAPPDATA%\Mini\Debug\session-<ms>\`:

- `frontend.jsonl` — console.*, uncaught errors, unhandled rejections
- `dom.jsonl` — batched DOM mutations (compact selector paths)
- `backend.jsonl` — navigations, loads, reloads, popups

`%LOCALAPPDATA%\Mini\Debug\current.json` points at the live session + CDP URL.

```powershell
mini --debug-status     # exit 0 only when the pid is alive, is mini.exe, and CDP answers
mini --debug-stop       # terminate the recorded debug browser
mini --observe          # print the indexed element table of the live page (no model call)
```

### Agent moves (Jev)

`mini --move` runs the [jev-ultrafast](https://github.com/browser-use/jev-ultrafast)
loop on the page already open in the debug window: one Jev request picks the
operation (`CLICK`, `TYPE_TEXT`, `SELECT`, scroll, `WAIT`, `DONE`, `BLOCKED`)
and a target per operation; Mini executes it with real CDP input after a
hit-test. Field values come from you, never from the model, and never reach
Jev, stdout or `move.json`. Pipe them on stdin so they stay off the command line:

```powershell
'{"goal":"Sign in","values":{"email":"dev@example.test","password":"..."}}' | mini --move
mini --move --goal "Search the docs for webhooks" --value q=webhooks --url http://localhost:3000
```

Loopback hosts only unless you pass `--allow-remote`. Needs a Jev key (see
[Jev API key](#jev-api-key)); `TYPESAFE_MODEL` overrides `jev-latest`. Exit `0` = the judge reported DONE,
`1` = stopped without DONE, `2` = could not run. `DONE` is the model's claim —
verify the outcome yourself.

Every step also tells Jev what earlier runs on the same page learned:
`past_runs` (what Jev sees) lists `operation`, `target`, `n` (times tried) and
`mean_return` — the discounted reward of that action, where above `0` helped
reach the goal and below `0` wasted steps or led to a wrong result. Jev is told
to prefer a high `mean_return` and to avoid a negative one unless nothing else
can progress. `steps[].prior_return` in the result reports the value of the
action that step took, absent when the page has no history; `--no-priors` asks
Jev without any of it. `page_text` in the result is read once the page stops
changing (max 5 s), with lines that are only a loading word (`Cargando…`,
`Loading…`) dropped.

`mini --desk --app Calculator --launch --goal "..."` runs the same loop on a native Windows app through cua-driver (see AGENTS.md, "Desktop apps").

## Episodes and feedback

Every `--move` run is appended to `%LOCALAPPDATA%\Mini\episodes\<host>.jsonl`
(host without `www.`), one episode per line, with a reward computed in code —
never by the model. Logging is best effort: if it fails you get one line on
stderr and the run's own exit code is unchanged.

```powershell
mini --feedback bad "clicked past the results"   # label the newest episode
mini --feedback good --id 1758742123456-8123     # or one by its id
mini --episodes                                  # summary of every episode (JSON)
mini --episodes github.com                       # just one host
```

`mini --feedback` prints the updated episode, `mini --episodes` the summary
(`episodes`, `ok_rate`, `mean_final_reward`, `mean_judge_calls`,
`mean_elapsed_ms`, `actions[]` sorted by mean reward). Neither opens a window
or calls Jev. `bad` costs the run more than its own reward; `good` never scores
below `1.0`.

`--move` reads this log back through `past_runs`: the summary's `actions[]`
rows carry `mean_return` (the same discounted action value the loop sends Jev)
next to the raw `mean_reward`.

## Site lessons

`--move` sends Jev the lessons for the current site: the file `<host>.md` (no
`www.`) in `%LOCALAPPDATA%\Mini\hints`, or in the folder you pass with
`--hints DIR`. Keep one short, imperative bullet per quirk ("Press Enter after
typing in the search box"). Front matter and headings are skipped.

## Jev API key

`--move` and `--qa` call Jev (api.typesafe.ai). Mini takes the key from the
first place that has it:

1. the `TYPESAFE_API_KEY` environment variable
2. a `TYPESAFE_API_KEY=...` line in `%LOCALAPPDATA%\Mini\.env`
3. a `TYPESAFE_API_KEY=...` line in `<Desktop>\jev-ultrafast\.env` (shared with the Python client)

So agents that start Mini from a fresh shell need no setup. Mini never prints the key.

## QA mode (for agents)

```powershell
target\debug\mini.exe --qa https://example.com --claim "The pricing table shows three plans"
```

Opens the URL, walks it at phone / tablet / desktop, exercises the controls it
finds (never submitting anything — writes are refused at the network layer,
write-like controls are never clicked, and ambiguous clicks are gated by a
model vote), and writes `report.md`, `report.json`, screenshots and the raw
evidence streams to `%LOCALAPPDATA%\Mini\QA\<stamp>-<host>\`. Exit code `0` =
clean, `1` = findings or an unverified claim, `2` = the run could not happen.

Viewports run phone (390x844), tablet, desktop. Jev explores (taps, clicks) at
the viewports in `--explore <list>` (default `phone,desktop`; `none` =
probe-only). Phone and tablet get touch emulation, an Android user agent and
touch taps. Two tap-flow rules computed in code (scroll lock leaked, overlay
across navigation) run after each explored step and fail the run at severity 2.
Step screenshots are `shots/<viewport>-step-NN.jpg`. The wall clock is 240 s
plus 120 s for each explored viewport after the first.

Needs a Jev key (see [Jev API key](#jev-api-key)). Single engine (WebView2/Chromium):
visual-only regressions and non-Chromium differences are out of scope — the
screenshots are there so a human can look.

## Daily builds

A GitHub Action builds `mini.exe` every day and publishes it as a dated release (`YYYY.MM.DD`). You can also hit **Run workflow** on the Actions tab.

## Not in v1

Tabs, bookmarks, extensions, downloads UI, settings, macOS/Linux, MSI, auto-update.

Contract: [`docs/SPEC.md`](docs/SPEC.md).

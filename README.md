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

Opens the URL, walks it at desktop / tablet / phone, exercises the controls it
finds (never submitting anything — writes are refused at the network layer,
write-like controls are never clicked, and ambiguous clicks are gated by a
model vote), and writes `report.md`, `report.json`, screenshots and the raw
evidence streams to `%LOCALAPPDATA%\Mini\QA\<stamp>-<host>\`. Exit code `0` =
clean, `1` = findings or an unverified claim, `2` = the run could not happen.

Needs a Jev key (see [Jev API key](#jev-api-key)). Single engine (WebView2/Chromium):
visual-only regressions and non-Chromium differences are out of scope — the
screenshots are there so a human can look.

## Daily builds

A GitHub Action builds `mini.exe` every day and publishes it as a dated release (`YYYY.MM.DD`). You can also hit **Run workflow** on the Actions tab.

## Not in v1

Tabs, bookmarks, extensions, downloads UI, settings, macOS/Linux, MSI, auto-update.

Contract: [`docs/SPEC.md`](docs/SPEC.md).

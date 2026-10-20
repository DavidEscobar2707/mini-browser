# Mini QA, mobile first: phone-first `--qa` in WebView2 + a real Android Chrome lane (`--device`)

Status: A DONE (commits 41025ac, d47f930, 2983ba0, 012e34a, A4 = the commit that adds this file), B TODO. See the progress log at the end.
Plan written by Hermes in plan mode. Nothing in the repo was changed. One side effect of the inspection: the `adb` daemon was started once (`adb devices`); harmless.

## Goal

Make `mini --qa <url>` judge a page the way a phone user meets it (phone viewport first, real touch, tap-flow rules), and add an optional lane that runs the same QA loop in real Chrome on an Android device or a Pixel emulator, all in Rust inside Mini.

## Decision: how to test "for mobile" (read before coding)

Three ways were weighed. Facts come from the code and the machine, not from memory.

| Option | What it is | Fidelity | Cost | Verdict |
|---|---|---|---|---|
| A. Mobile-first inside Mini's WebView2 | Keep the current engine; run the phone viewport FIRST and explore (Jev clicks) there; emulate touch + Android UA; add tap-flow rules in code | Chromium at 390x844 with touch events and a mobile UA. Misses Android-only behaviour (URL bar collapse / `100dvh`, soft keyboard resize, real fonts, back gesture) | ~half a day. No new runtime | DO FIRST. Cheapest fix for the known gap: today `--qa` explores at desktop only and only probes layout at phone (src/qa/agent.rs:272 `if vp.name == "desktop"`), so tap-flow bugs like the OhhLook overlay/scroll-lock bugs are never found |
| B. Real Chrome on Android via adb + CDP | Mini stays the driver (Rust). Chrome for Android exposes DevTools on the unix socket `chrome_devtools_remote`; `adb forward` turns it into `127.0.0.1:<port>`; Mini's QA loop runs through the existing `Browser` trait over a websocket CDP client | Real Chrome, real Android, real touch, real viewport units, real keyboard | ~2 days. Emulator image download ~1.3 GB once. The emulator is already installed (see facts) | DO SECOND. Reuses 100% of the QA loop (`qa::agent::run` only needs a `Browser` impl) |
| C. Appium / chromedriver / Playwright-on-Android | External Node/Java drivers | Same as B | New runtimes, duplicate loop, bypasses Mini's firewall/recorder/Jev gates | REJECTED. Not lightweight, and plans/README.md already rejected ad-hoc external drivers ("extend Mini") |

"A Google phone from Android Studio" = an AVD (Android Virtual Device). You do NOT need Android Studio: the SDK command-line tools, emulator and `adb` are already on this laptop (`%LOCALAPPDATA%\Android\Sdk`). A physical Android phone on USB with "USB debugging" works through exactly the same code path (B), and is faster than the emulator.

## Facts (verified 2026-10-20)

Machine:
- `%LOCALAPPDATA%\Android\Sdk`: `platform-tools` (adb 36.0.0), `emulator` 36.2.12, `cmdline-tools/latest` (sdkmanager, avdmanager), system image `android-36;google_apis;x86_64`, one AVD `tl36` (pixel_7, API 36, `google_apis`, belongs to the tapledger project; do not modify it).
- `emulator -accel-check` -> "WHPX(10.0.26200) is installed and usable" (the laptop is AMD; WHPX is the right accelerator).
- `avdmanager list device -c` includes `pixel_8`.
- The `google_apis` image type does NOT ship Chrome (Appium CI note: "the google_apis system image does not include Chrome"). Chrome comes with `google_apis_playstore` images. So Phase 0 installs `system-images;android-36;google_apis_playstore;x86_64` and creates a new AVD `mini-qa`.
- Play-store images are production builds: no `adb root`, so Chrome's `/data/local/tmp/chrome-command-line` (`--disable-fre`) trick needs a chrome://flags toggle. Plan: dismiss Chrome's first-run screens ONCE by hand on the first (windowed) boot; the state persists in the AVD's data partition.
- Node 24 is installed (only needed for the existing `tools/*.js` helpers, not for this plan).

Mini (code):
- Build/test ONLY through the wrapper: `cmd.exe /c "tools\\win-cargo.cmd" build|test` from the repo root (bash). Run `target/debug/mini.exe --debug-stop` before every build; a running Mini locks the exe and a build that prints `failed to remove file` leaves the OLD binary in place.
- Last known test count: 279 (commit cbcd707). Record the real baseline in Task A0.
- QA loop: `src/qa/agent.rs` (`pub fn run`, `pub fn execute`, `struct Pass`, `fn explore`, `fn act`). Interface types: `src/qa/mod.rs` (`QaConfig`, `Viewport`, `VIEWPORTS`, `trait Browser`, `trait Judge`, `ViewportFacts`, `Finding`, `QaReport`). Transport: `src/qa/bridge.rs` (`UiBridge: Browser`, in-process WebView2 CDP). Page scripts: `tools/qa-*.js` injected via `qa::PAGE_SCRIPTS`. Pure firewall: `src/qa/firewall.rs` (`should_block`, `FirewallPolicy`, `Blocked{ts_ms,method,url,mechanism}`). Report: `src/qa/report.rs` (`render_header` prints `- Engine:`).
- `VIEWPORTS` = desktop 1440x900 dsf 1, tablet 768x1024 dsf 2 mobile, phone 390x844 dsf 3 mobile (mod.rs:87). `bridge.set_viewport` calls only `Emulation.setDeviceMetricsOverride` (bridge.rs:301). No touch emulation, no UA override. Clicks are `Input.dispatchMouseEvent` (bridge.rs:367).
- Step screenshots are `shots/step-NN.jpg` (agent.rs:768) with no viewport prefix; two explored viewports would overwrite each other.
- CLI parsing for `--qa`: `src/main.rs` `parse_qa_args` (line 847), `struct QaCliArgs` (836), `run_qa` (909). Tests build a config with `fn config()` (agent.rs:1873) and run `execute` through `executed()`.
- Existing CDP websocket client: `src/cdp.rs` (`Cdp::connect/call/eval`, `page_ws_url(port)`, `pick_page_target`), blocking, drops event frames. Deps already present: `tungstenite` (handshake feature), `ureq`, `parking_lot`, `serde_json`.
- Recorder: `tools/recorder.js` posts batches through `window.__miniPost(line)` when that function exists, else `window.ipc.postMessage` (recorder.js:34-35). Host side: `fn route_debug` in `src/main.rs:245` writes lines into `debug::DebugRecorder` streams. `DebugRecorder::start()` (debug.rs:298) writes `%LOCALAPPDATA%\Mini\Debug\current.json` every time it rotates a run.
- Firewall transport today: `Fetch.enable` + an in-process `Fetch.requestPaused` receiver (bridge.rs:577-630) that calls `firewall::should_block` and answers `Fetch.failRequest` / `Fetch.continueRequest`.
- `--qa` refuses `file:` URLs. Fixtures are served with `python -m http.server 8000` from `tools/fixtures`.

Conventions the implementer must keep:
- TDD per task: write the failing test, run it, see it fail, implement, run it, see it pass, commit.
- Never print or commit an API key. Never loosen the `--qa` write-safety layers.
- Short Simplified Technical English in docs. Code, commits and tests in normal English.
- Commit after every task with the message given. Do not push (a push to `main` triggers the daily release).

## Architecture

Phase A changes only the order, input mode and rules of the existing WebView2 QA pass: `VIEWPORTS` becomes phone, tablet, desktop; `QaConfig.explore` lists the viewports Jev explores (default `phone,desktop`); mobile viewports get touch emulation, an Android UA, `Input.dispatchTouchEvent` taps, and two tap-flow rules computed in code after every step.

Phase B adds `--device auto|emulator|<serial>`: `src/android.rs` (adb discovery, port forward, Chrome launch, emulator boot), `src/cdp_bus.rs` (event-capable CDP websocket client with a reader thread), and `src/qa/remote.rs` (`RemoteBrowser: Browser` that injects the same page scripts via `Page.addScriptToEvaluateOnNewDocument`, receives recorder lines via `Runtime.addBinding`, and arms the same firewall via `Fetch.enable`). `qa::agent::run` is reused unchanged, with one viewport named `device`.

```
mini --qa URL                      mini --qa URL --device emulator
  |                                   |
  v                                   v
qa::agent::run(cfg, &UiBridge)     android::ensure_device -> adb forward 9333
  phone (explore, touch)              |
  tablet (probe)                      v
  desktop (explore, mouse)         qa::agent::run(cfg, &RemoteBrowser{CdpBus -> ws://127.0.0.1:9333})
                                      device (explore, touch)   [real Chrome on Android]
```

---

## Phase 0: prove the Android lane by hand (no code; ~30 min + download)

Do this before Phase B, in a bash terminal. Every step has its expected output. Stop and report if one differs.

0.1 Install the Play-store image (has Chrome). ~1.3 GB.
```bash
SDK="$LOCALAPPDATA/Android/Sdk"; export SDK
yes | "$SDK/cmdline-tools/latest/bin/sdkmanager.bat" --licenses > /dev/null
"$SDK/cmdline-tools/latest/bin/sdkmanager.bat" "system-images;android-36;google_apis_playstore;x86_64"
ls "$SDK/system-images/android-36/google_apis_playstore/x86_64"
```
Expected: the last `ls` prints `system.img` among other files.

0.2 Create the AVD `mini-qa` (Pixel 8) and tune it.
```bash
echo no | "$SDK/cmdline-tools/latest/bin/avdmanager.bat" create avd -n mini-qa -k "system-images;android-36;google_apis_playstore;x86_64" -d pixel_8
CFG=~/.android/avd/mini-qa.avd/config.ini
sed -i 's/^hw.keyboard *=.*/hw.keyboard = yes/' "$CFG"
grep -q '^hw.gpu.enabled' "$CFG" && sed -i 's/^hw.gpu.enabled *=.*/hw.gpu.enabled = yes/' "$CFG" || echo 'hw.gpu.enabled = yes' >> "$CFG"
grep -E '^hw.device.name|^hw.keyboard|^hw.gpu.enabled|^PlayStore' "$CFG"
```
Expected: `hw.device.name = pixel_8`, `hw.keyboard = yes`, `hw.gpu.enabled = yes`, `PlayStore.enabled = yes`.

0.3 First boot WITH a window (needed once, to tap through Chrome's first-run screens).
```bash
"$SDK/emulator/emulator.exe" -avd mini-qa -no-audio -no-boot-anim -gpu auto > "$TMPDIR/emu.log" 2>&1 &
ADB="$SDK/platform-tools/adb.exe"
"$ADB" wait-for-device
until [ "$("$ADB" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = "1" ]; do sleep 2; done; echo booted
"$ADB" devices -l
```
Expected: `booted`, then a line `emulator-5554   device product:... model:Pixel_8 ...`. First boot 1-3 min; later boots 10-30 s (quick-boot snapshot).

0.4 Chrome is present and starts on a URL.
```bash
"$ADB" shell pm path com.android.chrome
"$ADB" shell dumpsys package com.android.chrome | grep -m1 versionName
"$ADB" shell am start -W -n com.android.chrome/com.google.android.apps.chrome.Main -a android.intent.action.VIEW -d https://example.com
```
Expected: `package:/product/app/Chrome/Chrome.apk` (path may differ), `versionName=1xx.x.xxxx.xx`, and `Status: ok`. In the emulator window Chrome shows its first-run screens: tap "Use without an account" / "No thanks" / "Got it" until example.com is visible. This is a one-time step; it persists.

0.5 DevTools reachable from Windows.
```bash
"$ADB" forward tcp:9333 localabstract:chrome_devtools_remote
curl -s http://127.0.0.1:9333/json/version
echo; curl -s http://127.0.0.1:9333/json/list | head -c 600
```
Expected: `{"Android-Package":"com.android.chrome","Browser":"Chrome/1xx.0....","Protocol-Version":"1.3",...,"webSocketDebuggerUrl":"ws://127.0.0.1:9333/devtools/browser/..."}` and a list with one object `"type":"page"`, `"url":"https://example.com/"`, `"webSocketDebuggerUrl":"ws://127.0.0.1:9333/devtools/page/<id>"`.

0.6 Host apps: a localhost app on the laptop is reached from the device through `adb reverse`.
```bash
"$ADB" reverse tcp:3000 tcp:3000
"$ADB" shell am start -W -n com.android.chrome/com.google.android.apps.chrome.Main -a android.intent.action.VIEW -d http://localhost:3000
```
Expected (with `npx next dev --webpack -p 3000` running in `Desktop\ohh-look-frontend-next`): the OhhLook home page renders on the phone. `localhost:3000` is kept as the origin, which the OhhLook API's CORS list allows (Vercel previews are not allowed; see skill mini-browser).

0.7 Record in this plan (Facts section): Chrome version, cold boot seconds, warm boot seconds. Close the emulator with the window's X (saves the quick-boot snapshot).

---

## Phase A: mobile-first `--qa` in WebView2

### A0. Branch + baseline (5 min)
```bash
cd ~/Desktop/mini-browser && target/debug/mini.exe --debug-stop; git switch -c feat/qa-mobile-first
cmd.exe /c "tools\\win-cargo.cmd" test 2>&1 | grep -E "^test result|error\[" 
```
Expected: one or more `test result: ok. N passed; 0 failed; M ignored` lines and no `error[`. Write N into this plan under Facts ("baseline N").

### A1. Phone first + `--explore` (viewport order and which viewports Jev explores)

Files: `src/qa/mod.rs`, `src/qa/agent.rs`, `src/main.rs`.

A1.1 Failing tests. In `src/qa/agent.rs` tests module:
- Edit `every_viewport_gets_its_own_run_label_and_override`: both expected vectors become `vec!["phone", "tablet", "desktop"]`.
- Edit `fn config()` (agent.rs:1873): add `explore: vec!["phone".to_string(), "desktop".to_string()],` after `steps: 4,`.
- Add:
```rust
    #[test]
    fn exploration_runs_at_every_listed_viewport_and_marks_it() {
        let browser = FakeBrowser::new("explore-list");
        let judge = ScriptedJudge::with(&["CLICK", "DONE", "CLICK", "DONE"]);
        let report = executed(&browser, &judge);
        let phone = report.viewports.iter().find(|v| v.viewport == "phone").unwrap();
        let tablet = report.viewports.iter().find(|v| v.viewport == "tablet").unwrap();
        let desktop = report.viewports.iter().find(|v| v.viewport == "desktop").unwrap();
        assert!(phone.explored && desktop.explored && !tablet.explored);
        assert_eq!(phone.steps.len(), 2, "{:?}", phone.steps);
        assert_eq!(desktop.steps.len(), 2, "{:?}", desktop.steps);
        assert!(tablet.steps.is_empty());
    }

    #[test]
    fn explore_none_keeps_every_viewport_probe_only() {
        let browser = FakeBrowser::new("explore-none");
        let judge = ScriptedJudge::with(&["CLICK", "DONE"]);
        let mut cfg = config(&browser);
        cfg.explore.clear();
        let report = execute(&cfg, &browser, &judge);
        assert!(report.viewports.iter().all(|v| !v.explored && v.steps.is_empty()));
        assert!(browser.clicks.lock().is_empty());
    }
```
Run: `cmd.exe /c "tools\\win-cargo.cmd" test qa::agent 2>&1 | grep -E "^test |^error" | head -30`
Expected: compile error `no field explore` / `no field explored` (that is the failing state).

A1.2 Implement in `src/qa/mod.rs`:
```rust
pub const VIEWPORTS: [Viewport; 3] = [
    Viewport { name: "phone", width: 390, height: 844, dsf: 3.0, mobile: true },
    Viewport { name: "tablet", width: 768, height: 1024, dsf: 2.0, mobile: true },
    Viewport { name: "desktop", width: 1440, height: 900, dsf: 1.0, mobile: false },
];
/// Viewports Jev explores (clicks, types) when `--explore` is not given. Phone first: a
/// page is met as a phone user meets it, and the desktop pass is the second opinion.
pub const DEFAULT_EXPLORE: &[&str] = &["phone", "desktop"];
/// Wall-clock seconds added for every explored viewport beyond the first.
pub const EXTRA_WALL_CLOCK_S: u64 = 120;
```
In `QaConfig` add the field `pub explore: Vec<String>,` (after `steps`) and the method:
```rust
    /// True when Jev drives controls at this viewport (else the viewport is probe-only).
    pub fn explores(&self, viewport: &str) -> bool {
        self.explore.iter().any(|v| v == viewport)
    }
```
In `ViewportFacts` add `pub explored: bool,` (Default derives false).

In `src/qa/agent.rs`, the loop (around line 272): replace `if vp.name == "desktop" {` with
```rust
        if cfg.explores(vp.name) {
            facts.explored = true;
```
Fix the comment above it ("Exploration: desktop only" -> "Exploration: the viewports listed in cfg.explore").

In `src/main.rs`: `QaCliArgs` gets `explore: Option<Vec<String>>,`; `parse_qa_args` gets
```rust
            "--explore" => {
                out.explore = args.get(i + 1).map(|s| {
                    s.split(',').map(|v| v.trim().to_string()).filter(|v| v != "none" && !v.is_empty()).collect()
                });
                i += 2;
            }
```
(`--explore none` = probe-only everywhere.) In `run_qa`, before building `cfg`:
```rust
    let explore: Vec<String> = cli
        .explore
        .clone()
        .unwrap_or_else(|| qa::DEFAULT_EXPLORE.iter().map(|s| s.to_string()).collect());
    for v in &explore {
        if !qa::VIEWPORTS.iter().any(|vp| vp.name == v) {
            eprintln!("mini --qa: --explore names an unknown viewport {v} (phone, tablet, desktop)");
            std::process::exit(2);
        }
    }
    let wall_clock_s = qa::DEFAULT_WALL_CLOCK_S + qa::EXTRA_WALL_CLOCK_S * (explore.len().saturating_sub(1) as u64);
```
and set `explore,` and `wall_clock_s,` in the `QaConfig` literal (replacing `wall_clock_s: qa::DEFAULT_WALL_CLOCK_S`). Also add `explore: Vec::new()` wherever else `QaConfig` is built (`run_qa_spike` if it builds one; `grep -n "QaConfig {" src`).

A1.3 Fix the order-dependent assertions listed here (only these are expected to break): agent.rs:1940-1941 (`runs/run-desktop` -> `runs/run-phone`), 1958 and 1974 (`runs/run-desktop/` -> `runs/run-phone/`). Run `cmd.exe /c "tools\\win-cargo.cmd" test qa:: 2>&1 | grep -E "^test result|FAILED|panicked"`.
Expected: `test result: ok.` with baseline + 2 passed; nothing FAILED.

A1.4 Commit: `qa: phone first; --explore lists the viewports Jev explores (default phone,desktop)`.

### A2. Touch + Android UA at mobile viewports; taps instead of mouse clicks

Files: `src/qa/mod.rs`, `src/qa/bridge.rs`, `src/qa/agent.rs`.

A2.1 Failing tests (agent.rs tests module). Add `taps: Mutex<Vec<(f64, f64)>>,` to `FakeBrowser` (init `taps: Mutex::new(Vec::new()),`) and implement in `impl Browser for FakeBrowser`:
```rust
        fn tap(&self, x: f64, y: f64) -> Result<(), String> {
            self.taps.lock().push((x, y));
            Ok(())
        }
```
Edit `a_click_below_the_gate_is_dispatched`: it now runs at phone first, so assert `assert_eq!(browser.taps.lock().len(), 1); assert!(browser.clicks.lock().is_empty());`. Add:
```rust
    #[test]
    fn mobile_viewports_tap_and_desktop_clicks() {
        let browser = FakeBrowser::new("tap-vs-click");
        let mut judge = ScriptedJudge::with(&["CLICK", "DONE", "CLICK", "DONE"]);
        judge.mutate_p = 0.02;
        let _ = executed(&browser, &judge);
        assert_eq!(browser.taps.lock().len(), 1, "phone step is a touch tap");
        assert_eq!(browser.clicks.lock().len(), 1, "desktop step is a mouse click");
    }

    #[test]
    fn step_screenshots_carry_the_viewport_name() {
        let browser = FakeBrowser::new("step-shots");
        let judge = ScriptedJudge::with(&["CLICK", "DONE", "CLICK", "DONE"]);
        let report = executed(&browser, &judge);
        assert!(report.out_dir.join("shots/phone-step-01.jpg").exists());
        assert!(report.out_dir.join("shots/desktop-step-01.jpg").exists());
    }
```
Expected before implementing: compile error (`tap` is not a trait method), then after adding the trait method the two new tests fail.

A2.2 Implement. `src/qa/mod.rs`, inside `trait Browser` after `mouse_click`:
```rust
    /// One-finger tap at CSS-pixel coordinates. Transports without touch input
    /// fall back to a mouse click.
    fn tap(&self, x: f64, y: f64) -> Result<(), String> {
        self.mouse_click(x, y)
    }
```
`src/qa/bridge.rs`: add to `UiBridge` the field `original_ua: Arc<Mutex<Option<String>>>` (init `Arc::new(Mutex::new(None))` in `new`). Replace `set_viewport` with:
```rust
    fn set_viewport(&self, v: Viewport) -> Result<(), String> {
        // The real UA is read once, before any override, so desktop can restore it.
        if self.original_ua.lock().is_none() {
            if let Ok(raw) = self.eval("navigator.userAgent") {
                *self.original_ua.lock() = Some(raw.trim().trim_matches('"').to_string());
            }
        }
        let orientation = if v.width >= v.height { "landscapePrimary" } else { "portraitPrimary" };
        let params = json!({
            "width": v.width,
            "height": v.height,
            "deviceScaleFactor": v.dsf,
            "mobile": v.mobile,
            "screenWidth": v.width,
            "screenHeight": v.height,
            "screenOrientation": { "type": orientation, "angle": 0 },
        });
        match self.cdp("Emulation.setDeviceMetricsOverride", params) {
            Ok(_) => {
                *self.viewport_mechanism.lock() = "cdp";
            }
            Err(e) => {
                eprintln!(
                    "qa: Emulation.setDeviceMetricsOverride failed for {}x{} dsf={} ({e}); falling back to real window bounds",
                    v.width, v.height, v.dsf
                );
                match self.send(QaOp::SetViewportFallback(v)) {
                    Ok(_) => {
                        *self.viewport_mechanism.lock() = "bounds-fallback";
                    }
                    Err(e2) => {
                        return Err(format!("set_viewport: cdp failed ({e}); bounds fallback failed ({e2})"));
                    }
                }
            }
        }
        // Touch + UA follow the viewport: a phone page must see coarse pointers,
        // touch events and a mobile UA, and the desktop pass must see none of them.
        let touch = self.cdp(
            "Emulation.setTouchEmulationEnabled",
            json!({ "enabled": v.mobile, "maxTouchPoints": if v.mobile { 5 } else { 1 } }),
        );
        if let Err(e) = touch {
            eprintln!("qa: Emulation.setTouchEmulationEnabled failed ({e}); taps still dispatch as touch events");
        }
        let major = self
            .browser_version()
            .and_then(|s| s.split('.').next().map(str::to_string))
            .unwrap_or_else(|| "130".to_string());
        let ua = user_agent_for(&v, &major, self.original_ua.lock().as_deref());
        let _ = self.cdp("Emulation.setUserAgentOverride", ua);
        Ok(())
    }
```
and the pure helper (bottom of bridge.rs, above the tests):
```rust
/// `Emulation.setUserAgentOverride` params for a viewport. Mobile viewports get an
/// Android Chrome UA with matching client hints; desktop restores the original UA.
pub fn user_agent_for(v: &Viewport, chrome_major: &str, original: Option<&str>) -> serde_json::Value {
    if !v.mobile {
        return json!({
            "userAgent": original.unwrap_or(""),
            "platform": "Win32",
        });
    }
    let (model, kind) = if v.name == "tablet" { ("Pixel Tablet", "") } else { ("Pixel 8", "Mobile ") };
    json!({
        "userAgent": format!(
            "Mozilla/5.0 (Linux; Android 14; {model}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{chrome_major}.0.0.0 {kind}Safari/537.36"
        ),
        "platform": "Linux armv8l",
        "userAgentMetadata": {
            "brands": [
                { "brand": "Chromium", "version": chrome_major },
                { "brand": "Not=A?Brand", "version": "24" }
            ],
            "fullVersion": format!("{chrome_major}.0.0.0"),
            "platform": "Android",
            "platformVersion": "14.0.0",
            "architecture": "",
            "model": model,
            "mobile": v.name != "tablet",
            "bitness": "",
            "wow64": false
        }
    })
}
```
Note: `Emulation.setUserAgentOverride` with an empty `userAgent` is refused by Chromium; when `original` is `None` skip the call (`if !v.mobile && original.is_none() { return Ok(()) }` placed before the `let ua`). Add the `tap` impl to `impl Browser for UiBridge`:
```rust
    fn tap(&self, x: f64, y: f64) -> Result<(), String> {
        self.cdp("Input.dispatchTouchEvent", json!({ "type": "touchStart", "touchPoints": [{ "x": x, "y": y }] }))?;
        self.cdp("Input.dispatchTouchEvent", json!({ "type": "touchEnd", "touchPoints": [] }))?;
        Ok(())
    }
```
Unit tests for the helper (bridge.rs tests module; this file already has tests):
```rust
    #[test]
    fn phone_ua_is_android_mobile_and_desktop_restores_the_original() {
        let phone = crate::qa::VIEWPORTS[0];
        let desktop = crate::qa::VIEWPORTS[2];
        let p = user_agent_for(&phone, "141", Some("Original/1.0"));
        assert!(p["userAgent"].as_str().unwrap().contains("Android 14; Pixel 8"));
        assert!(p["userAgent"].as_str().unwrap().contains("Chrome/141.0.0.0 Mobile Safari"));
        assert_eq!(p["userAgentMetadata"]["mobile"], true);
        let d = user_agent_for(&desktop, "141", Some("Original/1.0"));
        assert_eq!(d["userAgent"], "Original/1.0");
    }
```
`src/qa/agent.rs`: add to `struct Pass` the fields `vp_name: String,` and `vp_mobile: bool,` (init `vp_name: String::new(), vp_mobile: false,` in `execute`). At the top of the viewport loop, right after `let mut facts = ViewportFacts {...}`: `pass.vp_name = vp.name.to_string(); pass.vp_mobile = vp.mobile;`. In `fn act`, `"CLICK"` arm: replace `self.browser.mouse_click(hit.x, hit.y)` with
```rust
                if self.vp_mobile { self.browser.tap(hit.x, hit.y) } else { self.browser.mouse_click(hit.x, hit.y) }
```
In `explore`, replace `let _ = self.shot(&format!("step-{step:02}.jpg"));` with `let _ = self.shot(&format!("{}-step-{step:02}.jpg", self.vp_name));`. Also make the explore goal say where it runs: `let goal = format!("Exercise the controls in {goal} as seen on a {} viewport and report anything that misbehaves.", self.vp_name);`.

A2.3 Run `cmd.exe /c "tools\\win-cargo.cmd" test qa:: 2>&1 | grep -E "^test result|FAILED|panicked"`. Expected: all ok, baseline + 5 (A1: 2, A2: 3).

A2.4 Live check of the transport (needs no Jev key): `target/debug/mini.exe --debug-stop; cmd.exe /c "tools\\win-cargo.cmd" build 2>&1 | grep -E "Finished|error"` then
```bash
cd tools/fixtures && python -m http.server 8000 > /dev/null 2>&1 &
cd ~/Desktop/mini-browser && target/debug/mini.exe --qa-spike http://127.0.0.1:8000/login.html
```
Expected: the existing five capability lines all `"ok":true`. Then extend `run_qa_spike` in main.rs with a sixth probe, after the viewport probe: set the phone viewport, then `eval("[navigator.maxTouchPoints, matchMedia('(pointer: coarse)').matches, /Android/.test(navigator.userAgent)]")` and print `{"capability":"touch","ok":<all three truthy>,"value":...}`. Expected line: `{"capability":"touch","ok":true,"value":[5,true,true]}`. If `ok` is false, stop: WebView2 refused the Emulation call; report which value failed.

A2.5 Commit: `qa: touch emulation + Android UA at mobile viewports; taps via Input.dispatchTouchEvent; per-viewport step shots`.

### A3. Tap-flow rules in code (the dogfood mobile checklist)

Files: `tools/qa-probe.js`, `src/qa/mobile.rs` (new), `src/qa/mod.rs`, `src/qa/agent.rs`.

Rules (both severity 2, so they fail the run; computed in code, never asked of Jev):
- R1 `scroll-lock-leaked`: after a step, body/html overflow is hidden, no dialog or large fixed layer is open, and the document is taller than the viewport. The page cannot scroll (the OhhLook out-of-order scroll-lock bug).
- R2 `overlay-across-navigation`: a step changed the path and an overlay that was open before the step is still open on the new page (the OhhLook "link inside the sheet does not close the sheet" bug).

A3.1 Failing unit tests. Create `src/qa/mobile.rs` with only the types and tests first:
```rust
//! Tap-flow facts for touch viewports, computed in code after every explored
//! step: the two rules that found the OhhLook overlay bugs (dogfood checklist).
use serde::Deserialize;

use crate::qa::{Finding, FindingKind, SeveritySource, Viewport};

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct MobileState {
    pub ok: bool,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub open_overlays: Vec<String>,
    #[serde(default)]
    pub scroll_locked: bool,
    #[serde(default)]
    pub doc_height: f64,
    #[serde(default)]
    pub inner_height: f64,
}

pub fn parse(raw: &str) -> Result<MobileState, String> {
    let s: MobileState = serde_json::from_str(raw).map_err(|e| format!("mobileState: {e}"))?;
    if !s.ok {
        return Err("mobileState: page returned ok:false".into());
    }
    Ok(s)
}

/// Rules over the state before and after one step. `shot` is the step screenshot
/// path relative to the out dir; it is the evidence.
pub fn judge(before: &MobileState, after: &MobileState, vp: &Viewport, step: usize, shot: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let page_taller = after.doc_height > after.inner_height + 1.0;
    if after.scroll_locked && after.open_overlays.is_empty() && page_taller {
        out.push(finding(
            vp,
            step,
            shot,
            "scroll-lock-leaked",
            "Scroll lock leaked: the page cannot scroll",
            format!(
                "After step {step} on {} the document is taller than the viewport ({:.0} > {:.0}) but body/html overflow is hidden and no dialog is open.",
                after.path, after.doc_height, after.inner_height
            ),
            None,
        ));
    }
    if before.path != after.path {
        if let Some(stale) = after.open_overlays.iter().find(|p| before.open_overlays.contains(p)) {
            out.push(finding(
                vp,
                step,
                shot,
                "overlay-across-navigation",
                "Overlay stayed open across a navigation",
                format!("Step {step} navigated {} -> {} but {stale} is still open on top of the new page.", before.path, after.path),
                Some(stale.clone()),
            ));
        }
    }
    out
}

fn finding(vp: &Viewport, step: usize, shot: &str, rule: &str, title: &str, detail: String, path: Option<String>) -> Finding {
    Finding {
        id: format!("{}-{rule}-{step}", vp.name),
        kind: FindingKind::Geometry,
        severity: 2,
        severity_source: SeveritySource::Code,
        viewport: vp.name.to_string(),
        title: title.to_string(),
        detail,
        dom_path: path,
        step: Some(step),
        blocked_by_firewall: false,
        evidence: vec![shot.to_string()],
        screenshot: Some(shot.to_string()),
        score: None,
        confidence: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phone() -> Viewport {
        crate::qa::VIEWPORTS[0]
    }

    fn state(path: &str, overlays: &[&str], locked: bool) -> MobileState {
        MobileState {
            ok: true,
            path: path.into(),
            open_overlays: overlays.iter().map(|s| s.to_string()).collect(),
            scroll_locked: locked,
            doc_height: 3000.0,
            inner_height: 844.0,
        }
    }

    #[test]
    fn a_leaked_scroll_lock_is_a_severity_2_fact() {
        let out = judge(&state("/", &["dialog"], true), &state("/", &[], true), &phone(), 3, "shots/phone-step-03.jpg");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "phone-scroll-lock-leaked-3");
        assert_eq!(out[0].severity, 2);
        assert_eq!(out[0].step, Some(3));
        assert_eq!(out[0].screenshot.as_deref(), Some("shots/phone-step-03.jpg"));
    }

    #[test]
    fn a_lock_with_an_open_dialog_is_normal() {
        assert!(judge(&state("/", &[], false), &state("/", &["dialog"], true), &phone(), 1, "s").is_empty());
    }

    #[test]
    fn an_overlay_that_survives_a_navigation_is_reported_once_with_its_path() {
        let out = judge(&state("/", &["div.sheet"], true), &state("/about", &["div.sheet"], true), &phone(), 2, "s");
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].id, "phone-overlay-across-navigation-2");
        assert_eq!(out[0].dom_path.as_deref(), Some("div.sheet"));
    }

    #[test]
    fn a_new_overlay_on_the_new_page_is_not_stale() {
        assert!(judge(&state("/", &[], false), &state("/about", &["div.cookie"], false), &phone(), 2, "s").is_empty());
    }

    #[test]
    fn parse_refuses_ok_false() {
        assert!(parse(r#"{"ok":false,"error":"x"}"#).is_err());
        assert_eq!(parse(r#"{"ok":true,"path":"/","open_overlays":[],"scroll_locked":false,"doc_height":1,"inner_height":2}"#).unwrap().path, "/");
    }
}
```
Add `pub mod mobile;` to `src/qa/mod.rs` (module list at the top) and in the mod.rs doc comment map add `//!   mobile.rs    tap-flow rules over before/after page state (pure).`.
Run `cmd.exe /c "tools\\win-cargo.cmd" test qa::mobile 2>&1 | grep -E "^test |^test result"`. Expected: 5 passed (these are pure tests; they pass as soon as they compile, which is the "green" of this sub-step. The integration test below is the red one).

A3.2 Failing integration test in agent.rs tests. Add to `FakeBrowser` the field `mobile_states: Mutex<Vec<String>>` (init empty) and in `fn payload`, before the final fallback:
```rust
            if js.contains("__miniQA.mobileState") {
                let mut q = self.mobile_states.lock();
                if q.len() > 1 {
                    return q.remove(0);
                }
                if let Some(last) = q.first() {
                    return last.clone();
                }
            }
```
Test:
```rust
    #[test]
    fn a_phone_step_that_leaks_a_scroll_lock_fails_the_run() {
        let mut browser = FakeBrowser::new("scroll-lock");
        *browser.mobile_states.lock() = vec![
            r#"{"ok":true,"path":"/","open_overlays":[],"scroll_locked":false,"doc_height":3000,"inner_height":844}"#.to_string(),
            r#"{"ok":true,"path":"/","open_overlays":[],"scroll_locked":true,"doc_height":3000,"inner_height":844}"#.to_string(),
        ];
        let mut judge = ScriptedJudge::with(&["CLICK", "DONE"]);
        judge.mutate_p = 0.02;
        let report = executed(&browser, &judge);
        let phone = report.viewports.iter().find(|v| v.viewport == "phone").unwrap();
        let fact = phone.findings.iter().find(|f| f.id == "phone-scroll-lock-leaked-1").expect("rule R1 fired");
        assert_eq!(fact.severity, 2);
        assert_eq!(fact.screenshot.as_deref(), Some("shots/phone-step-01.jpg"));
        let desktop = report.viewports.iter().find(|v| v.viewport == "desktop").unwrap();
        assert!(desktop.findings.iter().all(|f| !f.id.contains("scroll-lock")), "rules run on touch viewports only");
        assert_eq!(crate::qa::report::verdict(&report).code, 1);
    }
```
(`verdict(..).code`: check the `Verdict` struct in mod.rs:431 for the exact field name of the exit code and use it.) Run; expected: `panicked at 'rule R1 fired'`.

A3.3 Implement. `tools/qa-probe.js`: before the closing `})();` add
```js
  // Tap-flow state for src/qa/mobile.rs: what is open, whether the page can
  // scroll, and where we are. Large fixed layers count as overlays so a
  // bottom sheet without role=dialog does not look like a leaked lock.
  QA.mobileState = function () {
    try {
      const dialogs = [...document.querySelectorAll('[role="dialog"],dialog[open],[aria-modal="true"]')]
        .filter((el) => {
          const r = el.getBoundingClientRect();
          return r.width > 0 && r.height > 0 && dom.visible(el);
        })
        .map((el) => dom.cssPath(el));
      const layers = findFixedOverlays(document.body).map((item) => item.path);
      const hidden = (el) => !!el && ['hidden', 'clip'].includes(getComputedStyle(el).overflowY);
      return {
        ok: true,
        path: location.pathname + location.search,
        open_overlays: [...new Set([...dialogs, ...layers])],
        scroll_locked: hidden(document.body) || hidden(document.documentElement),
        doc_height: document.documentElement.scrollHeight,
        inner_height: innerHeight,
      };
    } catch (e) {
      return { ok: false, error: String((e && e.message) || e) };
    }
  };
```
`src/qa/agent.rs`:
- `struct Exploration` gets `facts: Vec<Finding>,`.
- `Pass` gets
```rust
    fn mobile_state(&mut self) -> Option<crate::qa::mobile::MobileState> {
        if !self.vp_mobile {
            return None;
        }
        let raw = self.browser.eval("window.__miniQA.mobileState()").ok()?;
        crate::qa::mobile::parse(&raw).ok()
    }
```
- `fn explore` gets a first parameter `vp: &Viewport` (call site: `pass.explore(vp, &goal_label, root.as_deref(), &observation)`), a local `let mut facts: Vec<Finding> = Vec::new();`, and in the step body: right before `let _ = self.browser.eval("window.__miniQA.arm()");` insert `let before = self.mobile_state();`; right after the `match self.snapshot(root) { ... }` block (still inside the `else` branch where the step acted) insert
```rust
                        if let (Some(before), Some(after)) = (before, self.mobile_state()) {
                            let shot = format!("shots/{}-step-{step:02}.jpg", self.vp_name);
                            for f in crate::qa::mobile::judge(&before, &after, vp, step, &shot) {
                                if !facts.iter().any(|g| g.title == f.title && g.dom_path == f.dom_path) {
                                    facts.push(f);
                                }
                            }
                        }
```
- return `Exploration { steps: history, stop, forbidden, refusals, facts }`, and at the call site after `facts.steps = exploration.steps;` add `facts.findings.extend(exploration.facts);`.
Run `cmd.exe /c "tools\\win-cargo.cmd" test qa:: 2>&1 | grep -E "^test result|FAILED|panicked"`. Expected: all ok, baseline + 11.

A3.4 Fixture for the live check. Create `tools/fixtures/mobile.html`:
```html
<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Mobile fixture</title>
<style>
  body { margin: 0; font-family: system-ui; }
  header { position: sticky; top: 0; background: #456796; color: #fff; padding: 12px; display: flex; justify-content: space-between; }
  button { font: inherit; min-width: 44px; min-height: 44px; }
  main { padding: 16px; }
  .tall { height: 2600px; background: linear-gradient(#eee, #999); }
  dialog { border: 0; padding: 24px; width: 80vw; }
  dialog::backdrop { background: rgba(0,0,0,.4); }
</style></head>
<body>
<header><strong>Fixture</strong><button id="menu" aria-label="Open menu">Menu</button></header>
<main>
  <h1>Mobile tap-flow fixture</h1>
  <p>Open the menu, then tap About. The bug: the sheet closes but the scroll lock stays.</p>
  <div class="tall"></div>
  <h2 id="about">About</h2>
</main>
<dialog id="sheet" aria-label="Menu">
  <p><a id="about-link" href="#about">About</a></p>
  <p><button id="close">Close</button></p>
</dialog>
<script>
  const sheet = document.getElementById('sheet');
  document.getElementById('menu').addEventListener('click', () => { sheet.showModal(); document.body.style.overflow = 'hidden'; });
  document.getElementById('close').addEventListener('click', () => { sheet.close(); document.body.style.overflow = ''; });
  // BUG under test: closes the dialog but never releases the scroll lock.
  document.getElementById('about-link').addEventListener('click', () => { sheet.close(); });
</script>
</body></html>
```
Live run (needs the Jev key, which Mini loads itself; never print it):
```bash
target/debug/mini.exe --debug-stop; cmd.exe /c "tools\\win-cargo.cmd" build 2>&1 | grep -E "Finished|error"
(cd tools/fixtures && python -m http.server 8000 > /dev/null 2>&1 &)
target/debug/mini.exe --qa http://127.0.0.1:8000/mobile.html --steps 6 > "$TMPDIR/qa-mobile.md"; echo "exit $?"
grep -E "^- |Scroll lock|step" "$TMPDIR/qa-mobile.md" | head -30
```
Expected: `exit 1`; the report lists `Scroll lock leaked: the page cannot scroll` under phone with screenshot `shots/phone-step-0N.jpg`; the desktop section has no such finding. If Jev never taps "About" inside the sheet within 6 steps, rerun with `--steps 10`; if it still never does, note it in the plan (the rule is unit-tested; the fixture only proves the wiring).

A3.5 Commit: `qa: tap-flow rules at touch viewports (scroll lock leaked, overlay across navigation) + mobile fixture`.

### A4. Report + docs

Files: `src/qa/report.rs`, `AGENTS.md`, `README.md`, `plans/README.md`, `plans/010-mobile-first-qa.md`, the Hermes skill `mini-browser`.

A4.1 Failing test (report.rs tests): 
```rust
    #[test]
    fn header_lists_viewports_in_run_order_with_explored_marks() {
        let mut phone = ok_viewport("phone");
        phone.explored = true;
        let r = base_report(vec![phone, ok_viewport("tablet")]);
        let md = render_md(&r);
        assert!(md.contains("- Viewports: phone (explored), tablet"), "{md}");
    }
```
Implement in `render_header` after the `- Steps budget` line:
```rust
    let viewports: Vec<String> = r
        .viewports
        .iter()
        .map(|v| if v.explored { format!("{} (explored)", v.viewport) } else { v.viewport.clone() })
        .collect();
    out.push_str(&format!("- Viewports: {}\n", viewports.join(", ")));
```
A4.2 Docs. `AGENTS.md` "QA runs" section: add `[--explore phone,desktop|phone|desktop|none]` to the usage line and a bullet:
"Viewports run phone, tablet, desktop. Jev explores (taps/clicks) at the viewports in `--explore` (default `phone,desktop`; `none` = probe-only). Touch viewports get touch emulation, an Android UA and `Input.dispatchTouchEvent` taps; two tap-flow rules (scroll lock leaked, overlay across navigation) are computed in code after each step and fail the run at severity 2. Step shots are `shots/<viewport>-step-NN.jpg`. The wall clock is 240 s + 120 s per extra explored viewport."
`README.md`: one line in the QA section with the same facts. `plans/README.md`: add row `| 010 | Mobile-first QA: phone first, touch, tap-flow rules; `--device` Android lane | P1 | L | 008 | A DONE / B TODO |`. Copy this file to `plans/010-mobile-first-qa.md` (status line updated). Skill `mini-browser` (`skill_manage patch`): replace the pitfall sentence "`--qa` explores controls only at desktop; at phone it probes layout only, so it misses tap-flow bugs." with "`--qa` explores at phone first (touch) and desktop; `--explore` changes it; tap-flow rules fail the run at severity 2."
A4.3 Run the full suite: `cmd.exe /c "tools\\win-cargo.cmd" test 2>&1 | grep -E "^test result|FAILED"`. Expected: all ok, baseline + 12, 0 failed.
A4.4 Commit: `qa(010): report lists viewports and explored marks; docs for phone-first QA`.

### A5. Live verification on a real app (report in the plan, no code)

```bash
cd ~/Desktop/ohh-look-frontend-next && (npx next dev --webpack -p 3000 > "$TMPDIR/next.log" 2>&1 &) ; sleep 25
cd ~/Desktop/mini-browser && target/debug/mini.exe --qa http://localhost:3000 --section "navigation" > "$TMPDIR/qa-ohhlook.md"; echo "exit $?"
grep -E "^- Viewports|^## |severity|Scroll lock|Overlay" "$TMPDIR/qa-ohhlook.md" | head -40
```
Expected: `- Viewports: phone (explored), tablet, desktop (explored)`, `shots/phone-step-01.jpg` exists in the artefact dir, total time under 6 min. Write the before/after finding counts (phone vs desktop) into `plans/010-mobile-first-qa.md`. A finding that is a false positive gets fixed in `mobile.rs` with a unit test before Phase B starts.

---

## Phase B: `--device` lane, real Chrome on Android

Requires Phase 0 (facts and the `mini-qa` AVD) and Phase A (taps, rules). Order: B1 -> B2 -> B3 -> B4 -> B5 -> B6.

### B1. `src/android.rs`: adb discovery, forward, reverse, Chrome launch, emulator boot

B1.1 Failing tests (pure parsers). Create `src/android.rs`:
```rust
//! Android devices over adb: discovery, DevTools port forward, Chrome launch,
//! emulator boot. Parsers are pure and tested; process calls are thin.
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdbDevice {
    pub serial: String,
    pub state: String,
    pub model: Option<String>,
}

/// `adb devices -l` output -> devices. The header and blank lines are skipped.
pub fn parse_devices(out: &str) -> Vec<AdbDevice> {
    out.lines()
        .skip_while(|l| !l.starts_with("List of devices"))
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let serial = parts.next()?.to_string();
            let state = parts.next()?.to_string();
            let model = parts.find_map(|p| p.strip_prefix("model:").map(str::to_string));
            Some(AdbDevice { serial, state, model })
        })
        .collect()
}

/// The device a `--device` value names: `auto` = first online device, `emulator` =
/// first online `emulator-*`, else an exact serial.
pub fn choose(devices: &[AdbDevice], want: &str) -> Result<AdbDevice, String> {
    let online: Vec<&AdbDevice> = devices.iter().filter(|d| d.state == "device").collect();
    let pick = match want {
        "auto" | "" => online.first().copied(),
        "emulator" => online.iter().find(|d| d.serial.starts_with("emulator-")).copied(),
        serial => online.iter().find(|d| d.serial == serial).copied(),
    };
    pick.cloned().ok_or_else(|| match want {
        "auto" | "" => "no online Android device (adb devices is empty); plug a phone with USB debugging on, or pass --device emulator".to_string(),
        "emulator" => "no running emulator; Mini boots the `mini-qa` AVD when MINI_AVD or the default AVD exists".to_string(),
        serial => format!("device {serial} is not online (adb devices)"),
    })
}

/// Port of a loopback URL (`http://localhost:3000/x` -> 3000), else None.
pub fn loopback_port(url: &str) -> Option<u16> {
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"))?;
    let authority = rest.split('/').next()?;
    let (host, port) = authority.rsplit_once(':')?;
    if host == "localhost" || host == "127.0.0.1" {
        port.parse().ok()
    } else {
        None
    }
}

pub fn sdk_root() -> Option<PathBuf> {
    for var in ["ANDROID_SDK_ROOT", "ANDROID_HOME"] {
        if let Ok(v) = std::env::var(var) {
            let p = PathBuf::from(v);
            if p.exists() {
                return Some(p);
            }
        }
    }
    let local = std::env::var("LOCALAPPDATA").ok()?;
    let p = PathBuf::from(local).join("Android").join("Sdk");
    p.exists().then_some(p)
}

fn adb_path() -> Result<PathBuf, String> {
    sdk_root()
        .map(|r| r.join("platform-tools").join("adb.exe"))
        .filter(|p| p.exists())
        .ok_or_else(|| "adb.exe not found: set ANDROID_SDK_ROOT or install platform-tools".to_string())
}

pub fn adb(args: &[&str]) -> Result<String, String> {
    let out = Command::new(adb_path()?).args(args).output().map_err(|e| format!("adb: {e}"))?;
    if !out.status.success() {
        return Err(format!("adb {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).replace('\r', ""))
}

pub fn devices() -> Result<Vec<AdbDevice>, String> {
    Ok(parse_devices(&adb(&["devices", "-l"])?))
}

pub fn chrome_present(serial: &str) -> Result<bool, String> {
    Ok(adb(&["-s", serial, "shell", "pm", "path", "com.android.chrome"])?.contains("package:"))
}

pub fn forward_devtools(serial: &str, port: u16) -> Result<(), String> {
    adb(&["-s", serial, "forward", &format!("tcp:{port}"), "localabstract:chrome_devtools_remote"]).map(|_| ())
}

pub fn reverse_port(serial: &str, port: u16) -> Result<(), String> {
    adb(&["-s", serial, "reverse", &format!("tcp:{port}"), &format!("tcp:{port}")]).map(|_| ())
}

pub fn launch_chrome(serial: &str, url: &str) -> Result<(), String> {
    adb(&[
        "-s", serial, "shell", "am", "start", "-W",
        "-n", "com.android.chrome/com.google.android.apps.chrome.Main",
        "-a", "android.intent.action.VIEW", "-d", url,
    ])
    .map(|_| ())
}

/// "Pixel 8 (emulator-5554), Android 16"
pub fn facts(serial: &str) -> String {
    let model = adb(&["-s", serial, "shell", "getprop", "ro.product.model"]).unwrap_or_default();
    let release = adb(&["-s", serial, "shell", "getprop", "ro.build.version.release"]).unwrap_or_default();
    format!("{} ({serial}), Android {}", model.trim(), release.trim())
}

/// Boots `avd` headless (MINI_EMU_WINDOW=1 shows the window) and returns its serial
/// once `sys.boot_completed` is 1.
pub fn boot_emulator(avd: &str, timeout: Duration) -> Result<String, String> {
    let emu = sdk_root().ok_or("no Android SDK")?.join("emulator").join("emulator.exe");
    let mut cmd = Command::new(emu);
    cmd.args(["-avd", avd, "-no-audio", "-no-boot-anim", "-gpu", "swiftshader_indirect"]);
    if std::env::var("MINI_EMU_WINDOW").ok().as_deref() != Some("1") {
        cmd.arg("-no-window");
    }
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0000_0008); // DETACHED_PROCESS: outlives this console
    }
    cmd.stdout(Stdio::null()).stderr(Stdio::null()).stdin(Stdio::null());
    cmd.spawn().map_err(|e| format!("emulator: {e}"))?;
    let start = Instant::now();
    loop {
        if let Ok(devs) = devices() {
            if let Some(d) = devs.iter().find(|d| d.serial.starts_with("emulator-") && d.state == "device") {
                let booted = adb(&["-s", &d.serial, "shell", "getprop", "sys.boot_completed"])
                    .map(|s| s.trim() == "1")
                    .unwrap_or(false);
                if booted {
                    return Ok(d.serial.clone());
                }
            }
        }
        if start.elapsed() > timeout {
            return Err(format!("emulator {avd} did not boot in {} s", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "List of devices attached\nemulator-5554          device product:sdk_gphone64_x86_64 model:Pixel_8 device:emu64xa transport_id:1\n1A2B3C4D       unauthorized transport_id:2\n\n";

    #[test]
    fn parses_adb_devices_l() {
        let d = parse_devices(LIST);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0], AdbDevice { serial: "emulator-5554".into(), state: "device".into(), model: Some("Pixel_8".into()) });
        assert_eq!(d[1].state, "unauthorized");
        assert!(parse_devices("List of devices attached\n\n").is_empty());
    }

    #[test]
    fn choose_auto_emulator_and_serial() {
        let d = parse_devices(LIST);
        assert_eq!(choose(&d, "auto").unwrap().serial, "emulator-5554");
        assert_eq!(choose(&d, "emulator").unwrap().serial, "emulator-5554");
        assert!(choose(&d, "1A2B3C4D").is_err(), "unauthorized is not online");
        assert!(choose(&[], "auto").unwrap_err().contains("no online Android device"));
    }

    #[test]
    fn loopback_port_only_for_localhost() {
        assert_eq!(loopback_port("http://localhost:3000/promotions"), Some(3000));
        assert_eq!(loopback_port("http://127.0.0.1:8000/mobile.html"), Some(8000));
        assert_eq!(loopback_port("https://ohhlook.co/"), None);
    }
}
```
Add `mod android;` to `src/main.rs` (next to the other `mod` lines). Run `cmd.exe /c "tools\\win-cargo.cmd" test android:: 2>&1 | grep -E "^test |^test result"`. Expected: 3 passed.
B1.2 Commit: `android: adb discovery, devtools forward, reverse, Chrome launch, emulator boot (plan 010)`.

### B2. `src/cdp_bus.rs`: event-capable CDP client

Why: `src/cdp.rs` drops event frames. The device lane needs `Fetch.requestPaused` answered at once (else every request hangs) and `Runtime.bindingCalled` lines from the recorder.

B2.1 Failing test with an in-process websocket server (tungstenite `accept`):
```rust
//! CDP over a websocket with events: one reader thread owns the socket, `call`
//! waits on a per-id channel, every event goes to the registered handlers.
//! The loopback debug window keeps using the simpler `cdp.rs` client.
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::TcpStream;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Duration;
use tungstenite::{Message, WebSocket};

type Handler = Arc<dyn Fn(&str, &Value, &CdpBus) + Send + Sync>;

const CALL_TIMEOUT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(50);

#[derive(Clone)]
pub struct CdpBus {
    socket: Arc<Mutex<WebSocket<TcpStream>>>,
    pending: Arc<Mutex<HashMap<u64, Sender<Value>>>>,
    next_id: Arc<Mutex<u64>>,
    handlers: Arc<Mutex<Vec<Handler>>>,
}

impl CdpBus {
    pub fn connect(ws_url: &str) -> Result<CdpBus, String> {
        let authority = ws_url
            .strip_prefix("ws://")
            .and_then(|rest| rest.split('/').next())
            .ok_or_else(|| format!("not a ws:// URL: {ws_url}"))?;
        let stream = TcpStream::connect(authority).map_err(|e| format!("CDP connect failed: {e}"))?;
        stream.set_read_timeout(Some(POLL)).map_err(|e| e.to_string())?;
        let (socket, _) = tungstenite::client(ws_url, stream).map_err(|e| format!("CDP handshake failed: {e}"))?;
        let bus = CdpBus {
            socket: Arc::new(Mutex::new(socket)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(Mutex::new(1)),
            handlers: Arc::new(Mutex::new(Vec::new())),
        };
        let reader = bus.clone();
        std::thread::Builder::new()
            .name("cdp-bus-reader".into())
            .spawn(move || reader.read_loop())
            .map_err(|e| format!("cdp-bus thread: {e}"))?;
        Ok(bus)
    }

    /// Register an event handler. Handlers run on the reader thread: they must not
    /// call `call` (deadlock); use `fire`.
    pub fn on_event(&self, f: impl Fn(&str, &Value, &CdpBus) + Send + Sync + 'static) {
        self.handlers.lock().push(Arc::new(f));
    }

    fn send_frame(&self, id: u64, method: &str, params: &Value) -> Result<(), String> {
        let frame = json!({ "id": id, "method": method, "params": params }).to_string();
        self.socket.lock().send(Message::Text(frame.into())).map_err(|e| format!("CDP send failed: {e}"))
    }

    /// Send and forget: the reply is dropped. Safe from inside an event handler.
    pub fn fire(&self, method: &str, params: Value) -> Result<(), String> {
        let id = self.take_id();
        self.send_frame(id, method, &params)
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.take_id();
        let (tx, rx) = channel();
        self.pending.lock().insert(id, tx);
        self.send_frame(id, method, &params)?;
        let reply = rx.recv_timeout(CALL_TIMEOUT).map_err(|_| {
            self.pending.lock().remove(&id);
            format!("cdp-timeout: {method} got no reply in {} s", CALL_TIMEOUT.as_secs())
        })?;
        if let Some(err) = reply.get("error") {
            return Err(format!("{method}: {}", err.get("message").and_then(Value::as_str).unwrap_or("CDP error")));
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }

    fn take_id(&self) -> u64 {
        let mut n = self.next_id.lock();
        let id = *n;
        *n += 1;
        id
    }

    fn read_loop(self) {
        loop {
            let msg = {
                let mut sock = self.socket.lock();
                sock.read()
            };
            match msg {
                Ok(Message::Text(text)) => self.dispatch(&text),
                Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => return,
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => return,
            }
        }
    }

    fn dispatch(&self, text: &str) {
        let Ok(v) = serde_json::from_str::<Value>(text) else { return };
        if let Some(id) = v.get("id").and_then(Value::as_u64) {
            if let Some(tx) = self.pending.lock().remove(&id) {
                let _ = tx.send(v);
            }
            return;
        }
        if let Some(method) = v.get("method").and_then(Value::as_str) {
            let params = v.get("params").cloned().unwrap_or(Value::Null);
            let handlers: Vec<Handler> = self.handlers.lock().clone();
            for h in handlers {
                h(method, &params, self);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A one-connection fake CDP server: on the first call it emits one event,
    /// then replies; on the second call it replies with an error.
    fn fake_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            let mut n = 0;
            while let Ok(Message::Text(text)) = ws.read() {
                let req: Value = serde_json::from_str(&text).unwrap();
                let id = req["id"].as_u64().unwrap();
                n += 1;
                if n == 1 {
                    ws.send(Message::Text(json!({"method":"Test.event","params":{"n":1}}).to_string().into())).unwrap();
                    ws.send(Message::Text(json!({"id":id,"result":{"value":42}}).to_string().into())).unwrap();
                } else {
                    ws.send(Message::Text(json!({"id":id,"error":{"message":"nope"}}).to_string().into())).unwrap();
                }
            }
        });
        format!("ws://{addr}/devtools/page/1")
    }

    #[test]
    fn calls_get_their_reply_and_events_reach_handlers() {
        let bus = CdpBus::connect(&fake_server()).unwrap();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = seen.clone();
        bus.on_event(move |m, p, _| sink.lock().push(format!("{m}:{}", p["n"])));
        let r = bus.call("Runtime.evaluate", json!({"expression": "1"})).unwrap();
        assert_eq!(r["value"], 42);
        let err = bus.call("Page.navigate", json!({})).unwrap_err();
        assert!(err.contains("nope"), "{err}");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(*seen.lock(), vec!["Test.event:1".to_string()]);
    }
}
```
Add `mod cdp_bus;` to `src/main.rs`. If `tungstenite::accept` is missing, the `handshake` feature is not enough for the server side: check `cargo tree`/the tungstenite docs; the fix is to keep the test but implement the fake server with a raw HTTP 101 upgrade by hand (sha1 of key + magic GUID, base64) — only if needed. Run `cmd.exe /c "tools\\win-cargo.cmd" test cdp_bus 2>&1 | grep -E "^test |^test result"`. Expected: 1 passed.
B2.2 Commit: `cdp_bus: event-capable CDP websocket client (reader thread, per-id replies, handlers)`.

### B3. `src/qa/remote.rs`: `RemoteBrowser` over `CdpBus`

B3.1 Preparations in other files:
- `src/main.rs`: move `const DEBUG_RECORDER_SCRIPT` to `src/debug.rs` as `pub const RECORDER_SCRIPT: &str = include_str!("../tools/recorder.js");` and use `debug::RECORDER_SCRIPT` at the two call sites (main.rs:506, 739).
- `src/debug.rs`: add `publish: bool` to `DebugRecorder` (true in `start()`), make `write_status` return at once when `!self.publish`, and add
```rust
    /// A recorder that writes runs under `session_dir` and never touches
    /// `current.json` (the device lane must not clobber a live debug window).
    pub fn start_private(session_dir: PathBuf) -> Option<DebugRecorder> {
        std::fs::create_dir_all(&session_dir).ok()?;
        let run = open_run(&session_dir, 0, "device", "")?;
        Some(DebugRecorder { session_dir, run_index: 1, run, started_ms: now_ms() as u64, publish: false })
    }
```
- Move `route_debug` + the `IpcCmd` fields it needs from `src/main.rs` into `src/debug.rs` as `pub fn route_line(rec: &mut DebugRecorder, line: &str)` (parse `{cmd, kind, label, incident, lines}` with a private serde struct `RecorderLine`; `debug-batch` recurses). `main.rs` keeps `IpcCmd` for its other verbs and calls `debug::route_line(rec, &body)` in the `"debug" | "debug-batch"` arm. Run the full suite after this move: expected unchanged counts.

B3.2 Create `src/qa/remote.rs`:
```rust
//! `Browser` over a remote Chromium (Chrome for Android through `adb forward`).
//! Same page scripts, recorder and firewall as the WebView2 bridge, carried by
//! CDP: `Page.addScriptToEvaluateOnNewDocument`, `Runtime.addBinding`
//! (`__miniPost`), `Fetch.enable`.
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cdp_bus::CdpBus;
use crate::debug::{self, DebugRecorder};
use crate::qa::firewall::{self, Blocked, FirewallPolicy};
use crate::qa::{Browser, Viewport, PAGE_SCRIPTS};

pub struct RemoteBrowser {
    bus: CdpBus,
    recorder: Arc<Mutex<Option<DebugRecorder>>>,
    blocked: Arc<Mutex<Vec<Blocked>>>,
    firewall: Result<(), String>,
    engine: String,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl RemoteBrowser {
    /// `device` is the human label ("Pixel 8 (emulator-5554), Android 16").
    pub fn connect(ws_url: &str, policy: FirewallPolicy, device: &str, session_dir: PathBuf) -> Result<RemoteBrowser, String> {
        let bus = CdpBus::connect(ws_url)?;
        let recorder = Arc::new(Mutex::new(DebugRecorder::start_private(session_dir)));
        let blocked = Arc::new(Mutex::new(Vec::new()));

        // Recorder lines: the page calls window.__miniPost(line).
        let rec = recorder.clone();
        let sink = blocked.clone();
        let policy = Arc::new(policy);
        bus.on_event(move |method, params, bus| match method {
            "Runtime.bindingCalled" if params["name"] == "__miniPost" => {
                if let Some(line) = params["payload"].as_str() {
                    if let Some(r) = rec.lock().as_mut() {
                        debug::route_line(r, line);
                    }
                }
            }
            "Fetch.requestPaused" => {
                let id = params["requestId"].as_str().unwrap_or("").to_string();
                let m = params.pointer("/request/method").and_then(Value::as_str).unwrap_or("GET").to_string();
                let url = params.pointer("/request/url").and_then(Value::as_str).unwrap_or("").to_string();
                if firewall::should_block(&policy, &m, &url) {
                    sink.lock().push(Blocked { ts_ms: now_ms(), method: m, url, mechanism: "fetch".into() });
                    let _ = bus.fire("Fetch.failRequest", json!({ "requestId": id, "errorReason": "Aborted" }));
                } else {
                    let _ = bus.fire("Fetch.continueRequest", json!({ "requestId": id }));
                }
            }
            _ => {}
        });

        bus.call("Page.enable", json!({}))?;
        bus.call("Runtime.enable", json!({}))?;
        bus.call("Runtime.addBinding", json!({ "name": "__miniPost" }))?;
        for script in std::iter::once(debug::RECORDER_SCRIPT).chain(PAGE_SCRIPTS.iter().copied()) {
            bus.call("Page.addScriptToEvaluateOnNewDocument", json!({ "source": script }))?;
        }
        // Fresh state, best effort: the device profile is not wiped like the QA profile.
        let _ = bus.call("Network.clearBrowserCookies", json!({}));
        let _ = bus.call("Network.clearBrowserCache", json!({}));
        let firewall = bus
            .call("Fetch.enable", json!({ "patterns": [{ "urlPattern": "*", "requestStage": "Request" }] }))
            .map(|_| ())
            .map_err(|e| format!("Fetch.enable refused on the device: {e}"));
        let product = bus
            .call("Browser.getVersion", json!({}))
            .ok()
            .and_then(|v| v["product"].as_str().map(str::to_string))
            .unwrap_or_else(|| "Chrome/unknown".into());
        let engine = format!("{product} on {device}");
        Ok(RemoteBrowser { bus, recorder, blocked, firewall, engine })
    }

    fn touch(&self, x: f64, y: f64) -> Result<(), String> {
        self.bus.call("Input.dispatchTouchEvent", json!({ "type": "touchStart", "touchPoints": [{ "x": x, "y": y }] }))?;
        self.bus.call("Input.dispatchTouchEvent", json!({ "type": "touchEnd", "touchPoints": [] }))?;
        Ok(())
    }
}

impl Browser for RemoteBrowser {
    fn eval(&self, js: &str) -> Result<String, String> {
        let v = self.bus.call("Runtime.evaluate", json!({ "expression": js, "returnByValue": true }))?;
        if let Some(ex) = v.get("exceptionDetails") {
            let text = ex.pointer("/exception/description").or_else(|| ex.get("text")).and_then(Value::as_str).unwrap_or("exception");
            return Err(format!("eval threw: {text}"));
        }
        Ok(v.pointer("/result/value").map(|x| x.to_string()).unwrap_or_else(|| "null".to_string()))
    }
    fn cdp(&self, method: &str, params: Value) -> Result<Value, String> {
        self.bus.call(method, params)
    }
    fn set_viewport(&self, _v: Viewport) -> Result<(), String> {
        Ok(()) // the device is the viewport
    }
    fn screenshot(&self, path: &Path) -> Result<(), String> {
        let v = self.bus.call("Page.captureScreenshot", json!({ "format": "jpeg", "quality": 80 }))?;
        let data = v.get("data").and_then(Value::as_str).ok_or("screenshot: no data")?;
        let bytes = crate::qa::bridge::base64_decode(data)?; // make that fn `pub(crate)`
        std::fs::write(path, bytes).map_err(|e| format!("screenshot: write failed: {e}"))
    }
    fn mouse_click(&self, x: f64, y: f64) -> Result<(), String> {
        self.touch(x, y)
    }
    fn tap(&self, x: f64, y: f64) -> Result<(), String> {
        self.touch(x, y)
    }
    fn key_text(&self, text: &str) -> Result<(), String> {
        self.bus.call("Input.insertText", json!({ "text": text })).map(|_| ())
    }
    fn run_label(&self, label: &str, incident: &str) -> Result<PathBuf, String> {
        let mut rec = self.recorder.lock();
        let rec = rec.as_mut().ok_or("recorder is off")?;
        rec.start_run(label, incident);
        Ok(rec.run_dir())
    }
    fn url(&self) -> Result<String, String> {
        self.eval("location.href").map(|raw| raw.trim().trim_matches('"').to_string())
    }
    fn blocked(&self) -> Vec<Blocked> {
        self.blocked.lock().clone()
    }
    fn firewall_status(&self) -> Result<(), String> {
        self.firewall.clone()
    }
    fn browser_version(&self) -> Option<String> {
        None
    }
    fn engine_label(&self) -> Option<String> {
        Some(self.engine.clone())
    }
}
```
Add to `trait Browser` in mod.rs: `fn engine_label(&self) -> Option<String> { None }` and in `execute` (agent.rs ~line 123) use it first: `let engine = match browser.engine_label() { Some(e) => e, None => match browser.browser_version() { ... existing ... } };`. Add `pub mod remote;` to mod.rs. Make `base64_decode` in bridge.rs `pub(crate)`.

B3.3 Test (unit, no device): the firewall handler and the binding route are testable through `CdpBus` with the fake server from B2. Add to `remote.rs` tests: a fake server that, after `Fetch.enable`, emits `Fetch.requestPaused` for `POST https://example.com/api` and asserts the next frame it receives is `Fetch.failRequest` with that `requestId`, and emits `Runtime.bindingCalled {name:"__miniPost", payload:"{\"cmd\":\"debug\",\"kind\":\"net\",\"t\":1}"}` and asserts `net.jsonl` in the run dir gets one line. The server replies `{"id":..,"result":{}}` to every other call (and `{"product":"Chrome/141.0.0.0"}` to `Browser.getVersion`). Session dir: `std::env::temp_dir().join(format!("mini-remote-{}", now_ms()))`. Expected: 1 test passing; `blocked()` returns one entry with `mechanism == "fetch"`.
B3.4 Commit: `qa(remote): RemoteBrowser over CdpBus: scripts via addScriptToEvaluateOnNewDocument, recorder via Runtime.addBinding, Fetch firewall`.

### B4. `--device` in `main.rs` + the `device` viewport

B4.1 `src/qa/mod.rs`: `QaConfig` gets `pub device: Option<String>,`;
```rust
/// The single viewport of a `--device` run: the real screen, nothing emulated.
pub const DEVICE_VIEWPORT: Viewport = Viewport { name: "device", width: 0, height: 0, dsf: 0.0, mobile: true };

impl QaConfig {
    pub fn viewports(&self) -> Vec<Viewport> {
        if self.device.is_some() { vec![DEVICE_VIEWPORT] } else { VIEWPORTS.to_vec() }
    }
}
```
`agent.rs`: the loop iterates `let viewport_list = cfg.viewports(); for (i, vp) in viewport_list.iter().enumerate()` (and the `VIEWPORTS.iter().skip(1)` on nav failure becomes `viewport_list.iter().skip(1)`). Failing test first (agent.rs tests): with `cfg.device = Some("emulator-5554".into())` and `cfg.explore = vec!["device".into()]`, `execute` yields exactly one `ViewportFacts` named `device`, `explored == true`, and `browser.viewports.lock()` is `["device"]`. Add `device: None,` to `config()` and to every `QaConfig {` literal.

B4.2 `src/main.rs`: `QaCliArgs.device: Option<String>`; in `parse_qa_args`: `"--device" => { out.device = Some(args.get(i + 1).cloned().unwrap_or_else(|| "auto".into())); i += 2; }`. In `run_qa`, validate: `--device` with `--allow-writes` -> exit 2 "service-worker writes are not intercepted on a device; --allow-writes is refused with --device". When `cli.device` is `Some`, default `explore` is `["device"]` and any other `--explore` value exits 2. After `cfg` is built: `if let Some(device) = cli.device.clone() { run_qa_device(cfg, &device); }` (before `profile::reset_qa_profile()`), with:
```rust
fn run_qa_device(cfg: qa::QaConfig, device: &str) -> ! {
    let fail = |msg: String| -> ! {
        eprintln!("mini --qa --device: {msg}");
        std::process::exit(2)
    };
    let serial = match android::devices().and_then(|d| android::choose(&d, device)) {
        Ok(d) => d.serial,
        Err(e) if device == "emulator" => {
            let avd = std::env::var("MINI_AVD").unwrap_or_else(|_| "mini-qa".to_string());
            eprintln!("mini --qa --device: {e}; booting AVD {avd} (headless; MINI_EMU_WINDOW=1 shows it)");
            android::boot_emulator(&avd, std::time::Duration::from_secs(240)).unwrap_or_else(|e| fail(e))
        }
        Err(e) => fail(e),
    };
    match android::chrome_present(&serial) {
        Ok(true) => {}
        Ok(false) => fail(format!("Chrome is not installed on {serial}: use a google_apis_playstore AVD or a phone with Chrome")),
        Err(e) => fail(e),
    }
    if let Some(port) = android::loopback_port(&cfg.url) {
        android::reverse_port(&serial, port).unwrap_or_else(|e| fail(e));
    }
    android::launch_chrome(&serial, "about:blank").unwrap_or_else(|e| fail(e));
    let port: u16 = std::env::var("MINI_DEVICE_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(9333);
    android::forward_devtools(&serial, port).unwrap_or_else(|e| fail(e));
    std::thread::sleep(std::time::Duration::from_millis(800));
    let ws = cdp::page_ws_url(port).unwrap_or_else(|e| fail(e));
    let policy = qa::firewall::FirewallPolicy { allow_writes: false, host: cfg.host.clone() };
    let session = cfg.out_dir.join("device-session");
    let label = android::facts(&serial);
    let browser = qa::remote::RemoteBrowser::connect(&ws, policy, &label, session).unwrap_or_else(|e| fail(e));
    let judge = qa::jev::JevClient::new(cfg.api_key.clone().expect("checked above"), cfg.model.clone());
    std::process::exit(qa::agent::run(&cfg, &browser, &judge));
}
```
`cdp::page_ws_url` picks the first `type == "page"` target; Chrome lists the newest tab first, which is the `about:blank` tab just opened. If a wrong tab is picked (URL not `about:blank`), add `pick_target_with_url(list, "about:blank")` to `cdp.rs` with a unit test and use it.

B4.3 Report: `QaReport` gets `pub device: Option<String>` (set from `cfg.device`); `render_header` prints `- Device: {label}` when set and swaps the coverage note for: "> Coverage note: this report comes from a single automated pass in Chrome for Android on the device above: visual, DOM and network facts only. Service-worker requests are not intercepted by the write firewall." Failing test first in report.rs (`header_names_the_device_lane`).

B4.4 Full suite: `cmd.exe /c "tools\\win-cargo.cmd" test 2>&1 | grep -E "^test result|FAILED"`. Expected: all ok, 0 failed.
B4.5 Commit: `qa: --device auto|emulator|<serial> runs the QA loop in Chrome for Android (plan 010 B)`.

### B5. Live verification on the emulator and (if available) a phone

```bash
target/debug/mini.exe --debug-stop; cmd.exe /c "tools\\win-cargo.cmd" build 2>&1 | grep -E "Finished|error"
(cd tools/fixtures && python -m http.server 8000 > /dev/null 2>&1 &)
target/debug/mini.exe --qa http://127.0.0.1:8000/mobile.html --device emulator --steps 6 > "$TMPDIR/qa-device.md"; echo "exit $?"
grep -E "^- (Device|Engine|Viewports)|Scroll lock|^## " "$TMPDIR/qa-device.md"
```
Expected: if no emulator runs, stderr says `booting AVD mini-qa`; `exit 1`; header `- Device: Pixel 8 (emulator-5554), Android 16`, `- Engine: Chrome/1xx... on Pixel 8 (emulator-5554), Android 16`, `- Viewports: device (explored)`; the scroll-lock finding appears; `shots/device.jpg` and `shots/device-step-01.jpg` exist in the artefact dir; `runs/run-001-device/frontend.jsonl` is non-empty (recorder lines arrived through the binding).
Then OhhLook: `target/debug/mini.exe --qa http://localhost:3000 --device emulator --section "navigation"` with `next dev` running. Expected: a report; the page loaded through `adb reverse` (no "Chromium error page" nav failure). Record times and finding counts in `plans/010-mobile-first-qa.md`. With a phone on USB: the same command with `--device auto`.

### B6. Docs + skill
- `AGENTS.md`: a `# Device QA: mini --qa <url> --device` section (prereqs: SDK at `%LOCALAPPDATA%\Android\Sdk` or `ANDROID_SDK_ROOT`; AVD `mini-qa` from Phase 0; `--device auto|emulator|<serial>`; `MINI_AVD`, `MINI_EMU_WINDOW=1`, `MINI_DEVICE_PORT`; loopback URLs are reversed with `adb reverse`; writes always refused; cookies cleared via CDP; service-worker limit).
- `README.md`: one paragraph. `plans/README.md`: row 010 status. Skill `mini-browser`: a "Phone QA on a real Android (Chrome)" bullet with the one-line command and the Phase 0 one-time setup pointer.
- Commit: `docs(010): device QA lane`.

---

## Tests / validation summary

| Task | Red test | Green command | Expected |
|---|---|---|---|
| A1 | `exploration_runs_at_every_listed_viewport_and_marks_it`, `explore_none_keeps_every_viewport_probe_only` | `cmd.exe /c "tools\\win-cargo.cmd" test qa::` | baseline + 2 |
| A2 | `mobile_viewports_tap_and_desktop_clicks`, `step_screenshots_carry_the_viewport_name`, `phone_ua_is_android_mobile_and_desktop_restores_the_original` | same | baseline + 5; `--qa-spike` touch line `ok:true` |
| A3 | 5 pure tests in `qa::mobile`, `a_phone_step_that_leaks_a_scroll_lock_fails_the_run` | same | baseline + 11; live fixture run exit 1 with the scroll-lock finding |
| A4 | `header_lists_viewports_in_run_order_with_explored_marks` | full suite | baseline + 12 |
| B1 | 3 tests in `android::tests` | `test android::` | +3 |
| B2 | `calls_get_their_reply_and_events_reach_handlers` | `test cdp_bus` | +1 |
| B3 | remote firewall + binding test | `test qa::remote` | +1 |
| B4 | device viewport test, `header_names_the_device_lane` | full suite | +2; live emulator run per B5 |

Live runs are the final gate for each phase (A3.4/A5, B5). A live result is reported with exactly what was and was not verified (no Jev key = model path untested; no phone = emulator only).

## Risks, tradeoffs, open questions

- Jev cost/time: `phone,desktop` runs two explorations (about 2x Jev calls, +60-120 s). Default wall clock grows to 360 s. If David wants the old cost, `--explore phone` keeps one exploration (and phone is the one that matters).
- Severity 2 for the two tap-flow rules fails the run. A non-ARIA bottom sheet that locks scroll could trip R1; the probe counts large fixed layers as overlays to reduce this. First false positive = fix with a unit test, not a severity drop.
- WebView2's in-process CDP must accept `Emulation.setTouchEmulationEnabled` / `setUserAgentOverride`. The A2.4 spike line proves it; if refused, taps still dispatch real touch events and the UA stays desktop (report notes it).
- Phase 0 is manual once: a 1.3 GB image download and tapping through Chrome's first-run screens in the emulator window. Afterwards the lane is hands-free (`--device emulator` boots `mini-qa` headless).
- The emulator's Chrome is whatever the image ships; Play Store can update it (needs a Google login once, optional). A USB phone avoids this and is faster.
- Device lane limits (documented in the report note): service-worker requests bypass `Fetch`, the device profile is only cookie/cache-cleared (not wiped), screenshots are the visual viewport, `--allow-writes` is refused with `--device`.
- `DebugRecorder` was single-owner on the UI thread; `start_private` + `Arc<Mutex<..>>` in `RemoteBrowser` keeps the write path single-threaded (the reader thread writes, the QA thread only rotates runs under the same lock).
- Out of scope (YAGNI, note for later): `--move --device` (Jev navigation on the phone: nearly free once `RemoteBrowser` exists, by pointing `move_cmd` at the forwarded port), screencast on device, several devices in parallel, iOS (Safari needs macOS).

Open questions for David (defaults chosen so work can start without answers):
1. Keep `--explore phone,desktop` as the default (2x Jev), or `phone` only? Default in this plan: `phone,desktop`.
2. Is a physical Android phone available for USB debugging? If yes, B5 runs on it too.
3. Should `--qa` with `--device` be the default once B lands for OhhLook mobile checks? Not in this plan; decide after B5 numbers.

---

## Progress log (2026-10-20)

- A0: branch `feat/qa-mobile-first` from b372318. Baseline full suite: `283 passed; 0 failed; 2 ignored` (`qa::` filter: 113).
- Phase 0 DONE (hands-on, 25 min): image `system-images;android-36;google_apis_playstore;x86_64` installed (2.3 GB on disk). AVD `mini-qa` = pixel_8, `hw.keyboard=yes`, `hw.gpu.enabled=yes`, `PlayStore.enabled=yes`. Cold boot 123 s (WHPX, `-gpu auto`). Chrome 133.0.6943.137 preinstalled (`/data/app/.../com.android.chrome`), Android 16, build type `user` (no root). First-run dismissed once with one tap on "Use without an account" (`adb shell input tap 540 2084`); no other prompts. `adb forward tcp:9333 localabstract:chrome_devtools_remote` -> `/json/version` Browser `Chrome/133.0.6943.137`, Protocol 1.3; `/json/list` lists page targets with `webSocketDebuggerUrl ws://127.0.0.1:9333/devtools/page/<id>`. `adb reverse tcp:8000 tcp:8000` + `am start ... -d http://localhost:8000/login.html` loaded the host fixture (title "Fixture login"). Closed with `adb emu kill` (snapshot `default_boot` saved). Note for B1: `ro.product.model` on the emulator is `sdk_gphone64_x86_64`, not `Pixel_8`; `facts()` should fall back to `ro.product.name`/AVD name for the label. Warm boot time: measure in B5.
- A1 DONE: commit 41025ac. Full suite 285 passed (+2). Deviations: (1) `src/qa/report.rs` test literals needed `explored: false` (3 places); (2) `src/main.rs` spike_checks indexed VIEWPORTS by position, now looks up by name (`viewport_named`); (3) `--explore` parsing has no committed unit test (main.rs has no tests module); (4) design gap found: the loop navigated only at i == 0, so after phone exploration the tablet probe and desktop pass saw the post-exploration page -> fixed in A2 (navigate per viewport); (5) `explored` was set even when exploration was skipped on a forbidden page -> fixed in A2.
- A2 DONE: commit d47f930. Full suite 296 passed (+11). `--qa-spike` now six lines; touch line ok:true (maxTouchPoints 5, pointer coarse, Android UA at phone; desktop restored). Deviations: (1) desktop UA restore = `Emulation.setUserAgentOverride {userAgent:""}` (clears the override and brings back real client hints); the plan's "restore original UA string" left `userAgentData.brands` empty, so `user_agent_for(v, major)` has 2 args and no `original_ua` field; (2) `Page.navigate` per viewport empties the page-side section registry, so `--section` runs now call `find_root` again on each later viewport (one extra Jev section question per viewport); (3) bridge.rs had no tests module before; created; (4) AGENTS.md still says "five transport capabilities" -> fix in A4.
- A3 DONE: commit 2983ba0. Full suite 303 passed (+7; one extra dedupe test). Live: on `http://127.0.0.1/mobile.html` (port-less) exit 1 with `[2] geometry (phone, step 2) Scroll lock leaked ... evidence shots/phone-step-02.jpg`; R2 proven on a throwaway pushState fixture (`Overlay stayed open across a navigation ... div#sheet`). On `:8000` the rule did not fire because of an OLD bug: `firewall::is_offsite_link` compared `href_host` (port included) with the bare QA host, so every same-site link on a ported host (localhost:3000 too) was dropped and never offered to Jev.
- Firewall fix: commit 012e34a (`strip_port` in `is_offsite_link` + test). Full suite 304 passed. With it, the plan-literal `:8000` run gives the same finding at phone step 2.

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

fn now_ms() -> u64 {
    crate::qa::epoch_ms()
}

/// This machine's clock moved onto the device's, given `offset` = device minus this machine.
fn on_device_clock(offset: i64) -> u64 {
    (now_ms() as i64 + offset).max(0) as u64
}

/// Device clock minus this machine's clock, in ms. Of five readings the one with the shortest
/// round trip wins: the device read its clock about half a round trip after the request left.
/// The page's own timestamps (the recorder's `t`) are on the device clock; an emulator restored
/// from a snapshot was measured 3.9 s behind this machine. 0 when the device does not answer.
fn measure_clock_offset(bus: &CdpBus) -> i64 {
    (0..5)
        .filter_map(|_| {
            let sent = now_ms();
            let reply = bus.call("Runtime.evaluate", json!({ "expression": "Date.now()", "returnByValue": true })).ok()?;
            let received = now_ms();
            let device = reply.pointer("/result/value")?.as_f64()? as i64;
            Some((received.saturating_sub(sent), device - ((sent + received) / 2) as i64))
        })
        .min_by_key(|(round_trip, _)| *round_trip)
        .map(|(_, offset)| offset)
        .unwrap_or(0)
}

/// Chrome for Android confirms a tap only after it has ruled out a double tap, so it answers
/// `touchEnd` ~250 ms before the page sees the click (measured on Chrome 133 in a fresh tab; a
/// tab that has been open a while answers in ~40 ms). `touch` arms this one-shot flag, then waits
/// for it inside the page (`wait_for_click_js`): the caller reads the page after the tap, not
/// before it.
const ARM_CLICK_WATCH: &str = "(() => { window.__miniClick = false; window.addEventListener('click', () => { window.__miniClick = true; }, { capture: true, once: true }); })()";
/// How long a tap waits for its click. A tap the page cancels (`preventDefault` on `touchend`)
/// has no click and costs the whole wait.
const CLICK_WAIT_MS: u32 = 1000;

/// A promise that resolves `true` when the armed click lands, `false` after `ms` (or at once in a
/// document that was never armed: the click navigated the page).
fn wait_for_click_js(ms: u32) -> String {
    format!(
        "new Promise((done) => {{ const end = performance.now() + {ms}; const poll = () => {{ \
         if (window.__miniClick !== false || performance.now() >= end) done(window.__miniClick === true); \
         else setTimeout(poll, 10); }}; poll(); }})"
    )
}

/// A Chrome (or other Chromium) page driven over one CDP websocket. Built by `connect`;
/// every `Browser` method is a CDP call on that socket.
pub struct RemoteBrowser {
    bus: CdpBus,
    recorder: Arc<Mutex<Option<DebugRecorder>>>,
    blocked: Arc<Mutex<Vec<Blocked>>>,
    firewall: Result<(), String>,
    engine: String,
    clock_offset_ms: i64,
}

impl RemoteBrowser {
    /// `device` is the human label ("Pixel 8 (emulator-5554), Android 16"); the report
    /// header shows it after the Chrome version. The recorder writes its runs under
    /// `session_dir` and never touches the debug window's `current.json`.
    pub fn connect(ws_url: &str, policy: FirewallPolicy, device: &str, session_dir: PathBuf) -> Result<RemoteBrowser, String> {
        let bus = CdpBus::connect(ws_url)?;
        let clock_offset_ms = measure_clock_offset(&bus);
        let recorder = Arc::new(Mutex::new(DebugRecorder::start_private(session_dir)));
        let blocked = Arc::new(Mutex::new(Vec::new()));

        // Recorder lines: the page calls window.__miniPost(line). Blocked writes: every request
        // is paused (`Fetch.enable` below) until this handler fails or continues it.
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
                    sink.lock().push(Blocked { ts_ms: on_device_clock(clock_offset_ms), method: m, url, mechanism: "fetch".into() });
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
        Ok(RemoteBrowser { bus, recorder, blocked, firewall, engine, clock_offset_ms })
    }

    /// Device clock minus this machine's clock, in ms (measured once by `connect`).
    pub fn clock_offset_ms(&self) -> i64 {
        self.clock_offset_ms
    }

    fn touch(&self, x: f64, y: f64) -> Result<(), String> {
        // The watch is best effort: a page that cannot run it (mid-navigation) is no reason to
        // refuse the tap, and a click that navigates destroys the context the wait runs in.
        let _ = self.bus.call("Runtime.evaluate", json!({ "expression": ARM_CLICK_WATCH, "returnByValue": true }));
        self.bus.call("Input.dispatchTouchEvent", json!({ "type": "touchStart", "touchPoints": [{ "x": x, "y": y }] }))?;
        self.bus.call("Input.dispatchTouchEvent", json!({ "type": "touchEnd", "touchPoints": [] }))?;
        let wait = json!({ "expression": wait_for_click_js(CLICK_WAIT_MS), "returnByValue": true, "awaitPromise": true });
        let _ = self.bus.call("Runtime.evaluate", wait);
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
        // The JSON text of the value, like the WebView2 bridge ("complete" keeps its quotes).
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
        let bytes = crate::qa::bridge::base64_decode(data)?;
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
        None // `engine_label` carries the version
    }
    fn engine_label(&self) -> Option<String> {
        Some(self.engine.clone())
    }
    fn clock_ms(&self) -> u64 {
        on_device_clock(self.clock_offset_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    use tungstenite::{Message, WebSocket};

    const DEVICE: &str = "Pixel 8 (emulator-5554), Android 16";
    /// How far ahead of this machine the fake device's clock runs (an emulator restored from a
    /// snapshot was measured 3.9 s behind; either sign must work).
    const DEVICE_CLOCK_SKEW_MS: i64 = 4_000;
    /// The recorder line the fake device posts while `connect` runs.
    const FIRST_LINE: &str = r#"{"cmd":"debug","kind":"net","t":1}"#;

    /// A one-connection fake Chrome for Android. It records every frame it receives and
    /// answers `{}` to everything except the calls named in the `match` in `fake_device`.
    struct FakeDevice {
        ws_url: String,
        frames: Arc<Mutex<Vec<Value>>>,
        /// While set, an `awaitPromise` evaluation fails the way one does when the click it waits
        /// for navigates the page: the execution context is destroyed.
        context_destroyed: Arc<AtomicBool>,
    }

    impl FakeDevice {
        fn frames(&self) -> Vec<Value> {
            self.frames.lock().clone()
        }

        fn methods(&self) -> Vec<String> {
            self.frames().iter().map(|f| f["method"].as_str().unwrap_or("").to_string()).collect()
        }
    }

    fn send(ws: &mut WebSocket<TcpStream>, frame: Value) {
        ws.send(Message::Text(frame.to_string().into())).unwrap();
    }

    fn event(method: &str, params: Value) -> Value {
        json!({ "method": method, "params": params })
    }

    fn paused(request_id: &str, method: &str, url: &str) -> Value {
        event(
            "Fetch.requestPaused",
            json!({ "requestId": request_id, "request": { "url": url, "method": method, "headers": {} }, "resourceType": "Fetch" }),
        )
    }

    fn binding(name: &str, payload: &str) -> Value {
        event("Runtime.bindingCalled", json!({ "name": name, "payload": payload, "executionContextId": 1 }))
    }

    /// What `Runtime.evaluate` answers for the expressions the tests use.
    fn evaluate(expression: &str) -> Value {
        match expression {
            "document.readyState" => json!({ "result": { "type": "string", "value": "complete" } }),
            "1 + 1" => json!({ "result": { "type": "number", "value": 2, "description": "2" } }),
            "Date.now()" => json!({ "result": { "type": "number", "value": now_ms() as i64 + DEVICE_CLOCK_SKEW_MS } }),
            "({ a: [1, 2] })" => json!({ "result": { "type": "object", "value": { "a": [1, 2] } } }),
            "location.href" => json!({ "result": { "type": "string", "value": "http://127.0.0.1:3000/mobile.html" } }),
            "boom()" => {
                let error = json!({
                    "type": "object", "subtype": "error", "className": "ReferenceError",
                    "description": "ReferenceError: boom is not defined\n    at <anonymous>:1:1"
                });
                json!({
                    "result": error,
                    "exceptionDetails": { "exceptionId": 1, "text": "Uncaught", "lineNumber": 0, "columnNumber": 0, "exception": error }
                })
            }
            _ => json!({ "result": { "type": "undefined" } }),
        }
    }

    /// With `refuse_fetch` the device answers `Fetch.enable` with a CDP error.
    fn fake_device(refuse_fetch: bool) -> FakeDevice {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let frames = Arc::new(Mutex::new(Vec::<Value>::new()));
        let log = frames.clone();
        let context_destroyed = Arc::new(AtomicBool::new(false));
        let destroyed = context_destroyed.clone();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            while let Ok(Message::Text(text)) = ws.read() {
                let req: Value = serde_json::from_str(&text).unwrap();
                log.lock().push(req.clone());
                let id = req["id"].clone();
                let params = &req["params"];
                let result = match req["method"].as_str().unwrap_or("") {
                    "Fetch.enable" if refuse_fetch => {
                        let error = json!({ "code": -32601, "message": "'Fetch.enable' wasn't found" });
                        send(&mut ws, json!({ "id": id, "error": error }));
                        continue;
                    }
                    "Fetch.enable" => {
                        // Chrome pauses requests as soon as Fetch is on. Both pauses and a recorder
                        // line reach the client before this reply, in this order.
                        send(&mut ws, paused("r1", "POST", "https://example.com/api"));
                        send(&mut ws, paused("r2", "GET", "https://example.com/app.js"));
                        send(&mut ws, binding("__miniPost", FIRST_LINE));
                        json!({})
                    }
                    // Stands for the page calling a binding: the event goes out, then the reply.
                    "Test.postBindingLine" => {
                        send(&mut ws, binding(params["name"].as_str().unwrap_or("__miniPost"), params["payload"].as_str().unwrap()));
                        json!({})
                    }
                    "Browser.getVersion" => json!({ "protocolVersion": "1.3", "product": "Chrome/141.0.0.0" }),
                    "Runtime.evaluate" if params["awaitPromise"] == true && destroyed.load(Ordering::SeqCst) => {
                        let error = json!({ "code": -32000, "message": "Execution context was destroyed." });
                        send(&mut ws, json!({ "id": id, "error": error }));
                        continue;
                    }
                    "Runtime.evaluate" => evaluate(params["expression"].as_str().unwrap_or("")),
                    "Page.captureScreenshot" => json!({ "data": "anBlZw==" }), // base64 of "jpeg"
                    _ => json!({}),
                };
                send(&mut ws, json!({ "id": id, "result": result }));
            }
        });
        FakeDevice { ws_url: format!("ws://{addr}/devtools/page/1"), frames, context_destroyed }
    }

    /// Poll `cond` every 20 ms, up to 2 s.
    fn wait_until(what: &str, cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn read_lines(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path).unwrap_or_default().lines().map(str::to_string).collect()
    }

    /// A `RemoteBrowser` for example.com on `device`. The caller removes the session dir.
    fn open(device: &FakeDevice, tag: &str) -> (RemoteBrowser, PathBuf) {
        let session = std::env::temp_dir().join(format!("mini-remote-{tag}-{}-{}", std::process::id(), now_ms()));
        let policy = FirewallPolicy { allow_writes: false, host: "example.com".to_string() };
        let browser = RemoteBrowser::connect(&device.ws_url, policy, DEVICE, session.clone()).expect("connects to the fake device");
        (browser, session)
    }

    #[test]
    fn a_write_is_failed_and_a_read_continues() {
        let device = fake_device(false);
        let (browser, session) = open(&device, "firewall");
        // Both pauses are answered, in order, before `connect` sends its next call: the reader
        // thread answers each event before it reads the next frame.
        let methods = device.methods();
        let at = methods.iter().position(|m| m == "Fetch.enable").expect("Fetch.enable was sent");
        assert_eq!(methods[at..], ["Fetch.enable", "Fetch.failRequest", "Fetch.continueRequest", "Browser.getVersion"]);
        let frames = device.frames();
        assert_eq!(frames[at + 1]["params"], json!({ "requestId": "r1", "errorReason": "Aborted" }));
        assert_eq!(frames[at + 2]["params"], json!({ "requestId": "r2" }));
        let blocked = browser.blocked();
        assert_eq!(blocked.len(), 1, "{blocked:?}");
        assert_eq!(blocked[0].method, "POST");
        assert_eq!(blocked[0].url, "https://example.com/api");
        assert_eq!(blocked[0].mechanism, "fetch");
        // Stamped on the device's clock, the one the page's own recorder lines use.
        let device_now = now_ms() as i64 + DEVICE_CLOCK_SKEW_MS;
        assert!((blocked[0].ts_ms as i64 - device_now).abs() < 2_000, "{} vs the device's {device_now}", blocked[0].ts_ms);
        assert_eq!(browser.firewall_status(), Ok(()));
        let _ = std::fs::remove_dir_all(&session);
    }

    #[test]
    fn a_device_that_refuses_fetch_leaves_the_firewall_unarmed_and_says_why() {
        let device = fake_device(true);
        let (browser, session) = open(&device, "nofetch");
        let err = browser.firewall_status().unwrap_err();
        assert!(err.contains("Fetch.enable refused on the device"), "{err}");
        assert!(err.contains("wasn't found"), "the device's own reason is kept: {err}");
        assert!(browser.blocked().is_empty());
        let _ = std::fs::remove_dir_all(&session);
    }

    #[test]
    fn connect_installs_the_binding_the_recorder_and_the_page_scripts_before_it_arms_fetch() {
        let device = fake_device(false);
        let (_browser, session) = open(&device, "setup");
        let frames = device.frames();
        let methods = device.methods();
        let first = |m: &str| methods.iter().position(|x| x == m).unwrap_or_else(|| panic!("{m} was never sent: {methods:?}"));
        assert_eq!(frames[first("Runtime.addBinding")]["params"], json!({ "name": "__miniPost" }));
        let scripts: Vec<&str> = frames
            .iter()
            .filter(|f| f["method"] == "Page.addScriptToEvaluateOnNewDocument")
            .map(|f| f["params"]["source"].as_str().expect("a script source"))
            .collect();
        let expected: Vec<&str> = std::iter::once(debug::RECORDER_SCRIPT).chain(PAGE_SCRIPTS.iter().copied()).collect();
        assert!(scripts == expected, "the recorder, then every page script in order ({} sent, {} expected)", scripts.len(), expected.len());
        let fetch = first("Fetch.enable");
        let last_script = methods.iter().rposition(|m| m == "Page.addScriptToEvaluateOnNewDocument").unwrap();
        assert!(last_script < fetch, "Fetch is armed after the scripts: {methods:?}");
        assert_eq!(frames[fetch]["params"], json!({ "patterns": [{ "urlPattern": "*", "requestStage": "Request" }] }));
        assert!(first("Network.clearBrowserCookies") < fetch);
        assert!(first("Network.clearBrowserCache") < fetch);
        let _ = std::fs::remove_dir_all(&session);
    }

    #[test]
    fn the_engine_label_names_chrome_and_the_device() {
        let device = fake_device(false);
        let (browser, session) = open(&device, "engine");
        assert_eq!(browser.engine_label().as_deref(), Some("Chrome/141.0.0.0 on Pixel 8 (emulator-5554), Android 16"));
        assert_eq!(browser.browser_version(), None, "the label carries the version");
        let _ = std::fs::remove_dir_all(&session);
    }

    #[test]
    fn recorder_lines_posted_through_the_binding_land_in_the_current_run() {
        let device = fake_device(false);
        let (browser, session) = open(&device, "binding");
        // The line posted while `connect` ran went to the run the private recorder opened.
        let first_run = session.join("run-000-device").join("net.jsonl");
        wait_until("the first recorder line", || read_lines(&first_run) == [FIRST_LINE]);
        // `run_label` rotates; the next line lands in the new run, not the old one.
        let run = browser.run_label("device", "").expect("run_label");
        assert!(run.ends_with("run-001-device"), "{}", run.display());
        let second = r#"{"cmd":"debug","kind":"net","t":2}"#;
        browser.cdp("Test.postBindingLine", json!({ "payload": second })).expect("the device posts a line");
        wait_until("the second recorder line", || read_lines(&run.join("net.jsonl")) == [second]);
        assert_eq!(read_lines(&first_run), [FIRST_LINE], "the first run keeps only its own line");
        // A binding that is not the recorder's is ignored.
        let other = json!({ "name": "somethingElse", "payload": r#"{"cmd":"debug","kind":"net","t":3}"# });
        browser.cdp("Test.postBindingLine", other).expect("the device posts another binding");
        assert_eq!(read_lines(&run.join("net.jsonl")), [second]);
        let _ = std::fs::remove_dir_all(&session);
    }

    #[test]
    fn eval_returns_the_value_as_json_text_and_a_thrown_error_is_an_err() {
        let device = fake_device(false);
        let (browser, session) = open(&device, "eval");
        assert_eq!(browser.eval("document.readyState").unwrap(), r#""complete""#);
        assert_eq!(browser.eval("1 + 1").unwrap(), "2");
        assert_eq!(browser.eval("({ a: [1, 2] })").unwrap(), r#"{"a":[1,2]}"#);
        assert_eq!(browser.eval("void 0").unwrap(), "null", "no value reads as null");
        let err = browser.eval("boom()").unwrap_err();
        assert!(err.contains("ReferenceError: boom is not defined"), "{err}");
        assert_eq!(browser.url().unwrap(), "http://127.0.0.1:3000/mobile.html");
        let sent = device
            .frames()
            .into_iter()
            .find(|f| f["method"] == "Runtime.evaluate" && f["params"]["expression"] == "document.readyState")
            .expect("the page's readyState was asked for");
        assert_eq!(sent["params"], json!({ "expression": "document.readyState", "returnByValue": true }));
        let _ = std::fs::remove_dir_all(&session);
    }

    #[test]
    fn taps_text_and_screenshots_go_out_as_cdp_input_and_capture_calls() {
        let device = fake_device(false);
        let (browser, session) = open(&device, "input");
        let before = device.frames().len();
        browser.tap(12.0, 34.0).unwrap();
        browser.mouse_click(5.0, 6.0).unwrap(); // a phone has no mouse: a click is a tap
        browser.key_text("hello").unwrap();
        browser.set_viewport(crate::qa::VIEWPORTS[0]).unwrap(); // the device is the viewport: nothing is sent
        let shot = session.join("shot.jpg");
        browser.screenshot(&shot).unwrap();
        let frames = device.frames();
        let sent: Vec<(&str, &Value)> = frames[before..].iter().map(|f| (f["method"].as_str().unwrap(), &f["params"])).collect();
        let touch = |kind: &str, points: Value| json!({ "type": kind, "touchPoints": points });
        // A tap is: arm the click watch, touchStart, touchEnd, then wait in the page for the click.
        let arm = json!({ "expression": ARM_CLICK_WATCH, "returnByValue": true });
        let wait = json!({ "expression": wait_for_click_js(CLICK_WAIT_MS), "returnByValue": true, "awaitPromise": true });
        assert_eq!(
            sent,
            [
                ("Runtime.evaluate", &arm),
                ("Input.dispatchTouchEvent", &touch("touchStart", json!([{ "x": 12.0, "y": 34.0 }]))),
                ("Input.dispatchTouchEvent", &touch("touchEnd", json!([]))),
                ("Runtime.evaluate", &wait),
                ("Runtime.evaluate", &arm),
                ("Input.dispatchTouchEvent", &touch("touchStart", json!([{ "x": 5.0, "y": 6.0 }]))),
                ("Input.dispatchTouchEvent", &touch("touchEnd", json!([]))),
                ("Runtime.evaluate", &wait),
                ("Input.insertText", &json!({ "text": "hello" })),
                ("Page.captureScreenshot", &json!({ "format": "jpeg", "quality": 80 })),
            ]
        );
        assert_eq!(std::fs::read(&shot).unwrap(), b"jpeg");
        let _ = std::fs::remove_dir_all(&session);
    }

    #[test]
    fn the_clock_is_the_devices_not_this_machines() {
        let device = fake_device(false);
        let (browser, session) = open(&device, "clock");
        // The round trip over loopback is a few ms; the estimate is the skew within that.
        assert!((browser.clock_offset_ms() - DEVICE_CLOCK_SKEW_MS).abs() < 300, "{}", browser.clock_offset_ms());
        let device_now = now_ms() as i64 + DEVICE_CLOCK_SKEW_MS;
        assert!((browser.clock_ms() as i64 - device_now).abs() < 300, "{} vs the device's {device_now}", browser.clock_ms());
        let _ = std::fs::remove_dir_all(&session);
    }

    #[test]
    fn a_tap_whose_click_navigates_the_page_still_succeeds() {
        let device = fake_device(false);
        let (browser, session) = open(&device, "tap-navigates");
        device.context_destroyed.store(true, Ordering::SeqCst);
        assert_eq!(browser.tap(5.0, 6.0), Ok(()), "the wait failing is not the tap failing");
        let _ = std::fs::remove_dir_all(&session);
    }

    /// The two page snippets, run for real in node (skipped without node, like the recorder's
    /// syntax check): a click that arrives late is seen, and with no click the wait ends at its bound.
    #[test]
    fn the_click_watch_sees_a_late_click_and_gives_up_without_one() {
        let script = format!(
            r#"
const handlers = [];
globalThis.window = globalThis;
globalThis.addEventListener = (type, fn) => {{ if (type === 'click') handlers.push(fn); }};
const ARM = {arm};
const WAIT = {wait};
const run = async (clickAfterMs) => {{
  handlers.length = 0;
  eval(ARM);
  const t0 = Date.now();
  if (clickAfterMs !== null) setTimeout(() => handlers.forEach((h) => h()), clickAfterMs);
  const seen = await eval(WAIT);
  return [seen, Date.now() - t0];
}};
(async () => {{
  const late = await run(150);
  const none = await run(null); // arming must have reset the flag the late click set
  console.log(JSON.stringify({{ late, none }}));
}})();
"#,
            arm = serde_json::to_string(ARM_CLICK_WATCH).unwrap(),
            wait = serde_json::to_string(&wait_for_click_js(400)).unwrap(),
        );
        let path = std::env::temp_dir().join(format!("mini-click-watch-{}.js", std::process::id()));
        std::fs::write(&path, script).expect("write temp script");
        let out = std::process::Command::new("node").arg(&path).output();
        let _ = std::fs::remove_file(&path);
        let out = match out {
            Ok(out) => out,
            Err(e) => {
                eprintln!("the_click_watch_sees_a_late_click_and_gives_up_without_one: node not available ({e}); skipped");
                return;
            }
        };
        assert!(out.status.success(), "node failed:\n{}", String::from_utf8_lossy(&out.stderr));
        let got: Value = serde_json::from_slice(&out.stdout).expect("node prints one JSON object");
        assert_eq!(got["late"][0], json!(true), "{got}");
        assert!(got["late"][1].as_u64().unwrap() < 380, "returns when the click lands, not at the bound: {got}");
        assert_eq!(got["none"][0], json!(false), "{got}");
        assert!(got["none"][1].as_u64().unwrap() >= 380, "no click: the wait runs to its bound: {got}");
    }
}

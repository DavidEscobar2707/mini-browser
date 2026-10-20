//! UI-thread transport for the QA loop.
//!
//! The QA session runs on its own `std::thread` (see `main.rs`); WebView2 (and
//! every wry `WebView`) is apartment-threaded and must only ever be touched
//! from the UI thread that created it. Every [`Browser`] method here therefore
//! builds a [`QaOp`], sends it as `QaEvent::Qa` through a `tao` event-loop
//! proxy, and blocks on a private `mpsc` channel with a [`BRIDGE_TIMEOUT_MS`]
//! deadline. [`on_reply`] runs on the UI thread (called from `main.rs`'s QA
//! event loop) and performs the actual WebView2/CDP call; synchronous ops
//! reply immediately, async ones (eval, CDP, screenshot fallback) reply later
//! from a WebView2 completion handler that looks the request back up by id.
//!
//! A `PageLoad` event on the content webview can silently abandon any pending
//! WebView2 completion (the document context that owned it is gone), so
//! [`fail_all`] drains the whole pending map and fails every in-flight
//! request rather than letting the QA thread block for the full timeout.
//!
//! CDP goes in-process through `ICoreWebView2::CallDevToolsProtocolMethod` —
//! no `--remote-debugging-port`, no socket. The mutation firewall prefers the
//! `Fetch` CDP domain; if WebView2 rejects it, [`UiBridge::arm_firewall`]
//! falls back to the native `WebResourceRequested`/`AddWebResourceRequestedFilter`
//! path and answers blocked writes with a synthetic 403.

use crate::qa::firewall::{self, Blocked, FirewallPolicy};
use crate::qa::{Browser, Viewport, BRIDGE_TIMEOUT_MS};
use dpi::LogicalSize;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;
use tao::dpi::LogicalPosition;
use tao::event_loop::EventLoopProxy;
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{
    take_pwstr, CallDevToolsProtocolMethodCompletedHandler, CapturePreviewCompletedHandler,
    DevToolsProtocolEventReceivedEventHandler, WebResourceRequestedEventHandler,
};
use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
use windows::Win32::System::Com::{IStream, STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET};
use wry::{WebView, WebViewExtWindows};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Minimal standard-alphabet base64 decoder (screenshot bytes only; no new dep).
pub(crate) fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    fn val(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut chunk = [0u8; 4];
    let mut chunk_len = 0usize;
    for b in input.bytes() {
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'=' {
            break;
        }
        let v = val(b).ok_or_else(|| format!("screenshot: invalid base64 byte {b:#x}"))?;
        chunk[chunk_len] = v;
        chunk_len += 1;
        if chunk_len == 4 {
            out.push((chunk[0] << 2) | (chunk[1] >> 4));
            out.push((chunk[1] << 4) | (chunk[2] >> 2));
            out.push((chunk[2] << 6) | chunk[3]);
            chunk_len = 0;
        }
    }
    if chunk_len >= 2 {
        out.push((chunk[0] << 2) | (chunk[1] >> 4));
    }
    if chunk_len >= 3 {
        out.push((chunk[1] << 4) | (chunk[2] >> 2));
    }
    Ok(out)
}

/// One in-flight reply payload; never carries a COM/raw-pointer type across
/// the channel (WebView2 objects have thread/apartment affinity).
enum Reply {
    Unit,
    Text(String),
    Json(Value),
    Bytes(Vec<u8>),
    Path(PathBuf),
}

type PendingMap = Arc<Mutex<HashMap<u64, mpsc::Sender<Result<Reply, String>>>>>;

/// UI-thread work items, marshalled from the QA thread via `QaEvent::Qa`.
pub enum QaOp {
    Eval(String),
    Cdp { method: String, params: Value },
    SetViewportFallback(Viewport),
    CapturePreviewFallback,
    RunLabel { label: String, incident: String },
    Url,
    InstallFetchListener,
    InstallWebResourceFallback,
    BrowserVersion,
}

impl QaOp {
    fn name(&self) -> &'static str {
        match self {
            QaOp::Eval(_) => "eval",
            QaOp::Cdp { .. } => "cdp",
            QaOp::SetViewportFallback(_) => "set_viewport(bounds-fallback)",
            QaOp::CapturePreviewFallback => "screenshot(capture-preview-fallback)",
            QaOp::RunLabel { .. } => "run_label",
            QaOp::Url => "url",
            QaOp::InstallFetchListener => "install_fetch_listener",
            QaOp::InstallWebResourceFallback => "install_web_resource_fallback",
            QaOp::BrowserVersion => "browser_version",
        }
    }
}

pub struct QaRequest {
    id: u64,
    op: QaOp,
}

/// Bridge state shared between the QA thread (`UiBridge`) and the UI thread
/// (`on_reply` / `fail_all`, driven by `main.rs`'s QA event loop).
#[derive(Clone)]
pub struct BridgeState {
    pending: PendingMap,
    blocked: Arc<Mutex<Vec<Blocked>>>,
    policy: Arc<Mutex<FirewallPolicy>>,
    /// Set when neither interception mechanism could be armed. A run with an
    /// unarmed firewall must abort rather than drive a page that can write.
    firewall_error: Arc<Mutex<Option<String>>>,
}

impl BridgeState {
    pub fn new(policy: FirewallPolicy) -> Self {
        BridgeState {
            pending: Arc::new(Mutex::new(HashMap::new())),
            blocked: Arc::new(Mutex::new(Vec::new())),
            policy: Arc::new(Mutex::new(policy)),
            firewall_error: Arc::new(Mutex::new(None)),
        }
    }
}

fn reply(pending: &PendingMap, id: u64, result: Result<Reply, String>) {
    if let Some(tx) = pending.lock().remove(&id) {
        let _ = tx.send(result);
    }
}

/// A `PageLoad` on the content webview must fail every in-flight request: a
/// torn-down document context can drop a WebView2 completion silently, which
/// would otherwise hang the QA thread until `BRIDGE_TIMEOUT_MS`.
pub fn fail_all(state: &BridgeState, reason: &str) {
    let mut pending = state.pending.lock();
    for (_, tx) in pending.drain() {
        let _ = tx.send(Err(format!("bridge: {reason}")));
    }
}

#[derive(Clone)]
pub struct UiBridge {
    proxy: EventLoopProxy<crate::QaEvent>,
    state: BridgeState,
    next_id: Arc<AtomicU64>,
    viewport_mechanism: Arc<Mutex<&'static str>>,
    screenshot_mechanism: Arc<Mutex<&'static str>>,
}

impl UiBridge {
    pub fn new(proxy: EventLoopProxy<crate::QaEvent>, state: BridgeState) -> Self {
        UiBridge {
            proxy,
            state,
            next_id: Arc::new(AtomicU64::new(1)),
            viewport_mechanism: Arc::new(Mutex::new("cdp")),
            screenshot_mechanism: Arc::new(Mutex::new("cdp")),
        }
    }

    /// Which mechanism the most recent `set_viewport` call actually used:
    /// `"cdp"` (`Emulation.setDeviceMetricsOverride`) or `"bounds-fallback"`.
    pub fn viewport_mechanism(&self) -> &'static str {
        *self.viewport_mechanism.lock()
    }

    /// Which mechanism the most recent `screenshot` call actually used:
    /// `"cdp"` (`Page.captureScreenshot`) or `"capture-preview-fallback"`.
    pub fn screenshot_mechanism(&self) -> &'static str {
        *self.screenshot_mechanism.lock()
    }

    /// Arms the mutation firewall: registers the `Fetch.requestPaused`
    /// listener and enables the `Fetch` CDP domain; falls back to WebView2's
    /// native `WebResourceRequested` when WebView2 rejects `Fetch.enable`.
    /// Must run before any navigation/request the caller cares about — call
    /// it first thing on the QA thread, before `agent::run` or any manual
    /// `cdp`/`eval` call, so message ordering on the UI thread guarantees it
    /// is armed before the first real request.
    ///
    /// Returns the mechanism that ended up active: `"fetch"` or
    /// `"web-resource-requested"`.
    pub fn arm_firewall(&self, policy: FirewallPolicy) -> &'static str {
        *self.state.policy.lock() = policy;
        // Without the paused-request listener, `Fetch.enable` would pause every
        // request with nobody to resume it: the page hangs and every bridge call
        // times out. A failed install therefore means "no Fetch path at all",
        // never "enable anyway and hope".
        let listener = self.send(QaOp::InstallFetchListener);
        if let Err(e) = &listener {
            eprintln!("qa: could not install the Fetch.requestPaused listener ({e}); using WebResourceRequested");
        }
        if listener.is_ok() {
            match self.cdp(
                "Fetch.enable",
                json!({ "patterns": [{ "urlPattern": "*", "requestStage": "Request" }] }),
            ) {
                Ok(_) => return "fetch",
                Err(e) => eprintln!(
                    "qa: WebView2 rejected the Fetch CDP domain ({e}); falling back to WebResourceRequested"
                ),
            }
        }
        match self.send(QaOp::InstallWebResourceFallback) {
            Ok(_) => "web-resource-requested",
            Err(e) => {
                eprintln!("qa: could not install the WebResourceRequested fallback: {e}");
                *self.state.firewall_error.lock() = Some(format!("firewall could not be armed: {e}"));
                "unarmed"
            }
        }
    }

    fn send(&self, op: QaOp) -> Result<Reply, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let name = op.name();
        let (tx, rx) = mpsc::channel();
        self.state.pending.lock().insert(id, tx);
        if self.proxy.send_event(crate::QaEvent::Qa(QaRequest { id, op })).is_err() {
            self.state.pending.lock().remove(&id);
            return Err(format!("bridge: UI thread is gone ({name})"));
        }
        match rx.recv_timeout(Duration::from_millis(BRIDGE_TIMEOUT_MS)) {
            Ok(result) => result,
            Err(_) => {
                self.state.pending.lock().remove(&id);
                Err(format!("bridge timeout: {name}"))
            }
        }
    }
}

impl Browser for UiBridge {
    fn eval(&self, js: &str) -> Result<String, String> {
        match self.send(QaOp::Eval(js.to_string()))? {
            Reply::Text(s) => Ok(s),
            _ => Err("eval: bridge returned the wrong reply shape".into()),
        }
    }

    fn firewall_status(&self) -> Result<(), String> {
        match self.state.firewall_error.lock().clone() {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    fn browser_version(&self) -> Option<String> {
        match self.send(QaOp::BrowserVersion) {
            Ok(Reply::Text(version)) if !version.is_empty() => Some(version),
            _ => None,
        }
    }

    fn cdp(&self, method: &str, params: Value) -> Result<Value, String> {
        match self.send(QaOp::Cdp { method: method.to_string(), params })? {
            Reply::Json(v) => Ok(v),
            _ => Err("cdp: bridge returned the wrong reply shape".into()),
        }
    }

    fn set_viewport(&self, v: Viewport) -> Result<(), String> {
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
        if let Err(e) = self.cdp("Emulation.setUserAgentOverride", user_agent_for(&v, &major)) {
            eprintln!("qa: Emulation.setUserAgentOverride failed ({e}); the page keeps its current user agent");
        }
        Ok(())
    }

    fn screenshot(&self, path: &Path) -> Result<(), String> {
        match self.cdp("Page.captureScreenshot", json!({"format": "jpeg", "quality": 80})) {
            Ok(v) => {
                let data = v
                    .get("data")
                    .and_then(|d| d.as_str())
                    .ok_or_else(|| "screenshot: cdp reply missing data field".to_string())?;
                let bytes = base64_decode(data)?;
                std::fs::write(path, &bytes).map_err(|e| format!("screenshot: write failed: {e}"))?;
                *self.screenshot_mechanism.lock() = "cdp";
                Ok(())
            }
            Err(cdp_err) => {
                eprintln!(
                    "qa: Page.captureScreenshot failed ({cdp_err}); falling back to ICoreWebView2::CapturePreview"
                );
                match self.send(QaOp::CapturePreviewFallback) {
                    Ok(Reply::Bytes(bytes)) => {
                        std::fs::write(path, &bytes)
                            .map_err(|e| format!("screenshot: write failed: {e}"))?;
                        *self.screenshot_mechanism.lock() = "capture-preview-fallback";
                        Ok(())
                    }
                    Ok(_) => Err(format!(
                        "screenshot: cdp failed ({cdp_err}); fallback returned the wrong reply shape"
                    )),
                    Err(fallback_err) => Err(format!(
                        "screenshot: cdp failed ({cdp_err}); CapturePreview fallback failed ({fallback_err})"
                    )),
                }
            }
        }
    }

    fn mouse_click(&self, x: f64, y: f64) -> Result<(), String> {
        let base = json!({"x": x, "y": y, "button": "left", "buttons": 1, "clickCount": 1});
        let mut press = base.clone();
        press["type"] = json!("mousePressed");
        self.cdp("Input.dispatchMouseEvent", press)?;
        let mut release = base;
        release["type"] = json!("mouseReleased");
        self.cdp("Input.dispatchMouseEvent", release)?;
        Ok(())
    }

    fn tap(&self, x: f64, y: f64) -> Result<(), String> {
        self.cdp("Input.dispatchTouchEvent", json!({ "type": "touchStart", "touchPoints": [{ "x": x, "y": y }] }))?;
        self.cdp("Input.dispatchTouchEvent", json!({ "type": "touchEnd", "touchPoints": [] }))?;
        Ok(())
    }

    fn key_text(&self, text: &str) -> Result<(), String> {
        if self.cdp("Input.insertText", json!({"text": text})).is_ok() {
            return Ok(());
        }
        for ch in text.chars() {
            let s = ch.to_string();
            self.cdp(
                "Input.dispatchKeyEvent",
                json!({"type": "keyDown", "text": s, "unmodifiedText": s}),
            )?;
            self.cdp(
                "Input.dispatchKeyEvent",
                json!({"type": "keyUp", "text": s, "unmodifiedText": s}),
            )?;
        }
        Ok(())
    }

    fn run_label(&self, label: &str, incident: &str) -> Result<PathBuf, String> {
        match self.send(QaOp::RunLabel { label: label.to_string(), incident: incident.to_string() })? {
            Reply::Path(p) => Ok(p),
            _ => Err("run_label: bridge returned the wrong reply shape".into()),
        }
    }

    fn url(&self) -> Result<String, String> {
        match self.send(QaOp::Url)? {
            Reply::Text(s) => Ok(s),
            _ => Err("url: bridge returned the wrong reply shape".into()),
        }
    }

    fn blocked(&self) -> Vec<Blocked> {
        self.state.blocked.lock().clone()
    }
}

// ---------------------------------------------------------------- UI thread

/// Runs on the UI thread: perform one `QaOp` against the content webview and
/// reply through the pending-request map. Never blocks the UI thread — every
/// WebView2 call here is either synchronous (replies immediately) or
/// fire-and-forget with an async completion handler that replies later, from
/// a subsequent tick of the same tao message loop.
pub fn on_reply(
    content: &WebView,
    debug: &mut Option<crate::debug::DebugRecorder>,
    state: &BridgeState,
    req: QaRequest,
) {
    let QaRequest { id, op } = req;
    let webview = content.webview();
    match op {
        QaOp::Eval(js) => {
            let pending = state.pending.clone();
            let result = content.evaluate_script_with_callback(&js, move |result_json| {
                reply(&pending, id, Ok(Reply::Text(result_json)));
            });
            if let Err(e) = result {
                reply(&state.pending, id, Err(format!("eval: {e}")));
            }
        }

        QaOp::Cdp { method, params } => {
            let pending = state.pending.clone();
            let method_hstring = HSTRING::from(method.as_str());
            let params_hstring = HSTRING::from(params.to_string());
            let handler: ICoreWebView2CallDevToolsProtocolMethodCompletedHandler =
                CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |hr, json_text| {
                    let outcome = match hr {
                        Ok(()) => match serde_json::from_str::<Value>(&json_text) {
                            Ok(v) => Ok(Reply::Json(v)),
                            Err(e) => Err(format!("cdp: non-JSON reply: {e}")),
                        },
                        Err(e) => Err(format!("cdp: {e}")),
                    };
                    reply(&pending, id, outcome);
                    Ok(())
                }));
            let outcome =
                unsafe { webview.CallDevToolsProtocolMethod(&method_hstring, &params_hstring, &handler) };
            if let Err(e) = outcome {
                reply(&state.pending, id, Err(format!("cdp: {e}")));
            }
        }

        QaOp::SetViewportFallback(v) => {
            let rect = wry::Rect {
                position: LogicalPosition::new(0.0, 0.0).into(),
                size: LogicalSize::new(v.width as f64, v.height as f64).into(),
            };
            let result = content
                .set_bounds(rect)
                .map(|_| Reply::Unit)
                .map_err(|e| format!("set_bounds: {e}"));
            reply(&state.pending, id, result);
        }

        QaOp::CapturePreviewFallback => match make_mem_stream() {
            Ok(stream) => {
                let pending = state.pending.clone();
                let stream_for_read = stream.clone();
                let handler: ICoreWebView2CapturePreviewCompletedHandler =
                    CapturePreviewCompletedHandler::create(Box::new(move |hr| {
                        let outcome = match hr {
                            Ok(()) => read_stream_bytes(&stream_for_read).map(Reply::Bytes),
                            Err(e) => Err(format!("CapturePreview: {e}")),
                        };
                        reply(&pending, id, outcome);
                        Ok(())
                    }));
                let outcome = unsafe {
                    webview.CapturePreview(COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG, &stream, &handler)
                };
                if let Err(e) = outcome {
                    reply(&state.pending, id, Err(format!("CapturePreview: {e}")));
                }
            }
            Err(e) => reply(&state.pending, id, Err(e)),
        },

        QaOp::RunLabel { label, incident } => {
            let result = match debug.as_mut() {
                Some(rec) => {
                    rec.start_run(&label, &incident);
                    Ok(Reply::Path(rec.run_dir()))
                }
                None => Err("run_label: recorder is not active".to_string()),
            };
            reply(&state.pending, id, result);
        }

        QaOp::Url => {
            let result = content.url().map(Reply::Text).map_err(|e| format!("url: {e}"));
            reply(&state.pending, id, result);
        }

        QaOp::InstallFetchListener => {
            let result = install_fetch_listener(&webview, state);
            reply(&state.pending, id, result.map(|_| Reply::Unit));
        }

        QaOp::InstallWebResourceFallback => {
            let result = install_web_resource_fallback(content, &webview, state);
            reply(&state.pending, id, result.map(|_| Reply::Unit));
        }

        QaOp::BrowserVersion => {
            reply(&state.pending, id, browser_version(content).map(Reply::Text));
        }
    }
}

/// The WebView2 runtime's own version. The page cannot tell us: Mini spoofs the
/// UA string and `userAgentData.brands` is derived from that spoof.
fn browser_version(content: &WebView) -> Result<String, String> {
    let env = content.environment();
    let mut raw = PWSTR::null();
    unsafe { env.BrowserVersionString(&mut raw) }
        .map_err(|e| format!("ICoreWebView2Environment::BrowserVersionString: {e}"))?;
    Ok(take_pwstr(raw))
}

fn make_mem_stream() -> Result<IStream, String> {
    unsafe { CreateStreamOnHGlobal(HGLOBAL::default(), true) }.map_err(|e| format!("CreateStreamOnHGlobal: {e}"))
}

fn read_stream_bytes(stream: &IStream) -> Result<Vec<u8>, String> {
    unsafe {
        let mut stat = STATSTG::default();
        stream
            .Stat(&mut stat, STATFLAG_NONAME)
            .map_err(|e| format!("IStream::Stat: {e}"))?;
        let len = stat.cbSize as usize;
        stream
            .Seek(0, STREAM_SEEK_SET, None)
            .map_err(|e| format!("IStream::Seek: {e}"))?;
        let mut buf = vec![0u8; len];
        let mut read_total = 0usize;
        while read_total < len {
            let mut read_now: u32 = 0;
            let hr = stream.Read(
                buf.as_mut_ptr().add(read_total).cast(),
                (len - read_total) as u32,
                Some(&mut read_now),
            );
            hr.ok().map_err(|e| format!("IStream::Read: {e}"))?;
            if read_now == 0 {
                break;
            }
            read_total += read_now as usize;
        }
        buf.truncate(read_total);
        Ok(buf)
    }
}

/// Registers the persistent `Fetch.requestPaused` listener. Enabling the
/// domain itself (`Fetch.enable`) goes through the normal `cdp()` path from
/// the QA thread (see `arm_firewall`), reusing the same generic machinery.
fn install_fetch_listener(webview: &ICoreWebView2, state: &BridgeState) -> Result<(), String> {
    let receiver = unsafe { webview.GetDevToolsProtocolEventReceiver(&HSTRING::from("Fetch.requestPaused")) }
        .map_err(|e| format!("GetDevToolsProtocolEventReceiver: {e}"))?;
    let webview_for_handler = webview.clone();
    let state_for_handler = state.clone();
    let handler: ICoreWebView2DevToolsProtocolEventReceivedEventHandler =
        DevToolsProtocolEventReceivedEventHandler::create(Box::new(move |_sender, args| {
            if let Some(args) = args {
                handle_fetch_paused(&webview_for_handler, &state_for_handler, &args);
            }
            Ok(())
        }));
    let mut token: i64 = 0;
    unsafe { receiver.add_DevToolsProtocolEventReceived(&handler, &mut token) }
        .map_err(|e| format!("add_DevToolsProtocolEventReceived: {e}"))
}

fn handle_fetch_paused(
    webview: &ICoreWebView2,
    state: &BridgeState,
    args: &ICoreWebView2DevToolsProtocolEventReceivedEventArgs,
) {
    let mut raw = PWSTR::null();
    if unsafe { args.ParameterObjectAsJson(&mut raw) }.is_err() {
        return;
    }
    let json_text = take_pwstr(raw);
    let Ok(payload) = serde_json::from_str::<Value>(&json_text) else {
        return;
    };
    let request_id = payload.get("requestId").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if request_id.is_empty() {
        return;
    }
    let method = payload.pointer("/request/method").and_then(|v| v.as_str()).unwrap_or("GET").to_string();
    let url = payload.pointer("/request/url").and_then(|v| v.as_str()).unwrap_or("").to_string();

    let policy = state.policy.lock().clone();
    let blocked = firewall::should_block(&policy, &method, &url);

    let (cdp_method, params) = if blocked {
        ("Fetch.failRequest", json!({"requestId": request_id, "errorReason": "Aborted"}))
    } else {
        ("Fetch.continueRequest", json!({"requestId": request_id}))
    };
    let method_hstring = HSTRING::from(cdp_method);
    let params_hstring = HSTRING::from(params.to_string());
    // Fire-and-forget: nothing downstream is waiting on Fetch.failRequest /
    // Fetch.continueRequest, so a no-op completion handler is enough.
    let handler: ICoreWebView2CallDevToolsProtocolMethodCompletedHandler =
        CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_hr, _json| Ok(())));
    let _ = unsafe { webview.CallDevToolsProtocolMethod(&method_hstring, &params_hstring, &handler) };

    if blocked {
        state.blocked.lock().push(Blocked {
            ts_ms: now_ms(),
            method,
            url,
            mechanism: "fetch".to_string(),
        });
    }
}

/// WebView2-native fallback when the `Fetch` CDP domain is rejected: filter +
/// `WebResourceRequested`, answering blocked writes with a synthetic 403
/// instead of failing the underlying network request.
fn install_web_resource_fallback(
    content: &WebView,
    webview: &ICoreWebView2,
    state: &BridgeState,
) -> Result<(), String> {
    unsafe { webview.AddWebResourceRequestedFilter(&HSTRING::from("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL) }
        .map_err(|e| format!("AddWebResourceRequestedFilter: {e}"))?;
    let env = content.environment();
    let state_for_handler = state.clone();
    let handler: ICoreWebView2WebResourceRequestedEventHandler =
        WebResourceRequestedEventHandler::create(Box::new(move |_sender, args| {
            if let Some(args) = args {
                handle_web_resource_requested(&env, &state_for_handler, &args);
            }
            Ok(())
        }));
    let mut token: i64 = 0;
    unsafe { webview.add_WebResourceRequested(&handler, &mut token) }
        .map_err(|e| format!("add_WebResourceRequested: {e}"))
}

fn handle_web_resource_requested(
    env: &ICoreWebView2Environment,
    state: &BridgeState,
    args: &ICoreWebView2WebResourceRequestedEventArgs,
) {
    let Ok(request) = (unsafe { args.Request() }) else {
        return;
    };
    let mut uri_raw = PWSTR::null();
    let mut method_raw = PWSTR::null();
    if unsafe { request.Uri(&mut uri_raw) }.is_err() {
        return;
    }
    if unsafe { request.Method(&mut method_raw) }.is_err() {
        return;
    }
    let url = take_pwstr(uri_raw);
    let method = take_pwstr(method_raw);

    let policy = state.policy.lock().clone();
    if !firewall::should_block(&policy, &method, &url) {
        return; // not a write (or writes are allowed) -> let it through untouched
    }

    if let Ok(response) =
        unsafe { env.CreateWebResourceResponse(None::<&IStream>, 403, &HSTRING::from("Forbidden"), &HSTRING::from("")) }
    {
        let _ = unsafe { args.SetResponse(&response) };
    }

    state.blocked.lock().push(Blocked {
        ts_ms: now_ms(),
        method,
        url,
        mechanism: "web-resource-requested".to_string(),
    });
}

/// `Emulation.setUserAgentOverride` params for a viewport. Mobile viewports get an
/// Android Chrome UA with matching client hints. Desktop clears the override: an empty
/// `userAgent` without `userAgentMetadata` hands the webview its own UA and client hints
/// back, which re-sending the old UA string does not (that leaves `userAgentData.brands`
/// empty). Clearing when nothing was overridden is a no-op.
pub fn user_agent_for(v: &Viewport, chrome_major: &str) -> Value {
    if !v.mobile {
        return json!({ "userAgent": "" });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qa::VIEWPORTS;

    /// A QA viewport by name: `VIEWPORTS` is in run order, never index it.
    fn viewport(name: &str) -> Viewport {
        *VIEWPORTS.iter().find(|v| v.name == name).expect("known QA viewport")
    }

    #[test]
    fn phone_ua_is_android_mobile() {
        let p = user_agent_for(&viewport("phone"), "141");
        assert!(p["userAgent"].as_str().unwrap().contains("Android 14; Pixel 8"));
        assert!(p["userAgent"].as_str().unwrap().contains("Chrome/141.0.0.0 Mobile Safari"));
        assert_eq!(p["userAgentMetadata"]["mobile"], true);
    }

    #[test]
    fn tablet_ua_is_android_without_the_mobile_token() {
        let t = user_agent_for(&viewport("tablet"), "141");
        let ua = t["userAgent"].as_str().unwrap();
        assert!(ua.contains("Android 14; Pixel Tablet"), "{ua}");
        assert!(!ua.contains("Mobile"), "{ua}");
        assert_eq!(t["userAgentMetadata"]["mobile"], false);
    }

    #[test]
    fn desktop_clears_the_override_instead_of_resending_a_ua_string() {
        // An empty userAgent without metadata resets the webview to its own UA and client
        // hints. Re-sending the old UA string would leave navigator.userAgentData empty.
        assert_eq!(user_agent_for(&viewport("desktop"), "141"), json!({ "userAgent": "" }));
    }
}

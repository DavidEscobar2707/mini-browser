#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod omnibox;
mod debug;
mod profile;
mod analyze;
mod qa;
mod cdp;
mod move_cmd;
mod text_helper;

use crate::qa::Browser;

use dpi::PhysicalSize;
use tao::dpi::LogicalPosition;
use tao::event::{ElementState, Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder, EventLoopProxy};
use tao::keyboard::{Key, ModifiersState};
use tao::platform::windows::WindowBuilderExtWindows;
use tao::window::{Icon, Window, WindowBuilder};
use wry::{NewWindowResponse, PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows};

const CTRL_L_SCRIPT: &str = r#"
document.addEventListener('keydown', (e) => {
  if (e.ctrlKey && !e.shiftKey && !e.altKey && e.key === 'l') {
    e.preventDefault();
    window.ipc.postMessage('{"cmd":"focus-url"}');
  }
}, true);
"#;

/// Debug mode only. Canonical source: tools/recorder.js (also injected into
/// external Chromiums by tools/record.js). Its `post` prefers the __miniPost
/// CDP binding and falls back to window.ipc — inside Mini only window.ipc
/// exists, so everything ships as {"cmd":"debug","kind":...} lines.
const DEBUG_RECORDER_SCRIPT: &str = include_str!("../tools/recorder.js");


enum Msg {
    ChromeIpc(String),
    ContentIpc(String),
    FabIpc(String),
    TitleChanged(String),
    PageLoad(PageLoadEvent, String),
    NewWindow(String),
}

/// `--qa` / `--qa-spike` event-loop message. A separate, single-webview shell
/// from the interactive browser's `Msg`/`App`: QA has no chrome bar or FAB,
/// and its content webview must be exactly the configured viewport size, so
/// it gets its own tiny loop instead of reusing `layout()`'s chrome-offset math.
pub(crate) enum QaEvent {
    /// UI-thread work item from `qa::bridge::UiBridge`; see `qa::bridge::on_reply`.
    Qa(qa::bridge::QaRequest),
    PageLoad(String),
    ContentIpc(String),
}

struct App {
    window: Window,
    chrome: WebView,
    content: WebView,
    fab: WebView,
    on_start: bool,
    mods: ModifiersState,
    debug: Option<debug::DebugRecorder>,
}

fn layout(scale: f64, px: PhysicalSize<u32>) -> (Rect, Rect, Rect) {
    let s = px.to_logical::<f64>(scale);
    let chrome = Rect {
        position: LogicalPosition::new(0.0, 0.0).into(),
        size: dpi::LogicalSize::new(s.width, 40.0).into(),
    };
    let content = Rect {
        position: LogicalPosition::new(0.0, 40.0).into(),
        size: dpi::LogicalSize::new(s.width, s.height - 40.0).into(),
    };
    let fab = Rect {
        position: LogicalPosition::new(16.0, s.height - 16.0 - 48.0).into(),
        size: dpi::LogicalSize::new(48.0, 48.0).into(),
    };
    (chrome, content, fab)
}

fn relayout(scale: f64, px: PhysicalSize<u32>, chrome: &WebView, content: &WebView, fab: &WebView) {
    let (rc, rk, rf) = layout(scale, px);
    let _ = chrome.set_bounds(rc);
    let _ = content.set_bounds(rk);
    let _ = fab.set_bounds(rf);
}

fn die_if_webview_missing<T>(result: wry::Result<T>, what: &str) -> T {
    match result {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Failed to create the {what}: {e}");
            eprintln!(
                "Install the Evergreen WebView2 Runtime: https://go.microsoft.com/fwlink/p/?LinkId=2124703"
            );
            std::process::exit(1); // SPEC.md §6: no silent hang
        }
    }
}

fn load_icon() -> Option<Icon> {
    None // v1: taskbar/exe icon comes from the winresource icon (Task 5);
         // the window is undecorated so it draws no titlebar icon anyway.
}

fn allow_http_https_only(url: String) -> bool {
    // WebView2 NavigateToString (with_html / load_html) starts as data:text/html;charset=utf-8;base64,…
    // Without this arm the start page is cancelled and the content HWND stays blank.
    url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("data:text/html")
}

/// Evergreen WebView2 otherwise sends a WebView2 client-hint brand. Search engines
/// and GitHub treat that as an embedded/automation client and spam CAPTCHAs.
fn edge_like_user_agent() -> String {
    // pv verified on this machine at v1 (Evergreen 151). Major must match the runtime.
    const VER: &str = "151.0.4129.101";
    format!(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36 Edg/{VER}"
    )
}

/// Keep wry's default feature disables; add AutomationControlled off so
/// `navigator.webdriver` is not set. Content webview only (own User Data dir).
const CONTENT_BROWSER_ARGS: &str =
    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-blink-features=AutomationControlled";

impl App {
    fn set_chrome_url(&self, url: &str) {
        let js = format!("window.setUrl({})", serde_json::to_string(url).unwrap());
        let _ = self.chrome.evaluate_script(&js);
    }

    fn set_chrome_title(&self, title: &str) {
        let js = format!("window.setTitle({})", serde_json::to_string(title).unwrap());
        let _ = self.chrome.evaluate_script(&js);
    }

    /// SPEC.md §4 omnibox; SPEC.md §7 {"cmd":"navigate","text":…}
    fn navigate(&mut self, raw: &str) {
        if let Some(url) = omnibox::resolve(raw) {
            let _ = self.content.load_url(&url);
            if let Some(rec) = self.debug.as_mut() {
                rec.backend("navigate", &url);
            }
            self.on_start = false;
            let _ = self.fab.set_visible(true);
            self.set_chrome_url(&url);
            let _ = self.content.focus();
        }
        // None -> ignored/rejected: empty, javascript:, file:, …
    }

    /// SPEC.md §4: FAB / Alt+Home -> start page, hide FAB, reset chrome title/url.
    fn go_home(&mut self) {
        let _ = self.content.load_html(include_str!("../ui/start.html"));
        if let Some(rec) = self.debug.as_mut() {
            rec.backend("home", "");
        }
        self.on_start = true;
        let _ = self.fab.set_visible(false);
        self.set_chrome_title("Mini");
        self.set_chrome_url("");
        let _ = self.chrome.focus();
    }

    /// True while content shows the null-origin start page (not an http(s) site).
    fn content_on_start_page(&self) -> bool {
        // Appendix B.1: navigate only while we still show the start page we loaded.
        // url()-only is also true for about:blank / data: after a remote page
        // (T6 allows data:text/html), which would reopen a content navigate channel.
        if !self.on_start {
            return false;
        }
        match self.content.url() {
            Ok(u) => !(u.starts_with("http://") || u.starts_with("https://")),
            Err(_) => true,
        }
    }
}

fn handle_key(app: &mut App, key: &Key<'_>, _cf: &mut ControlFlow) {
    // tao 0.37 has no NamedKey: named keys are Key::F5 / ArrowLeft / Home (Task 6).
    // mods.ctrl()/alt() are control_key()/alt_key() on this crate.
    let ctrl = app.mods.control_key();
    let alt = app.mods.alt_key();
    match key {
        Key::F5 => {
            let _ = app.content.reload();
            if let Some(rec) = app.debug.as_mut() {
                rec.backend("reload", "");
            }
        }
        Key::Character("r") if ctrl => {
            let _ = app.content.reload();
            if let Some(rec) = app.debug.as_mut() {
                rec.backend("reload", "");
            }
        }
        Key::ArrowLeft if alt && !ctrl => {
            let _ = app.content.go_back(); // errors ignored when history empty (SPEC.md §4)
            if let Some(rec) = app.debug.as_mut() {
                rec.backend("back", "");
            }
        }
        Key::ArrowRight if alt && !ctrl => {
            let _ = app.content.go_forward();
            if let Some(rec) = app.debug.as_mut() {
                rec.backend("forward", "");
            }
        }
        Key::Home if alt && !ctrl => app.go_home(),
        _ => {}
    }
}

#[derive(serde::Deserialize)]
struct IpcCmd {
    cmd: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    incident: Option<String>,
    /// `debug-batch`: raw recorder lines, each a `{"cmd":"debug",...}` object.
    #[serde(default)]
    lines: Vec<String>,
}

/// Files one recorder line (or every line of a `debug-batch`) into its stream.
fn route_debug(rec: &mut debug::DebugRecorder, c: &IpcCmd, body: &str) {
    if c.cmd == "debug-batch" {
        for line in &c.lines {
            if let Ok(inner) = serde_json::from_str::<IpcCmd>(line) {
                if inner.cmd == "debug" {
                    route_debug(rec, &inner, line);
                }
            }
        }
        return;
    }
    match c.kind.as_deref() {
        Some("dom") => rec.dom_line(body),
        Some("net") => rec.net_line(body),
        Some("react") => rec.react_line(body),
        Some("frame") => rec.frames_line(body),
        Some("mark") => rec.marks_line(body),
        Some("run") => rec.start_run(c.label.as_deref().unwrap_or("run"), c.incident.as_deref().unwrap_or("")),
        _ => rec.frontend_line(body),
    }
}

fn handle_msg(app: &mut App, msg: Msg, control_flow: &mut ControlFlow) {
    match msg {
        Msg::ChromeIpc(body) | Msg::FabIpc(body) => {
            if let Ok(c) = serde_json::from_str::<IpcCmd>(&body) {
                match c.cmd.as_str() {
                    "navigate" => app.navigate(c.text.as_deref().unwrap_or("")),
                    "drag" => {
                        let _ = app.window.drag_window();
                    }
                    "minimize" => app.window.set_minimized(true),
                    "maximize" => app.window.set_maximized(!app.window.is_maximized()),
                    "close" => *control_flow = ControlFlow::Exit,
                    "home" => app.go_home(),
                    "back" => {
                        let _ = app.content.go_back();
                    }
                    "forward" => {
                        let _ = app.content.go_forward();
                    }
                    "reload" => {
                        let _ = app.content.reload();
                    }
                    _ => {} // unknown cmd -> ignore (SPEC.md §7)
                }
            }
        }
        Msg::ContentIpc(body) => {
            if let Ok(c) = serde_json::from_str::<IpcCmd>(&body) {
                match c.cmd.as_str() {
                    "focus-url" => {
                        let _ = app.chrome.focus();
                        let _ = app.chrome.evaluate_script("window.focusUrl()");
                    }
                    "navigate" if app.content_on_start_page() => {
                        app.navigate(c.text.as_deref().unwrap_or(""))
                    }
                    "debug" | "debug-batch" => {
                        if let Some(rec) = app.debug.as_mut() {
                            route_debug(rec, &c, &body);
                        }
                    }
                    _ => {} // everything else from content: ignored
                }
            }
        }
        Msg::TitleChanged(title) => {
            if let Some(rec) = app.debug.as_mut() {
                rec.backend("title", &title);
            }
            // SPEC.md §4: start-page title pill is "Mini". WebView2 reports
            // "about:blank" after load_html (non-empty), so the empty-title
            // fallback never fires. Skip that junk while on_start.
            if app.on_start {
                app.set_chrome_title("Mini");
            } else if !title.is_empty() {
                app.set_chrome_title(&title);
            } else {
                let host = omnibox::host_of(&app.content.url().unwrap_or_default());
                app.set_chrome_title(&host);
            }
        }
        Msg::PageLoad(event, url) => {
            let loading = matches!(event, PageLoadEvent::Started);
            let _ = app.chrome.evaluate_script(&format!("window.setLoading({loading})"));
            if let Some(rec) = app.debug.as_mut() {
                rec.backend(if loading { "page-load-start" } else { "page-load" }, &url);
            }
            // Start page: SPEC.md §4 URL field empty. WebView2 reports about:blank / data:text/html.
            if app.on_start {
                app.set_chrome_url("");
            } else {
                app.set_chrome_url(&url);
            }
        }
        Msg::NewWindow(url) => {
            if let Some(rec) = app.debug.as_mut() {
                rec.backend("popup-redirected", &url);
            }
            let _ = app.content.load_url(&url); // in-place; Deny already returned
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--debug-diff") {
        std::process::exit(analyze::diff(args.get(i + 1), args.get(i + 2)));
    }
    if let Some(i) = args.iter().position(|a| a == "--debug-explain") {
        std::process::exit(analyze::explain(args.get(i + 1)));
    }
    if let Some(i) = args.iter().position(|a| a == "--qa-spike") {
        run_qa_spike(args.get(i + 1).cloned());
    }
    if let Some(i) = args.iter().position(|a| a == "--qa") {
        run_qa(&args, i);
    }
    if args.iter().any(|a| a == "--debug-status") {
        std::process::exit(debug::status_code());
    }
    if args.iter().any(|a| a == "--debug-stop") {
        std::process::exit(debug::stop_running());
    }
    if args.iter().any(|a| a == "--observe") {
        std::process::exit(move_cmd::observe());
    }
    if args.iter().any(|a| a == "--read") {
        std::process::exit(move_cmd::read());
    }
    if let Some(i) = args.iter().position(|a| a == "--move") {
        std::process::exit(move_cmd::run(&args, i));
    }

    // An explicit `--debug` (or `MINI_DEBUG`) launch re-execs itself detached
    // from this process's console/job, then waits for the child to publish
    // a listening CDP port before exiting. Debug builds are a console
    // subsystem (see line 1): closing the launching console delivers
    // CTRL_CLOSE_EVENT to every process still attached to it, so the
    // interactive window must not be one of them.
    let env_child = std::env::var("MINI_DEBUG_CHILD").ok();
    let env_mini_debug = std::env::var("MINI_DEBUG").ok();
    if debug::should_detach(&args, env_child.as_deref(), env_mini_debug.as_deref()) {
        let mut child = match debug::spawn_detached_child(&args) {
            Ok(child) => child,
            Err(code) => std::process::exit(code),
        };
        match debug::await_child_ready(child.id(), 20_000) {
            Some(json) => {
                println!("{json}");
                std::process::exit(0);
            }
            None => {
                eprintln!(
                    "mini: debug browser did not open 127.0.0.1:{} within 20s",
                    debug::port()
                );
                let _ = child.kill();
                std::process::exit(2);
            }
        }
    }

    // First non-flag arg = start URL/omnibox input (e.g. `mini --debug http://localhost:3000`).
    let start_input = args.iter().skip(1).find(|a| !a.starts_with('-')).cloned();
    profile::ensure_dirs();

    let debug_on = debug::enabled();
    let mut recorder = if debug_on { debug::DebugRecorder::start() } else { None };
    if let Some(rec) = recorder.as_mut() {
        rec.backend("session-start", &format!("cdp-port={}", debug::port()));
    }

    let event_loop: EventLoop<Msg> = EventLoopBuilder::<Msg>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let send = move |msg: Msg| {
        let _ = proxy.send_event(msg);
    };

    let mut builder = WindowBuilder::new()
        .with_title("Mini")
        .with_inner_size(dpi::LogicalSize::new(1100.0, 740.0))
        .with_min_inner_size(dpi::LogicalSize::new(640.0, 480.0))
        .with_decorations(false)
        .with_visible(true)
        .with_window_icon(load_icon());
    builder = builder.with_undecorated_shadow(true);
    builder = builder.with_window_classname("MiniBrowser");
    let window = builder.build(&event_loop).expect("tao window");

    // Two contexts, never swapped: cookies must not leak between them (SPEC.md §3, §5).
    let mut shell_ctx = WebContext::new(Some(profile::shell_dir()));
    let mut profile_ctx = WebContext::new(Some(profile::profile_dir()));

    let (rc, rk, rf) = layout(window.scale_factor(), window.inner_size());

    let content_args = if debug_on {
        format!("{CONTENT_BROWSER_ARGS} --remote-debugging-port={}", debug::port())
    } else {
        CONTENT_BROWSER_ARGS.to_string()
    };

    let mut content_builder = WebViewBuilder::new_with_web_context(&mut profile_ctx)
        .with_id("content")
        .with_html(include_str!("../ui/start.html")) // first paint
        .with_bounds(rk)
        .with_navigation_handler(allow_http_https_only)
        .with_document_title_changed_handler({
            let send = send.clone();
            move |t| {
                let _ = send(Msg::TitleChanged(t));
            }
        })
        .with_on_page_load_handler({
            let send = send.clone();
            move |e, url| {
                let _ = send(Msg::PageLoad(e, url));
            }
        })
        .with_new_window_req_handler({
            let send = send.clone();
            move |url, _| {
                let _ = send(Msg::NewWindow(url)); // load in place (Task 8)
                NewWindowResponse::Deny
            }
        })
        .with_ipc_handler({
            let send = send.clone();
            move |request| {
                let _ = send(Msg::ContentIpc(request.body().clone()));
            }
        })
        .with_initialization_script(CTRL_L_SCRIPT)
        .with_clipboard(true)
        .with_hotkeys_zoom(false)
        .with_user_agent(edge_like_user_agent())
        .with_additional_browser_args(&content_args)
        .with_devtools(cfg!(debug_assertions) || debug_on)
        .with_browser_extensions_enabled(false) // WebViewBuilderExtWindows
        .with_browser_accelerator_keys(false);
    if debug_on {
        content_builder = content_builder.with_initialization_script(DEBUG_RECORDER_SCRIPT);
    }
    let content = die_if_webview_missing(content_builder.build_as_child(&window), "content webview");

    if debug_on {
        if debug::wait_for_port(debug::port(), 15_000) {
            match recorder.as_ref() {
                Some(rec) => rec.mark_ready(),
                None => debug::write_unrecorded_ready(),
            }
        } else {
            eprintln!("mini: CDP port {} never accepted a connection", debug::port());
            std::process::exit(2);
        }
    }

    let fab = die_if_webview_missing(
        WebViewBuilder::new_with_web_context(&mut shell_ctx)
            .with_id("fab")
            .with_html(include_str!("../ui/fab.html"))
            .with_transparent(true)
            .with_background_color((0, 0, 0, 0))
            .with_bounds(rf)
            .with_visible(false) // hidden on start page (SPEC.md §2)
            .with_ipc_handler({
                let send = send.clone();
                move |request| {
                    let _ = send(Msg::FabIpc(request.body().clone()));
                }
            })
            .with_devtools(cfg!(debug_assertions) || debug_on)
            .with_browser_extensions_enabled(false)
            .with_browser_accelerator_keys(false)
            .build_as_child(&window),
        "fab webview",
    );

    let chrome = die_if_webview_missing(
        WebViewBuilder::new_with_web_context(&mut shell_ctx)
            .with_id("chrome")
            .with_html(include_str!("../ui/chrome.html"))
            .with_bounds(rc)
            .with_focused(false)
            .with_ipc_handler(move |request| {
                let _ = send(Msg::ChromeIpc(request.body().clone()));
            })
            .with_devtools(cfg!(debug_assertions) || debug_on)
            .with_browser_extensions_enabled(false)
            .with_browser_accelerator_keys(false)
            .build_as_child(&window),
        "chrome webview",
    );

    let mut app = App {
        window,
        chrome,
        content,
        fab,
        on_start: true,
        mods: ModifiersState::default(),
        debug: recorder,
    };

    if debug_on {
        let _ = app.chrome.evaluate_script(&format!("window.setAgent({})", debug::port()));
    }

    // Deferred until App exists: navigate() also updates chrome URL + FAB.
    if let Some(input) = &start_input {
        app.navigate(input);
    }

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(msg) => handle_msg(&mut app, msg, control_flow),
            Event::WindowEvent { event: WindowEvent::Resized(px), .. } => {
                relayout(app.window.scale_factor(), px, &app.chrome, &app.content, &app.fab);
            }
            Event::WindowEvent {
                event: WindowEvent::ScaleFactorChanged { new_inner_size, .. },
                ..
            } => {
                relayout(
                    app.window.scale_factor(),
                    *new_inner_size,
                    &app.chrome,
                    &app.content,
                    &app.fab,
                );
            }
            Event::WindowEvent { event: WindowEvent::ModifiersChanged(m), .. } => {
                app.mods = m;
            }
            Event::WindowEvent {
                event: WindowEvent::KeyboardInput { event: ke, .. },
                ..
            } => {
                if ke.state == ElementState::Pressed {
                    handle_key(&mut app, &ke.logical_key, control_flow); // Task 8
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                *control_flow = ControlFlow::Exit;
            }
            // tao exits the process after this event, so `Drop` never runs.
            Event::LoopDestroyed if debug_on => debug::tombstone_self(),
            _ => {}
        }
    });
}

// =============================================================== QA mode
//
// `--qa <url>` (self-driving QA session) and `--qa-spike <url>` (raw
// capability diagnostic) share a minimal, single-webview shell: no chrome
// bar, no FAB, content fills the window exactly so the desktop viewport
// (1440x900, see `qa::VIEWPORTS`) matches the real window with no offset.
// Both flows never touch the interactive browser's `Msg`/`App`/`handle_msg`.
//
// Content-IPC allowlist is unchanged by QA mode: `focus-url` and start-page
// `navigate` never fire here (QA never shows `ui/start.html`), so only
// `debug` (recorder) messages are handled; navigation happens exclusively
// through CDP `Page.navigate` via the bridge, in-process, no new IPC verb.

struct QaShell {
    event_loop: EventLoop<QaEvent>,
    window: Window,
    content: WebView,
    proxy: EventLoopProxy<QaEvent>,
    // Kept alive for the shell's lifetime: WebView2's profile directory
    // association is established at construction but the on-disk profile
    // must stay valid for as long as `content` is alive.
    _ctx: WebContext,
}

fn qa_layout(scale: f64, px: PhysicalSize<u32>) -> Rect {
    let s = px.to_logical::<f64>(scale);
    Rect {
        position: LogicalPosition::new(0.0, 0.0).into(),
        size: dpi::LogicalSize::new(s.width, s.height).into(),
    }
}

/// Builds the fixed-size (1440x900, non-resizable), single-webview QA
/// window: content webview on the QA profile context (reset by the caller
/// before this runs), recorder script injected when `debug::enabled()`,
/// devtools + no `--remote-debugging-port` gated on `devtools` (an explicit
/// `--debug`, never `--qa`/`--qa-spike` alone).
///
/// Also returns a one-shot "ready" receiver: WebView2's own load of the
/// placeholder first-paint HTML fires a `PageLoad` event once the event loop
/// starts pumping, which races the QA thread's very first bridge call (e.g.
/// `arm_firewall`). Treating that placeholder load like a real navigation
/// would `fail_all` it out from under the QA thread. The caller's spawned
/// thread MUST `ready_rx.recv()` before issuing any bridge request, so the
/// placeholder's load is consumed as the ready signal instead of forwarded
/// as `QaEvent::PageLoad` (real navigations after that still fail-all normally).
fn build_qa_shell(title: &str, host: String, devtools: bool) -> (QaShell, std::sync::mpsc::Receiver<()>) {
    let mut ctx = WebContext::new(Some(profile::qa_profile_dir()));

    let event_loop: EventLoop<QaEvent> = EventLoopBuilder::<QaEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title(title)
        .with_inner_size(dpi::LogicalSize::new(1440.0, 900.0))
        .with_min_inner_size(dpi::LogicalSize::new(1440.0, 900.0))
        .with_resizable(false)
        .with_decorations(true)
        .with_visible(true)
        .with_window_icon(load_icon())
        .with_window_classname("MiniQA")
        .build(&event_loop)
        .expect("tao window");

    let bounds = qa_layout(window.scale_factor(), window.inner_size());

    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let ready_sent = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // SPEC: off-host navigation is refused under --qa. `first_party` also
    // allows subdomains of the target host. The URL the caller asked for is
    // allowed even when it is a login page; anything the run navigates to
    // afterwards is not — that is where the write safety layers live.
    let nav_host = host;
    let first_nav = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut content_builder = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_id("qa-content")
        .with_html("<!doctype html><title>Mini QA</title>") // first paint; the real load is CDP Page.navigate
        .with_bounds(bounds)
        .with_navigation_handler({
            let nav_host = nav_host.clone();
            let first_nav = first_nav.clone();
            move |url: String| {
                if !allow_http_https_only(url.clone()) || !qa::firewall::first_party(&nav_host, &url) {
                    return false;
                }
                if first_nav.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    return true;
                }
                !qa::firewall::is_forbidden_page(&url)
            }
        })
        .with_on_page_load_handler({
            let proxy = proxy.clone();
            move |_event, url| {
                if ready_sent.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    let _ = proxy.send_event(QaEvent::PageLoad(url));
                } else {
                    let _ = ready_tx.send(()); // placeholder first paint: not a real navigation
                }
            }
        })
        .with_new_window_req_handler(|_url, _| NewWindowResponse::Deny)
        .with_ipc_handler({
            let proxy = proxy.clone();
            move |request| {
                let _ = proxy.send_event(QaEvent::ContentIpc(request.body().clone()));
            }
        })
        .with_clipboard(true)
        .with_hotkeys_zoom(false)
        .with_user_agent(edge_like_user_agent())
        .with_additional_browser_args(CONTENT_BROWSER_ARGS) // never --remote-debugging-port
        .with_devtools(devtools)
        .with_browser_extensions_enabled(false)
        .with_browser_accelerator_keys(false);
    if debug::enabled() {
        content_builder = content_builder.with_initialization_script(DEBUG_RECORDER_SCRIPT);
    }
    // The QA API the loop drives the page through (`__miniQA.*`).
    for script in qa::PAGE_SCRIPTS {
        content_builder = content_builder.with_initialization_script(*script);
    }
    let content = die_if_webview_missing(content_builder.build_as_child(&window), "QA content webview");

    (QaShell { event_loop, window, content, proxy, _ctx: ctx }, ready_rx)
}

/// Drives the QA shell's event loop forever (until the QA thread calls
/// `std::process::exit`, or the window is closed by hand).
fn run_qa_event_loop(shell: QaShell, state: qa::bridge::BridgeState, abort: Option<qa::QaConfig>) -> ! {
    let QaShell { event_loop, window, content, .. } = shell;
    let mut debug_rec = if debug::enabled() { debug::DebugRecorder::start() } else { None };
    if let Some(rec) = debug_rec.as_mut() {
        rec.backend("qa-session-start", "");
    }
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(QaEvent::Qa(req)) => {
                qa::bridge::on_reply(&content, &mut debug_rec, &state, req);
            }
            Event::UserEvent(QaEvent::PageLoad(url)) => {
                if let Some(rec) = debug_rec.as_mut() {
                    rec.backend("page-load", &url);
                }
                // A torn-down document context can silently drop any pending
                // WebView2 completion; fail in-flight bridge requests fast
                // rather than block the QA thread for the full timeout.
                qa::bridge::fail_all(&state, "page navigated");
            }
            Event::UserEvent(QaEvent::ContentIpc(body)) => {
                if let Some(rec) = debug_rec.as_mut() {
                    if let Ok(c) = serde_json::from_str::<IpcCmd>(&body) {
                        // Content-IPC allowlist unchanged: only `debug` (recorder)
                        // messages are meaningful here (see module doc comment above).
                        if c.cmd == "debug" || c.cmd == "debug-batch" {
                            route_debug(rec, &c, &body);
                        }
                    }
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                eprintln!("mini: QA window closed before the run finished");
                if let Some(cfg) = &abort {
                    qa::agent::write_aborted_report(cfg, "the QA window was closed before the run finished");
                }
                std::process::exit(2); // SPEC: 2 = QA could not run
            }
            _ => {
                let _ = &window; // window has no other event handling; keep it alive
            }
        }
    });
}

fn qa_host_of(url: &str) -> String {
    omnibox::host_of(url).split(':').next().unwrap_or("").trim().to_lowercase()
}

/// yyyymmdd-hhmmss (UTC), hand-rolled — no chrono/time dep. Howard Hinnant's
/// public-domain `civil_from_days` algorithm (also used by LLVM/absl).
fn civil_timestamp(unix_secs: u64) -> String {
    let unix_secs = unix_secs as i64;
    let days = unix_secs.div_euclid(86400);
    let secs_of_day = unix_secs.rem_euclid(86400);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day / 60) % 60;
    let second = secs_of_day % 60;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}")
}

fn qa_out_dir(host: &str) -> std::path::PathBuf {
    let unix_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    dirs::data_local_dir()
        .expect("%LOCALAPPDATA% is unavailable")
        .join("Mini")
        .join("QA")
        .join(format!("{}-{host}", civil_timestamp(unix_secs)))
}

struct QaCliArgs {
    url: String,
    section: Option<String>,
    claims: Vec<String>,
    out: Option<std::path::PathBuf>,
    allow_writes: bool,
    steps: Option<usize>,
}

/// Parses `--qa <url> [--section <name>] [--claim <text>]... [--out <dir>]
/// [--allow-writes] [--steps N]`. `qa_index` is the position of `--qa` itself.
fn parse_qa_args(args: &[String], qa_index: usize) -> QaCliArgs {
    let url = args.get(qa_index + 1).cloned().unwrap_or_default();
    let mut out = QaCliArgs {
        url,
        section: None,
        claims: Vec::new(),
        out: None,
        allow_writes: false,
        steps: None,
    };
    let mut i = qa_index + 2;
    while i < args.len() {
        match args[i].as_str() {
            "--section" => {
                out.section = args.get(i + 1).cloned();
                i += 2;
            }
            "--claim" => {
                if let Some(c) = args.get(i + 1) {
                    out.claims.push(c.clone());
                }
                i += 2;
            }
            "--out" => {
                out.out = args.get(i + 1).map(std::path::PathBuf::from);
                i += 2;
            }
            "--allow-writes" => {
                out.allow_writes = true;
                i += 1;
            }
            "--steps" => {
                out.steps = args.get(i + 1).and_then(|s| s.parse::<usize>().ok());
                i += 2;
            }
            _ => i += 1, // ignore unrelated flags (--debug, --i-know-this-is-prod, ...)
        }
    }
    out
}

/// `mini --qa <url> ...`: one-shot self-driving QA run. Validates and builds
/// `QaConfig` before opening any window; runs the QA loop on its own thread
/// once the content webview exists, then exits the process with its result.
/// Spawns the QA work on its own thread and always terminates the process
/// with a definite exit code: `f`'s return value, or 2 ("QA could not run")
/// if `f` panics. `event_loop.run` on the UI thread never returns on its
/// own, so this is the only thing standing between an internal QA bug and a
/// window that silently hangs forever with nothing progressing.
fn spawn_qa_thread(f: impl FnOnce() -> i32 + Send + 'static, abort: qa::QaConfig) {
    std::thread::spawn(move || {
        let code = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
            Ok(code) => code,
            Err(_) => {
                eprintln!("mini: QA thread panicked");
                qa::agent::write_aborted_report(&abort, "the QA thread panicked")
            }
        };
        std::process::exit(code);
    });
}

fn run_qa(args: &[String], qa_index: usize) -> ! {
    let cli = parse_qa_args(args, qa_index);
    if cli.url.is_empty() {
        eprintln!("mini --qa: missing <url>");
        std::process::exit(2);
    }
    let host = qa_host_of(&cli.url);
    if host.is_empty() {
        eprintln!("mini --qa: could not parse a host from {}", cli.url);
        std::process::exit(2);
    }

    // Write-safety refusal: --allow-writes against a *.vercel.app production
    // host is a hard refuse unless the caller explicitly acknowledges it.
    let prod_ack = args.iter().any(|a| a == "--i-know-this-is-prod");
    if cli.allow_writes && host.ends_with(".vercel.app") && !prod_ack {
        eprintln!(
            "mini --qa: refusing --allow-writes against {host} (a *.vercel.app production host) without --i-know-this-is-prod."
        );
        std::process::exit(2);
    }

    let api_key = qa::jev::api_key();
    if api_key.is_none() {
        eprintln!(
            "mini --qa: no Jev API key found. QA runs judge findings with Jev (api.typesafe.ai) \u{2014} {}.",
            qa::jev::key_help()
        );
        std::process::exit(2);
    }
    let model = qa::jev::model();
    let out_dir = cli.out.clone().unwrap_or_else(|| qa_out_dir(&host));

    let cfg = qa::QaConfig {
        url: cli.url,
        host: host.clone(),
        section: cli.section,
        claims: cli.claims,
        out_dir,
        allow_writes: cli.allow_writes,
        steps: cli.steps.unwrap_or(qa::DEFAULT_STEPS),
        wall_clock_s: qa::DEFAULT_WALL_CLOCK_S,
        model,
        api_key,
    };

    profile::reset_qa_profile();
    let debug_flag = args.iter().any(|a| a == "--debug");
    let (shell, ready_rx) = build_qa_shell("Mini QA", host.clone(), debug_flag);
    let policy = qa::firewall::FirewallPolicy { allow_writes: cfg.allow_writes, host: cfg.host.clone() };
    let state = qa::bridge::BridgeState::new(policy.clone());
    let bridge = qa::bridge::UiBridge::new(shell.proxy.clone(), state.clone());

    let abort_cfg = cfg.clone();
    spawn_qa_thread(
        move || {
            let _ = ready_rx.recv(); // wait for the placeholder first paint; see build_qa_shell
            bridge.arm_firewall(policy);
            let judge = qa::jev::JevClient::new(cfg.api_key.clone().expect("checked above"), cfg.model.clone());
            qa::agent::run(&cfg, &bridge, &judge)
        },
        abort_cfg.clone(),
    );

    run_qa_event_loop(shell, state, Some(abort_cfg));
}

/// `mini --qa-spike <url>`: diagnostic that exercises the five bridge
/// capabilities directly (no Jev, no agent loop) and prints one JSON line
/// per capability plus a summary. Exits 0 only if every capability passed.
fn run_qa_spike(url: Option<String>) -> ! {
    let url = match url.filter(|u| !u.is_empty()) {
        Some(u) => u,
        None => {
            eprintln!("mini --qa-spike: missing <url>");
            std::process::exit(2);
        }
    };
    let host = qa_host_of(&url);
    profile::reset_qa_profile();
    let debug_flag = std::env::args().any(|a| a == "--debug");
    let (shell, ready_rx) = build_qa_shell("Mini QA Spike", host.clone(), debug_flag);
    let policy = qa::firewall::FirewallPolicy { allow_writes: false, host };
    let state = qa::bridge::BridgeState::new(policy.clone());
    let bridge = qa::bridge::UiBridge::new(shell.proxy.clone(), state.clone());

    std::thread::spawn(move || {
        let _ = ready_rx.recv(); // wait for the placeholder first paint; see build_qa_shell
        let code = spike_checks(&bridge, &url, policy);
        std::process::exit(code);
    });

    run_qa_event_loop(shell, state, None);
}

fn wait_for_load(bridge: &qa::bridge::UiBridge, timeout: std::time::Duration) -> Result<(), String> {
    let start = std::time::Instant::now();
    loop {
        if let Ok(s) = bridge.eval("document.readyState") {
            if s.trim_matches('"') == "complete" {
                return Ok(());
            }
        }
        if start.elapsed() > timeout {
            return Err("timed out waiting for document.readyState == 'complete'".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn read_counter(bridge: &qa::bridge::UiBridge) -> Option<i64> {
    bridge
        .eval("Number(document.getElementById('n').textContent)")
        .ok()?
        .trim()
        .parse::<i64>()
        .ok()
}

/// Evaluates the element's bounding rect as a plain-object literal (never a
/// live `DOMRect` — those serialize to `{}`, their fields are prototype
/// getters) and dispatches a real CDP click at its center.
fn click_element(bridge: &qa::bridge::UiBridge, id: &str) -> Option<bool> {
    let js = format!(
        "(() => {{ const el = document.getElementById('{id}'); if (!el) return null; \
         const r = el.getBoundingClientRect(); return {{x:r.x,y:r.y,w:r.width,h:r.height}}; }})()"
    );
    let raw = bridge.eval(&js).ok()?;
    let rect: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let x = rect.get("x")?.as_f64()? + rect.get("w")?.as_f64()? / 2.0;
    let y = rect.get("y")?.as_f64()? + rect.get("h")?.as_f64()? / 2.0;
    Some(bridge.mouse_click(x, y).is_ok())
}

fn spike_checks(bridge: &qa::bridge::UiBridge, url: &str, policy: qa::firewall::FirewallPolicy) -> i32 {
    let _ = &policy;
    let mut all_ok = true;
    let mut pass = 0usize;
    let mut total = 0usize;
    let mut record = |v: serde_json::Value| {
        total += 1;
        if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
            pass += 1;
        } else {
            all_ok = false;
        }
        println!("{v}");
    };

    // Arm the firewall before navigating so the /blocked POST check (and the
    // real GET navigation itself) both exercise it.
    let fw_mechanism = bridge.arm_firewall(policy);

    if let Err(e) = bridge.cdp("Page.navigate", json_url(url)) {
        eprintln!("mini --qa-spike: Page.navigate failed: {e}");
    }
    if let Err(e) = wait_for_load(bridge, std::time::Duration::from_secs(15)) {
        eprintln!("mini --qa-spike: {e}");
    }

    // 1. eval
    let eval_r = bridge.eval("1+1");
    let eval_ok = eval_r.as_deref().map(|s| s.trim() == "2").unwrap_or(false);
    record(serde_json::json!({
        "cap": "eval", "ok": eval_ok,
        "detail": format!("1+1 -> {eval_r:?}"),
    }));

    // 2. viewport: 390x844 dsf 3 mobile, verified via window.innerWidth.
    let phone = qa::VIEWPORTS[2];
    let vp_result = bridge.set_viewport(phone);
    let vp_mechanism = bridge.viewport_mechanism();
    let inner_w = bridge.eval("window.innerWidth");
    let vp_ok = vp_result.is_ok() && inner_w.as_deref() == Ok("390");
    record(serde_json::json!({
        "cap": "viewport", "ok": vp_ok, "mechanism": vp_mechanism,
        "detail": format!(
            "set_viewport({}x{} dsf={})={:?} window.innerWidth={:?}",
            phone.width, phone.height, phone.dsf, vp_result, inner_w
        ),
    }));

    // 3. screenshot: file written with bytes.
    let shot_dir = dirs::data_local_dir()
        .expect("%LOCALAPPDATA% is unavailable")
        .join("Mini")
        .join("QA")
        .join("spike");
    let _ = std::fs::create_dir_all(&shot_dir);
    let shot_path = shot_dir.join("spike-screenshot.jpg");
    let shot_result = bridge.screenshot(&shot_path);
    let shot_mechanism = bridge.screenshot_mechanism();
    let bytes = std::fs::metadata(&shot_path).map(|m| m.len()).unwrap_or(0);
    let shot_ok = shot_result.is_ok() && bytes > 0;
    record(serde_json::json!({
        "cap": "screenshot", "ok": shot_ok, "mechanism": shot_mechanism,
        "path": shot_path.display().to_string(), "bytes": bytes,
        "detail": format!("{shot_result:?}"),
    }));

    // Desktop viewport again so the fixture's element rects are predictable
    // for the click checks below.
    let _ = bridge.set_viewport(qa::VIEWPORTS[0]);

    // 4. click: a real CDP click increments the fixture's counter 0 -> 1.
    let before = read_counter(bridge);
    let click_ok = click_element(bridge, "inc").unwrap_or(false);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let after = read_counter(bridge);
    let counter_ok = click_ok && before == Some(0) && after == Some(1);
    record(serde_json::json!({
        "cap": "click", "ok": counter_ok,
        "detail": format!("clicked={click_ok} counter {before:?} -> {after:?}"),
    }));

    // 5. firewall: a POST to /blocked fails and blocked() gains one entry.
    let blocked_before = bridge.blocked().len();
    let post_clicked = click_element(bridge, "post-blocked").unwrap_or(false);
    std::thread::sleep(std::time::Duration::from_millis(400));
    let blocked_after = bridge.blocked();
    let err_text = bridge.eval("document.getElementById('err').textContent");
    let fw_ok = post_clicked && blocked_after.len() == blocked_before + 1;
    record(serde_json::json!({
        "cap": "firewall", "ok": fw_ok, "mechanism": fw_mechanism,
        "detail": format!(
            "posted={post_clicked} blocked {blocked_before} -> {} err-span={err_text:?}",
            blocked_after.len()
        ),
    }));

    println!("{}", serde_json::json!({"summary": true, "pass": pass, "total": total, "ok": all_ok}));
    if all_ok {
        0
    } else {
        1
    }
}

fn json_url(url: &str) -> serde_json::Value {
    serde_json::json!({ "url": url })
}

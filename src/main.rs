mod omnibox;
mod profile;

use dpi::PhysicalSize;
use tao::dpi::LogicalPosition;
use tao::event::{ElementState, Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder};
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

enum Msg {
    ChromeIpc(String),
    ContentIpc(String),
    FabIpc(String),
    TitleChanged(String),
    PageLoad(PageLoadEvent, String),
    NewWindow(String),
}

struct App {
    window: Window,
    chrome: WebView,
    content: WebView,
    fab: WebView,
    on_start: bool,
    mods: ModifiersState,
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

fn handle_key(_app: &mut App, _key: &Key<'_>, _cf: &mut ControlFlow) {}

fn handle_msg(_app: &mut App, _msg: Msg, _control_flow: &mut ControlFlow) {}

fn main() {
    profile::ensure_dirs();

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

    let content = die_if_webview_missing(
        WebViewBuilder::new_with_web_context(&mut profile_ctx)
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
            .with_devtools(cfg!(debug_assertions))
            .with_browser_extensions_enabled(false) // WebViewBuilderExtWindows
            .with_browser_accelerator_keys(false)
            .build_as_child(&window),
        "content webview",
    );

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
            .with_devtools(cfg!(debug_assertions))
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
            .with_devtools(cfg!(debug_assertions))
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
    };

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
            _ => {}
        }
    });
}

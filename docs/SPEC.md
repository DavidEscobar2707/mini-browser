# Mini — v1 design spec

Windows personal browser. One window, one page, isolated Chromium (WebView2). Clone of Guillermo Rauch’s Mini UI, not Orion Focus Mode and not a Chrome wrapper.

Pinned APIs: **wry 0.56.1**, **tao 0.37.0**. Do not copy pre-0.50 `wry::application` examples. Windowing is `tao`; webviews are `wry`.

---

## 1. Goal and non-goals

**Goal.** A packaged `Mini.exe` you pin to the taskbar. Open a URL or search without Chrome/Edge cookies, tabs, or extensions. Screen-share / stream / test.

**v1 does**

- One OS window, one content page.
- Compact titlebar: document-title pill + URL field.
- Native-looking caption buttons on the **right** (min / max / close).
- Start page with centered “Enter URL or search…”.
- Home FAB bottom-left → start page.
- Isolated WebView2 profile under `%LOCALAPPDATA%\Mini\User Data`.

**v1 does not**

- Tabs, bookmarks, extensions, downloads UI, settings, hover-hide chrome, incognito, CEF, Electron, Tauri, Electrobun, macOS/Linux, installers, auto-update.
- Sharing `%LOCALAPPDATA%\Google\Chrome` or `Microsoft\Edge` User Data.
- `window.open` popups as extra windows (navigate in-place).

---

## 2. UX contract

Windows 11, rounded window. Chrome is **always visible** and tiny. Not hide-on-hover.

```
┌───────────────────────────────────────────────────────────[─][□][×]┐
│ [Guillermo Rauch's blog]  [ https://rauchg.com/                ]   │  ← 40px chrome
├────────────────────────────────────────────────────────────────────┤
│                                                                    │
│                         page content                               │
│                                                                    │
│  (home FAB)                                                        │
└────────────────────────────────────────────────────────────────────┘
```

| Region | Behavior |
|---|---|
| Caption buttons | Right. HTML in the chrome webview. Call host `minimize` / `maximize` / `close`. |
| Title pill | Left. Document title. Not a tab. Not editable. |
| URL field | Stretches. Shows the live URL. Submit → omnibox rule (§4). |
| Drag | Empty chrome (not the URL input, not caption buttons) → `Window::drag_window`. |
| Start page | Content area white; one centered field, placeholder `Enter URL or search…`. Same omnibox rule. |
| Home FAB | Dark rounded square ~48px, white home icon, bottom-left overlay on the **page**. Hidden on start page. Click → start page. Does not steal clicks outside its 48×48. |
| Resize | Bottom-right via `Window::drag_resize_window(SouthEast)` from a 6px chrome/window edge is optional; OS undocumented resize on undecorated windows is enough if it works. If it does not, add a 6px hit strip that IPCs `resize-se`. |

Keyboard (v1)

| Shortcut | Action |
|---|---|
| `Ctrl+L` | Focus URL field in chrome |
| `Enter` in URL or start field | Navigate |
| `Alt+Left` / `Alt+Right` | Back / forward |
| `F5` / `Ctrl+R` | Reload |
| `Alt+Home` | Start page |
| `Alt+F4` | Close (OS) |

---

## 3. Architecture

One `tao` window, three **child** WebView2 HWNDs. No overlapping hit-testing except the FAB sitting on the content corner (intentional). CSS `pointer-events: none` cannot punch through a WebView2 HWND — do not build a full-window transparent overlay.

```
tao::Window  (undecorated, shadow on)
 ├─ chrome  WebView   y=0,     h=40 logical,  full width     trusted HTML
 ├─ content WebView   y=40,    rest of client                untrusted https
 └─ fab     WebView   48×48 at bottom-left of content        trusted HTML
```

### Window (tao)

```rust
WindowBuilder::new()
  .with_title("Mini")
  .with_inner_size(LogicalSize::new(1100.0, 740.0))
  .with_min_inner_size(LogicalSize::new(640.0, 480.0))
  .with_decorations(false)
  .with_visible(true)
  .with_window_icon(/* from assets */)
```

Windows-only:

```rust
use tao::platform::windows::WindowBuilderExtWindows;
builder.with_undecorated_shadow(true)
       .with_window_classname("MiniBrowser")
```

- `Window::drag_window` on chrome `drag` IPC (must run on mousedown, same as docs: left button already down).
- `Window::set_minimized(true)`, `set_maximized(!is_maximized())`, close = `ControlFlow::Exit`.
- Feature: `tao = { version = "0.37", features = ["rwh_06"] }` so `Window` implements `HasWindowHandle` for wry.

Known limit: HTML maximize button will **not** show the Win11 snap flyout (`HTMAXBUTTON`). Accept in v1.

### Three webviews (wry)

All created with `WebViewBuilder::build_as_child(&window)` and `with_bounds(Rect { position, size })`. On `WindowEvent::Resized` and `ScaleFactorChanged`, recompute bounds from `window.inner_size()` + `scale_factor()` and `set_bounds` on all three.

**Chrome + FAB** share `WebContext::new(Some(shell_dir))`. **Content** uses a **different** `WebContext::new(Some(profile_dir))`. Do not share the content context with chrome (cookies must not leak into shell, and vice versa). Keep both `WebContext` values alive for the process lifetime.

Chrome:

```rust
WebViewBuilder::new_with_web_context(&mut shell_ctx)
  .with_id("chrome")
  .with_html(include_str!("../ui/chrome.html"))
  .with_ipc_handler(chrome_ipc)
  .with_devtools(cfg!(debug_assertions))
  .with_focused(false)
  .with_bounds(chrome_rect)
  .with_browser_extensions_enabled(false) // WebViewBuilderExtWindows
  .with_browser_accelerator_keys(false)
  .build_as_child(&window)
```

FAB: same context, `with_html(fab.html)`, `with_transparent(true)`, `with_background_color((0,0,0,0))`, tiny bounds, ipc only `home`.

Content:

```rust
WebViewBuilder::new_with_web_context(&mut profile_ctx)
  .with_id("content")
  .with_html(include_str!("../ui/start.html")) // first paint
  .with_navigation_handler(allow_http_https_only)
  .with_document_title_changed_handler(push_title_to_chrome)
  .with_on_page_load_handler(sync_url_to_chrome)
  .with_new_window_req_handler(|url, _| { /* load in place; */ NewWindowResponse::Deny })
  .with_ipc_handler(content_accel_only) // see Ctrl+L
  .with_initialization_script(CTRL_L_SCRIPT)
  .with_clipboard(true)
  .with_hotkeys_zoom(false)
  .with_browser_extensions_enabled(false)
  .with_browser_accelerator_keys(false)
  .with_devtools(cfg!(debug_assertions))
  .build_as_child(&window)
```

When `window.open` fires: `content.load_url(&url)` then `NewWindowResponse::Deny`. (Handler cannot both navigate and return Allow.)

`with_html` origin is `null`. Fine for start/chrome/fab. After first omnibox submit, content is `https://…`.

### Click targeting

| Surface | Clicks |
|---|---|
| Chrome HWND | Title, URL, caption, drag. Never the page. |
| Content HWND | The page. Never the titlebar. |
| FAB HWND | Only its 48×48. Hidden (`set_visible(false)`) on start page so the start field is fully clickable, including bottom-left. |

On resize: FAB origin = `(16, inner_h - 16 - 48)` logical, below the chrome.

### Ctrl+L

WebView2 eats keys while content is focused. Do **not** inject a general IPC bridge into pages.

Content init script, privileged, small:

```js
document.addEventListener('keydown', (e) => {
  if (e.ctrlKey && !e.shiftKey && !e.altKey && e.key === 'l') {
    e.preventDefault();
    window.ipc.postMessage('{"cmd":"focus-url"}');
  }
}, true);
```

Content IPC allowlist: **only** `{"cmd":"focus-url"}`. Host then `chrome.focus()` + `chrome.evaluate_script("window.focusUrl()")`.

XSS can only request “focus the URL bar”. Navigate is never accepted from content IPC.

---

## 4. Navigation

Omnibox (URL field and start field). Freeze this; unit-test in `src/omnibox.rs`.

```
trim
empty → ignore
has scheme http:// or https:// → use as-is
has scheme other (javascript:, file:, data:, about:) → reject
localhost or 127.0.0.1[:port][/path] → https:// is wrong; use http://localhost…
looks like host: no spaces, has a dot, or is localhost → prepend https://
else → https://www.google.com/search?q={urlencoded}
```

“Looks like host” = `^[a-zA-Z0-9.-]+(?::\d+)?(/.*)?$` and (contains `.` or equals `localhost`).

Host updates chrome via:

- `with_on_page_load_handler`: on `PageLoadEvent::Started` and `Finished`, `content.url()` → `chrome.evaluate_script(setUrl(...))`.
- `with_document_title_changed_handler`: title string → `chrome.evaluate_script(setTitle(...))`. Empty title → host name.
- Start page: title pill `Mini`, URL field empty (placeholder only).

FAB / `Alt+Home`: `content.load_html(start.html)`, hide FAB, reset chrome title/url.

Back/forward/reload: `WebView::go_back` / `go_forward` / `reload`. Ignore errors if history is empty.

Navigation handler: allow `http:` and `https:` only. Deny `file:`, `javascript:`. Custom-protocol chrome URLs never load in content.

---

## 5. Profile isolation

| Role | Path |
|---|---|
| Content cookies/cache/IndexedDB | `%LOCALAPPDATA%\Mini\User Data` |
| Shell (chrome+fab) | `%LOCALAPPDATA%\Mini\Shell` |

Resolve with `dirs::data_local_dir().unwrap().join("Mini").join("User Data")`. Create dirs on boot.

Never:

- `%LOCALAPPDATA%\Google\Chrome\User Data`
- `%LOCALAPPDATA%\Microsoft\Edge\User Data`
- the unpackaged default `<exe>.WebView2` next to the binary

Proof: log into a site in Mini, confirm Chrome/Edge still logged out; confirm cookies live only under `Mini\User Data`.

`with_incognito` is out of v1.

---

## 6. WebView2 runtime

- Engine: Evergreen WebView2 Runtime (Edge Chromium). Win11 has it.
- Do not bundle Fixed Version. Do not require Edge Stable as the host.
- `AreBrowserExtensionsEnabled` = false (`with_browser_extensions_enabled(false)`).
- If environment creation fails (runtime missing): print a message with the Evergreen bootstrapper URL and exit nonzero. No silent hang.

https://go.microsoft.com/fwlink/p/?LinkId=2124703

---

## 7. Crate plan

```toml
[package]
name = "mini"
edition = "2021"

[dependencies]
wry = "0.56"
tao = { version = "0.37", features = ["rwh_06"] }
dirs = "6"
dpi = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[build-dependencies]
winresource = "0.1"
```

`build.rs`: `WindowsResource::new().set_icon("assets/icon.ico").compile()`. Skip on non-Windows.

| API | Use |
|---|---|
| `tao::event_loop::EventLoop` | Host loop |
| `WindowBuilder::with_decorations(false)` | Custom chrome |
| `WindowBuilderExtWindows::with_undecorated_shadow` | Drop shadow |
| `Window::drag_window` / `set_minimized` / `set_maximized` / `is_maximized` | Caption + drag |
| `Window::inner_size` / `scale_factor` | Layout |
| `wry::WebContext::new(Some(path))` | UDF |
| `WebViewBuilder::new_with_web_context` | Shared ctx per role |
| `with_html` | chrome, fab, start |
| `with_ipc_handler` | chrome/fab/content-accel |
| `with_bounds` + `build_as_child` | Three HWNDs |
| `WebView::set_bounds` / `set_visible` / `focus` / `load_url` / `load_html` / `url` / `evaluate_script` / `go_back` / `go_forward` / `reload` | Runtime |
| `with_navigation_handler` | http(s) only |
| `with_document_title_changed_handler` | Title pill |
| `with_on_page_load_handler` | URL sync (`PageLoadEvent::{Started,Finished}`) |
| `with_new_window_req_handler` → `NewWindowResponse::Deny` | No extra windows |
| `WebViewBuilderExtWindows::with_browser_extensions_enabled(false)` | No extensions |
| `with_browser_accelerator_keys(false)` | Host owns Ctrl+L |
| `include_str!("../ui/…")` | Ship HTML in the binary |

IPC JSON from chrome/fab (trusted):

```json
{"cmd":"navigate","text":"…"}
{"cmd":"drag"}
{"cmd":"minimize"}
{"cmd":"maximize"}
{"cmd":"close"}
{"cmd":"home"}
{"cmd":"focus-url"}   // from content allowlist only
```

Unknown `cmd` → ignore.

---

## 8. File tree

```
mini-browser/
  Cargo.toml
  build.rs
  assets/icon.ico          # black squircle, inner dark circle, thin light ring
  ui/chrome.html           # title pill, URL, caption buttons, drag region
  ui/fab.html              # home button only, transparent page
  ui/start.html            # centered omnibox
  src/main.rs              # event loop, window, webviews, layout, ipc
  src/omnibox.rs           # URL vs search (unit tests)
  src/profile.rs           # Mini\User Data and Mini\Shell paths
  docs/SPEC.md             # this file
```

Keep rust in two small modules plus `main.rs`. Do not add a framework.

---

## 9. Packaging (v1)

- Dev: `cargo run` from this folder. `target/debug/mini.exe`.
- “Packaged app”: that exe, with icon via `winresource`. Pin to taskbar by hand.
- No MSI, no auto-update, no code signing in v1.
- Icon: generate a simple ICO matching Rauch’s Mini mark (black rounded square, silver ring). Good enough; not a brand exercise.

---

## 10. v1 acceptance

Verify on Windows 11, WebView2 present.

| # | Behavior | How to verify |
|---|---|---|
| 1 | Window opens undecorated, rounded/shadow, Mini icon | `cargo run` → visual |
| 2 | Start page: centered “Enter URL or search…” | Visual; FAB hidden |
| 3 | Title pill left, URL field, caption buttons **right** | Visual |
| 4 | Drag empty chrome moves the window | Click-drag titlebar whitespace |
| 5 | Min / max / close work | Click the three buttons |
| 6 | Type `https://example.com` → page loads | Address + content |
| 7 | Type `rauchg.com` → `https://rauchg.com` | URL field shows https |
| 8 | Type `how rust works` → Google search | URL becomes google.com/search?q=… |
| 9 | Title pill updates to document title | example.com → “Example Domain” |
| 10 | URL field tracks in-page navigations | Click a link; field updates |
| 11 | FAB appears after leaving start; click returns to start | Visual |
| 12 | `Ctrl+L` focuses URL while on a https page | Key while content focused |
| 13 | `window.open` / target=_blank stays in the same view | Click an `_blank` link |
| 14 | Profile is only under `%LOCALAPPDATA%\Mini\User Data` | Create folder; log in on a site; Chrome/Edge session unchanged; no writes under Google\Chrome or Microsoft\Edge User Data |
| 15 | Extensions off | No Edge extension UI |
| 16 | `javascript:alert(1)` in omnibox does nothing | Reject |
| 17 | `cargo test` omnibox cases pass | `cargo test` |

---

## 11. Open questions

None. Frozen:

- Engine: WebView2, not CEF.
- Caption buttons: HTML on the right, not OS titlebar.
- Omnibox: §4.
- Profile: `%LOCALAPPDATA%\Mini\User Data`.
- `window.open`: same view.
- Snap-layout flyout: skipped in v1.

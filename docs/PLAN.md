# Mini — Implementation Plan (v1)

## Goal
Build `Mini.exe`: a packaged, taskbar-pinnable personal browser for Windows 11 per SPEC.md. One OS window, one content page, compact always-visible chrome, isolated WebView2 profile under `%LOCALAPPDATA%\Mini\User Data`. Nothing from SPEC.md §1 “v1 does not” gets built.

## Architecture
One `tao::Window` (undecorated, Win11 shadow on), hosting **three child WebView HWNDs** built with `WebViewBuilder::build_as_child` — **not** a fullscreen overlay, never CSS hit-test tricks:

```
tao::Window  (undecorated, shadow on, class "MiniBrowser")
 ├─ chrome  WebView   y=0,   h=40 logical, full width   trusted HTML
 ├─ content WebView   y=40,  rest of client             untrusted https
 └─ fab     WebView   48×48 logical at bottom-left          trusted HTML, hidden on start
```

Bounds recomputed on `Resized` / `ScaleFactorChanged` via `WebView::set_bounds`. All cross-closure communication funnels through ONE `tao::EventLoopProxy<Msg>` user-event channel (avoids every Rc/circular-capture problem between webview handlers).

## Tech stack
Rust (edition 2021, MSVC toolchain), pinned from SPEC.md §7: `wry 0.56` · `tao 0.37 (features=["rwh_06"])` · `dirs 6` · `dpi 0.1` · `serde 1 (+derive)` · `serde_json 1` · build-dep `winresource 0.1`. Engine: Evergreen WebView2 Runtime (Win11 ships it; do not bundle Fixed Version). No Tauri/Electron/CEF/tabs/installer.

## Spec path
`C:\Users\USUARIO\Desktop\mini-browser\docs\SPEC.md` — the contract. Do not reopen stack/UX/profile decisions.

---

## Ground rules for every task
- Work only in `C:\Users\USUARIO\Desktop\mini-browser`. Only ever create the files listed in a task. Final file tree = SPEC.md §8 plus `.gitignore`.
- Commands run from the repo root, PowerShell. Never `git add .` / `-A` / `-u`; always `git add <paths>` then `git commit -m "<msg>" -- <same paths>` (first commit only possible after Task 1).
- Every task ends in something testable. If a verification fails, fix before moving on.
- API truth: docs.rs targeted pages `https://docs.rs/wry/0.56.1/x86_64-pc-windows-msvc/wry/` and `https://docs.rs/tao/0.37.0/x86_64-pc-windows-msvc/tao/`. All signatures used below were verified against those pages.

---

## Task checklist

### [ ] Task 1 — Repo bootstrap: git, Cargo.toml, .gitignore
**Files to create:** `Cargo.toml`, `.gitignore`
**Prereqs (verify, don't install blindly):**
- `rustup show` → active toolchain `stable-x86_64-pc-windows-msvc` (MSVC linker required).
- WebView2 Evergreen present: `(Get-ItemProperty "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" -ErrorAction SilentlyContinue).pv` → prints a version (Win11 has it). If missing, install via https://go.microsoft.com/fwlink/p/?LinkId=2124703 and re-run.

**Steps:**
1. `git init -b main` (in `mini-browser\`).
2. Create `Cargo.toml` — copy SPEC.md §7 **verbatim** (crate pins are frozen):

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

3. Create `.gitignore` with exactly one line: `/target`. (The Mini profile lives under `%LOCALAPPDATA%\Mini` — outside the repo, nothing to ignore.)
4. `cargo metadata --no-deps --format-version 1 | Out-Null ; echo $LASTEXITCODE` → prints `0`, and a `Cargo.lock` appears. This proves the dependency graph resolves.
5. Commit: `git add Cargo.toml .gitignore Cargo.lock ; git commit -m "chore: bootstrap cargo project with pinned crates" -- Cargo.toml .gitignore Cargo.lock`
6. Do NOT create `src/`, `ui/`, `assets/` yet.

---

### [ ] Task 2 — `src/omnibox.rs` + unit tests for every §4 branch
**Files to create:** `src/omnibox.rs`, `src/main.rs` (temporary stub so `cargo test` sees the module)
**Modify:** none
**Interfaces produced (used by Tasks 6–8):**

```rust
pub fn resolve(input: &str) -> Option<String> // None = ignore/reject; Some(url) = load
pub fn host_of(url: &str) -> String           // "https://example.com/a?b" -> "example.com"
```

**Steps:** create `src/main.rs` stub:

```rust
mod omnibox;

fn main() {}
```

Create `src/omnibox.rs`. Branch order matters: `localhost:3000` must hit the loopback rule **before** the generic scheme detector, or its port reads as a scheme:

```rust
//! Omnibox rule. Frozen by SPEC.md §4:
//! trim; empty -> ignore; http(s):// -> as-is; other scheme -> reject;
//! localhost / 127.0.0.1[:port][/path] -> http://; looks-like-host -> https://;
//! else -> google search.

pub fn resolve(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix("https://") {
        return Some(format!("https://{rest}"));
    }
    if let Some(rest) = s.strip_prefix("http://") {
        return Some(format!("http://{rest}"));
    }
    if is_loopback(s) {
        return Some(format!("http://{s}"));
    }
    if has_scheme(s) {
        return None; // javascript:, file:, data:, about:, any other scheme
    }
    if looks_like_host(s) {
        return Some(format!("https://{s}"));
    }
    Some(format!(
        "https://www.google.com/search?q={}",
        percent_encode(s)
    ))
}

/// SPEC.md §4: empty title pill falls back to the host name.
pub fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .to_string()
}

fn has_scheme(s: &str) -> bool {
    match s.find(':') {
        Some(i) if i > 0 => s[..i]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.'),
        _ => false,
    }
}

fn authority(s: &str) -> &str {
    s.split(['/', '?', '#']).next().unwrap_or(s)
}

fn split_port(a: &str) -> (&str, bool) {
    match a.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => (h, true),
        _ if !a.contains(':') => (a, true),
        _ => (a, false),
    }
}

fn is_loopback(s: &str) -> bool {
    let (host, ok) = split_port(authority(s));
    ok && (host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1")
}

/// SPEC.md §4 regex ^[a-zA-Z0-9.-]+(?::\d+)?(/.*)?$ AND (contains '.' or equals localhost).
fn looks_like_host(s: &str) -> bool {
    let (host, ok) = split_port(authority(s));
    if !ok || host.is_empty() {
        return false;
    }
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return false;
    }
    host.contains('.') || host.eq_ignore_ascii_case("localhost")
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_whitespace_are_ignored() {
        assert_eq!(resolve(""), None);
        assert_eq!(resolve("   "), None);
    }

    #[test]
    fn https_url_used_as_is() {
        assert_eq!(resolve("https://example.com"), Some("https://example.com".into()));
        assert_eq!(
            resolve("  https://example.com/a?b#c  "),
            Some("https://example.com/a?b#c".into())
        );
    }

    #[test]
    fn http_url_used_as_is() {
        assert_eq!(resolve("http://neverssl.com"), Some("http://neverssl.com".into()));
    }

    #[test]
    fn https_loopback_not_downgraded() {
        assert_eq!(resolve("https://localhost:3000"), Some("https://localhost:3000".into()));
    }

    #[test]
    fn javascript_rejected() {
        assert_eq!(resolve("javascript:alert(1)"), None);
    }

    #[test]
    fn file_rejected() {
        assert_eq!(resolve("file:///C:/Windows/win.ini"), None);
    }

    #[test]
    fn data_rejected() {
        assert_eq!(resolve("data:text/html,<h1>x</h1>"), None);
    }

    #[test]
    fn about_rejected() {
        assert_eq!(resolve("about:blank"), None);
    }

    #[test]
    fn bare_host_prepends_https() {
        assert_eq!(resolve("rauchg.com"), Some("https://rauchg.com".into()));
    }

    #[test]
    fn host_with_path_keeps_path() {
        assert_eq!(
            resolve("example.com/a/b?q=1"),
            Some("https://example.com/a/b?q=1".into())
        );
    }

    #[test]
    fn host_with_port_keeps_port() {
        assert_eq!(resolve("example.com:8080"), Some("https://example.com:8080".into()));
    }

    #[test]
    fn localhost_becomes_http() {
        assert_eq!(resolve("localhost"), Some("http://localhost".into()));
        assert_eq!(resolve("LocalHost"), Some("http://LocalHost".into()));
    }

    #[test]
    fn localhost_port_path_becomes_http() {
        assert_eq!(resolve("localhost:3000/api"), Some("http://localhost:3000/api".into()));
    }

    #[test]
    fn loopback_ip_becomes_http() {
        assert_eq!(resolve("127.0.0.1:8000"), Some("http://127.0.0.1:8000".into()));
    }

    #[test]
    fn words_go_to_google_search() {
        assert_eq!(
            resolve("how rust works"),
            Some("https://www.google.com/search?q=how%20rust%20works".into())
        );
    }

    #[test]
    fn punctuation_is_percent_encoded() {
        assert_eq!(
            resolve("rust & go?"),
            Some("https://www.google.com/search?q=rust%20%26%20go%3F".into())
        );
    }

    #[test]
    fn host_of_extracts_authority() {
        assert_eq!(host_of("https://example.com/a?b=c"), "example.com");
        assert_eq!(host_of("http://localhost:3000/x"), "localhost:3000");
        assert_eq!(host_of("about:blank"), "blank"); // unused fallback, must not panic
    }
}
```

**Verify:** `cargo test` → `test result: ok.` with **17 passed**, 0 failed.
**Commit:** `git add src/omnibox.rs src/main.rs ; git commit -m "feat(omnibox): spec section 4 url-vs-search resolver with full branch tests" -- src/omnibox.rs src/main.rs`

---

### [ ] Task 3 — `src/profile.rs`: exact Mini paths
**Files to create:** `src/profile.rs` **Modify:** `src/main.rs` (stub gains `mod profile;`)
**Interfaces produced (used by Task 6):**

```rust
pub fn profile_dir() -> std::path::PathBuf  // %LOCALAPPDATA%\Mini\User Data
pub fn shell_dir()   -> std::path::PathBuf  // %LOCALAPPDATA%\Mini\Shell
pub fn ensure_dirs() -> ()                  // creates both on boot
```

**Steps:** make `src/main.rs`:

```rust
mod omnibox;
mod profile;

fn main() {}
```

Create `src/profile.rs`:

```rust
//! Profile isolation roots. SPEC.md §5:
//! content (cookies/cache) -> %LOCALAPPDATA%\Mini\User Data
//! shell (chrome+fab)      -> %LOCALAPPDATA%\Mini\Shell
//! Never Chrome/Edge User Data, never <exe>.WebView2.

use std::path::PathBuf;

pub fn profile_dir() -> PathBuf {
    dirs::data_local_dir()
        .expect("%LOCALAPPDATA% is unavailable")
        .join("Mini")
        .join("User Data")
}

pub fn shell_dir() -> PathBuf {
    dirs::data_local_dir()
        .expect("%LOCALAPPDATA% is unavailable")
        .join("Mini")
        .join("Shell")
}

pub fn ensure_dirs() {
    std::fs::create_dir_all(profile_dir()).expect("create %LOCALAPPDATA%\\Mini\\User Data");
    std::fs::create_dir_all(shell_dir()).expect("create %LOCALAPPDATA%\\Mini\\Shell");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_dir_suffix_matches_spec() {
        let p = profile_dir().to_string_lossy().replace('/', "\\");
        assert!(p.ends_with("\\Mini\\User Data"), "got: {p}");
    }

    #[test]
    fn shell_dir_suffix_matches_spec() {
        let s = shell_dir().to_string_lossy().replace('/', "\\");
        assert!(s.ends_with("\\Mini\\Shell"), "got: {s}");
    }

    #[test]
    fn neither_touches_real_browsers() {
        let p = profile_dir().to_string_lossy().to_lowercase();
        assert!(!p.contains("google\\chrome"));
        assert!(!p.contains("microsoft\\edge"));
    }
}
```

**Verify:** `cargo test` → all previous 17 tests + **3 passed** (20 total). Then `cargo run` (exits instantly — stub) and check `$env:LOCALAPPDATA` is untouched: `Test-Path "$env:LOCALAPPDATA\Mini"` → `False` (ensure_dirs is only called from real main in Task 6).
**Commit:** `git add src/profile.rs src/main.rs ; git commit -m "feat(profile): isolated Mini User Data and Shell paths" -- src/profile.rs src/main.rs`

---

### [ ] Task 4 — UI HTML: `ui/chrome.html`, `ui/fab.html`, `ui/start.html`
**Files to create:** `ui/chrome.html`, `ui/fab.html`, `ui/start.html`
**Interfaces produced (JS surface Tasks 7–8 call into):** chrome exposes `window.setTitle(t)`, `window.setUrl(u)`, `window.focusUrl()`; all three pages post IPC JSON strings exactly as SPEC.md §7.
**Visual contract:** SPEC.md §2 ASCII mock — pills left (title pill + stretching URL field), caption buttons `[─][□][×]` on the RIGHT, chrome 40px, FAB dark rounded square 48px bottom-left, start placeholder string **exactly** `Enter URL or search…` (U+2026 ellipsis).

**Steps:** create the three files:

`ui/chrome.html`:

```html
<!doctype html>
<html>
<head>
<meta charset="utf-8">
<style>
  html, body { margin:0; height:100%; overflow:hidden; background:#f5f5f5;
               font-family:"Segoe UI", system-ui, sans-serif; user-select:none; }
  #bar { display:flex; align-items:center; gap:8px; height:40px; box-sizing:border-box; padding-left:12px; }
  #title { max-width:230px; padding:3px 10px; border-radius:999px; background:#e7e7e7;
           font-size:12px; color:#333; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
  #url { flex:1; min-width:60px; height:26px; border:none; border-radius:999px; padding:0 12px;
         font-size:13px; background:#fff; color:#111; outline:none; }
  .cap { width:46px; height:40px; border:none; background:transparent; padding:0;
         display:flex; align-items:center; justify-content:center; }
  .cap:hover { background:#e0e0e0; }
  #close:hover { background:#e81123; }
  #close:hover svg { stroke:#fff; }
  svg { stroke:#444; fill:none; stroke-width:1; }
</style>
</head>
<body>
<div id="bar">
  <span id="title">Mini</span>
  <input id="url" spellcheck="false" autocomplete="off">
  <button class="cap" id="btn-min" title="Minimize"><svg width="10" height="10" viewBox="0 0 10 10"><path d="M0 5H10"/></svg></button>
  <button class="cap" id="btn-max" title="Maximize"><svg width="10" height="10" viewBox="0 0 10 10"><rect x="0.5" y="0.5" width="9" height="9"/></svg></button>
  <button class="cap" id="close" title="Close"><svg width="10" height="10" viewBox="0 0 10 10"><path d="M0 0L10 10 M10 0L0 10"/></svg></button>
</div>
<script>
  const post = (msg) => window.ipc.postMessage(msg);
  document.getElementById('url').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') post(JSON.stringify({ cmd: 'navigate', text: e.target.value }));
  });
  window.setTitle = (t) => { document.getElementById('title').textContent = t; };
  window.setUrl = (u) => { document.getElementById('url').value = u; };
  window.focusUrl = () => { document.getElementById('url').focus(); document.getElementById('url').select(); };
  for (const [id, cmd] of [['btn-min','minimize'], ['btn-max','maximize'], ['close','close']])
    document.getElementById(id).addEventListener('click', () => post(JSON.stringify({ cmd })));
  document.body.addEventListener('mousedown', (e) => {
    if (e.target.id === 'url' || e.target.closest('.cap')) return;
    if (e.button === 0) post(JSON.stringify({ cmd: 'drag' }));
  });
  document.addEventListener('keydown', (e) => {
    if (e.ctrlKey && !e.shiftKey && !e.altKey && e.key === 'l') { e.preventDefault(); focusUrl(); }
  });
</script>
</body>
</html>
```

`ui/fab.html`:

```html
<!doctype html>
<html>
<head>
<meta charset="utf-8">
<style>
  html, body { margin:0; height:100%; background:transparent; overflow:hidden; }
  #fab { width:48px; height:48px; border-radius:14px; background:#141414; cursor:default;
         display:flex; align-items:center; justify-content:center; }
  svg { stroke:#ffffff; fill:none; }
</style>
</head>
<body>
<div id="fab">
  <svg width="22" height="22" viewBox="0 0 24 24" stroke-width="2"
       stroke-linecap="round" stroke-linejoin="round">
    <path d="M3 11l9-8 9 8"/><path d="M5 10v10h14V10"/>
  </svg>
</div>
<script>
  document.getElementById('fab').addEventListener('click',
    () => window.ipc.postMessage('{"cmd":"home"}'));
</script>
</body>
</html>
```

`ui/start.html`:

```html
<!doctype html>
<html>
<head>
<meta charset="utf-8">
<style>
  html, body { margin:0; height:100%; background:#ffffff;
               font-family:"Segoe UI", system-ui, sans-serif; }
  body { display:flex; align-items:center; justify-content:center; }
  input { width:min(520px, 80vw); height:38px; border:1px solid #ddd; border-radius:999px;
          padding:0 18px; font-size:15px; color:#111; outline:none; text-align:center; }
  input:focus { border-color:#888; }
</style>
</head>
<body>
<input id="q" placeholder="Enter URL or search…" spellcheck="false" autocomplete="off" autofocus>
<script>
  document.getElementById('q').addEventListener('keydown', (e) => {
    if (e.key === 'Enter')
      window.ipc.postMessage(JSON.stringify({ cmd: 'navigate', text: e.target.value }));
  });
</script>
</body>
</html>
```

**Verify (pure static):** `Select-String -Path ui\start.html -Pattern 'Enter URL or search…' -SimpleMatch` → 1 match. Open `ui\start.html` in Edge manually → centered field with the placeholder renders.
**Commit:** `git add ui/chrome.html ui/fab.html ui/start.html ; git commit -m "feat(ui): chrome bar, home fab, start page html" -- ui/chrome.html ui/fab.html ui/start.html`

---

### [ ] Task 5 — Icon + `build.rs` (winresource, cfg-windows)
**Files to create:** `assets/icon.ico`, `build.rs`
**Interfaces produced:** exe embeds the icon automatically at link time (SPEC.md §9). Asset path contract: `assets/icon.ico`.
**Icon look (SPEC.md §8):** black squircle, inner dark circle, thin light ring.

**Steps:**
1. `New-Item -ItemType Directory -Force assets | Out-Null`
2. Generate the ICO (single 256×256 PNG-compressed entry, valid since Vista) — run this whole block in PowerShell:

```powershell
Add-Type -AssemblyName System.Drawing
$size = 256
$bmp = New-Object System.Drawing.Bitmap($size, $size)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = 'AntiAlias'
$g.Clear([System.Drawing.Color]::Transparent)
$p = New-Object System.Drawing.Drawing2D.GraphicsPath
$r = 64
$p.AddArc(0,0,$r,$r,180,90); $p.AddArc($size-$r,0,$r,$r,270,90)
$p.AddArc($size-$r,$size-$r,$r,$r,0,90); $p.AddArc(0,$size-$r,$r,$r,90,90)
$p.CloseFigure()
$g.FillPath((New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255,17,17,17))), $p)
$g.DrawEllipse((New-Object System.Drawing.Pen([System.Drawing.Color]::Silver, 8)), 68, 68, 120, 120)
$g.FillEllipse((New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255,34,34,34))), 88, 88, 80, 80)
$ms = New-Object System.IO.MemoryStream
$bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
$png = $ms.ToArray()
$ico = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter($ico)
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]1)
$w.Write([byte]0); $w.Write([byte]0); $w.Write([byte]0); $w.Write([byte]0)
$w.Write([uint16]1); $w.Write([uint16]32)
$w.Write([uint32]$png.Length); $w.Write([uint32]22)
$w.Write($png); $w.Flush()
[System.IO.File]::WriteAllBytes("$PWD\assets\icon.ico", $ico.ToArray())
$g.Dispose(); $bmp.Dispose()
```

3. Sanity-check: `(Get-Item assets\icon.ico).Length` → >200 bytes and <50KB.
4. Create `build.rs`:

```rust
#[cfg(windows)]
fn main() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    res.compile()
        .expect("failed to compile Windows resources (assets/icon.ico)");
}

#[cfg(not(windows))]
fn main() {}
```

5. `cargo build` (compiles the stub binary; build script must succeed) → exit 0. Confirm icon landed: right-click `target\debug\mini.exe` → Properties shows the squircle icon.
**Commit:** `git add assets/icon.ico build.rs ; git commit -m "feat(brand): black squircle icon via winresource" -- assets/icon.ico build.rs`

---

### [ ] Task 6 — `src/main.rs`: window + three child webviews + layout
**Files to modify:** `src/main.rs` (replace stub entirely)
**Interfaces produced (Tasks 7–8 extend these):**

```rust
#[derive(Debug)] enum Msg { ChromeIpc(String), ContentIpc(String), FabIpc(String),
                            TitleChanged(String), PageLoad(PageLoadEvent, String), NewWindow(String) }
struct App { window: Window, chrome: WebView, content: WebView, fab: WebView,
             on_start: bool, mods: ModifiersState }
fn layout(scale: f64, px: dpi::PhysicalSize<u32>) -> (Rect, Rect, Rect)
fn relayout(scale: f64, px: dpi::PhysicalSize<u32>, chrome: &WebView, content: &WebView, fab: &WebView)
fn handle_msg(app: &mut App, msg: Msg, control_flow: &mut ControlFlow)
```

**Steps:** replace `src/main.rs` with the full skeleton below. Handlers forward everything as `Msg` through the event-loop proxy; `handle_msg`/`handle_key` start as empty stubs (filled by Tasks 7–8):

```rust
mod omnibox;
mod profile;

use dpi::PhysicalSize;
use tao::dpi::LogicalPosition;
use tao::event::{ElementState, Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::keyboard::{Key, ModifiersState, NamedKey};
use tao::platform::windows::WindowBuilderExtWindows;
use tao::window::{Icon, Window, WindowBuilder};
use wry::{NewWindowResponse, PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder};

const CTRL_L_SCRIPT: &str = r#"
document.addEventListener('keydown', (e) => {
  if (e.ctrlKey && !e.shiftKey && !e.altKey && e.key === 'l') {
    e.preventDefault();
    window.ipc.postMessage('{"cmd":"focus-url"}');
  }
}, true);
"#;

#[derive(Debug)]
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
    url.starts_with("http://") || url.starts_with("https://")
}

fn handle_key(_app: &mut App, _key: &Key<'_>, _cf: &mut ControlFlow) {}

fn handle_msg(_app: &mut App, _msg: Msg, _control_flow: &mut ControlFlow) {}

fn main() {
    profile::ensure_dirs();

    let event_loop = EventLoop::<Msg>::with_user_event();
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
```

If the compiler complains about the `Key<'_>` spelling in `handle_key`, use `Key<'static>` per its suggestion — do not change logic.

**Verify:** `cargo run` → undecorated 1100×740 window, Win11 shadow/rounded corners, chrome bar painted top, centered `Enter URL or search…` start page, **no** FAB visible. Resize the window → chrome stays 40px full-width, content fills the rest. Close via Alt+F4. Also confirm `%LOCALAPPDATA%\Mini\User Data` and `%LOCALAPPDATA%\Mini\Shell` now exist.
**Commit:** `git add src/main.rs ; git commit -m "feat(main): tao window with three child webviews and live layout" -- src/main.rs`

---

### [ ] Task 7 — Chrome/FAB IPC: navigate, drag, min/max/close, home
**Files to modify:** `src/main.rs`
**Interfaces produced (used by Task 8):**

```rust
impl App {
    fn set_chrome_url(&self, url: &str) -> ()
    fn set_chrome_title(&self, title: &str) -> ()
    fn navigate(&mut self, raw: &str) -> ()
    fn go_home(&mut self) -> ()
}
```

**Steps:** in `src/main.rs`:
1. Add the `impl App` block:

```rust
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
        self.on_start = true;
        let _ = self.fab.set_visible(false);
        self.set_chrome_title("Mini");
        self.set_chrome_url("");
        let _ = self.chrome.focus();
    }
}
```

2. Replace `handle_msg` with:

```rust
#[derive(serde::Deserialize)]
struct IpcCmd {
    cmd: String,
    #[serde(default)]
    text: Option<String>,
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
                    _ => {} // unknown cmd -> ignore (SPEC.md §7)
                }
            }
        }
        Msg::ContentIpc(_body) => {}                          // Task 8
        Msg::TitleChanged(title) => {
            let label = if title.is_empty() {
                omnibox::host_of(app.content.url().unwrap_or_default())
            } else {
                title
            };
            app.set_chrome_title(&label);                     // provisional; refined in Task 8
        }
        Msg::PageLoad(_, url) => app.set_chrome_url(&url),
        Msg::NewWindow(url) => {
            let _ = app.content.load_url(&url); // in-place; Deny already returned
        }
    }
}
```

**Verify:** `cargo run`:
- Type `https://example.com` in the chrome URL field + Enter → page loads, URL field shows the URL, FAB appears bottom-left.
- Type `rauchg.com` → loads `https://rauchg.com`; type `how rust works` → URL becomes `https://www.google.com/search?q=how%20rust%20works`.
- Click FAB → back to start page, FAB gone, title pill `Mini`, URL field emptied.
- Drag empty chrome area (and the title pill) → window moves; dragging from URL input or caption buttons does NOT move it.
- Min/max/close buttons work (max toggles, close exits).
- Type `javascript:alert(1)` → nothing happens.
**Commit:** `git add src/main.rs ; git commit -m "feat(chrome): wire caption drag minimize maximize close navigate home ipc" -- src/main.rs`

---

### [ ] Task 8 — Content sync + security: title/url, window.open, Ctrl+L, keys
**Files to modify:** `src/main.rs`
**Interfaces produced:** completes `Msg::ContentIpc` dispatch (allowlist), `App::content_on_start_page()`, real `handle_key`.

```rust
impl App {
    fn content_on_start_page(&self) -> bool
}
fn handle_key(app: &mut App, key: &tao::keyboard::Key<'_>, cf: &mut ControlFlow) -> ()
```

**Steps:**
1. Add to `impl App`:

```rust
    /// True while content shows the null-origin start page (not an http(s) site).
    fn content_on_start_page(&self) -> bool {
        match self.content.url() {
            Ok(u) => !(u.starts_with("http://") || u.starts_with("https://")),
            Err(_) => true,
        }
    }
```

2. Replace the two provisional arms in `handle_msg`:
   - `Msg::ContentIpc` — allowlist per SPEC.md §3: remote pages may only ask `focus-url`; `navigate` is accepted **only while the content webview still shows the null-origin start page**, which is how `ui/start.html`'s field submits without giving untrusted pages any navigate channel:

```rust
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
                    _ => {} // everything else from content: ignored
                }
            }
        }
```

   - `Msg::TitleChanged` — refine the empty-title fallback so it never shows junk for non-http pages:

```rust
        Msg::TitleChanged(title) => {
            if !title.is_empty() {
                app.set_chrome_title(&title);
            } else if !app.on_start {
                let host = omnibox::host_of(app.content.url().unwrap_or_default());
                app.set_chrome_title(&host);
            }
        }
```

3. Replace the stub `handle_key` (keys land here because `with_browser_accelerator_keys(false)` stops WebView2 from eating browser accelerators):

```rust
fn handle_key(app: &mut App, key: &Key<'_>, _cf: &mut ControlFlow) {
    let ctrl = app.mods.ctrl();
    let alt = app.mods.alt();
    match key {
        Key::Named(NamedKey::F5) => {
            let _ = app.content.reload();
        }
        Key::Character("r") if ctrl => {
            let _ = app.content.reload();
        }
        Key::Named(NamedKey::ArrowLeft) if alt && !ctrl => {
            let _ = app.content.go_back(); // errors ignored when history empty (SPEC.md §4)
        }
        Key::Named(NamedKey::ArrowRight) if alt && !ctrl => {
            let _ = app.content.go_forward();
        }
        Key::Named(NamedKey::Home) if alt && !ctrl => app.go_home(),
        _ => {}
    }
}
```

(`Key`/`NamedKey` are already imported at the top of `main.rs` from Task 6.)

**Verify:** `cargo run`, then walk: `https://example.com` → title pill becomes `Example Domain` (row 9); click a link on some site → URL field follows (row 10); on any page press `Ctrl+L` → chrome URL focused+selected (row 12); `Alt+Left`/`Alt+Right` history-walk; `F5` and `Ctrl+R` reload; `Alt+Home` → start page; find a link with `target=_blank` (e.g. on `davidwalsh.name`) → opens in the same view, no second window (row 13); `javascript:` in either field → nothing (row 16).
**Commit:** `git add src/main.rs ; git commit -m "feat(content): title url sync ctrl+l allowlist window.open inplace and hotkeys" -- src/main.rs`

---

### [ ] Task 9 — Isolation proof procedure (SPEC.md §10 row 14, §5)
**Files to modify/create:** none (verification-only task; keep a transcript of results)
**Steps (execute in order):**
1. Baseline snapshot:
   `Get-ChildItem -Recurse "$env:LOCALAPPDATA\Google\Chrome\User Data" -ErrorAction SilentlyContinue | Measure-Object | Select-Object Count` → note count C₁ (if Chrome never installed: record “absent”).
   Same for `$env:LOCALAPPDATA\Microsoft\Edge\User Data` → note E₁.
2. `cargo run`. First-run check: `Get-ChildItem "$env:LOCALAPPDATA\Mini"` → exactly `Shell` and `User Data`. Inside `User Data` expect the WebView2 root folder `EBWebView`. Confirm no default-profile folder next to the binary: `Test-Path "target\debug\mini.exe.WebView2"` → `False`.
3. Log into a real account in Mini (e.g., `github.com`). Then close Mini.
4. Prove the cookie lives only in Mini's profile:
   - Re-open Mini → still logged in (persisted storage worked).
   - Run again after `Remove-Item -Recurse -Force "$env:LOCALAPPDATA\Mini"` → logged out. This deletion is the strongest proof that session state lives under `Mini\User Data`. Log in again to restore.
5. Prove Chrome/Edge untouched: recount step 1 dirs → counts equal C₁/E₁ (any new subfolder there is a failure). Open real Chrome/Edge → still logged out of the account used in step 3.
6. Prove shell/content separation: in Mini visit a site that sets cookies; open devtools on the chrome webview (debug build: right-click chrome bar → Inspect) and run `document.cookie` → must NOT contain that site's cookies; the content webview's devtools must.
7. Negative checks from SPEC.md §5: confirm no `with_incognito` anywhere: `Select-String -Path src\*.rs -Pattern incognito` → no matches.
**Exit criterion:** all seven observations recorded and passing.

---

### [ ] Task 10 — Walk SPEC.md §10 acceptance table end-to-end
**Files to modify:** none expected; if a row fails, return to the owning task, fix, re-run this table.
**Steps:** `cargo test` then `cargo run` and execute every row. Mapping of each row to its covering task:

| §10 row | Covered by |
|---|---|
| 1 undecorated, rounded/shadow, Mini icon | Task 6 (shadow/class) + Task 5 (exe/taskbar icon) + Task 1 (prereqs) |
| 2 start page centered `Enter URL or search…`, FAB hidden | Task 4 (start.html, exact placeholder) + Task 6 (`with_visible(false)`) |
| 3 title pill left, URL field, caption right | Task 4 (chrome.html layout) + Task 7 (behavior) |
| 4 drag empty chrome moves window | Task 4 (drag region) + Task 7 (`drag_window`) |
| 5 min / max / close | Task 7 (`set_minimized`/`set_maximized`/Exit) |
| 6 `https://example.com` loads | Task 7 (navigate) |
| 7 `rauchg.com` → `https://rauchg.com` | Task 2 (unit) + Task 7 (live) |
| 8 `how rust works` → Google | Task 2 (unit) + Task 7 (live) |
| 9 title pill updates ("Example Domain") | Task 8 (`TitleChanged`) |
| 10 URL field tracks in-page nav | Task 8 (`PageLoad`) |
| 11 FAB appears off start; click returns | Task 6 (hidden at boot) + Task 7 (show/go_home) |
| 12 Ctrl+L focuses URL on https page | Task 4/6 (CTRL_L_SCRIPT byte-exact) + Task 8 (focus-url arm) |
| 13 `window.open`/_target blank in place | Task 6 (`Deny`) + Task 8 (`load_url`) |
| 14 profile only under `%LOCALAPPDATA%\Mini\User Data` | Task 3 (paths) + Task 6 (two contexts) + Task 9 (proof) |
| 15 extensions off | Task 6 (`with_browser_extensions_enabled(false)` ×3) |
| 16 `javascript:alert(1)` rejected | Task 2 (unit) + Task 7 (ignored live) |
| 17 `cargo test` omnibox cases | Task 2 (17 tests) + Task 3 (3 tests) |

Every row maps. Final gate: `cargo test` → all green; `cargo run` → all 17 rows observed.
**Commit (only if you changed something to make a row pass):** `git add src/main.rs ; git commit -m "fix: satisfy acceptance row N" -- src/main.rs`

---

## Appendix A — Verified API facts used above (docs.rs, x86_64-pc-windows-msvc)
- wry 0.56.1: `WebViewBuilder::new_with_web_context(&mut WebContext)`, `with_id(&str)` (`WebViewId<'a> = &'a str`), `with_html(impl Into<String>)`, `with_ipc_handler(Fn(Request<String>))`, `with_navigation_handler(Fn(String)->bool)`, `with_document_title_changed_handler(Fn(String))`, `with_on_page_load_handler(Fn(PageLoadEvent, String))`, `with_new_window_req_handler(Fn(String, NewWindowFeatures)->NewWindowResponse)`, `with_bounds(Rect)`, `with_focused/with_visible/with_transparent/with_background_color(RGBA)/with_initialization_script/with_clipboard/with_hotkeys_zoom/with_devtools(bool)`, `build_as_child(&HasWindowHandle)`; ext-Windows `with_browser_extensions_enabled(bool)`, `with_browser_accelerator_keys(bool)`; `WebView::{set_bounds,set_visible,focus,load_url,load_html,url,evaluate_script,go_back,go_forward,reload}` inherent methods; `NewWindowResponse::{Allow, Create{webview}, Deny}`; `PageLoadEvent::{Started, Finished}`; `Rect { position: Position, size: Size }`.
- tao 0.37.0: classic `EventLoop::<T>::with_user_event()` + `create_proxy()` (`EventLoopProxy<T: 'static>::send_event(T) -> Result<(), EventLoopClosed<T>>`, `Clone`) + `run(move |Event<T>, _, &mut ControlFlow|)`; `Event::UserEvent(T)`; `WindowEvent::{Resized(PhysicalSize), ScaleFactorChanged{new_inner_size,..}, ModifiersChanged(ModifiersState), KeyboardInput{event: KeyEvent,..}, CloseRequested}`; `Window::{drag_window, set_minimized, set_maximized, is_maximized, inner_size, scale_factor}`; ext-Windows `with_undecorated_shadow(bool)`, `with_window_classname(S: Into<String>)`.

## Appendix B — Spec tensions found during planning, and their resolutions
1. **Start-page submit vs content-IPC lockdown.** SPEC §2/§4 require the start-page field to navigate via the omnibox rule, but §3 says "Navigate is never accepted from content IPC" and restricts content IPC to `focus-url` only. **Resolution (Task 8):** content IPC accepts `navigate` only while the content webview still shows the null-origin start page; once on http(s), navigate from content is dead. This preserves §3's stated threat model ("XSS can only request focus the URL bar") and makes §2 work.
2. **Window icon.** SPEC §3 shows `.with_window_icon(/* from assets */)`, but decoding an .ICO into a tao `Icon` needs an image codec absent from the §7 dependency list. **Resolution:** rely on the winresource exe icon (taskbar shows it; an undecorated window draws no titlebar icon anyway). Acceptance row 1 still passes visually.
3. **Resize strip.** SPEC §2 offers an optional 6px `resize-se` IPC strip. **Not built:** that IPC command is not in §7's frozen JSON set, and Win11's undecorated-shadow windows already expose resize borders. Revisit only if live resize actually fails during Task 10 — and if so, amend SPEC.md first.

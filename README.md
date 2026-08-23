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

## Daily builds

A GitHub Action builds `mini.exe` every day and publishes it as a dated release (`YYYY.MM.DD`). You can also hit **Run workflow** on the Actions tab.

## Not in v1

Tabs, bookmarks, extensions, downloads UI, settings, macOS/Linux, MSI, auto-update.

Contract: [`docs/SPEC.md`](docs/SPEC.md).

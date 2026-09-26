//! Ad blocker for the content webview: a host blocklist enforced natively via
//! WebView2 `WebResourceRequested` (WebView2 loads MV3 extensions, but their
//! declarativeNetRequest rules are matched and NOT enforced, measured 2026-09-26).
//!
//! List: `%LOCALAPPDATA%\Mini\adblock\hosts.txt`, hosts-file or plain-domain
//! lines. A request is answered with an empty 403 when its host or any parent
//! domain is listed. No list file = no blocking. `MINI_ADBLOCK=0` turns it off.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{take_pwstr, WebResourceRequestedEventHandler};
use windows::core::{HSTRING, PWSTR};
use windows::Win32::System::Com::IStream;
use wry::{WebView, WebViewExtWindows};

/// YouTube serves its video ads from youtube.com itself, so host blocking
/// cannot stop them. On youtube.com this script skips each ad: mutes it,
/// jumps to its end, and clicks "Skip". Runs only while `.ad-showing` is set.
pub const YOUTUBE_AD_SKIP_SCRIPT: &str = r#"(() => {
  if (!/(^|\.)youtube\.com$/.test(location.hostname)) return;
  setInterval(() => {
    const player = document.querySelector('.html5-video-player');
    if (!player || !player.classList.contains('ad-showing')) return;
    const v = player.querySelector('video');
    if (v) { v.muted = true; if (isFinite(v.duration) && v.duration > 0) v.currentTime = v.duration; }
    for (const b of document.querySelectorAll('.ytp-skip-ad-button, .ytp-ad-skip-button, .ytp-ad-skip-button-modern')) b.click();
  }, 300);
  // Restore sound after the ad (the ad's mute can stick to the main video).
  let wasAd = false;
  setInterval(() => {
    const player = document.querySelector('.html5-video-player');
    const ad = !!(player && player.classList.contains('ad-showing'));
    if (wasAd && !ad) { const v = player.querySelector('video'); if (v) v.muted = false; }
    wasAd = ad;
  }, 300);
})();"#;

/// True when a block list is installed (the YouTube script is added with it).
pub fn enabled() -> bool {
    load().is_some()
}

pub fn list_path() -> PathBuf {
    dirs::data_local_dir()
        .expect("%LOCALAPPDATA% is unavailable")
        .join("Mini")
        .join("adblock")
        .join("hosts.txt")
}

/// Parses hosts-file / plain-domain text into a set of lowercase hosts.
pub fn parse_list(text: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let first = parts.next().unwrap_or("");
        let host = match first {
            "0.0.0.0" | "127.0.0.1" | "::" | "::1" => parts.next().unwrap_or(""),
            _ => first,
        };
        let host = host.trim_start_matches("||").trim_end_matches('^').trim_end_matches('.').to_ascii_lowercase();
        if host.contains('.') && host != "localhost" && !host.contains('/') {
            set.insert(host);
        }
    }
    set
}

/// Host part of an http(s)/ws(s) URL, lowercase, no port / credentials.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let host = if authority.starts_with('[') {
        authority.split(']').next()?.trim_start_matches('[')
    } else {
        authority.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// True when the host or any parent domain is on the list.
pub fn is_blocked(list: &HashSet<String>, url: &str) -> bool {
    let Some(host) = host_of(url) else { return false };
    let mut h = host.as_str();
    loop {
        if list.contains(h) {
            return true;
        }
        match h.split_once('.') {
            Some((_, parent)) if parent.contains('.') => h = parent,
            _ => return false,
        }
    }
}

pub fn load() -> Option<HashSet<String>> {
    if std::env::var("MINI_ADBLOCK").map(|v| v == "0").unwrap_or(false) {
        return None;
    }
    let text = std::fs::read_to_string(list_path()).ok()?;
    let set = parse_list(&text);
    (!set.is_empty()).then_some(set)
}

/// Installs the blocker on the content webview. Returns the number of listed
/// hosts, or 0 when there is no list. Errors are printed, never fatal.
pub fn install(content: &WebView) -> usize {
    let Some(list) = load() else { return 0 };
    let n = list.len();
    let list = Arc::new(list);
    let blocked = Arc::new(AtomicU64::new(0));
    let webview = content.webview();
    let env = content.environment();
    if let Err(e) = unsafe { webview.AddWebResourceRequestedFilter(&HSTRING::from("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL) } {
        eprintln!("mini: adblock filter failed: {e}");
        return 0;
    }
    let handler = WebResourceRequestedEventHandler::create(Box::new(move |_sender, args| {
        let Some(args) = args else { return Ok(()) };
        let Ok(request) = (unsafe { args.Request() }) else { return Ok(()) };
        let mut uri = PWSTR::null();
        if unsafe { request.Uri(&mut uri) }.is_err() {
            return Ok(());
        }
        let url = take_pwstr(uri);
        if is_blocked(&list, &url) {
            if let Ok(resp) = unsafe {
                env.CreateWebResourceResponse(None::<&IStream>, 403, &HSTRING::from("Blocked by Mini"), &HSTRING::from(""))
            } {
                let _ = unsafe { args.SetResponse(&resp) };
                blocked.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(())
    }));
    let mut token: i64 = 0;
    if let Err(e) = unsafe { webview.add_WebResourceRequested(&handler, &mut token) } {
        eprintln!("mini: adblock handler failed: {e}");
        return 0;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hosts_and_plain_lines() {
        let s = parse_list("# c\n0.0.0.0 ads.example.com\n127.0.0.1 localhost\ndoubleclick.net\n||adnxs.com^\n\nbad\n");
        assert!(s.contains("ads.example.com") && s.contains("doubleclick.net") && s.contains("adnxs.com"));
        assert!(!s.contains("localhost") && !s.contains("bad"));
    }

    #[test]
    fn blocks_host_and_subdomains_only() {
        let s = parse_list("doubleclick.net\n");
        assert!(is_blocked(&s, "https://securepubads.g.doubleclick.net/tag/js/gpt.js"));
        assert!(is_blocked(&s, "https://doubleclick.net:443/x"));
        assert!(!is_blocked(&s, "https://notdoubleclick.net/"));
        assert!(!is_blocked(&s, "https://example.com/?u=doubleclick.net"));
        assert!(!is_blocked(&s, "data:text/html,hi"));
    }

    #[test]
    fn host_of_strips_port_and_userinfo() {
        assert_eq!(host_of("https://u:p@Ads.Example.com:8080/a").as_deref(), Some("ads.example.com"));
        assert_eq!(host_of("http://[::1]:9/").as_deref(), Some("::1"));
    }
}

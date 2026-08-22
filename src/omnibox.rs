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
    let rest = url
        .split_once("://")
        .or_else(|| url.split_once(':'))
        .map(|(_, r)| r)
        .unwrap_or(url);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .to_string()
}

fn has_scheme(s: &str) -> bool {
    match s.find(':') {
        Some(i) if i > 0 => {
            // `.` is a valid RFC 3986 scheme char, but treating it as one
            // makes `example.com:8080` look like scheme `example.com` and
            // rejects the host:port case required by the §4 tests.
            s[..i]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-')
        }
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

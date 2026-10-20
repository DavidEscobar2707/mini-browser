//! Mutation firewall and element blocklist. Pure policy: the transport calls
//! [`should_block`], the loop calls [`candidate_elements`].
//!
//! Frozen signatures — implement the bodies, do not change the shapes.

use crate::qa::ObservedElement;
use serde::Serialize;

/// One request the firewall failed.
#[derive(Clone, Debug, Serialize)]
pub struct Blocked {
    pub ts_ms: u64,
    pub method: String,
    pub url: String,
    /// `fetch` | `web-resource-requested`
    pub mechanism: String,
}

#[derive(Clone, Debug)]
pub struct FirewallPolicy {
    pub allow_writes: bool,
    /// Target host, lowercase, no port.
    pub host: String,
}

/// Everything that is not GET/HEAD/OPTIONS is a write. `allow_writes` disables the firewall.
///
/// `allow_writes` only lifts the block for the site under test: a write aimed
/// at any other host (tracker, ad network, payment processor picked up along
/// the way) stays blocked no matter what the operator asked for.
pub fn should_block(p: &FirewallPolicy, method: &str, url: &str) -> bool {
    if !is_write(method) {
        return false;
    }
    if !p.allow_writes {
        return true;
    }
    !first_party(&p.host, url)
}

fn is_write(method: &str) -> bool {
    !matches!(method.to_ascii_uppercase().as_str(), "GET" | "HEAD" | "OPTIONS")
}

/// Same host, or a subdomain of it.
pub fn first_party(host: &str, url: &str) -> bool {
    same_site(host, &host_only(url))
}

/// Case-insensitive "same host, or a subdomain of it" check on two bare
/// hostnames (no scheme, no port).
fn same_site(target_host: &str, candidate_host: &str) -> bool {
    let target = target_host.to_ascii_lowercase();
    let candidate = candidate_host.to_ascii_lowercase();
    candidate == target || candidate.ends_with(&format!(".{target}"))
}

/// Lowercased hostname out of an absolute URL; port and everything else
/// stripped. `pub(crate)` so `probes::read_net` can report a consistent
/// bare host without re-deriving it.
pub(crate) fn host_only(url: &str) -> String {
    let authority = crate::omnibox::host_of(url);
    let host = match authority.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => authority.as_str(),
    };
    host.to_ascii_lowercase()
}

/// Destructive verbs, matched case-insensitively against the accessible label.
pub const WRITE_VERBS: [&str; 18] = [
    "submit", "send", "pay", "buy", "checkout", "delete", "remove", "sign up", "create", "book",
    "confirm", "post", "publish", "apply", "save", "subscribe", "log in", "sign in",
];

/// Code-only blocklist (SPEC: never ask the model what a regex knows).
///
/// This only ever sees one element, so "off-host link" is not judged here —
/// that needs the QA target host, which [`candidate_elements`] already has
/// and excludes separately.
pub fn is_write_like_element(el: &ObservedElement) -> bool {
    let submit_like = matches!(el.input_type.as_deref(), Some("submit") | Some("image"));
    let in_post_form = el.form_method.as_deref() == Some("post");
    let risky_scheme =
        matches!(el.href_host.as_deref(), Some("mailto") | Some("tel") | Some("javascript"));
    submit_like || in_post_form || risky_scheme || label_matches_write_verb(&el.label)
}

/// Word-level match for single-word verbs (so "Play" never hits "pay"),
/// substring match for the multi-word phrases ("sign up", "log in", "sign in").
fn label_matches_write_verb(label: &str) -> bool {
    let lower = label.to_ascii_lowercase();
    WRITE_VERBS.iter().any(|verb| {
        if verb.contains(' ') {
            lower.contains(verb)
        } else {
            lower.split(|c: char| !c.is_alphanumeric()).any(|word| word == *verb)
        }
    })
}

/// Path/query substrings that mark a page the run must never act on.
const FORBIDDEN_PATH_MARKERS: [&str; 14] = [
    "login",
    "log-in",
    "signin",
    "sign-in",
    "signup",
    "sign-up",
    "register",
    "checkout",
    "cart",
    "pay",
    "payment",
    "billing",
    "account-delete",
    "delete-account",
];

/// Pages the QA run must not act on at all.
pub fn is_forbidden_page(url: &str) -> bool {
    let path = path_and_query_of(url).to_ascii_lowercase();
    forbidden_marker_in(&path)
}

/// Same predicate for a link's `href`, which is often relative (`/checkout`)
/// and has no scheme to strip.
pub fn is_forbidden_target(href: &str) -> bool {
    forbidden_marker_in(&href.to_ascii_lowercase())
}

fn forbidden_marker_in(haystack: &str) -> bool {
    // Token boundaries, not raw substrings: `/payload` must not read as `pay`
    // and `/registered-users` must not read as `register`.
    let normalized: String =
        haystack.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let padded = format!("-{normalized}-");
    FORBIDDEN_PATH_MARKERS.iter().any(|marker| padded.contains(&format!("-{marker}-")))
}

/// Everything from the first `/` after the scheme+authority onward (path,
/// query, fragment); empty when the URL is bare (e.g. `https://example.com`).
fn path_and_query_of(url: &str) -> &str {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    match after_scheme.find('/') {
        Some(i) => &after_scheme[i..],
        None => "",
    }
}

/// The click/fill candidates the model is allowed to choose from.
pub fn candidate_elements(els: &[ObservedElement], host: &str) -> Vec<ObservedElement> {
    els.iter()
        .filter(|el| matches!(el.kind.as_str(), "click" | "fill" | "select"))
        .filter(|el| is_visible(el))
        .filter(|el| !el.disabled)
        .filter(|el| !is_write_like_element(el))
        .filter(|el| !is_offsite_link(el, host))
        // Never walk into login/checkout/account pages, even on our own host.
        .filter(|el| !el.href.as_deref().map(is_forbidden_target).unwrap_or(false))
        // `<button formaction="/checkout">` carries no href.
        .filter(|el| !el.form_action.as_deref().map(is_forbidden_target).unwrap_or(false))
        .cloned()
        .collect()
}

/// The candidates for `mini --move` on a page the operator owns: every
/// visible, enabled click/fill/select control plus the page-level scroll/wait
/// controls. No write-verb or blocklist filter — signing in to a local app is
/// the point of `--move`. `--qa` keeps using [`candidate_elements`].
pub fn dev_candidates(els: &[ObservedElement]) -> Vec<ObservedElement> {
    let mut out: Vec<ObservedElement> = els
        .iter()
        // "goto" = off-screen heading jump (move mode only; the snapshot emits it only there).
        .filter(|el| matches!(el.kind.as_str(), "click" | "fill" | "select" | "goto"))
        .filter(|el| is_visible(el))
        .filter(|el| !el.disabled)
        .cloned()
        .collect();
    out.extend(global_controls(els));
    out
}

fn is_visible(el: &ObservedElement) -> bool {
    el.rect.w > 0.0 && el.rect.h > 0.0
}

/// The snapshot's non-element operations (scroll up/down, wait). They act on the
/// page, never on a control, so the element blocklist does not apply — the loop
/// adds them to the candidates so the model can follow a long page or wait out a
/// load instead of declaring itself stuck.
pub fn global_controls(els: &[ObservedElement]) -> Vec<ObservedElement> {
    els.iter().filter(|el| matches!(el.kind.as_str(), "scroll" | "wait")).cloned().collect()
}

fn is_offsite_link(el: &ObservedElement, host: &str) -> bool {
    match el.href_host.as_deref() {
        // `href_host` is the URL's `host`, port included; `host` is a bare hostname.
        Some(h) => !same_site(host, strip_port(h)),
        None => false,
    }
}

/// `127.0.0.1:8000` -> `127.0.0.1`; anything that does not end in `:<digits>` is unchanged.
fn strip_port(authority: &str) -> &str {
    match authority.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => authority,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qa::Rect;

    fn el(kind: &str) -> ObservedElement {
        ObservedElement { kind: kind.to_string(), ..Default::default() }
    }

    fn visible_el(kind: &str) -> ObservedElement {
        let mut e = el(kind);
        e.rect = Rect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 };
        e
    }

    #[test]
    fn dev_candidates_keep_sign_in_that_qa_drops() {
        let mut sign_in = visible_el("click");
        sign_in.label = "Sign in".into();
        assert!(candidate_elements(&[sign_in.clone()], "localhost").is_empty());
        assert_eq!(dev_candidates(&[sign_in]).len(), 1);
    }

    #[test]
    fn dev_candidates_drop_disabled_and_keep_both_forms() {
        let mut disabled = visible_el("click");
        disabled.disabled = true;
        assert!(dev_candidates(&[disabled]).is_empty());
        let mut a = visible_el("click");
        a.form_id = Some(1);
        a.node = 1;
        let mut b = visible_el("click");
        b.form_id = Some(2);
        b.node = 2;
        assert_eq!(dev_candidates(&[a, b]).len(), 2);
    }

    // ---------------------------------------------------------- should_block

    #[test]
    fn get_head_options_are_never_blocked() {
        let p = FirewallPolicy { allow_writes: false, host: "example.com".into() };
        assert!(!should_block(&p, "GET", "https://example.com/api"));
        assert!(!should_block(&p, "HEAD", "https://example.com/api"));
        assert!(!should_block(&p, "OPTIONS", "https://example.com/api"));
    }

    #[test]
    fn post_is_blocked_by_default() {
        let p = FirewallPolicy { allow_writes: false, host: "example.com".into() };
        assert!(should_block(&p, "POST", "https://example.com/api"));
    }

    #[test]
    fn allow_writes_permits_first_party_post() {
        let p = FirewallPolicy { allow_writes: true, host: "example.com".into() };
        assert!(!should_block(&p, "POST", "https://example.com/api"));
    }

    #[test]
    fn allow_writes_still_blocks_offsite_post() {
        let p = FirewallPolicy { allow_writes: true, host: "example.com".into() };
        assert!(should_block(&p, "POST", "https://tracker.example/collect"));
    }

    #[test]
    fn other_non_get_methods_blocked_by_default() {
        let p = FirewallPolicy { allow_writes: false, host: "example.com".into() };
        for method in ["PUT", "PATCH", "DELETE"] {
            assert!(should_block(&p, method, "https://example.com/api"), "{method} should be blocked");
        }
    }

    // -------------------------------------------------------- first_party

    #[test]
    fn first_party_same_host() {
        assert!(first_party("example.com", "https://example.com/a"));
    }

    #[test]
    fn first_party_subdomain() {
        assert!(first_party("example.com", "https://www.example.com/a"));
        assert!(first_party("example.com", "https://api.example.com/a"));
    }

    #[test]
    fn first_party_case_insensitive() {
        assert!(first_party("Example.COM", "https://EXAMPLE.com/a"));
    }

    #[test]
    fn first_party_ignores_port() {
        assert!(first_party("example.com", "https://example.com:8443/a"));
    }

    #[test]
    fn first_party_rejects_lookalike_domains() {
        assert!(!first_party("example.com", "https://notexample.com/a"));
        assert!(!first_party("example.com", "https://example.com.evil.tld/a"));
    }

    #[test]
    fn mailto_and_other_schemes_are_not_first_party() {
        assert!(!first_party("example.com", "mailto:person@example.com"));
    }

    // ---------------------------------------------------- is_write_like_element

    #[test]
    fn submit_button_is_write_like() {
        let mut e = el("click");
        e.input_type = Some("submit".into());
        assert!(is_write_like_element(&e));
    }

    #[test]
    fn image_input_is_write_like() {
        let mut e = el("click");
        e.input_type = Some("image".into());
        assert!(is_write_like_element(&e));
    }

    #[test]
    fn post_form_descendant_is_write_like() {
        let mut e = el("fill");
        e.form_method = Some("post".into());
        assert!(is_write_like_element(&e));
    }

    #[test]
    fn save_changes_label_is_write_like() {
        let mut e = el("click");
        e.label = "Save changes".into();
        assert!(is_write_like_element(&e));
    }

    #[test]
    fn mailto_tel_javascript_hrefs_are_write_like() {
        for scheme in ["mailto", "tel", "javascript"] {
            let mut e = el("click");
            e.href_host = Some(scheme.into());
            assert!(is_write_like_element(&e), "{scheme} href should be write-like");
        }
    }

    #[test]
    fn plain_buttons_are_not_write_like() {
        let mut learn_more = el("click");
        learn_more.label = "Learn more".into();
        assert!(!is_write_like_element(&learn_more));

        let mut cancel = el("click");
        cancel.label = "Cancel".into();
        assert!(!is_write_like_element(&cancel));
    }

    #[test]
    fn word_boundary_avoids_play_matching_pay() {
        let mut e = el("click");
        e.label = "Play video".into();
        assert!(!is_write_like_element(&e));
    }

    // ---------------------------------------------------- is_forbidden_page

    #[test]
    fn login_signup_checkout_pay_account_delete_paths_are_forbidden() {
        assert!(is_forbidden_page("https://example.com/login"));
        assert!(is_forbidden_page("https://example.com/signup"));
        assert!(is_forbidden_page("https://example.com/account/checkout"));
        assert!(is_forbidden_page("https://example.com/settings/account-delete"));
    }

    #[test]
    fn ordinary_paths_are_not_forbidden() {
        assert!(!is_forbidden_page("https://example.com/"));
        assert!(!is_forbidden_page("https://example.com/products/display-case"));
        // Token boundaries: these must not read as `pay` / `register` / `cart`.
        assert!(!is_forbidden_page("https://example.com/payload"));
        assert!(!is_forbidden_page("https://example.com/registered-users"));
        assert!(!is_forbidden_page("https://example.com/cartography"));
        assert!(is_forbidden_page("https://example.com/pay"));
        assert!(is_forbidden_page("https://example.com/account/delete"));
        assert!(is_forbidden_page("https://example.com/sign-up?next=/home"));
    }

    // ---------------------------------------------------- candidate_elements

    #[test]
    fn candidate_elements_filters_write_like_offsite_hidden_and_disabled() {
        let mut clickable = visible_el("click");
        clickable.label = "Learn more".into();

        let mut submit = visible_el("click");
        submit.input_type = Some("submit".into());

        let mut offsite_link = visible_el("click");
        offsite_link.label = "Visit partner".into();
        offsite_link.href_host = Some("other.tld".into());

        let mut hidden = el("click"); // zero rect: not visible
        hidden.label = "Hidden".into();

        let mut disabled = visible_el("click");
        disabled.label = "Disabled".into();
        disabled.disabled = true;

        let scroll = visible_el("scroll"); // wrong kind

        let els = vec![clickable.clone(), submit, offsite_link, hidden, disabled, scroll];
        let candidates = candidate_elements(&els, "example.com");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].label, "Learn more");
    }

    #[test]
    fn candidate_elements_keeps_same_site_link() {
        let mut same_site_link = visible_el("click");
        same_site_link.label = "See pricing".into();
        same_site_link.href_host = Some("www.example.com".into());

        let candidates = candidate_elements(&[same_site_link], "example.com");
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn candidate_elements_keeps_a_same_host_link_that_carries_a_port() {
        // The snapshot reports `new URL(href).host`, port included (`127.0.0.1:8000`);
        // the QA target host is a bare hostname.
        let mut link = visible_el("click");
        link.label = "About".into();
        link.href_host = Some("127.0.0.1:8000".into());
        assert_eq!(candidate_elements(&[link.clone()], "127.0.0.1").len(), 1);
        link.href_host = Some("other.tld:8000".into());
        assert!(candidate_elements(&[link], "127.0.0.1").is_empty(), "a port never makes another host same-site");
    }

    #[test]
    fn global_controls_survive_the_blocklist() {
        let mut down = visible_el("scroll");
        down.id = "scroll_down".into();
        down.label = "Scroll down".into();
        let mut wait = visible_el("wait");
        wait.id = "wait".into();
        wait.label = "Wait for the page to update".into();
        let mut button = visible_el("click");
        button.label = "Filters".into();
        // A write-like element must still be refused, controls or not.
        let mut submit = visible_el("click");
        submit.input_type = Some("submit".into());

        let els = vec![down, wait, button, submit];
        assert_eq!(candidate_elements(&els, "example.com").len(), 1);
        let controls = global_controls(&els);
        assert_eq!(controls.len(), 2);
        assert_eq!(controls[0].id, "scroll_down");
        assert_eq!(controls[1].kind, "wait");
    }
}

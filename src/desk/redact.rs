//! OWNER: desk-core. Secret firewall: runs on every string before Jev, move/desk.json,
//! episodes, or the Hermes log see it. Pure; tests use FAKE values only.

const MASK: &str = "[redacted]";

/// Words whose following word is a secret when a `:`, `=` or space follows them.
const KEYWORDS: &[&str] = &[
    "api key", "apikey", "password", "passwd", "contraseña", "clave", "database", "secret", "token", "pass", "pwd",
    "key", "db",
];

/// Mask secret-looking substrings with "[redacted]".
pub fn redact(text: &str) -> String {
    mask_patterns(&mask_after_keywords(text))
}

fn is_tok(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// `password: x`, `token=x`, `api key x` -> the word after the keyword is masked.
fn mask_after_keywords(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let boundary = i == 0 || !chars[i - 1].is_alphanumeric();
        let hit = if boundary { KEYWORDS.iter().find(|k| starts_with_ci(&chars[i..], k)) } else { None };
        let Some(k) = hit else {
            out.push(chars[i]);
            i += 1;
            continue;
        };
        let mut j = i + k.chars().count();
        let delim_start = j;
        while j < chars.len() && matches!(chars[j], ' ' | ':' | '=') {
            j += 1;
        }
        let word_start = j;
        while j < chars.len() && !chars[j].is_whitespace() {
            j += 1;
        }
        out.extend(&chars[i..word_start]);
        if word_start > delim_start && j > word_start {
            out.push_str(MASK);
        } else {
            out.extend(&chars[word_start..j]);
        }
        i = j;
    }
    out
}

fn starts_with_ci(chars: &[char], needle: &str) -> bool {
    let n: Vec<char> = needle.chars().collect();
    chars.len() >= n.len() && chars.iter().zip(&n).all(|(a, b)| a.to_ascii_lowercase() == *b)
}

/// Key prefixes, JWTs, AWS ids, and long mixed letter+digit runs.
fn mask_patterns(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for c in text.chars() {
        if is_tok(c) || c == '.' {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

fn flush(run: &mut String, out: &mut String) {
    let parts: Vec<&str> = run.split('.').collect();
    if run.starts_with("eyJ") && parts.len() == 3 && parts.iter().all(|p| !p.is_empty()) {
        out.push_str(MASK);
    } else {
        let masked: Vec<&str> = parts.iter().map(|p| if secret_segment(p) { MASK } else { p }).collect();
        out.push_str(&masked.join("."));
    }
    run.clear();
}

fn secret_segment(s: &str) -> bool {
    let after = |prefix: &str, min: usize| s.strip_prefix(prefix).is_some_and(|r| r.len() >= min);
    let prefixed = ["sk_live_", "sk_test_", "rk_live_", "pk_live_", "sk-", "ghp_", "gho_", "github_pat_"]
        .iter()
        .any(|p| after(p, 8));
    let slack = s.len() > 5 && s.starts_with("xox") && matches!(s.as_bytes()[3], b'a' | b'b' | b'p' | b'r') && s.as_bytes()[4] == b'-';
    let aws = s.len() == 20 && s.starts_with("AKIA") && s[4..].chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    let long_mixed = s.len() >= 24 && s.chars().any(|c| c.is_ascii_alphabetic()) && s.chars().any(|c| c.is_ascii_digit());
    // UIA shortens long labels ("sk_liv...ZXZy", "AIzaSyDQ..."): a key prefix alone is enough.
    let short_prefix = ["sk_liv", "sk_tes", "rk_liv", "pk_liv", "sk-pro", "sk-or-", "sk-ant", "ghp_", "gho_", "github_pat"]
        .iter()
        .any(|p| s.starts_with(p));
    let hex_or_id = s.len() >= 16 && s.chars().any(|c| c.is_ascii_alphabetic()) && s.chars().any(|c| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_alphanumeric())
        && s.chars().filter(|c| c.is_ascii_digit()).count() >= 3;
    prefixed || after("AIza", 8) || short_prefix || slack || aws || long_mixed || hex_or_id
}

/// Lowercased alphanumeric words of `s`.
fn words(s: &str) -> Vec<String> {
    s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect()
}

/// A field whose value must never be read or offered (password boxes).
pub fn is_secret_field(role: &str, label: &str) -> bool {
    let all = format!("{role} {label}");
    let lower = all.to_lowercase();
    ["password", "contraseña", "secret"].iter().any(|n| lower.contains(n))
        || words(&all).iter().any(|w| w == "pin" || w == "cvv")
}

const REFUSED: &[&str] = &[
    "notepad", "bloc de notas", "1password", "bitwarden", "keepass", "lastpass", "bank", "banco", "outlook", "mail",
    "whatsapp", "telegram", "hermes", "windows terminal", "powershell", "cmd.exe", "herdr", "chrome", "edge", "firefox",
    "brave",
];

/// Why this window must never be driven, or None. Browsers are refused too: web goes to `--move`.
pub fn refused_window(app: &str, title: &str) -> Option<&'static str> {
    let hay = format!("{app} {title}").to_lowercase();
    REFUSED.iter().copied().find(|n| hay.contains(n))
}

const WRITE_VERBS: &[&str] = &[
    "delete", "remove", "send", "pay", "purchase", "buy", "sign", "submit", "publish", "post", "transfer", "format",
    "uninstall", "eliminar", "borrar", "enviar", "pagar", "comprar", "firmar", "publicar",
];

/// A control that writes, sends, pays, deletes, or signs. Word-prefix match: "Payment" hits, "Design" does not.
pub fn is_write_label(label: &str) -> bool {
    words(label).iter().any(|w| WRITE_VERBS.iter().any(|v| w.starts_with(v)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_after_db_words() {
        for s in ["shop database Fakepass1", "db: Fakepass1", "clave Fakepass1", "contraseña Fakepass1"] {
            assert!(masked(s) && !redact(s).contains("Fakepass1"), "not masked: {s}");
        }
    }

    #[test]
    fn shortened_labels_seen_in_uia() {
        // Shapes copied from a real UIA spike; every value here is FAKE.
        for s in [
            "Stripe sk_liv...QQzz",
            "sk-pro...AbCd",
            "sk-or-...9999",
            "gemini FAKEUSER AIzaSyFAKE123fake",
            "deepgram 0f0f0f0f0f0f0f0f0f0fab",
            "api key FAKEfake123FAKEfake",
        ] {
            assert!(masked(s), "not masked: {s}");
        }
        for s in ["Display is 84", "Calculator", "Multiply by", "Bluetooth & devices", "Windows 11 version 24H2"] {
            assert!(!masked(s), "false positive: {s}");
        }
    }

    fn masked(s: &str) -> bool {
        redact(s).contains(MASK)
    }

    #[test]
    fn key_prefixes() {
        for s in [
            "sk-FAKEFAKE1234",
            "use sk_live_FAKEFAKEFAKE",
            "sk_test_FAKEFAKE",
            "rk_live_FAKEFAKE",
            "pk_live_FAKEFAKE",
            "ghp_FAKEFAKEFAKE",
            "gho_FAKEFAKEFAKE",
            "github_pat_FAKEFAKEFAKE",
        ] {
            assert!(masked(s), "{s}");
            assert!(!redact(s).contains("FAKEFAKE"), "{s}");
        }
        // A bare key prefix ("ghp_abc") is masked on purpose: UIA shortens long labels.
        assert!(masked("ghp_abc"));
        for s in ["sk-1234", "task-list"] {
            assert_eq!(redact(s), s);
        }
    }

    #[test]
    fn google_slack_aws_jwt() {
        assert_eq!(redact("AIzaFAKEFAKEFAKEFAKEFAKE"), MASK);
        assert_eq!(redact("AIzaShort"), "AIzaShort");
        assert_eq!(redact("xoxb-FAKE-1234"), MASK);
        assert_eq!(redact("xoxo-hugs"), "xoxo-hugs");
        assert_eq!(redact("id AKIAFAKEFAKEFAKE1234."), format!("id {MASK}."));
        assert_eq!(redact("AKIA1234"), "AKIA1234");
        assert_eq!(redact("Bearer eyJhbGciOiJ.eyJzdWIiOiIx.FAKEsig"), format!("Bearer {MASK}"));
        assert_eq!(redact("eyJhbGci.only"), "eyJhbGci.only");
    }

    #[test]
    fn long_mixed_runs() {
        assert_eq!(redact("x FAKEfake1234FAKEfake5678ab y"), format!("x {MASK} y"));
        let letters = "Supercalifragilisticexpialidocious";
        assert_eq!(redact(letters), letters);
        assert_eq!(redact("12345678901234567890123456"), "12345678901234567890123456");
    }

    #[test]
    fn word_after_keyword() {
        assert_eq!(redact("password: hunter2FAKE"), format!("password: {MASK}"));
        assert_eq!(redact("Token=abc"), format!("Token={MASK}"));
        assert_eq!(redact("API key fakevalue now"), format!("API key {MASK} now"));
        assert_eq!(redact("pwd abc"), format!("pwd {MASK}"));
        for s in ["Passport number", "keyboard shortcuts", "Monkey island", "Password", "Keep on top"] {
            assert_eq!(redact(s), s);
        }
    }

    #[test]
    fn near_misses_unchanged() {
        for s in ["Seven", "Display is 84", "Multiply by", "C:\\Users\\x\\file.txt", "There’s no history yet."] {
            assert_eq!(redact(s), s);
        }
    }

    #[test]
    fn secret_fields() {
        assert!(is_secret_field("Edit", "Password"));
        assert!(is_secret_field("Edit", "Contraseña"));
        assert!(is_secret_field("Edit", "Enter PIN"));
        assert!(is_secret_field("Edit", "CVV"));
        assert!(is_secret_field("Edit", "Client secret"));
        assert!(!is_secret_field("Button", "Seven"));
        assert!(!is_secret_field("Spinner", "Spin speed"));
    }

    #[test]
    fn refused_windows() {
        for (app, title) in [
            ("Notepad", ""),
            ("", "Sin título: Bloc de notas"),
            ("chrome", "New Tab"),
            ("", "Windows PowerShell"),
            ("", "hermes chat"),
        ] {
            assert!(refused_window(app, title).is_some(), "{app} {title}");
        }
        assert_eq!(refused_window("Calculator", "Calculator"), None);
        assert_eq!(refused_window("Settings", "Settings"), None);
    }

    #[test]
    fn write_labels() {
        for s in ["Delete", "Send message", "Payment", "Borrar todo", "Uninstall"] {
            assert!(is_write_label(s), "{s}");
        }
        for s in ["Seven", "Design", "Multiply by", "Clear", "Keep on top"] {
            assert!(!is_write_label(s), "{s}");
        }
    }
}

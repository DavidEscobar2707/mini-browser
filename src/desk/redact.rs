//! OWNER: desk-core. Secret firewall: runs on every string before Jev, move/desk.json,
//! episodes, or the Hermes log see it. Stubs fail closed.

/// Mask secret-looking substrings with "[redacted]".
pub fn redact(_text: &str) -> String {
    String::new()
}

/// A field whose value must never be read or offered (password boxes).
pub fn is_secret_field(_role: &str, _label: &str) -> bool {
    true
}

/// Why this window must never be driven, or None.
pub fn refused_window(_app: &str, _title: &str) -> Option<&'static str> {
    Some("desk redaction not implemented")
}

/// A control that writes, sends, pays, deletes, or signs.
pub fn is_write_label(_label: &str) -> bool {
    true
}

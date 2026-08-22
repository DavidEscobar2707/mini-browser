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

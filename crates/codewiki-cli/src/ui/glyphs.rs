// T-441 — Glyph detection (Unicode vs ASCII fallback).
// Respects CODEWIKI_ASCII=1, CODEWIKI_UNICODE=1, TERM=linux, and Windows defaults.
#![allow(dead_code)]

/// Whether the current terminal supports Unicode glyphs.
///
/// Decision order (matches the TypeScript implementation):
///  1. `CODEWIKI_ASCII=1`   → ASCII
///  2. `CODEWIKI_UNICODE=1` → Unicode
///  3. Windows target         → ASCII
///  4. `TERM=linux`           → ASCII (Linux kernel console, VT100)
///  5. Everything else        → Unicode
pub fn use_unicode() -> bool {
    if std::env::var("CODEWIKI_ASCII").as_deref() == Ok("1") {
        return false;
    }
    if std::env::var("CODEWIKI_UNICODE").as_deref() == Ok("1") {
        return true;
    }
    if cfg!(target_os = "windows") {
        return false;
    }
    if std::env::var("TERM").as_deref() == Ok("linux") {
        return false;
    }
    true
}

/// Glyph set for success / failure / info / warning indicators.
pub struct Glyphs {
    pub success: &'static str,
    pub error: &'static str,
    pub info: &'static str,
    pub warn: &'static str,
    pub spinner: &'static [&'static str],
    pub bullet: &'static str,
}

/// Unicode glyph set.
pub const UNICODE: Glyphs = Glyphs {
    success: "✓",
    error: "✗",
    info: "ℹ",
    warn: "⚠",
    spinner: &["·", "✢", "✳", "✶", "✻", "✽"],
    bullet: "◆",
};

/// ASCII fallback glyph set.
pub const ASCII: Glyphs = Glyphs {
    success: "[OK]",
    error: "[ERR]",
    info: "[i]",
    warn: "[!]",
    spinner: &[".", "*", "+", "x", "o", "O"],
    bullet: "*",
};

/// Return the appropriate glyph set for the current environment.
pub fn glyphs() -> &'static Glyphs {
    if use_unicode() {
        &UNICODE
    } else {
        &ASCII
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn ascii_env_overrides_to_ascii() {
        std::env::set_var("CODEWIKI_ASCII", "1");
        assert!(!use_unicode());
        std::env::remove_var("CODEWIKI_ASCII");
    }

    #[test]
    #[serial]
    fn unicode_env_overrides_to_unicode() {
        // Remove ASCII override first.
        std::env::remove_var("CODEWIKI_ASCII");
        std::env::remove_var("TERM");
        std::env::set_var("CODEWIKI_UNICODE", "1");
        assert!(use_unicode());
        std::env::remove_var("CODEWIKI_UNICODE");
    }

    #[test]
    #[serial]
    fn term_linux_returns_ascii() {
        std::env::remove_var("CODEWIKI_ASCII");
        std::env::remove_var("CODEWIKI_UNICODE");
        std::env::set_var("TERM", "linux");
        // Only meaningful on non-Windows; on Windows this code path is never
        // reached since the Windows-target branch fires first.
        #[cfg(not(target_os = "windows"))]
        assert!(!use_unicode());
        std::env::remove_var("TERM");
    }
}

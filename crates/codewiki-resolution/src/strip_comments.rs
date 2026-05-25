// T-304 / T-309 — strip_comments.rs
//
// Line-preserving comment blanker for 9 languages.  Every character in a
// comment or string literal is replaced with a SPACE, but newlines are kept
// so that line-number arithmetic on the stripped string remains valid.
//
// Languages: Python, Ruby, JavaScript/TypeScript, Go, Rust, PHP, Java, C#, Swift.

/// Supported language tags for `strip_comments`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentLang {
    Python,
    Ruby,
    JavaScript,
    TypeScript,
    Go,
    Rust,
    Php,
    Java,
    CSharp,
    Swift,
}

/// Strip comments (and docstring content) from `src` for the given language.
/// Returns a new `String` with the same byte length and the same newline
/// positions as `src`; all comment/string characters are replaced by spaces.
pub fn strip_comments(src: &str, lang: CommentLang) -> String {
    match lang {
        CommentLang::Python => strip_python(src),
        CommentLang::Ruby => strip_ruby(src),
        CommentLang::JavaScript | CommentLang::TypeScript => {
            strip_c_style(src, /* allow_single_quote_strings */ true)
        }
        CommentLang::Go => strip_go(src),
        CommentLang::Rust => strip_rust(src),
        CommentLang::Php => strip_php(src),
        CommentLang::Java | CommentLang::CSharp | CommentLang::Swift => strip_c_style(src, false),
    }
}

/// Replace chars in `out[start..end]` with spaces, preserving newlines.
#[inline]
fn blank_range(out: &mut [u8], start: usize, end: usize, src: &[u8]) {
    for i in start..end {
        out[i] = if src[i] == b'\n' { b'\n' } else { b' ' };
    }
}

// ─── Python ──────────────────────────────────────────────────────────────────

fn strip_python(src: &str) -> String {
    let src_bytes = src.as_bytes();
    let n = src_bytes.len();
    let mut out = src_bytes.to_vec();
    let mut i = 0usize;

    while i < n {
        let c = src_bytes[i];
        let c2 = src_bytes.get(i + 1).copied().unwrap_or(0);
        let c3 = src_bytes.get(i + 2).copied().unwrap_or(0);

        // Triple-quoted string: """...""" or '''...'''
        if (c == b'"' || c == b'\'') && c2 == c && c3 == c {
            let quote = c;
            let start = i;
            i += 3;
            while i < n {
                if src_bytes[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src_bytes[i] == quote
                    && src_bytes.get(i + 1).copied() == Some(quote)
                    && src_bytes.get(i + 2).copied() == Some(quote)
                {
                    i += 3;
                    break;
                }
                i += 1;
            }
            blank_range(&mut out, start, i, src_bytes);
            continue;
        }

        // Single-line comment: #
        if c == b'#' {
            let start = i;
            while i < n && src_bytes[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, start, i, src_bytes);
            continue;
        }

        // Single-quoted string: '...' or "..."
        // TRAVERSE without blanking so framework route strings remain visible.
        if c == b'"' || c == b'\'' {
            let quote = c;
            i += 1;
            while i < n {
                if src_bytes[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src_bytes[i] == quote {
                    i += 1;
                    break;
                }
                if src_bytes[i] == b'\n' {
                    // Unterminated string — stop here
                    break;
                }
                i += 1;
            }
            continue;
        }

        i += 1;
    }

    String::from_utf8(out).unwrap_or_else(|_| src.to_string())
}

// ─── Ruby ─────────────────────────────────────────────────────────────────────

fn strip_ruby(src: &str) -> String {
    let src_bytes = src.as_bytes();
    let n = src_bytes.len();
    let mut out = src_bytes.to_vec();
    let mut i = 0usize;

    while i < n {
        let c = src_bytes[i];

        // =begin ... =end block comment (must be at start of line)
        if c == b'=' && (i == 0 || src_bytes[i - 1] == b'\n') {
            let rest = &src_bytes[i..];
            if rest.starts_with(b"=begin") {
                let start = i;
                // Find =end
                i += 6;
                loop {
                    if i >= n {
                        break;
                    }
                    if src_bytes[i] == b'\n' {
                        i += 1;
                        // Check if this line starts with =end
                        if src_bytes[i..].starts_with(b"=end") {
                            i += 4;
                            // Skip rest of that line
                            while i < n && src_bytes[i] != b'\n' {
                                i += 1;
                            }
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                blank_range(&mut out, start, i, src_bytes);
                continue;
            }
        }

        // Line comment: #
        if c == b'#' {
            let start = i;
            while i < n && src_bytes[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, start, i, src_bytes);
            continue;
        }

        // String literals: '...' or "..."
        // TRAVERSE without blanking so route strings remain visible.
        if c == b'\'' || c == b'"' {
            let quote = c;
            i += 1;
            while i < n {
                if src_bytes[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src_bytes[i] == quote {
                    i += 1;
                    break;
                }
                if src_bytes[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            continue;
        }

        i += 1;
    }

    String::from_utf8(out).unwrap_or_else(|_| src.to_string())
}

// ─── C-style (JS/TS/Java/C#/Swift) ──────────────────────────────────────────

/// C-style comment stripper.
/// String literals are **traversed but NOT blanked** — only comment regions
/// are blanked.  This matches the TS `stripCStyle` behaviour exactly:
/// strings are scanned to avoid treating `/*` or `//` inside a string as a
/// comment, but the route paths they contain must remain visible to framework
/// route regexes that run over the stripped output.
fn strip_c_style(src: &str, allow_single_quote: bool) -> String {
    let src_bytes = src.as_bytes();
    let n = src_bytes.len();
    let mut out = src_bytes.to_vec();
    let mut i = 0usize;

    while i < n {
        let c = src_bytes[i];
        let c2 = src_bytes.get(i + 1).copied().unwrap_or(0);

        // Block comment: /* ... */  — BLANK this region
        if c == b'/' && c2 == b'*' {
            let start = i;
            i += 2;
            while i + 1 < n {
                if src_bytes[i] == b'*' && src_bytes[i + 1] == b'/' {
                    i += 2;
                    break;
                }
                i += 1;
            }
            blank_range(&mut out, start, i, src_bytes);
            continue;
        }

        // Line comment: //  — BLANK this region
        if c == b'/' && c2 == b'/' {
            let start = i;
            while i < n && src_bytes[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, start, i, src_bytes);
            continue;
        }

        // Template literal: `...`  (JS/TS) — TRAVERSE without blanking
        if c == b'`' {
            i += 1;
            while i < n {
                if src_bytes[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src_bytes[i] == b'`' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }

        // Double-quoted string — TRAVERSE without blanking
        if c == b'"' {
            i += 1;
            while i < n {
                if src_bytes[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src_bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                if src_bytes[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            continue;
        }

        // Single-quoted string (JS/TS only) — TRAVERSE without blanking
        if allow_single_quote && c == b'\'' {
            i += 1;
            while i < n {
                if src_bytes[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src_bytes[i] == b'\'' {
                    i += 1;
                    break;
                }
                if src_bytes[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            continue;
        }

        i += 1;
    }

    String::from_utf8(out).unwrap_or_else(|_| src.to_string())
}

// ─── Go ──────────────────────────────────────────────────────────────────────

fn strip_go(src: &str) -> String {
    let src_bytes = src.as_bytes();
    let n = src_bytes.len();
    let mut out = src_bytes.to_vec();
    let mut i = 0usize;

    while i < n {
        let c = src_bytes[i];
        let c2 = src_bytes.get(i + 1).copied().unwrap_or(0);

        // Line comment: //
        if c == b'/' && c2 == b'/' {
            let start = i;
            while i < n && src_bytes[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, start, i, src_bytes);
            continue;
        }

        // Block comment: /* ... */
        if c == b'/' && c2 == b'*' {
            let start = i;
            i += 2;
            while i + 1 < n {
                if src_bytes[i] == b'*' && src_bytes[i + 1] == b'/' {
                    i += 2;
                    break;
                }
                i += 1;
            }
            blank_range(&mut out, start, i, src_bytes);
            continue;
        }

        // Raw string: `...`
        // TRAVERSE without blanking so route strings remain visible.
        if c == b'`' {
            i += 1;
            while i < n && src_bytes[i] != b'`' {
                i += 1;
            }
            if i < n {
                i += 1;
            }
            continue;
        }

        // Interpreted string: "..."
        // TRAVERSE without blanking so route strings remain visible.
        if c == b'"' {
            i += 1;
            while i < n {
                if src_bytes[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src_bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                if src_bytes[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            continue;
        }

        i += 1;
    }

    String::from_utf8(out).unwrap_or_else(|_| src.to_string())
}

// ─── Rust ────────────────────────────────────────────────────────────────────

fn strip_rust(src: &str) -> String {
    let src_bytes = src.as_bytes();
    let n = src_bytes.len();
    let mut out = src_bytes.to_vec();
    let mut i = 0usize;

    while i < n {
        let c = src_bytes[i];
        let c2 = src_bytes.get(i + 1).copied().unwrap_or(0);

        // Line comment: //
        if c == b'/' && c2 == b'/' {
            let start = i;
            while i < n && src_bytes[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, start, i, src_bytes);
            continue;
        }

        // Block comment: /* ... */ (nested, so track depth)
        if c == b'/' && c2 == b'*' {
            let start = i;
            i += 2;
            let mut depth = 1usize;
            while i + 1 < n && depth > 0 {
                if src_bytes[i] == b'/' && src_bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if src_bytes[i] == b'*' && src_bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            blank_range(&mut out, start, i, src_bytes);
            continue;
        }

        // String literal: "..."
        if c == b'"' {
            let start = i;
            i += 1;
            while i < n {
                if src_bytes[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src_bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            blank_range(&mut out, start, i, src_bytes);
            continue;
        }

        i += 1;
    }

    String::from_utf8(out).unwrap_or_else(|_| src.to_string())
}

// ─── PHP ─────────────────────────────────────────────────────────────────────

fn strip_php(src: &str) -> String {
    let src_bytes = src.as_bytes();
    let n = src_bytes.len();
    let mut out = src_bytes.to_vec();
    let mut i = 0usize;

    while i < n {
        let c = src_bytes[i];
        let c2 = src_bytes.get(i + 1).copied().unwrap_or(0);

        // Line comment: // or #
        if (c == b'/' && c2 == b'/') || c == b'#' {
            let start = i;
            while i < n && src_bytes[i] != b'\n' {
                i += 1;
            }
            blank_range(&mut out, start, i, src_bytes);
            continue;
        }

        // Block comment: /* ... */
        if c == b'/' && c2 == b'*' {
            let start = i;
            i += 2;
            while i + 1 < n {
                if src_bytes[i] == b'*' && src_bytes[i + 1] == b'/' {
                    i += 2;
                    break;
                }
                i += 1;
            }
            blank_range(&mut out, start, i, src_bytes);
            continue;
        }

        // String literals
        // TRAVERSE without blanking so route strings remain visible.
        if c == b'"' || c == b'\'' {
            let quote = c;
            i += 1;
            while i < n {
                if src_bytes[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src_bytes[i] == quote {
                    i += 1;
                    break;
                }
                if src_bytes[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            continue;
        }

        // Backtick strings in PHP (shell exec) — TRAVERSE without blanking
        if c == b'`' {
            i += 1;
            while i < n {
                if src_bytes[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if src_bytes[i] == b'`' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }

        i += 1;
    }

    String::from_utf8(out).unwrap_or_else(|_| src.to_string())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_line_comment_blanked() {
        let src = "x = 1  # path('/fake/', V)\nreal = 2";
        let out = strip_comments(src, CommentLang::Python);
        // The comment portion is blanked; real code preserved
        assert!(out.contains("real = 2"), "real code must survive: {out:?}");
        assert!(
            !out.contains("path('/fake/'"),
            "comment content must be blanked: {out:?}"
        );
        // Newline preserved
        assert_eq!(out.lines().count(), src.lines().count());
    }

    #[test]
    fn python_triple_quoted_string_line_count_preserved() {
        // T-311 acceptance: strip_comments preserves line numbers for Python triple-quoted strings
        let src = "x = 1\n\"\"\"\nsome docs\npath('/fake/', View)\n\"\"\"\ny = 2\n";
        let out = strip_comments(src, CommentLang::Python);
        // Line count must be identical
        assert_eq!(
            out.lines().count(),
            src.lines().count(),
            "line count must match: original={} stripped={}",
            src.lines().count(),
            out.lines().count()
        );
        // y = 2 must survive
        assert!(out.contains("y = 2"), "code after docstring must survive");
        // The fake route inside docstring must be blanked
        assert!(
            !out.contains("path('/fake/'"),
            "docstring content must be blanked"
        );
    }

    #[test]
    fn javascript_block_comment_blanked() {
        let src = "/* TODO: app.get('/x', f) */\napp.get('/real', handler);";
        let out = strip_comments(src, CommentLang::JavaScript);
        assert!(!out.contains("/x"), "block comment content blanked");
        assert!(out.contains("/real"), "real route preserved");
    }

    #[test]
    fn rust_nested_block_comment() {
        let src = "/* outer /* inner */ still outer */\nlet x = 1;";
        let out = strip_comments(src, CommentLang::Rust);
        assert!(!out.contains("outer"), "nested block comment blanked");
        assert!(out.contains("let x"), "code after comment preserved");
    }

    #[test]
    fn ruby_begin_end_block_comment() {
        let src = "code1\n=begin\nsome stuff\n=end\ncode2\n";
        let out = strip_comments(src, CommentLang::Ruby);
        assert!(out.contains("code1"), "before block preserved");
        assert!(out.contains("code2"), "after block preserved");
        assert!(!out.contains("some stuff"), "block content blanked");
    }

    #[test]
    fn line_count_preserved_for_all_langs() {
        let src = "line1\n// comment\nline3\n";
        for lang in [
            CommentLang::JavaScript,
            CommentLang::TypeScript,
            CommentLang::Go,
            CommentLang::Rust,
            CommentLang::Java,
            CommentLang::CSharp,
            CommentLang::Swift,
        ] {
            let out = strip_comments(src, lang);
            assert_eq!(
                out.lines().count(),
                src.lines().count(),
                "line count mismatch for {:?}",
                lang
            );
        }
    }

    /// Bug 10 fix: strip_comments must NOT blank string content for Python/Ruby/Go/PHP.
    /// Framework route patterns like @app.route('/users') must survive so regexes can
    /// match them after stripping.
    #[test]
    fn python_route_regex_survives_strip_comments() {
        let src = "@app.route('/users')\ndef index(): pass\n";
        let out = strip_comments(src, CommentLang::Python);
        assert!(
            out.contains("/users"),
            "route string must survive strip_comments; got: {out:?}"
        );
        // Line count must be preserved
        assert_eq!(out.lines().count(), src.lines().count());
    }

    #[test]
    fn ruby_route_string_survives_strip_comments() {
        let src = "get '/users', to: 'users#index'\n# comment\n";
        let out = strip_comments(src, CommentLang::Ruby);
        assert!(
            out.contains("/users"),
            "Ruby route string must survive; got: {out:?}"
        );
        assert!(!out.contains("comment"), "comment must be blanked");
    }

    #[test]
    fn go_route_string_survives_strip_comments() {
        let src = "mux.HandleFunc(\"/users\", handler)\n// comment\n";
        let out = strip_comments(src, CommentLang::Go);
        assert!(
            out.contains("/users"),
            "Go route string must survive; got: {out:?}"
        );
        assert!(!out.contains("comment"), "comment must be blanked");
    }

    #[test]
    fn php_route_string_survives_strip_comments() {
        let src = "Route::get('/users', [UserController::class, 'index']);\n// comment\n";
        let out = strip_comments(src, CommentLang::Php);
        assert!(
            out.contains("/users"),
            "PHP route string must survive; got: {out:?}"
        );
        assert!(!out.contains("comment"), "comment must be blanked");
    }

    /// Verify that Python docstrings (triple-quoted strings with fake imports)
    /// are still blanked — the extractor (not strip_comments) handles this,
    /// but strip_comments must blank triple-quoted strings.
    #[test]
    fn python_docstring_still_blanked() {
        let src = "x = 1\n\"\"\"\nimport fake\n\"\"\"\ny = 2\n";
        let out = strip_comments(src, CommentLang::Python);
        assert!(
            !out.contains("import fake"),
            "docstring content must be blanked"
        );
        assert!(out.contains("y = 2"), "code after docstring must survive");
        assert_eq!(out.lines().count(), src.lines().count());
    }
}

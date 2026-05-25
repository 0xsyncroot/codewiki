// Shared char-walking helpers for balanced-delimiter scanning.
//
// Used by nestjs.rs (read_args) and vue.rs (read_args, read_object,
// read_bracket_array).  All functions are pure — they do not allocate beyond
// the returned String.

/// Balanced-paren scanner.
///
/// Given source `s` and `open_index` — the byte offset of the opening `(` —
/// walks characters tracking nesting depth, handling `()`, `[]`, `{}`.
/// Returns `(inner_text, index_past_closing_paren)` or `None` if unbalanced.
pub(crate) fn read_args(s: &str, open_index: usize) -> Option<(String, usize)> {
    read_balanced(s, open_index, b'(', b')')
}

/// Balanced-brace scanner.
///
/// Given source `s` and `open_index` — the byte offset of the opening `{` —
/// walks characters tracking nesting depth, handling `{}`, `()`, `[]`.
/// Returns `(inner_text, index_past_closing_brace)` or `None` if unbalanced.
pub(crate) fn read_object(s: &str, open_index: usize) -> Option<(String, usize)> {
    read_balanced(s, open_index, b'{', b'}')
}

/// Balanced-bracket scanner.
///
/// Given source `s` and `open_index` — the byte offset of the opening `[` —
/// walks characters tracking nesting depth, handling `[]`, `()`, `{}`.
/// Returns the inner text (without the surrounding `[` and `]`) or `None` if
/// the brackets are unbalanced.
pub(crate) fn read_bracket_array(s: &str, open_index: usize) -> Option<String> {
    read_balanced(s, open_index, b'[', b']').map(|(inner, _)| inner)
}

// ─── Private implementation ───────────────────────────────────────────────────

/// Core balanced-delimiter walk.  Handles all three bracket pairs (`()`, `[]`,
/// `{}`) as depth contributors so that inner brackets of any kind do not
/// prematurely close the scan.  String literals (`'`, `"`, `` ` ``) are skipped
/// so brackets inside strings are ignored.
///
/// `open_byte`  — the expected byte at `open_index` (e.g. `b'('`).
/// `close_byte` — the matching closer (e.g. `b')'`).
///
/// Returns `(inner, end)` where `inner` is the text between the delimiters and
/// `end` is the byte index one past the closing delimiter.
fn read_balanced(
    s: &str,
    open_index: usize,
    open_byte: u8,
    close_byte: u8,
) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    if open_index >= bytes.len() || bytes[open_index] != open_byte {
        return None;
    }
    let mut depth = 0usize;
    let mut i = open_index;
    let start = open_index + 1; // content starts one past the opener
    loop {
        if i >= bytes.len() {
            return None; // unbalanced
        }
        let b = bytes[i];
        if b == open_byte || b == b'(' || b == b'[' || b == b'{' {
            // All openers increase depth; the primary opener starts at depth 0→1.
            // Secondary openers (different kind) also increase depth so they can't
            // accidentally trigger the close condition.
            if b == open_byte {
                depth += 1;
            } else if b != open_byte {
                // A bracket of a different kind — still increases depth to avoid
                // the matching `close_byte` being confused.  We only care that the
                // *primary* opener/closer pair is balanced.
                depth += 1;
            }
            i += 1;
        } else if b == close_byte {
            depth -= 1;
            if depth == 0 {
                let inner = &s[start..i];
                return Some((inner.to_string(), i + 1));
            }
            i += 1;
        } else if b == b')' || b == b']' || b == b'}' {
            // Closing bracket of a different kind — decrease depth.
            if depth == 0 {
                return None; // mismatched
            }
            depth -= 1;
            i += 1;
        } else if b == b'\'' || b == b'"' || b == b'`' {
            // Skip string literal so brackets inside don't affect depth.
            let q = b;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 2; // skip escaped character
                } else if bytes[i] == q {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else {
            i += 1;
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_args_simple() {
        let s = "foo('bar', baz)rest";
        let (inner, end) = read_args(s, 3).unwrap();
        assert_eq!(inner, "'bar', baz");
        assert_eq!(&s[end..], "rest");
    }

    #[test]
    fn read_args_nested() {
        let s = "(@Query(() => [User]))X";
        let (inner, end) = read_args(s, 0).unwrap();
        assert_eq!(inner, "@Query(() => [User])");
        let _ = end;
    }

    #[test]
    fn read_args_arrow_lambda() {
        let src = "@Query(() => [User])";
        let open = src.find('(').unwrap();
        let (inner, _) = read_args(src, open).unwrap();
        assert_eq!(inner, "() => [User]");
    }

    #[test]
    fn read_object_basic() {
        let s = "x = { a: 1, b: 2 } rest";
        let open = s.find('{').unwrap();
        let (inner, end) = read_object(s, open).unwrap();
        assert_eq!(inner, " a: 1, b: 2 ");
        assert_eq!(&s[end..], " rest");
    }

    #[test]
    fn read_object_nested() {
        let s = "{ outer: { inner: 42 } }";
        let (inner, _) = read_object(s, 0).unwrap();
        assert_eq!(inner, " outer: { inner: 42 } ");
    }

    #[test]
    fn read_bracket_array_basic() {
        let s = "routes: ['/a', '/b']";
        let open = s.find('[').unwrap();
        let inner = read_bracket_array(s, open).unwrap();
        assert_eq!(inner, "'/a', '/b'");
    }

    #[test]
    fn read_bracket_array_nested_objects() {
        let s = "[{ path: '/a' }, { path: '/b' }]";
        let inner = read_bracket_array(s, 0).unwrap();
        assert_eq!(inner, "{ path: '/a' }, { path: '/b' }");
    }

    #[test]
    fn string_literals_dont_confuse_depth() {
        // A `}` inside a string must not close the outer brace.
        let s = r#"{ key: "val}ue" }"#;
        let (inner, _) = read_object(s, 0).unwrap();
        assert_eq!(inner, r#" key: "val}ue" "#);
    }
}

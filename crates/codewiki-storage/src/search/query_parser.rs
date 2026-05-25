/// Field-qualified search query parser.
/// Port of research-source/src/search/query-parser.ts.
use codewiki_core::{Language, NodeKind};
use unicode_normalization::UnicodeNormalization;

/// Normalize text for FTS matching.
///
/// Two-pass:
///   1. NFD-decompose and strip Unicode combining marks (U+0300–U+036F,
///      U+1DC0–U+1DFF, U+20D0–U+20FF) so that `remove_diacritics=2`-indexed
///      tokens are reachable from accented input and vice versa.
///   2. Explicit character map for non-decomposable Latin strokes:
///      - đ (U+0111) → d,  Đ (U+0110) → D
///      - ø (U+00F8) → o,  Ø (U+00D8) → O
///      - ł (U+0142) → l,  Ł (U+0141) → L
///
/// Applied at both write time (insert_node) and query time (parse_query).
/// ASCII input (U+0000–U+007F) is returned unchanged (fast path).
pub fn normalize_for_fts(input: &str) -> String {
    // Fast path: pure ASCII — nothing to do.
    if input.is_ascii() {
        return input.to_string();
    }

    // Pass 1: NFD decompose then filter combining marks.
    let nfd_stripped: String = input
        .nfd()
        .filter(|c| {
            let n = *c as u32;
            // Strip combining diacritical marks ranges:
            //   U+0300–U+036F  Combining Diacritical Marks
            //   U+1DC0–U+1DFF  Combining Diacritical Marks Supplement
            //   U+20D0–U+20FF  Combining Diacritical Marks for Symbols
            !(0x0300..=0x036F).contains(&n)
                && !(0x1DC0..=0x1DFF).contains(&n)
                && !(0x20D0..=0x20FF).contains(&n)
        })
        .collect();

    // Pass 2: Explicit map for non-decomposable Latin strokes.
    nfd_stripped
        .chars()
        .map(|c| match c {
            '\u{0111}' => 'd', // đ
            '\u{0110}' => 'D', // Đ
            '\u{00F8}' => 'o', // ø
            '\u{00D8}' => 'O', // Ø
            '\u{0142}' => 'l', // ł
            '\u{0141}' => 'L', // Ł
            other => other,
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct ParsedQuery {
    /// Free-text portion to feed to FTS / LIKE.
    pub text: String,
    /// kind: filters (OR'd).
    pub kinds: Vec<NodeKind>,
    /// lang:/language: filters (OR'd).
    pub languages: Vec<Language>,
    /// path: filters (OR'd, case-insensitive substring of file_path).
    pub path_filters: Vec<String>,
    /// name: filters (OR'd, case-insensitive substring of node.name).
    pub name_filters: Vec<String>,
}

/// Valid NodeKind string values.
fn is_valid_kind(s: &str) -> bool {
    matches!(
        s,
        "function"
            | "method"
            | "class"
            | "interface"
            | "enum"
            | "enum_member"
            | "struct"
            | "trait"
            | "type"
            | "variable"
            | "constant"
            | "module"
            | "namespace"
            | "property"
            | "field"
            | "constructor"
            | "getter"
            | "setter"
            | "file"
            | "component"
            | "route"
            | "unknown"
            | "type_alias"
            | "import"
            | "export"
            | "parameter"
            | "protocol"
    )
}

/// Valid Language string values (serde snake_case serialization of the Language enum).
fn is_valid_language(s: &str) -> bool {
    matches!(
        s,
        "type_script"
            | "javascript"
            | "python"
            | "go"
            | "rust"
            | "java"
            | "cpp"
            | "c"
            | "c_sharp"
            | "php"
            | "ruby"
            | "swift"
            | "kotlin"
            | "dart"
            | "scala"
            | "lua"
            | "luau"
            | "pascal"
            | "svelte"
            | "vue"
            | "razor"
            | "liquid"
            | "dfm"
            | "unknown"
    )
}

/// Normalize user-friendly language aliases to the canonical serde snake_case form.
///
/// Accepts: `typescript`, `javascript`, `csharp`, `cs`, `cpp`, `c++`
/// as well as all canonical snake_case forms (passthrough).
fn normalize_language(s: &str) -> &str {
    match s {
        "typescript" | "ts" => "type_script",
        "javascript" | "js" => "javascript",
        "csharp" | "cs" | "c#" => "c_sharp",
        "c++" => "cpp",
        other => other,
    }
}

fn kind_from_str(s: &str) -> Option<NodeKind> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

fn lang_from_str(s: &str) -> Option<Language> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

fn unquote(s: &str) -> &str {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Parse a raw query into structured filters + remaining text.
/// Never panics.
pub fn parse_query(raw: &str) -> ParsedQuery {
    let mut out = ParsedQuery::default();

    // Tokenize on whitespace, preserving quoted spans.
    let mut tokens: Vec<String> = Vec::new();
    let chars: Vec<char> = raw.chars().collect();
    let n = chars.len();
    let mut i = 0;

    while i < n {
        // Skip whitespace
        while i < n && chars[i].is_whitespace() { i += 1; }
        if i >= n { break; }
        let _start = i;
        let mut tok = String::new();
        while i < n && !chars[i].is_whitespace() {
            if chars[i] == '"' {
                tok.push(chars[i]);
                i += 1;
                while i < n && chars[i] != '"' {
                    tok.push(chars[i]);
                    i += 1;
                }
                if i < n { tok.push(chars[i]); i += 1; }
                continue;
            }
            tok.push(chars[i]);
            i += 1;
        }
        if !tok.is_empty() { tokens.push(tok); }
    }

    let mut text_parts: Vec<String> = Vec::new();

    for tok in &tokens {
        let colon_pos = tok.find(':');
        match colon_pos {
            None | Some(0) => {
                text_parts.push(tok.clone());
                continue;
            }
            Some(pos) if pos == tok.len() - 1 => {
                text_parts.push(tok.clone());
                continue;
            }
            Some(pos) => {
                let key = tok[..pos].to_lowercase();
                let value_raw = unquote(&tok[pos + 1..]);
                if value_raw.is_empty() {
                    text_parts.push(tok.clone());
                    continue;
                }
                match key.as_str() {
                    "kind" => {
                        if is_valid_kind(value_raw) {
                            if let Some(k) = kind_from_str(value_raw) {
                                out.kinds.push(k);
                            } else {
                                text_parts.push(tok.clone());
                            }
                        } else {
                            text_parts.push(tok.clone());
                        }
                    }
                    "lang" | "language" => {
                        let lower = value_raw.to_lowercase();
                        let canonical = normalize_language(&lower);
                        if is_valid_language(canonical) {
                            if let Some(l) = lang_from_str(canonical) {
                                out.languages.push(l);
                            } else {
                                text_parts.push(tok.clone());
                            }
                        } else {
                            text_parts.push(tok.clone());
                        }
                    }
                    "path" => {
                        out.path_filters.push(value_raw.to_string());
                    }
                    "name" => {
                        out.name_filters.push(value_raw.to_string());
                    }
                    _ => {
                        text_parts.push(tok.clone());
                    }
                }
            }
        }
    }

    out.text = normalize_for_fts(text_parts.join(" ").trim());
    out
}

/// Bounded edit distance (Levenshtein). Returns `max_dist + 1` early if
/// distance exceeds `max_dist`.
pub fn bounded_edit_distance(a: &str, b: &str, max_dist: usize) -> usize {
    if a == b { return 0; }
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let al = a_chars.len();
    let bl = b_chars.len();

    if al.abs_diff(bl) > max_dist { return max_dist + 1; }
    if al == 0 { return bl; }
    if bl == 0 { return al; }

    let mut prev: Vec<usize> = (0..=bl).collect();
    let mut cur: Vec<usize> = vec![0; bl + 1];

    for i in 1..=al {
        cur[0] = i;
        let mut row_min = cur[0];
        for j in 1..=bl {
            let cost = if a_chars[i - 1] == b_chars[j - 1] { 0 } else { 1 };
            let insertion = cur[j - 1] + 1;
            let deletion = prev[j] + 1;
            let substitution = prev[j - 1] + cost;
            cur[j] = insertion.min(deletion).min(substitution);
            row_min = row_min.min(cur[j]);
        }
        if row_min > max_dist { return max_dist + 1; }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[bl]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ascii_passthrough() {
        assert_eq!(normalize_for_fts("getUserById"), "getUserById");
    }

    #[test]
    fn normalize_vn_combining() {
        // người → nguoi (multi-diacritic decomposable chars)
        assert_eq!(normalize_for_fts("người"), "nguoi");
    }

    #[test]
    fn normalize_vn_d_stroke() {
        // đăng → dang (đ is non-decomposable, handled by explicit map)
        assert_eq!(normalize_for_fts("đăng"), "dang");
    }

    #[test]
    fn normalize_vn_combined() {
        // đường → duong
        assert_eq!(normalize_for_fts("đường"), "duong");
    }

    #[test]
    fn normalize_polish() {
        // łódź → lodz
        assert_eq!(normalize_for_fts("łódź"), "lodz");
    }

    #[test]
    fn normalize_idempotent() {
        let once = normalize_for_fts("người");
        let twice = normalize_for_fts(&once);
        assert_eq!(once, twice);
        assert_eq!(twice, "nguoi");
    }

    #[test]
    fn parse_kind_filter() {
        let q = parse_query("kind:function authenticate");
        assert_eq!(q.kinds.len(), 1);
        assert_eq!(q.text, "authenticate");
    }

    #[test]
    fn parse_lang_filter() {
        // Language enum serializes as snake_case: TypeScript → "type_script"
        let q = parse_query("lang:type_script foo");
        assert_eq!(q.languages.len(), 1);
        assert_eq!(q.text, "foo");
    }

    #[test]
    fn parse_path_filter() {
        let q = parse_query(r#"path:"src/api" handler"#);
        assert_eq!(q.path_filters, vec!["src/api"]);
        assert_eq!(q.text, "handler");
    }

    #[test]
    fn unknown_prefix_passes_through() {
        let q = parse_query("TODO:fix something");
        assert!(q.kinds.is_empty());
        assert!(q.text.contains("TODO:fix"));
    }

    #[test]
    fn lang_typescript_alias_resolves_to_typescript() {
        // User-natural "typescript" must be recognized and produce Language::TypeScript.
        let q = parse_query("lang:typescript getUser");
        assert_eq!(q.languages.len(), 1, "expected one language filter");
        assert_eq!(q.languages[0], Language::TypeScript);
        assert_eq!(q.text, "getUser");

        // Alias "ts" should also work.
        let q2 = parse_query("lang:ts foo");
        assert_eq!(q2.languages.len(), 1);
        assert_eq!(q2.languages[0], Language::TypeScript);

        // Canonical snake_case form must still work.
        let q3 = parse_query("lang:type_script bar");
        assert_eq!(q3.languages.len(), 1);
        assert_eq!(q3.languages[0], Language::TypeScript);

        // "csharp" alias.
        let q4 = parse_query("lang:csharp baz");
        assert_eq!(q4.languages.len(), 1);
        assert_eq!(q4.languages[0], Language::CSharp);

        // "cs" alias.
        let q5 = parse_query("lang:cs baz");
        assert_eq!(q5.languages.len(), 1);
        assert_eq!(q5.languages[0], Language::CSharp);

        // "c++" alias.
        let q6 = parse_query("lang:c++ fn");
        assert_eq!(q6.languages.len(), 1);
        assert_eq!(q6.languages[0], Language::Cpp);
    }

    #[test]
    fn edit_distance_exact() {
        assert_eq!(bounded_edit_distance("hello", "hello", 2), 0);
    }

    #[test]
    fn edit_distance_one_off() {
        assert_eq!(bounded_edit_distance("getUser", "getUssr", 2), 1);
    }

    #[test]
    fn edit_distance_exceeds_max() {
        assert!(bounded_edit_distance("process", "prosody", 2) > 2);
    }
}

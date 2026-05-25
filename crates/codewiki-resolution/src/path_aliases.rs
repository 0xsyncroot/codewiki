// T-303 — path_aliases.rs
//
// Load tsconfig.json / jsconfig.json `paths` and `baseUrl`, strip JSONC
// (comments + trailing commas), sort by specificity (longest literal first),
// and resolve wildcard alias substitutions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A single alias pattern: prefix → list of replacement templates.
#[derive(Debug, Clone)]
pub struct AliasEntry {
    /// The pattern prefix (e.g. `"@/*"` or `"@components/*"`).
    pub pattern: String,
    /// Whether this pattern contains a wildcard `*`.
    pub is_wildcard: bool,
    /// The literal prefix to match (everything before the `*`).
    pub prefix: String,
    /// Replacement templates (paths relative to `baseUrl`).
    pub replacements: Vec<String>,
}

/// Resolved alias map loaded from tsconfig / jsconfig.
#[derive(Debug, Clone, Default)]
pub struct PathAliasMap {
    pub base_url: Option<PathBuf>,
    /// Sorted entries: longest literal prefix first; literals before wildcards.
    pub entries: Vec<AliasEntry>,
}

impl PathAliasMap {
    /// Load path aliases from `tsconfig.json` or `jsconfig.json` in `project_root`.
    /// Returns `None` if no config is found or it has no `paths` / `baseUrl`.
    pub fn load(project_root: &Path) -> Option<Self> {
        for name in &["tsconfig.json", "jsconfig.json"] {
            let path = project_root.join(name);
            if let Ok(raw) = std::fs::read_to_string(&path) {
                let stripped = strip_jsonc(&raw);
                if let Some(map) = parse_tsconfig(&stripped, project_root) {
                    return Some(map);
                }
            }
        }
        None
    }

    /// Attempt to resolve `import_path` using the alias map.
    /// Returns a list of candidate file paths (without extensions) relative to
    /// the project root, sorted by specificity.  Returns empty vec if no alias
    /// matches.
    pub fn resolve(&self, import_path: &str) -> Vec<PathBuf> {
        let base = self.base_url.as_deref().unwrap_or(Path::new("."));

        for entry in &self.entries {
            if entry.is_wildcard {
                if import_path.starts_with(&entry.prefix) {
                    let captured = &import_path[entry.prefix.len()..];
                    let mut results = Vec::new();
                    for tpl in &entry.replacements {
                        let replaced = tpl.replacen('*', captured, 1);
                        results.push(base.join(&replaced));
                    }
                    return results;
                }
            } else {
                // Exact pattern match
                if import_path == entry.pattern {
                    let mut results = Vec::new();
                    for tpl in &entry.replacements {
                        // No wildcard substitution — strip trailing /*
                        let clean = tpl.trim_end_matches("/*");
                        results.push(base.join(clean));
                    }
                    return results;
                }
            }
        }
        Vec::new()
    }
}

// ─── JSONC Stripper ───────────────────────────────────────────────────────────

/// Strip JSONC extensions from a JSON string:
/// - single-line `//` comments
/// - block `/* */` comments
/// - trailing commas before `}` or `]`
pub fn strip_jsonc(src: &str) -> String {
    let bytes = src.as_bytes();
    let n = bytes.len();
    let mut out = bytes.to_vec();
    let mut i = 0usize;
    let mut in_string = false;

    while i < n {
        let c = bytes[i];
        let c2 = bytes.get(i + 1).copied().unwrap_or(0);

        if in_string {
            if c == b'\\' && i + 1 < n {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        if c == b'"' {
            in_string = true;
            i += 1;
            continue;
        }

        // Line comment
        if c == b'/' && c2 == b'/' {
            let start = i;
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            out[start..i].fill(b' ');
            continue;
        }

        // Block comment
        if c == b'/' && c2 == b'*' {
            let start = i;
            i += 2;
            while i + 1 < n {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    break;
                }
                i += 1;
            }
            for j in start..i {
                out[j] = if bytes[j] == b'\n' { b'\n' } else { b' ' };
            }
            continue;
        }

        i += 1;
    }

    // Remove trailing commas before } or ]
    let s = String::from_utf8(out).unwrap_or_else(|_| src.to_string());
    remove_trailing_commas(&s)
}

fn remove_trailing_commas(src: &str) -> String {
    // Replace ,\s*[}\]] patterns — simple state-machine approach
    let mut out = String::with_capacity(src.len());
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    let mut in_string = false;

    while i < n {
        let c = chars[i];

        if in_string {
            if c == '\\' && i + 1 < n {
                out.push(c);
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            out.push(c);
            i += 1;
            continue;
        }

        if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }

        if c == ',' {
            // Look ahead: skip whitespace, check next non-whitespace char
            let mut j = i + 1;
            while j < n && (chars[j] == ' ' || chars[j] == '\t' || chars[j] == '\n' || chars[j] == '\r') {
                j += 1;
            }
            if j < n && (chars[j] == '}' || chars[j] == ']') {
                // Trailing comma — skip it (replace with space)
                out.push(' ');
                i += 1;
                continue;
            }
        }

        out.push(c);
        i += 1;
    }

    out
}

// ─── tsconfig parser ──────────────────────────────────────────────────────────

fn parse_tsconfig(stripped: &str, project_root: &Path) -> Option<PathAliasMap> {
    let v: serde_json::Value = serde_json::from_str(stripped).ok()?;
    let opts = v.get("compilerOptions")?;

    let base_url = opts
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .map(|b| project_root.join(b));

    let paths = opts.get("paths")?.as_object()?;
    if paths.is_empty() {
        return None;
    }

    let mut entries: Vec<AliasEntry> = paths
        .iter()
        .filter_map(|(pattern, replacements)| {
            let reps: Vec<String> = replacements
                .as_array()?
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            if reps.is_empty() {
                return None;
            }
            let is_wildcard = pattern.contains('*');
            let prefix = if is_wildcard {
                pattern.split('*').next().unwrap_or("").to_string()
            } else {
                pattern.clone()
            };
            Some(AliasEntry {
                pattern: pattern.clone(),
                is_wildcard,
                prefix,
                replacements: reps,
            })
        })
        .collect();

    // Sort: longer literal prefix first; literals before wildcards at same length
    entries.sort_by(|a, b| {
        b.prefix
            .len()
            .cmp(&a.prefix.len())
            .then(a.is_wildcard.cmp(&b.is_wildcard))
    });

    Some(PathAliasMap {
        base_url,
        entries,
    })
}

// ─── Convenience HashMap API ─────────────────────────────────────────────────

/// Build a simple alias map from a raw `HashMap<pattern, Vec<replacement>>`.
/// Used in tests.
pub fn from_raw(
    base_url: Option<PathBuf>,
    raw: HashMap<String, Vec<String>>,
) -> PathAliasMap {
    let mut entries: Vec<AliasEntry> = raw
        .into_iter()
        .map(|(pattern, replacements)| {
            let is_wildcard = pattern.contains('*');
            let prefix = if is_wildcard {
                pattern.split('*').next().unwrap_or("").to_string()
            } else {
                pattern.clone()
            };
            AliasEntry {
                pattern,
                is_wildcard,
                prefix,
                replacements,
            }
        })
        .collect();

    entries.sort_by(|a, b| {
        b.prefix
            .len()
            .cmp(&a.prefix.len())
            .then(a.is_wildcard.cmp(&b.is_wildcard))
    });

    PathAliasMap { base_url, entries }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn alias_wildcard_resolves_correctly() {
        // T-311: tsconfig paths alias `@/utils` resolves to `src/utils.ts`
        let mut raw: HashMap<String, Vec<String>> = HashMap::new();
        raw.insert("@/*".to_string(), vec!["src/*".to_string()]);
        let map = from_raw(Some(PathBuf::from(".")), raw);
        let resolved = map.resolve("@/utils");
        assert!(!resolved.is_empty(), "must resolve");
        assert!(
            resolved[0].to_string_lossy().ends_with("src/utils"),
            "resolved to src/utils, got: {:?}",
            resolved[0]
        );
    }

    #[test]
    fn jsonc_strips_line_comments() {
        let src = r#"{ "a": 1 // comment
, "b": 2 }"#;
        let stripped = strip_jsonc(src);
        let v: serde_json::Value = serde_json::from_str(&stripped).expect("valid JSON after strip");
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], 2);
    }

    #[test]
    fn jsonc_strips_trailing_commas() {
        let src = r#"{ "a": 1, "b": [1, 2,] }"#;
        let stripped = strip_jsonc(src);
        let v: serde_json::Value =
            serde_json::from_str(&stripped).expect("valid JSON after trailing comma removal");
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn specificity_sort_longer_prefix_wins() {
        let mut raw: HashMap<String, Vec<String>> = HashMap::new();
        raw.insert("@/*".to_string(), vec!["src/*".to_string()]);
        raw.insert("@components/*".to_string(), vec!["src/components/*".to_string()]);
        let map = from_raw(Some(PathBuf::from(".")), raw);
        // @components/* has a longer prefix so it should come first
        assert!(map.entries[0].prefix.len() > map.entries[1].prefix.len());

        let resolved = map.resolve("@components/Button");
        assert!(
            resolved[0].to_string_lossy().contains("components/Button"),
            "resolved via longer prefix: {:?}",
            resolved
        );
    }
}

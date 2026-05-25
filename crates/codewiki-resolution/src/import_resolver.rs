// T-301 / T-304 — import_resolver.rs
//
// Per-language module resolution:
//   - TS/JS: tsconfig paths + extension probing
//   - Python: relative imports
//   - Go: module path
//   - Rust: mod tree
//   - Java / PHP: package-based

use crate::path_aliases::PathAliasMap;
use codewiki_core::Language;
use std::path::{Path, PathBuf};

/// Maximum re-export chain depth to prevent infinite loops.
pub const REEXPORT_MAX_DEPTH: usize = 8;

/// Context needed by the import resolver.
pub trait ImportContext {
    fn file_exists(&self, path: &Path) -> bool;
    fn project_root(&self) -> &Path;
    fn path_aliases(&self) -> Option<&PathAliasMap>;
}

/// The public resolver entry point.
pub struct ImportResolver;

impl ImportResolver {
    /// Resolve `import_path` as written in a source file at `from_file` for
    /// the given `language`.  Returns the resolved file path (absolute or
    /// relative to project root) if found, otherwise `None`.
    pub fn resolve_import_path(
        import_path: &str,
        from_file: &Path,
        language: &Language,
        ctx: &dyn ImportContext,
    ) -> Option<PathBuf> {
        // External imports are never resolved
        if is_external_import(import_path, language) {
            return None;
        }

        let from_dir = from_file.parent().unwrap_or(Path::new("."));

        // Relative import
        if import_path.starts_with("./") || import_path.starts_with("../") {
            let raw = from_dir.join(import_path);
            return probe_extensions(&raw, language, ctx);
        }

        // Alias / absolute import — try path-alias map first
        if let Some(aliases) = ctx.path_aliases() {
            let candidates = aliases.resolve(import_path);
            for candidate in candidates {
                if let Some(resolved) = probe_extensions(&candidate, language, ctx) {
                    return Some(resolved);
                }
            }
        }

        // Fallback alias heuristics for JS/TS: @/ → src/
        if matches!(language, Language::TypeScript | Language::JavaScript) {
            if let Some(stripped) = import_path.strip_prefix('@') {
                let stripped = stripped.trim_start_matches('/');
                let candidate = ctx.project_root().join("src").join(stripped);
                if let Some(resolved) = probe_extensions(&candidate, language, ctx) {
                    return Some(resolved);
                }
            }
        }

        // Direct path from project root with extension probing
        let direct = ctx.project_root().join(import_path);
        probe_extensions(&direct, language, ctx)
    }
}

// ─── External import detection ───────────────────────────────────────────────

fn is_external_import(import_path: &str, language: &Language) -> bool {
    match language {
        Language::TypeScript | Language::JavaScript => {
            // Relative or aliased with @/ are not external
            if import_path.starts_with("./")
                || import_path.starts_with("../")
                || import_path.starts_with('@')
                || import_path.starts_with('~')
            {
                return false;
            }
            // Node built-ins and bare npm package names are external
            !import_path.starts_with('.')
        }
        Language::Python => {
            // Relative imports start with .  or ..
            if import_path.starts_with('.') {
                return false;
            }
            // Absolute import — could be stdlib or local
            // We treat single-component names without dots as potentially external
            // but let extension probing decide
            false
        }
        Language::Go => {
            // Single-word imports (fmt, os, io) are stdlib
            !import_path.contains('/')
        }
        _ => false,
    }
}

// ─── Extension probing ───────────────────────────────────────────────────────

/// Try adding known extensions and index-file fallbacks until we find a file
/// that exists in `ctx`.
fn probe_extensions(base: &Path, language: &Language, ctx: &dyn ImportContext) -> Option<PathBuf> {
    let extensions: &[&str] = match language {
        Language::TypeScript | Language::JavaScript => {
            &[".ts", ".tsx", ".d.ts", ".js", ".jsx"]
        }
        Language::Python => &[".py"],
        Language::Go => &[".go"],
        Language::Rust => &[".rs"],
        Language::Java => &[".java"],
        Language::Php => &[".php"],
        _ => &[],
    };

    // Try with the given extension already included
    if ctx.file_exists(base) {
        return Some(base.to_path_buf());
    }

    // Try appending each extension
    for ext in extensions {
        let candidate = append_ext(base, ext);
        if ctx.file_exists(&candidate) {
            return Some(candidate);
        }
    }

    // Directory index fallbacks
    let index_files: &[&str] = match language {
        Language::TypeScript | Language::JavaScript => {
            &[
                "index.ts",
                "index.tsx",
                "index.d.ts",
                "index.js",
                "index.jsx",
            ]
        }
        Language::Python => &["__init__.py"],
        Language::Rust => &["mod.rs"],
        _ => &[],
    };

    for idx in index_files {
        let candidate = base.join(idx);
        if ctx.file_exists(&candidate) {
            return Some(candidate);
        }
    }

    None
}

fn append_ext(base: &Path, ext: &str) -> PathBuf {
    let mut s = base.as_os_str().to_os_string();
    s.push(ext);
    PathBuf::from(s)
}

// ─── Import mapping extraction ───────────────────────────────────────────────

/// A single import statement's mapping.
#[derive(Debug, Clone)]
pub struct ImportMapping {
    /// The local name used in this file (e.g. `UserService` or `*` for namespace).
    pub local_name: String,
    /// The source module path as written.
    pub source_path: String,
    /// Optional alias: `import { foo as bar }` → original = "foo".
    pub original_name: Option<String>,
}

/// Extract all import mappings from file contents for a given language.
pub fn extract_import_mappings(
    _file_path: &str,
    content: &str,
    language: &Language,
) -> Vec<ImportMapping> {
    match language {
        Language::TypeScript | Language::JavaScript => extract_js_imports(content),
        Language::Python => extract_python_imports(content),
        Language::Go => extract_go_imports(content),
        Language::Php => extract_php_imports(content),
        _ => Vec::new(),
    }
}

// ─── JS/TS imports ───────────────────────────────────────────────────────────

fn extract_js_imports(content: &str) -> Vec<ImportMapping> {
    use regex::Regex;
    use std::sync::OnceLock;

    // ES6 named: import { A, B as C } from 'module'
    static NAMED_RE: OnceLock<Regex> = OnceLock::new();
    // ES6 default: import D from 'module'
    static DEFAULT_RE: OnceLock<Regex> = OnceLock::new();
    // ES6 namespace: import * as NS from 'module'
    static NS_RE: OnceLock<Regex> = OnceLock::new();
    // CommonJS: const { X } = require('module')
    static CJS_RE: OnceLock<Regex> = OnceLock::new();

    let named_re = NAMED_RE.get_or_init(|| {
        Regex::new(r#"import\s*\{([^}]+)\}\s*from\s*['"]([^'"]+)['"]"#).unwrap()
    });
    let default_re = DEFAULT_RE.get_or_init(|| {
        Regex::new(r#"import\s+([A-Za-z_$][A-Za-z0-9_$]*)\s+from\s*['"]([^'"]+)['"]"#).unwrap()
    });
    let ns_re = NS_RE.get_or_init(|| {
        Regex::new(r#"import\s*\*\s+as\s+([A-Za-z_$][A-Za-z0-9_$]*)\s+from\s*['"]([^'"]+)['"]"#)
            .unwrap()
    });
    let cjs_re = CJS_RE.get_or_init(|| {
        Regex::new(r#"(?:const|let|var)\s*\{([^}]+)\}\s*=\s*require\s*\(\s*['"]([^'"]+)['"]\s*\)"#)
            .unwrap()
    });

    let mut result = Vec::new();

    for cap in named_re.captures_iter(content) {
        let source = cap[2].to_string();
        for part in cap[1].split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (local_name, original_name) = if let Some((orig, loc)) =
                part.split_once(" as ")
            {
                (loc.trim().to_string(), Some(orig.trim().to_string()))
            } else {
                (part.to_string(), None)
            };
            result.push(ImportMapping {
                local_name,
                source_path: source.clone(),
                original_name,
            });
        }
    }

    for cap in default_re.captures_iter(content) {
        result.push(ImportMapping {
            local_name: cap[1].to_string(),
            source_path: cap[2].to_string(),
            original_name: None,
        });
    }

    for cap in ns_re.captures_iter(content) {
        result.push(ImportMapping {
            local_name: cap[1].to_string(),
            source_path: cap[2].to_string(),
            original_name: Some("*".to_string()),
        });
    }

    for cap in cjs_re.captures_iter(content) {
        let source = cap[2].to_string();
        for part in cap[1].split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (local_name, original_name) = if let Some((orig, loc)) =
                part.split_once(':')
            {
                (loc.trim().to_string(), Some(orig.trim().to_string()))
            } else {
                (part.to_string(), None)
            };
            result.push(ImportMapping {
                local_name,
                source_path: source.clone(),
                original_name,
            });
        }
    }

    result
}

// ─── Python imports ──────────────────────────────────────────────────────────

fn extract_python_imports(content: &str) -> Vec<ImportMapping> {
    use regex::Regex;
    use std::sync::OnceLock;

    static FROM_RE: OnceLock<Regex> = OnceLock::new();
    static IMPORT_RE: OnceLock<Regex> = OnceLock::new();

    // from X import Y [as Z], A [as B]
    let from_re = FROM_RE.get_or_init(|| {
        Regex::new(r#"^from\s+(\.+\S*|\S+)\s+import\s+(.+)$"#).unwrap()
    });
    // import X.Y [as Z]
    let import_re = IMPORT_RE.get_or_init(|| {
        Regex::new(r#"^import\s+(\S+)(?:\s+as\s+(\S+))?"#).unwrap()
    });

    let mut result = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if let Some(cap) = from_re.captures(line) {
            let source = cap[1].to_string();
            for part in cap[2].split(',') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                let (local_name, original_name) = if let Some((orig, loc)) =
                    part.split_once(" as ")
                {
                    (loc.trim().to_string(), Some(orig.trim().to_string()))
                } else {
                    (part.to_string(), None)
                };
                result.push(ImportMapping {
                    local_name,
                    source_path: source.clone(),
                    original_name,
                });
            }
        } else if let Some(cap) = import_re.captures(line) {
            let source = cap[1].to_string();
            let local_name = cap
                .get(2)
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| source.split('.').next_back().unwrap_or("").to_string());
            result.push(ImportMapping {
                local_name,
                source_path: source,
                original_name: None,
            });
        }
    }

    result
}

// ─── Go imports ──────────────────────────────────────────────────────────────

fn extract_go_imports(content: &str) -> Vec<ImportMapping> {
    use regex::Regex;
    use std::sync::OnceLock;

    static SINGLE_RE: OnceLock<Regex> = OnceLock::new();
    static BLOCK_RE: OnceLock<Regex> = OnceLock::new();

    // import "path/pkg"  or  import alias "path/pkg"
    let single_re = SINGLE_RE.get_or_init(|| {
        Regex::new(r#"import\s+(?:(\w+)\s+)?["'`]([^"'`]+)["'`]"#).unwrap()
    });
    // import ( ... ) block
    let block_re = BLOCK_RE.get_or_init(|| {
        Regex::new(r#"(?m)^\s*(?:(\w+)\s+)?["'`]([^"'`]+)["'`]"#).unwrap()
    });

    let mut result = Vec::new();

    for cap in single_re.captures_iter(content) {
        let source = cap[2].to_string();
        let local_name = cap
            .get(1)
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| package_name_from_path(&source));
        result.push(ImportMapping {
            local_name,
            source_path: source,
            original_name: None,
        });
    }

    // Also scan inside import() blocks
    if let Some(block_start) = content.find("import (") {
        let block = &content[block_start..];
        if let Some(block_end) = block.find(')') {
            let inner = &block[..block_end];
            for cap in block_re.captures_iter(inner) {
                let source = cap[2].to_string();
                if source.is_empty() {
                    continue;
                }
                let local_name = cap
                    .get(1)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_else(|| package_name_from_path(&source));
                result.push(ImportMapping {
                    local_name,
                    source_path: source,
                    original_name: None,
                });
            }
        }
    }

    result
}

fn package_name_from_path(path: &str) -> String {
    path.split('/').next_back().unwrap_or(path).to_string()
}

// ─── PHP imports ─────────────────────────────────────────────────────────────

fn extract_php_imports(content: &str) -> Vec<ImportMapping> {
    use regex::Regex;
    use std::sync::OnceLock;

    static USE_RE: OnceLock<Regex> = OnceLock::new();

    let use_re = USE_RE.get_or_init(|| {
        Regex::new(r#"use\s+([\w\\]+)(?:\s+as\s+(\w+))?\s*;"#).unwrap()
    });

    use_re
        .captures_iter(content)
        .map(|cap| {
            let source = cap[1].to_string();
            let local_name = cap
                .get(2)
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| {
                    source
                        .split('\\')
                        .next_back()
                        .unwrap_or(&source)
                        .to_string()
                });
            ImportMapping {
                local_name,
                source_path: source,
                original_name: None,
            }
        })
        .collect()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    struct MockCtx {
        root: PathBuf,
        existing_files: Vec<PathBuf>,
        aliases: Option<PathAliasMap>,
    }

    impl ImportContext for MockCtx {
        fn file_exists(&self, path: &Path) -> bool {
            self.existing_files.iter().any(|f| f == path)
        }
        fn project_root(&self) -> &Path {
            &self.root
        }
        fn path_aliases(&self) -> Option<&PathAliasMap> {
            self.aliases.as_ref()
        }
    }

    #[test]
    fn resolves_relative_ts_import() {
        let root = PathBuf::from("/project");
        let existing = vec![PathBuf::from("/project/src/utils.ts")];
        let ctx = MockCtx {
            root: root.clone(),
            existing_files: existing,
            aliases: None,
        };
        let from = root.join("src/index.ts");
        let resolved =
            ImportResolver::resolve_import_path("./utils", &from, &Language::TypeScript, &ctx);
        assert!(resolved.is_some());
        assert!(resolved.unwrap().ends_with("utils.ts"));
    }

    #[test]
    fn resolves_alias_import() {
        use crate::path_aliases::from_raw;
        use std::collections::HashMap;

        let root = PathBuf::from("/project");
        let existing = vec![PathBuf::from("/project/src/auth/service.ts")];

        let mut raw: HashMap<String, Vec<String>> = HashMap::new();
        raw.insert("@/*".to_string(), vec!["src/*".to_string()]);
        let aliases = from_raw(Some(root.clone()), raw);

        let ctx = MockCtx {
            root: root.clone(),
            existing_files: existing,
            aliases: Some(aliases),
        };
        let from = root.join("src/index.ts");
        let resolved = ImportResolver::resolve_import_path(
            "@/auth/service",
            &from,
            &Language::TypeScript,
            &ctx,
        );
        assert!(resolved.is_some(), "must resolve @/auth/service");
    }

    #[test]
    fn extracts_js_named_imports() {
        let src = r#"import { UserService, AuthHelper as Auth } from './services';"#;
        let mappings = extract_import_mappings("file.ts", src, &Language::TypeScript);
        let names: Vec<&str> = mappings.iter().map(|m| m.local_name.as_str()).collect();
        assert!(names.contains(&"UserService"), "UserService must be extracted");
        assert!(names.contains(&"Auth"), "Auth alias must be extracted");
    }

    #[test]
    fn extracts_python_from_import() {
        let src = "from mymodule import Foo, Bar as Baz\n";
        let mappings = extract_import_mappings("file.py", src, &Language::Python);
        let names: Vec<&str> = mappings.iter().map(|m| m.local_name.as_str()).collect();
        assert!(names.contains(&"Foo"));
        assert!(names.contains(&"Baz"));
    }
}

//! T-414 — `codewiki_files` tool handler.
//!
//! Get the project file structure from the CodeWiki index.

use crate::input_limits::validate_path;
use crate::tools::MAX_OUTPUT_LENGTH;
use codewiki_core::CodeWikiError;
use codewiki_storage::{FileFilter, QueryHandle};
use std::collections::HashMap;
use std::sync::Arc;

#[tracing::instrument(skip(handle), fields(path = ?path, format = %format))]
pub async fn handle_files(
    handle: Arc<dyn QueryHandle>,
    path: Option<String>,
    pattern: Option<String>,
    format: String,
    include_metadata: bool,
    max_depth: Option<usize>,
) -> Result<String, CodeWikiError> {
    if let Some(p) = &path {
        validate_path(p)?;
    }
    if let Some(p) = &pattern {
        validate_path(p)?;
    }

    let filter = path.as_deref().map(|p| FileFilter {
        path_prefix: Some(p.to_string()),
        ..Default::default()
    });

    let all_files = handle.get_files(filter.as_ref())?;

    // Apply glob pattern filter if provided
    let files: Vec<_> = if let Some(pat) = &pattern {
        let pat_lower = pat.to_lowercase();
        // Simple glob: support * wildcard
        all_files
            .into_iter()
            .filter(|f| {
                let fname = f.path.to_string_lossy().to_lowercase();
                glob_match(&pat_lower, &fname)
            })
            .collect()
    } else {
        all_files
    };

    if files.is_empty() {
        return Ok("No indexed files found matching the filter.".to_string());
    }

    let root = handle.root_path();
    let root_ref = root.as_deref();

    let body = match format.as_str() {
        "flat" => format_flat(&files, include_metadata, root_ref),
        "grouped" => format_grouped(&files, include_metadata, root_ref),
        _ => format_tree(&files, include_metadata, max_depth, root_ref),
    };

    let mut out = crate::tools::root_header(root_ref);
    out.push_str(&body);

    Ok(crate::tools::truncate_output(out, MAX_OUTPUT_LENGTH))
}

/// Convert a `FileRecord` path to a forward-slash-normalized string, then strip
/// the workspace root so the tree/list renders workspace-relative.
fn rel_path(f: &codewiki_core::FileRecord, root: Option<&str>) -> String {
    let normalized = f.path.to_string_lossy().replace('\\', "/");
    crate::tools::rel(&normalized, root).to_string()
}

fn format_flat(
    files: &[codewiki_core::FileRecord],
    include_metadata: bool,
    root: Option<&str>,
) -> String {
    let mut out = format!("## Files ({} total)\n\n", files.len());
    for f in files {
        let p = rel_path(f, root);
        if include_metadata {
            out.push_str(&format!(
                "- `{}` ({}, {} nodes)\n",
                p, f.language, f.node_count,
            ));
        } else {
            out.push_str(&format!("- `{p}`\n"));
        }
    }
    out
}

fn format_grouped(
    files: &[codewiki_core::FileRecord],
    include_metadata: bool,
    root: Option<&str>,
) -> String {
    let mut by_lang: HashMap<&str, Vec<&codewiki_core::FileRecord>> = HashMap::new();
    for f in files {
        by_lang.entry(f.language.as_str()).or_default().push(f);
    }

    let mut out = format!("## Files by Language ({} total)\n\n", files.len());
    let mut langs: Vec<_> = by_lang.keys().cloned().collect();
    langs.sort();

    for lang in langs {
        let group = &by_lang[lang];
        out.push_str(&format!("### {} ({})\n\n", lang, group.len()));
        for f in group.iter() {
            let p = rel_path(f, root);
            if include_metadata {
                out.push_str(&format!("- `{}` ({} nodes)\n", p, f.node_count));
            } else {
                out.push_str(&format!("- `{p}`\n"));
            }
        }
        out.push('\n');
    }
    out
}

fn format_tree(
    files: &[codewiki_core::FileRecord],
    include_metadata: bool,
    max_depth: Option<usize>,
    root: Option<&str>,
) -> String {
    // Build directory tree keyed on the ROOT-RELATIVE, forward-slash-normalized
    // directory path. Indent depth is derived from '/' count in that relative
    // path — NOT MAIN_SEPARATOR on the absolute path (which over-counts the
    // root's own segments on Unix and is always 0 on Windows where the
    // separator is '\').
    let mut tree: HashMap<String, Vec<(String, &codewiki_core::FileRecord)>> = HashMap::new();
    for f in files {
        let rel = rel_path(f, root);
        let (dir, fname) = match rel.rsplit_once('/') {
            Some((d, name)) => (d.to_string(), name.to_string()),
            None => (".".to_string(), rel.clone()),
        };
        // Apply max_depth filter on the relative dir's '/' depth.
        if let Some(max) = max_depth {
            let depth = if dir == "." {
                0
            } else {
                dir.matches('/').count() + 1
            };
            if depth > max {
                continue;
            }
        }
        tree.entry(dir).or_default().push((fname, f));
    }

    let mut out = format!("## File Tree ({} total)\n\n", files.len());
    let mut dirs: Vec<_> = tree.keys().cloned().collect();
    dirs.sort();

    for dir in dirs {
        let dir_files = &tree[&dir];
        let depth = if dir == "." {
            0
        } else {
            dir.matches('/').count() + 1
        };
        let indent = "  ".repeat(depth);
        out.push_str(&format!("{indent}**{dir}/**\n"));
        for (fname, f) in dir_files.iter() {
            if include_metadata {
                out.push_str(&format!(
                    "{indent}  - `{fname}` ({}, {} nodes)\n",
                    f.language, f.node_count
                ));
            } else {
                out.push_str(&format!("{indent}  - `{fname}`\n"));
            }
        }
    }
    out
}

/// Simple glob matching supporting `*` and `?` wildcards.
fn glob_match(pattern: &str, text: &str) -> bool {
    glob_match_inner(pattern.as_bytes(), text.as_bytes())
}

fn glob_match_inner(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.split_first(), text.split_first()) {
        (None, None) => true,
        (None, Some(_)) => false,
        (Some((&b'*', rest_pat)), _) => {
            // * matches zero or more characters
            if glob_match_inner(rest_pat, text) {
                return true;
            }
            for i in 0..=text.len() {
                if glob_match_inner(rest_pat, &text[i..]) {
                    return true;
                }
            }
            false
        }
        (Some((&b'?', rest_pat)), Some((_, rest_text))) => glob_match_inner(rest_pat, rest_text),
        (Some((pc, rest_pat)), Some((tc, rest_text))) if pc == tc => {
            glob_match_inner(rest_pat, rest_text)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_exact_match() {
        assert!(glob_match("foo.ts", "foo.ts"));
        assert!(!glob_match("foo.ts", "foo.js"));
    }

    #[test]
    fn glob_star_suffix() {
        assert!(glob_match("*.ts", "foo.ts"));
        assert!(glob_match("*.ts", "bar/baz.ts"));
        assert!(!glob_match("*.ts", "foo.js"));
    }

    #[test]
    fn glob_double_star_path() {
        assert!(glob_match("**/*.test.ts", "src/components/foo.test.ts"));
    }

    #[test]
    fn glob_question_mark() {
        assert!(glob_match("foo?.ts", "fooX.ts"));
        assert!(!glob_match("foo?.ts", "foo.ts"));
    }

    fn fr(path: &str) -> codewiki_core::FileRecord {
        codewiki_core::FileRecord {
            path: std::path::PathBuf::from(path),
            language: "Rust".to_string(),
            node_count: 1,
            ..Default::default()
        }
    }

    #[test]
    fn tree_indent_from_root_relative_slash_depth() {
        // With the root stripped, indent depth = '/' count in the relative dir,
        // not the absolute path's separator count.
        let files = vec![fr("/home/u/proj/src/a/b.rs")];
        let out = format_tree(&files, false, None, Some("/home/u/proj"));
        // dir is "src/a" (depth 2) → file line indented 6 spaces (2*depth+2).
        assert!(out.contains("**src/a/**"), "got:\n{out}");
        assert!(
            out.contains("      - `b.rs`"),
            "expected 6-space indent (depth 2), got:\n{out}"
        );
    }

    #[test]
    fn tree_root_relative_no_absolute_prefix() {
        let files = vec![fr("/home/u/proj/src/main.rs")];
        let out = format_tree(&files, false, None, Some("/home/u/proj"));
        assert!(
            !out.contains("/home/u/proj"),
            "absolute root leaked:\n{out}"
        );
        assert!(out.contains("**src/**"), "got:\n{out}");
    }
}

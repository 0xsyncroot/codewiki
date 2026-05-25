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

    let out = match format.as_str() {
        "flat" => format_flat(&files, include_metadata),
        "grouped" => format_grouped(&files, include_metadata),
        _ => format_tree(&files, include_metadata, max_depth),
    };

    Ok(crate::tools::truncate_output(out, MAX_OUTPUT_LENGTH))
}

fn format_flat(
    files: &[codewiki_core::FileRecord],
    include_metadata: bool,
) -> String {
    let mut out = format!("## Files ({} total)\n\n", files.len());
    for f in files {
        if include_metadata {
            out.push_str(&format!(
                "- `{}` ({}, {} nodes)\n",
                f.path.display(),
                f.language,
                f.node_count,
            ));
        } else {
            out.push_str(&format!("- `{}`\n", f.path.display()));
        }
    }
    out
}

fn format_grouped(
    files: &[codewiki_core::FileRecord],
    include_metadata: bool,
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
            if include_metadata {
                out.push_str(&format!(
                    "- `{}` ({} nodes)\n",
                    f.path.display(),
                    f.node_count
                ));
            } else {
                out.push_str(&format!("- `{}`\n", f.path.display()));
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
) -> String {
    // Build directory tree
    let mut tree: HashMap<String, Vec<&codewiki_core::FileRecord>> = HashMap::new();
    for f in files {
        let dir = f
            .path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());
        // Apply max_depth filter
        if let Some(max) = max_depth {
            let depth = dir.matches(std::path::MAIN_SEPARATOR).count();
            if depth > max {
                continue;
            }
        }
        tree.entry(dir).or_default().push(f);
    }

    let mut out = format!("## File Tree ({} total)\n\n", files.len());
    let mut dirs: Vec<_> = tree.keys().cloned().collect();
    dirs.sort();

    for dir in dirs {
        let dir_files = &tree[&dir];
        let depth = if dir == "." {
            0
        } else {
            dir.matches(std::path::MAIN_SEPARATOR).count() + 1
        };
        let indent = "  ".repeat(depth);
        out.push_str(&format!("{indent}**{dir}/**\n"));
        for f in dir_files.iter() {
            let fname = f
                .path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| f.path.to_string_lossy().to_string());
            if include_metadata {
                out.push_str(&format!(
                    "{indent}  - `{fname}` ({}, {} nodes)\n",
                    f.language,
                    f.node_count
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
        (Some((&b'?', rest_pat)), Some((_, rest_text))) => {
            glob_match_inner(rest_pat, rest_text)
        }
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
}

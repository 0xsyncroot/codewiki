// T-431 — Shared installer utilities: atomic file I/O, JSON helpers, marker-block
// replacement, and idempotency checks. Ported from research-source/src/installer/targets/shared.ts.

use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;

// ── Constants ────────────────────────────────────────────────────────────────

/// Start marker for the codewiki instruction block in markdown files.
pub const CODEWIKI_SECTION_START: &str = "<!-- CODEWIKI_START -->";
/// End marker for the codewiki instruction block in markdown files.
pub const CODEWIKI_SECTION_END: &str = "<!-- CODEWIKI_END -->";

/// The permissions injected into Claude's settings.json `allow` list.
pub const CODEWIKI_PERMISSIONS: &[&str] = &[
    "mcp__codewiki__codewiki_search",
    "mcp__codewiki__codewiki_context",
    "mcp__codewiki__codewiki_callers",
    "mcp__codewiki__codewiki_callees",
    "mcp__codewiki__codewiki_impact",
    "mcp__codewiki__codewiki_node",
    "mcp__codewiki__codewiki_status",
];

/// The agent-agnostic instructions template, embedded at compile time.
/// The file ends with `\n` (POSIX convention); we strip it so the template
/// matches the TypeScript template literal which does NOT have a trailing
/// newline. The file-writing helpers add exactly one trailing newline.
pub const INSTRUCTIONS_TEMPLATE_RAW: &str = include_str!("templates/instructions.md");

/// Returns the instructions template without trailing whitespace.
/// This matches the TypeScript INSTRUCTIONS_TEMPLATE const (no trailing newline).
#[inline]
pub fn instructions_template() -> &'static str {
    INSTRUCTIONS_TEMPLATE_RAW.trim_end()
}

// ── Atomic file write ────────────────────────────────────────────────────────

/// Write `content` to `path` atomically via a `.tmp.<pid>` sibling + rename.
///
/// Creates all ancestor directories if absent. Cleans up the temp file on
/// rename failure before re-throwing the error.
#[tracing::instrument(level = "debug", skip(content))]
pub fn atomic_write(path: &Path, content: &str) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;

    let tmp_path = path.with_extension(format!("tmp.{}", std::process::id()));

    let write_result = fs::write(&tmp_path, content);
    if let Err(e) = write_result {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }

    if let Err(e) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }

    Ok(())
}

// ── JSON helpers ─────────────────────────────────────────────────────────────

/// Read a JSON file, returning an empty object when missing or unparseable.
///
/// On parse failure the corrupted file is backed up to `<path>.backup` before
/// returning `{}`, so an idempotent re-run never silently deletes user config.
#[tracing::instrument(level = "debug")]
pub fn read_json_file(path: &Path) -> Value {
    match fs::read_to_string(path) {
        Err(_) => Value::Object(serde_json::Map::new()),
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v) if v.is_object() => v,
            Ok(_) => Value::Object(serde_json::Map::new()),
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    "Could not parse JSON ({err}); backing up and returning empty object"
                );
                let backup = path.with_extension("backup");
                let _ = fs::copy(path, &backup);
                Value::Object(serde_json::Map::new())
            }
        },
    }
}

/// Write `data` to `path` as pretty-printed JSON with a trailing newline.
#[tracing::instrument(level = "debug", skip(data))]
pub fn write_json_file(path: &Path, data: &Value) -> io::Result<()> {
    let text = serde_json::to_string_pretty(data).map_err(io::Error::other)?;
    atomic_write(path, &(text + "\n"))
}

/// Deep-equality check that ignores object key order.
///
/// Mirrors `jsonDeepEqual` in shared.ts.
pub fn json_deep_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| json_deep_equal(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            if x.len() != y.len() {
                return false;
            }
            let mut ak: Vec<&str> = x.keys().map(|k| k.as_str()).collect();
            let mut bk: Vec<&str> = y.keys().map(|k| k.as_str()).collect();
            ak.sort_unstable();
            bk.sort_unstable();
            if ak != bk {
                return false;
            }
            ak.iter()
                .all(|k| json_deep_equal(x.get(*k).unwrap(), y.get(*k).unwrap()))
        }
        _ => false,
    }
}

// ── Marker-block helpers ─────────────────────────────────────────────────────

/// Outcome of a marker-section write operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionAction {
    Created,
    Updated,
    Appended,
    Unchanged,
}

/// Replace or append a `<!-- CODEWIKI_START --> ... <!-- CODEWIKI_END -->`
/// block in a markdown file.
///
/// - `Created`   — file did not exist; written fresh.
/// - `Updated`   — existing markers found and content swapped.
/// - `Appended`  — no markers found; section added at end.
/// - `Unchanged` — existing block already matches `body` exactly.
#[tracing::instrument(level = "debug", skip(body))]
pub fn replace_or_append_marked_section(
    path: &Path,
    body: &str,
    start_marker: &str,
    end_marker: &str,
) -> io::Result<SectionAction> {
    if !path.exists() {
        atomic_write(path, &format!("{body}\n"))?;
        return Ok(SectionAction::Created);
    }

    let content = fs::read_to_string(path)?;
    let start_idx = content.find(start_marker);
    let end_idx = content.find(end_marker);

    match (start_idx, end_idx) {
        (Some(si), Some(ei)) if ei > si => {
            let existing_block = &content[si..ei + end_marker.len()];
            if existing_block == body {
                return Ok(SectionAction::Unchanged);
            }
            let before = &content[..si];
            let after = &content[ei + end_marker.len()..];
            atomic_write(path, &format!("{before}{body}{after}"))?;
            Ok(SectionAction::Updated)
        }
        _ => {
            let trimmed = content.trim_end();
            let sep = if trimmed.is_empty() { "" } else { "\n\n" };
            atomic_write(path, &format!("{trimmed}{sep}{body}\n"))?;
            Ok(SectionAction::Appended)
        }
    }
}

/// Outcome of a marker-section remove operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveAction {
    Removed,
    NotFound,
    Kept,
}

/// Strip the marker-delimited block from a file; delete the file if empty.
#[tracing::instrument(level = "debug")]
pub fn remove_marked_section(
    path: &Path,
    start_marker: &str,
    end_marker: &str,
) -> io::Result<RemoveAction> {
    if !path.exists() {
        return Ok(RemoveAction::Kept);
    }

    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(RemoveAction::Kept),
    };

    let start_idx = content.find(start_marker);
    let end_idx = content.find(end_marker);

    match (start_idx, end_idx) {
        (Some(si), Some(ei)) if ei > si => {
            let before = content[..si].trim_end();
            let after = content[ei + end_marker.len()..].trim_start();
            let joined = if !before.is_empty() && !after.is_empty() {
                format!("{before}\n\n{after}")
            } else {
                format!("{before}{after}")
            };

            if joined.trim().is_empty() {
                let _ = fs::remove_file(path);
            } else {
                atomic_write(path, &format!("{}\n", joined.trim()))?;
            }
            Ok(RemoveAction::Removed)
        }
        _ => Ok(RemoveAction::NotFound),
    }
}

// ── Write-result helpers ─────────────────────────────────────────────────────

/// The action taken on a single file during install/uninstall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    Created,
    Updated,
    Unchanged,
    Removed,
    NotFound,
}

impl std::fmt::Display for FileAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileAction::Created => write!(f, "created"),
            FileAction::Updated => write!(f, "updated"),
            FileAction::Unchanged => write!(f, "unchanged"),
            FileAction::Removed => write!(f, "removed"),
            FileAction::NotFound => write!(f, "not-found"),
        }
    }
}

/// Result of writing one file.
#[derive(Debug, Clone)]
pub struct FileResult {
    pub path: std::path::PathBuf,
    pub action: FileAction,
}

/// Aggregate result of an install/uninstall call.
#[derive(Debug, Default, Clone)]
pub struct WriteResult {
    pub files: Vec<FileResult>,
    pub notes: Vec<String>,
}

impl WriteResult {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, path: impl Into<std::path::PathBuf>, action: FileAction) {
        self.files.push(FileResult {
            path: path.into(),
            action,
        });
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmp() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn atomic_write_creates_file() {
        let dir = tmp();
        let p = dir.path().join("out.txt");
        atomic_write(&p, "hello").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "hello");
    }

    #[test]
    fn read_json_missing_returns_empty_object() {
        let dir = tmp();
        let val = read_json_file(&dir.path().join("nope.json"));
        assert!(val.is_object());
        assert!(val.as_object().unwrap().is_empty());
    }

    #[test]
    fn json_deep_equal_ignores_key_order() {
        let a: Value = serde_json::json!({"b": 2, "a": 1});
        let b: Value = serde_json::json!({"a": 1, "b": 2});
        assert!(json_deep_equal(&a, &b));

        let c: Value = serde_json::json!({"a": 1, "b": 3});
        assert!(!json_deep_equal(&a, &c));
    }

    #[test]
    fn replace_or_append_creates_file() {
        let dir = tmp();
        let p = dir.path().join("CLAUDE.md");
        let action = replace_or_append_marked_section(
            &p,
            "<!-- CODEWIKI_START -->\nhi\n<!-- CODEWIKI_END -->",
            CODEWIKI_SECTION_START,
            CODEWIKI_SECTION_END,
        )
        .unwrap();
        assert_eq!(action, SectionAction::Created);
    }

    #[test]
    fn replace_or_append_unchanged() {
        let dir = tmp();
        let p = dir.path().join("CLAUDE.md");
        let body = format!("{}\nhi\n{}", CODEWIKI_SECTION_START, CODEWIKI_SECTION_END);
        replace_or_append_marked_section(&p, &body, CODEWIKI_SECTION_START, CODEWIKI_SECTION_END)
            .unwrap();
        let action = replace_or_append_marked_section(
            &p,
            &body,
            CODEWIKI_SECTION_START,
            CODEWIKI_SECTION_END,
        )
        .unwrap();
        assert_eq!(action, SectionAction::Unchanged);
    }

    #[test]
    fn remove_marked_section_removes_and_deletes_empty_file() {
        let dir = tmp();
        let p = dir.path().join("CLAUDE.md");
        let body = format!("{}\nhi\n{}", CODEWIKI_SECTION_START, CODEWIKI_SECTION_END);
        atomic_write(&p, &format!("{body}\n")).unwrap();
        let action =
            remove_marked_section(&p, CODEWIKI_SECTION_START, CODEWIKI_SECTION_END).unwrap();
        assert_eq!(action, RemoveAction::Removed);
        assert!(!p.exists(), "empty file should be deleted");
    }
}

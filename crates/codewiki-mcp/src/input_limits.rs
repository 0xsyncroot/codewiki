//! T-406 — Input validation: character-length caps for query and path strings.
//!
//! Called before any `QueryHandle` method to guard against DoS attacks where a
//! buggy or hostile MCP client sends a huge payload.

use codewiki_core::CodeWikiError;

/// Maximum characters for free-form string inputs (query, task, symbol).
pub const MAX_QUERY_LEN: usize = 10_000;

/// Maximum characters for path-like string inputs (projectPath, path, pattern).
pub const MAX_PATH_LEN: usize = 4_096;

/// Validate a free-form query/task/symbol string.
///
/// Returns `Err(CodeWikiError::Mcp)` with a descriptive message when `s`
/// exceeds [`MAX_QUERY_LEN`] characters.
pub fn validate_query(s: &str) -> Result<(), CodeWikiError> {
    if s.len() > MAX_QUERY_LEN {
        return Err(CodeWikiError::Mcp(format!(
            "query exceeds {MAX_QUERY_LEN} character limit (got {})",
            s.len()
        )));
    }
    Ok(())
}

/// Validate a path/pattern string.
///
/// Returns `Err(CodeWikiError::Mcp)` with a descriptive message when `s`
/// exceeds [`MAX_PATH_LEN`] characters.
pub fn validate_path(s: &str) -> Result<(), CodeWikiError> {
    if s.len() > MAX_PATH_LEN {
        return Err(CodeWikiError::Mcp(format!(
            "path exceeds {MAX_PATH_LEN} character limit (got {})",
            s.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_within_limit_is_ok() {
        assert!(validate_query("search term").is_ok());
    }

    #[test]
    fn query_at_limit_is_ok() {
        let s = "a".repeat(MAX_QUERY_LEN);
        assert!(validate_query(&s).is_ok());
    }

    #[test]
    fn query_over_limit_is_err() {
        let s = "a".repeat(MAX_QUERY_LEN + 1);
        let err = validate_query(&s).unwrap_err();
        assert!(err.to_string().contains("10000"));
    }

    #[test]
    fn path_within_limit_is_ok() {
        assert!(validate_path("/src/components").is_ok());
    }

    #[test]
    fn path_over_limit_is_err() {
        let s = "a".repeat(MAX_PATH_LEN + 1);
        let err = validate_path(&s).unwrap_err();
        assert!(err.to_string().contains("4096"));
    }
}

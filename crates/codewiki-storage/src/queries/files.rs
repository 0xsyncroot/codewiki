/// Prepared-statement helpers for file record operations.
use codewiki_core::{CodeWikiError, FileRecord};
use rusqlite::{Connection, params};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Row → FileRecord
// ---------------------------------------------------------------------------

pub fn row_to_file_record(row: &rusqlite::Row<'_>) -> Result<FileRecord, rusqlite::Error> {
    let errors_json: Option<String> = row.get("errors")?;
    let errors: Vec<String> = errors_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    Ok(FileRecord {
        path: PathBuf::from(row.get::<_, String>("path")?),
        content_hash: row.get("content_hash")?,
        language: row.get("language")?,
        size: row.get::<_, i64>("size")? as u64,
        modified_at: row.get("modified_at")?,
        indexed_at: row.get("indexed_at")?,
        node_count: row.get::<_, i64>("node_count")? as u32,
        errors,
    })
}

#[cfg(test)]
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ---------------------------------------------------------------------------
// INSERT / UPDATE / DELETE
// ---------------------------------------------------------------------------

pub fn upsert_file(conn: &Connection, file: &FileRecord) -> Result<(), CodeWikiError> {
    let errors_json: Option<String> = if file.errors.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&file.errors)?)
    };
    let lang_str = &file.language;
    let mut stmt = conn.prepare_cached(
        r#"INSERT INTO files (path, content_hash, language, size, modified_at, indexed_at, node_count, errors)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
           ON CONFLICT(path) DO UPDATE SET
             content_hash = excluded.content_hash,
             language     = excluded.language,
             size         = excluded.size,
             modified_at  = excluded.modified_at,
             indexed_at   = excluded.indexed_at,
             node_count   = excluded.node_count,
             errors       = excluded.errors"#,
    )?;
    stmt.execute(params![
        file.path.to_string_lossy().as_ref(),
        file.content_hash,
        lang_str,
        file.size as i64,
        file.modified_at,
        file.indexed_at,
        file.node_count as i64,
        errors_json,
    ])?;
    Ok(())
}

/// Delete a file record.  Callers must manually delete nodes/edges/unresolved_refs
/// first — the nodes table has no FK referencing files, so no cascade fires here.
pub fn delete_file(conn: &Connection, path: &str) -> Result<(), CodeWikiError> {
    conn.execute("DELETE FROM files WHERE path = ?1", params![path])?;
    Ok(())
}

pub fn get_file_by_path(
    conn: &Connection,
    path: &str,
) -> Result<Option<FileRecord>, CodeWikiError> {
    let mut stmt = conn.prepare_cached("SELECT * FROM files WHERE path = ?1")?;
    let mut rows = stmt.query(params![path])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row_to_file_record(row).map_err(CodeWikiError::Sqlite)?))
    } else {
        Ok(None)
    }
}

pub fn get_all_files(conn: &Connection) -> Result<Vec<FileRecord>, CodeWikiError> {
    let mut stmt = conn.prepare_cached("SELECT * FROM files ORDER BY path")?;
    let rows = stmt.query_map([], row_to_file_record)?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

pub fn get_all_file_paths(conn: &Connection) -> Result<Vec<String>, CodeWikiError> {
    let mut stmt = conn.prepare_cached("SELECT path FROM files ORDER BY path")?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

/// Return all tracked file records ordered by modified_at so A5 can compare
/// against live FS metadata.  A5 owns the "is this stale?" decision.
pub fn get_stale_files(conn: &Connection) -> Result<Vec<FileRecord>, CodeWikiError> {
    let mut stmt = conn.prepare_cached(
        "SELECT * FROM files ORDER BY modified_at ASC",
    )?;
    let rows = stmt.query_map([], row_to_file_record)?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open_in_memory;

    fn make_file(path: &str, hash: &str) -> FileRecord {
        FileRecord {
            path: PathBuf::from(path),
            content_hash: hash.to_string(),
            language: "typescript".to_string(),
            size: 1024,
            modified_at: 1_000_000,
            indexed_at: now_ms(),
            node_count: 3,
            errors: vec![],
        }
    }

    #[test]
    fn upsert_twice_is_idempotent() {
        let conn = open_in_memory().unwrap();
        let file = make_file("src/foo.ts", "abc123");
        upsert_file(&conn, &file).unwrap();
        upsert_file(&conn, &file).unwrap(); // second upsert should not error

        let all = get_all_files(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].content_hash, "abc123");
    }

    #[test]
    fn delete_removes_file() {
        let conn = open_in_memory().unwrap();
        upsert_file(&conn, &make_file("src/bar.ts", "hash1")).unwrap();
        delete_file(&conn, "src/bar.ts").unwrap();
        let got = get_file_by_path(&conn, "src/bar.ts").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn get_stale_files_returns_all() {
        let conn = open_in_memory().unwrap();
        upsert_file(&conn, &make_file("a.ts", "h1")).unwrap();
        upsert_file(&conn, &make_file("b.ts", "h2")).unwrap();
        let stale = get_stale_files(&conn).unwrap();
        assert_eq!(stale.len(), 2);
    }
}

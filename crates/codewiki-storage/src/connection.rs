/// SQLite connection factory with WAL pragmas and migration runner.
use crate::migrations::run_migrations;
use crate::schema::apply_schema;
use codewiki_core::CodeWikiError;
use rusqlite::Connection;
use std::path::Path;

/// Open a SQLite connection at `path`, apply schema and all pending migrations,
/// then configure all 7 PRAGMAs in the prescribed order.
///
/// Pragma order matters:
///   1. `busy_timeout` — must be set first so any lock-wait during `journal_mode`
///      change uses the correct timeout rather than the 0ms default.
///   2. `foreign_keys` — structural correctness (cascades).
///   3. `journal_mode = WAL` — enable write-ahead logging.
///   4. `synchronous = NORMAL` — safe with WAL; faster than FULL.
///   5. `cache_size = -64000` — 64 MB page cache.
///   6. `temp_store = MEMORY` — temp tables in RAM.
///   7. `mmap_size` — 256 MB memory-mapped I/O.
#[tracing::instrument(skip(path))]
pub fn open(path: &Path) -> Result<Connection, CodeWikiError> {
    let conn = Connection::open(path)?;
    configure_pragmas(&conn)?;
    apply_schema(&conn)?;
    run_migrations(&conn)?;
    Ok(conn)
}

/// Open an in-memory connection (used in tests).
pub fn open_in_memory() -> Result<Connection, CodeWikiError> {
    let conn = Connection::open_in_memory()?;
    configure_pragmas(&conn)?;
    apply_schema(&conn)?;
    run_migrations(&conn)?;
    Ok(conn)
}

fn configure_pragmas(conn: &Connection) -> Result<(), CodeWikiError> {
    // 1. busy_timeout first (issue #238) — must be set before any operation that touches DB
    conn.execute_batch("PRAGMA busy_timeout = 5000;")?;
    // 2. foreign_keys
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    // 3. journal_mode = WAL
    conn.execute_batch("PRAGMA journal_mode = WAL;")?;
    // 4. synchronous = NORMAL
    conn.execute_batch("PRAGMA synchronous = NORMAL;")?;
    // 5. cache_size = -64000 (64 MB, negative = kibibytes)
    conn.execute_batch("PRAGMA cache_size = -64000;")?;
    // 6. temp_store = MEMORY
    conn.execute_batch("PRAGMA temp_store = MEMORY;")?;
    // 7. mmap_size = 256 MB
    conn.execute_batch("PRAGMA mmap_size = 268435456;")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn open_sets_wal_mode() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = open(&db_path).unwrap();

        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn open_applies_schema_and_migrations() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = open(&db_path).unwrap();

        let v: u32 = conn
            .query_row("SELECT MAX(version) FROM schema_versions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(v, crate::migrations::CURRENT_SCHEMA_VERSION);
    }
}

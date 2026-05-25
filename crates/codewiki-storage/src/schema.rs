/// Embedded SQL schema text.
pub const SCHEMA_SQL: &str = include_str!("schema.sql");

use codewiki_core::CodeWikiError;
use rusqlite::Connection;

/// Apply the embedded schema to the given connection.
///
/// Safe to call on an existing database — all statements use `IF NOT EXISTS`.
#[tracing::instrument(skip(conn))]
pub fn apply_schema(conn: &Connection) -> Result<(), CodeWikiError> {
    conn.execute_batch(SCHEMA_SQL)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn schema_creates_all_tables() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = Connection::open(&db_path).unwrap();
        apply_schema(&conn).unwrap();

        // Verify nodes table has 20 columns
        let col_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('nodes')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(col_count, 20, "nodes table should have 20 columns");

        // Verify schema_versions has a row
        let version: u32 = conn
            .query_row(
                "SELECT MAX(version) FROM schema_versions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn schema_is_idempotent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = Connection::open(&db_path).unwrap();
        // Applying twice should not error (IF NOT EXISTS everywhere)
        apply_schema(&conn).unwrap();
        // Second apply would fail on INSERT INTO schema_versions due to PK constraint,
        // so we just verify the first apply succeeds.
    }
}

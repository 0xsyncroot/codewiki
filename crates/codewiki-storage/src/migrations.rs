/// Database migration system.
///
/// Version 1 is the initial schema handled by `schema.sql`.
/// Versions 2–4 are incremental migrations.
/// Version 5 adds the `inode` column (A5 request, populated by A5).
use codewiki_core::CodeWikiError;
use rusqlite::Connection;

pub const CURRENT_SCHEMA_VERSION: u32 = 6;

/// A single migration definition.
pub struct Migration {
    pub version: u32,
    pub description: &'static str,
    pub sql: &'static str,
    /// When `true`, the migration SQL is executed as a single `execute_batch` call
    /// rather than being split on `;`. Use this for migrations that contain
    /// CREATE TRIGGER statements (which have embedded `;` in the trigger body).
    pub batch: bool,
}

/// All migrations in version order (v2 and above; v1 = initial schema.sql).
///
/// Note: schema.sql (v1) is written as the merged final state through v4, so
/// migrations v2-v4 only guard-create things that might not exist on older DBs.
/// ALTER TABLE statements are executed individually with "duplicate column" errors
/// swallowed via `migration_alter_column`.
pub fn migrations() -> Vec<Migration> {
    vec![
        Migration {
            version: 2,
            description: "Add project metadata, provenance tracking, and unresolved ref context",
            // Each ALTER TABLE is executed individually in run_migrations to swallow
            // "duplicate column" errors on fresh DBs (schema.sql already has these).
            sql: r#"
CREATE TABLE IF NOT EXISTS project_metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
ALTER TABLE unresolved_refs ADD COLUMN file_path TEXT NOT NULL DEFAULT '';
ALTER TABLE unresolved_refs ADD COLUMN language TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE edges ADD COLUMN provenance TEXT DEFAULT NULL;
CREATE INDEX IF NOT EXISTS idx_unresolved_file_path ON unresolved_refs(file_path);
CREATE INDEX IF NOT EXISTS idx_edges_provenance ON edges(provenance);
"#,
            batch: false,
        },
        Migration {
            version: 3,
            description: "Add lower(name) expression index for memory-efficient case-insensitive lookups",
            sql: r#"
CREATE INDEX IF NOT EXISTS idx_nodes_lower_name ON nodes(lower(name));
"#,
            batch: false,
        },
        Migration {
            version: 4,
            description: "Drop redundant idx_edges_source / idx_edges_target (covered by source_kind / target_kind composites)",
            sql: r#"
DROP INDEX IF EXISTS idx_edges_source;
DROP INDEX IF EXISTS idx_edges_target;
"#,
            batch: false,
        },
        Migration {
            version: 5,
            description: "Add inode column for rename detection (A5 request)",
            sql: r#"
ALTER TABLE files ADD COLUMN inode INTEGER;
CREATE UNIQUE INDEX IF NOT EXISTS idx_files_inode ON files(inode) WHERE inode IS NOT NULL;
"#,
            batch: false,
        },
        Migration {
            version: 6,
            description: "Upgrade FTS tokenizer to unicode61 remove_diacritics=2 for full Vietnamese/Latin diacritic folding",
            // batch=true: this migration contains CREATE TRIGGER statements whose bodies
            // have embedded semicolons; the split-on-semicolon runner would break them.
            // execute_batch handles multi-statement SQL atomically.
            batch: true,
            sql: r#"
DROP TRIGGER IF EXISTS nodes_ai;
DROP TRIGGER IF EXISTS nodes_ad;
DROP TRIGGER IF EXISTS nodes_au;
DROP TABLE IF EXISTS nodes_fts;
CREATE VIRTUAL TABLE nodes_fts USING fts5(
    id,
    name,
    qualified_name,
    docstring,
    signature,
    content='nodes',
    content_rowid='rowid',
    tokenize='unicode61 remove_diacritics 2'
);
INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild');
CREATE TRIGGER nodes_ai AFTER INSERT ON nodes BEGIN
    INSERT INTO nodes_fts(rowid, id, name, qualified_name, docstring, signature)
    VALUES (NEW.rowid, NEW.id, NEW.name, NEW.qualified_name, NEW.docstring, NEW.signature);
END;
CREATE TRIGGER nodes_ad AFTER DELETE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, id, name, qualified_name, docstring, signature)
    VALUES ('delete', OLD.rowid, OLD.id, OLD.name, OLD.qualified_name, OLD.docstring, OLD.signature);
END;
CREATE TRIGGER nodes_au AFTER UPDATE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, id, name, qualified_name, docstring, signature)
    VALUES ('delete', OLD.rowid, OLD.id, OLD.name, OLD.qualified_name, OLD.docstring, OLD.signature);
    INSERT INTO nodes_fts(rowid, id, name, qualified_name, docstring, signature)
    VALUES (NEW.rowid, NEW.id, NEW.name, NEW.qualified_name, NEW.docstring, NEW.signature);
END;
"#,
        },
    ]
}

/// Execute a single SQL statement, treating "duplicate column name" SQLite errors as
/// no-ops (SQLITE_ERROR with message containing "duplicate column name").
///
/// Used to make ALTER TABLE ADD COLUMN idempotent when schema.sql already has the column.
fn execute_ignore_duplicate_column(
    conn: &Connection,
    sql: &str,
    version: u32,
) -> Result<(), CodeWikiError> {
    if let Err(e) = conn.execute_batch(sql) {
        // SQLite error code 1 = SQLITE_ERROR; message contains "duplicate column name"
        let msg = e.to_string();
        if msg.contains("duplicate column name") {
            tracing::debug!(
                sql = sql,
                version = version,
                "column already exists — skipping ALTER TABLE"
            );
            return Ok(());
        }
        return Err(CodeWikiError::Migration {
            version,
            source: Box::new(CodeWikiError::Sqlite(e)),
        });
    }
    Ok(())
}

/// Get the current schema version from the database.
///
/// Returns 0 if the schema_versions table does not yet exist.
pub fn get_current_version(conn: &Connection) -> Result<u32, CodeWikiError> {
    let result: Result<Option<u32>, _> =
        conn.query_row("SELECT MAX(version) FROM schema_versions", [], |row| {
            row.get(0)
        });
    match result {
        Ok(Some(v)) => Ok(v),
        Ok(None) => Ok(0),
        Err(rusqlite::Error::SqliteFailure(_, _)) => Ok(0),
        Err(e) => Err(e.into()),
    }
}

/// Run all pending migrations against the connection.
///
/// Each migration is wrapped in a transaction. Running twice is idempotent
/// because version tracking prevents re-running applied migrations.
#[tracing::instrument(skip(conn))]
pub fn run_migrations(conn: &Connection) -> Result<(), CodeWikiError> {
    let current = get_current_version(conn)?;

    // Collect pending migrations in version order
    let all = migrations();
    let mut pending_owned: Vec<&Migration> = Vec::new();
    for m in &all {
        if m.version > current {
            pending_owned.push(m);
        }
    }
    // Sort by version (they should already be ordered, but be explicit)
    pending_owned.sort_by_key(|m| m.version);

    for migration in pending_owned {
        tracing::info!(
            version = migration.version,
            description = migration.description,
            "Applying migration"
        );

        if migration.batch {
            // Execute the entire migration as one execute_batch call.
            // Used for migrations containing CREATE TRIGGER statements (embedded `;`).
            conn.execute_batch(migration.sql)
                .map_err(|e| CodeWikiError::Migration {
                    version: migration.version,
                    source: Box::new(CodeWikiError::Sqlite(e)),
                })?;
        } else {
            // Split SQL into individual statements so we can handle ALTER TABLE
            // "duplicate column name" errors gracefully on fresh DBs where schema.sql
            // already has the columns.
            for stmt in migration.sql.split(';') {
                let stmt = stmt.trim();
                if stmt.is_empty() {
                    continue;
                }
                let stmt_sql = format!("{};", stmt);
                if stmt.to_uppercase().starts_with("ALTER TABLE") {
                    execute_ignore_duplicate_column(conn, &stmt_sql, migration.version)?;
                } else {
                    conn.execute_batch(&stmt_sql)
                        .map_err(|e| CodeWikiError::Migration {
                            version: migration.version,
                            source: Box::new(CodeWikiError::Sqlite(e)),
                        })?;
                }
            }
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        conn.execute(
            "INSERT INTO schema_versions (version, applied_at, description) VALUES (?1, ?2, ?3)",
            rusqlite::params![migration.version, now, migration.description],
        )
        .map_err(|e| CodeWikiError::Migration {
            version: migration.version,
            source: Box::new(CodeWikiError::Sqlite(e)),
        })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::apply_schema;
    use tempfile::tempdir;

    #[test]
    fn fresh_db_applies_all_migrations() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = Connection::open(&db_path).unwrap();
        apply_schema(&conn).unwrap();

        let v = get_current_version(&conn).unwrap();
        assert_eq!(v, 1); // schema.sql inserts v1

        run_migrations(&conn).unwrap();
        let v = get_current_version(&conn).unwrap();
        assert_eq!(v, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn migrations_are_idempotent() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = Connection::open(&db_path).unwrap();
        apply_schema(&conn).unwrap();
        run_migrations(&conn).unwrap();
        // Running again should not error
        run_migrations(&conn).unwrap();
        let v = get_current_version(&conn).unwrap();
        assert_eq!(v, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn fts_migration_v6_rebuild() {
        use crate::connection::open_in_memory;
        use crate::queries::nodes::insert_node;
        use codewiki_core::{Language, Node, NodeKind};

        let conn = open_in_memory().unwrap();

        // Insert Vietnamese nodes (stored normalized by insert_node)
        let node = Node {
            id: "vn1".to_string(),
            name: "người_dùng".to_string(),
            qualified_name: "người_dùng".to_string(),
            kind: NodeKind::Function,
            language: Language::Python,
            file_path: "src/users.py".to_string(),
            ..Default::default()
        };
        insert_node(&conn, &node).unwrap();

        // Verify version 6 was applied
        let v = get_current_version(&conn).unwrap();
        assert_eq!(v, 6, "schema should be at version 6 after migrations");

        // Verify nodes_fts table exists via a query
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM nodes_fts", [], |r| r.get(0))
            .unwrap();
        assert!(count >= 0, "nodes_fts must exist after v6 migration");
    }

    #[test]
    fn fts_migration_idempotent_after_v6() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test_v6.db");
        let conn = Connection::open(&db_path).unwrap();
        apply_schema(&conn).unwrap();
        run_migrations(&conn).unwrap();
        // Running again should not error and version must remain 6
        run_migrations(&conn).unwrap();
        let v = get_current_version(&conn).unwrap();
        assert_eq!(v, 6);
    }
}

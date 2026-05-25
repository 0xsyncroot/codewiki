/// Project metadata and statistics helpers.
use codewiki_core::{CodeWikiError, GraphStats};
use rusqlite::{Connection, params};
use std::collections::HashMap;

/// Return a `HashMap<kind_string, count>` of all edges grouped by kind.
///
/// Used by the graph web server's `/api/stats` endpoint.
/// Does NOT modify `GraphStats` — it's merged in the handler.
///
/// SQL: `SELECT kind, COUNT(*) AS cnt FROM edges GROUP BY kind`
/// Uses `idx_edges_kind` (or an implicit scan).
pub fn get_edges_by_kind(conn: &Connection) -> Result<HashMap<String, u64>, CodeWikiError> {
    let mut stmt =
        conn.prepare_cached("SELECT kind, COUNT(*) AS cnt FROM edges GROUP BY kind")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut result = HashMap::new();
    for row in rows {
        let (k, v) = row.map_err(CodeWikiError::Sqlite)?;
        result.insert(k, v as u64);
    }
    Ok(result)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub fn get_metadata(conn: &Connection, key: &str) -> Result<Option<String>, CodeWikiError> {
    let mut stmt =
        conn.prepare_cached("SELECT value FROM project_metadata WHERE key = ?1")?;
    let mut rows = stmt.query(params![key])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row.get(0)?))
    } else {
        Ok(None)
    }
}

pub fn set_metadata(conn: &Connection, key: &str, value: &str) -> Result<(), CodeWikiError> {
    conn.execute(
        r#"INSERT INTO project_metadata (key, value, updated_at) VALUES (?1, ?2, ?3)
           ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at"#,
        params![key, value, now_ms()],
    )?;
    Ok(())
}

pub fn get_all_metadata(conn: &Connection) -> Result<HashMap<String, String>, CodeWikiError> {
    let mut stmt =
        conn.prepare_cached("SELECT key, value FROM project_metadata")?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
    let mut result = HashMap::new();
    for row in rows {
        let (k, v) = row.map_err(CodeWikiError::Sqlite)?;
        result.insert(k, v);
    }
    Ok(result)
}

pub fn clear_all(conn: &Connection) -> Result<(), CodeWikiError> {
    conn.execute_batch(
        r#"DELETE FROM unresolved_refs;
           DELETE FROM edges;
           DELETE FROM nodes;
           DELETE FROM files;
           DELETE FROM project_metadata;"#,
    )?;
    Ok(())
}

pub fn get_stats(conn: &Connection) -> Result<GraphStats, CodeWikiError> {
    let (node_count, edge_count, file_count): (i64, i64, i64) = conn.query_row(
        r#"SELECT
            (SELECT COUNT(*) FROM nodes) AS node_count,
            (SELECT COUNT(*) FROM edges) AS edge_count,
            (SELECT COUNT(*) FROM files) AS file_count"#,
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    let unresolved_ref_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM unresolved_refs", [], |row| row.get(0))?;

    let journal_mode: String =
        conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;

    // DB size
    let (page_count, page_size): (i64, i64) = conn.query_row(
        "SELECT page_count, page_size FROM pragma_page_count(), pragma_page_size()",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap_or((0, 4096));
    let db_size_bytes = (page_count * page_size) as u64;

    let mut nodes_by_kind: HashMap<String, u64> = HashMap::new();
    {
        let mut stmt =
            conn.prepare_cached("SELECT kind, COUNT(*) as cnt FROM nodes GROUP BY kind")?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
        for row in rows {
            let (k, v) = row.map_err(CodeWikiError::Sqlite)?;
            nodes_by_kind.insert(k, v as u64);
        }
    }

    let mut files_by_language: HashMap<String, u64> = HashMap::new();
    {
        let mut stmt =
            conn.prepare_cached("SELECT language, COUNT(*) as cnt FROM files GROUP BY language")?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
        for row in rows {
            let (k, v) = row.map_err(CodeWikiError::Sqlite)?;
            files_by_language.insert(k, v as u64);
        }
    }

    Ok(GraphStats {
        node_count: node_count as u64,
        edge_count: edge_count as u64,
        file_count: file_count as u64,
        unresolved_ref_count: unresolved_ref_count as u64,
        db_size_bytes,
        journal_mode,
        nodes_by_kind,
        files_by_language,
    })
}

/// Run `PRAGMA optimize` and `PRAGMA wal_checkpoint(PASSIVE)` (T-112).
///
/// Both are best-effort and non-blocking; errors are logged but not propagated.
pub fn run_maintenance(conn: &Connection) {
    if let Err(e) = conn.execute_batch("PRAGMA optimize;") {
        tracing::warn!(error = %e, "PRAGMA optimize failed (non-fatal)");
    }
    if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);") {
        tracing::warn!(error = %e, "PRAGMA wal_checkpoint failed (non-fatal)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open_in_memory;
    use crate::queries::nodes::insert_node;
    use codewiki_core::{Language, Node, NodeKind};

    fn make_node(id: &str) -> Node {
        Node {
            id: id.to_string(),
            name: id.to_string(),
            qualified_name: id.to_string(),
            kind: NodeKind::Function,
            language: Language::TypeScript,
            file_path: "src/x.ts".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn get_stats_counts_nodes() {
        let conn = open_in_memory().unwrap();
        for i in 0..5 {
            insert_node(&conn, &make_node(&format!("n{}", i))).unwrap();
        }
        let stats = get_stats(&conn).unwrap();
        assert_eq!(stats.node_count, 5);
    }

    #[test]
    fn set_and_get_metadata() {
        let conn = open_in_memory().unwrap();
        set_metadata(&conn, "last_sync", "12345").unwrap();
        let v = get_metadata(&conn, "last_sync").unwrap();
        assert_eq!(v, Some("12345".to_string()));
    }

    #[test]
    fn run_maintenance_does_not_panic() {
        let conn = open_in_memory().unwrap();
        run_maintenance(&conn);
    }

    #[test]
    fn get_edges_by_kind_returns_hashmap() {
        use crate::queries::edges::insert_edge;
        use codewiki_core::{Edge, EdgeKind};

        let conn = open_in_memory().unwrap();
        insert_node(&conn, &make_node("a")).unwrap();
        insert_node(&conn, &make_node("b")).unwrap();
        insert_edge(
            &conn,
            &Edge {
                id: "e1".into(),
                source_id: "a".into(),
                target_id: "b".into(),
                kind: EdgeKind::Calls,
                ..Default::default()
            },
        )
        .unwrap();

        let map = get_edges_by_kind(&conn).unwrap();
        assert!(map.contains_key("calls"), "expected 'calls' key in edges_by_kind");
        assert_eq!(map["calls"], 1);
    }
}

/// Prepared-statement helpers for edge operations.
use codewiki_core::{CodeWikiError, Edge, EdgeKind};
use rusqlite::{params, Connection};

// ---------------------------------------------------------------------------
// Row → Edge conversion
// ---------------------------------------------------------------------------

pub fn row_to_edge(row: &rusqlite::Row<'_>) -> Result<Edge, rusqlite::Error> {
    let kind_str: String = row.get("kind")?;
    let id: i64 = row.get("id")?;
    Ok(Edge {
        id: id.to_string(),
        source_id: row.get("source")?,
        target_id: row.get("target")?,
        kind: serde_json::from_value(serde_json::Value::String(kind_str))
            .unwrap_or(EdgeKind::Unknown),
        line: row.get::<_, Option<i64>>("line")?.map(|v| v as u32),
        col: row.get::<_, Option<i64>>("col")?.map(|v| v as u32),
        provenance: row.get("provenance")?,
        metadata: row.get("metadata")?,
        confidence: None,
    })
}

fn edge_kind_str(edge: &Edge) -> String {
    serde_json::to_value(&edge.kind)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "unknown".to_string())
}

fn kind_to_str(k: &EdgeKind) -> String {
    serde_json::to_value(k)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "unknown".to_string())
}

// ---------------------------------------------------------------------------
// INSERT / DELETE
// ---------------------------------------------------------------------------

pub fn insert_edge(conn: &Connection, edge: &Edge) -> Result<(), CodeWikiError> {
    let mut stmt = conn.prepare_cached(
        r#"INSERT OR IGNORE INTO edges (source, target, kind, metadata, line, col, provenance)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
    )?;
    stmt.execute(params![
        edge.source_id,
        edge.target_id,
        edge_kind_str(edge),
        edge.metadata,
        edge.line.map(|v| v as i64),
        edge.col.map(|v| v as i64),
        edge.provenance,
    ])?;
    Ok(())
}

pub fn insert_edges_batch(conn: &Connection, edges: &[Edge]) -> Result<(), CodeWikiError> {
    for edge in edges {
        insert_edge(conn, edge)?;
    }
    Ok(())
}

/// OPT-14: Bulk edge insert using multi-row `INSERT OR IGNORE INTO edges … VALUES (…),(…),…`.
///
/// SQLite compiles and executes a single statement per chunk instead of one
/// statement per edge, cutting the per-row overhead by ~5–10x for large batches.
/// Chunk size of 100 rows keeps the generated SQL under SQLite's SQLITE_LIMIT_VARIABLE_NUMBER
/// (default 999; 100 rows × 7 columns = 700 bind vars, safely within limit).
pub fn insert_resolved_edges_bulk(conn: &Connection, edges: &[Edge]) -> Result<(), CodeWikiError> {
    const CHUNK_SIZE: usize = 100;
    const COLS: usize = 7; // source, target, kind, metadata, line, col, provenance

    for chunk in edges.chunks(CHUNK_SIZE) {
        if chunk.is_empty() {
            continue;
        }
        // Build: VALUES (?1,?2,?3,?4,?5,?6,?7),(?8,?9,...),…
        let row_placeholders: Vec<String> = (0..chunk.len())
            .map(|i| {
                let base = i * COLS + 1;
                format!(
                    "(?{},?{},?{},?{},?{},?{},?{})",
                    base,
                    base + 1,
                    base + 2,
                    base + 3,
                    base + 4,
                    base + 5,
                    base + 6
                )
            })
            .collect();
        let sql = format!(
            "INSERT OR IGNORE INTO edges (source, target, kind, metadata, line, col, provenance) VALUES {}",
            row_placeholders.join(",")
        );
        // Collect bind params as Box<dyn rusqlite::ToSql>.
        // We use rusqlite::types::Value to erase the type.
        use rusqlite::types::Value;
        let mut params: Vec<Value> = Vec::with_capacity(chunk.len() * COLS);
        for edge in chunk {
            params.push(Value::Text(edge.source_id.clone()));
            params.push(Value::Text(edge.target_id.clone()));
            params.push(Value::Text(edge_kind_str(edge)));
            params.push(match &edge.metadata {
                Some(m) => Value::Text(m.clone()),
                None => Value::Null,
            });
            params.push(match edge.line {
                Some(v) => Value::Integer(v as i64),
                None => Value::Null,
            });
            params.push(match edge.col {
                Some(v) => Value::Integer(v as i64),
                None => Value::Null,
            });
            params.push(match &edge.provenance {
                Some(p) => Value::Text(p.clone()),
                None => Value::Null,
            });
        }
        let mut stmt = conn.prepare_cached(&sql)?;
        stmt.execute(rusqlite::params_from_iter(params.iter()))?;
    }
    Ok(())
}

pub fn delete_edges_by_source(conn: &Connection, source_id: &str) -> Result<(), CodeWikiError> {
    conn.execute("DELETE FROM edges WHERE source = ?1", params![source_id])?;
    Ok(())
}

/// Delete all edges whose source or target node belongs to the given file.
/// Because the edges table has FK ON DELETE CASCADE referencing nodes(id),
/// deleting nodes already cascades edges — but this helper is called
/// explicitly before `delete_nodes_by_file` to handle any edges that
/// reference nodes from *other* files pointing into the deleted file's nodes.
/// Delete every edge whose source is one of `ids` (chunked IN-list).
///
/// Used when re-storing a file: outgoing edges are always rebuilt from the
/// fresh parse plus re-resolution, so they are deleted wholesale — unlike
/// incoming edges, which are retargeted or harvested, never cascaded away.
pub fn delete_edges_by_sources(conn: &Connection, ids: &[&str]) -> Result<(), CodeWikiError> {
    for chunk in ids.chunks(500) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!("DELETE FROM edges WHERE source IN ({placeholders})");
        let mut stmt = conn.prepare(&sql)?;
        stmt.execute(rusqlite::params_from_iter(chunk.iter()))?;
    }
    Ok(())
}

/// Repoint every incoming edge from `old_target` to `new_target`.
///
/// `UPDATE OR IGNORE`: once the unique edge-identity index exists, a retarget
/// that would collide with an already-present identical edge is skipped — the
/// stale row is then swept by the stale-node delete's cascade.
pub fn retarget_incoming_edges(
    conn: &Connection,
    old_target: &str,
    new_target: &str,
) -> Result<(), CodeWikiError> {
    conn.execute(
        "UPDATE OR IGNORE edges SET target = ?2 WHERE target = ?1",
        params![old_target, new_target],
    )?;
    Ok(())
}

/// Delete every edge carrying `provenance`. Used to refresh a whole class of
/// synthesised edges (e.g. Go structural `implements`) before re-inserting it.
pub fn delete_edges_by_provenance(
    conn: &Connection,
    provenance: &str,
) -> Result<usize, CodeWikiError> {
    let n = conn.execute(
        "DELETE FROM edges WHERE provenance = ?1",
        params![provenance],
    )?;
    Ok(n)
}

pub fn delete_edges_by_file(conn: &Connection, file_path: &str) -> Result<(), CodeWikiError> {
    conn.execute(
        "DELETE FROM edges WHERE source IN (SELECT id FROM nodes WHERE file_path = ?1)
              OR target IN (SELECT id FROM nodes WHERE file_path = ?2)",
        params![file_path, file_path],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// SELECT helpers
// ---------------------------------------------------------------------------

fn collect_rows_from_stmt(
    stmt: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
) -> Result<Vec<Edge>, CodeWikiError> {
    let rows = stmt.query_map(params, row_to_edge)?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

// ---------------------------------------------------------------------------
// SELECT
// ---------------------------------------------------------------------------

pub fn get_outgoing_edges(
    conn: &Connection,
    source_id: &str,
    kinds: Option<&[EdgeKind]>,
) -> Result<Vec<Edge>, CodeWikiError> {
    if let Some(kinds) = kinds {
        if !kinds.is_empty() {
            let kind_strs: Vec<String> = kinds.iter().map(kind_to_str).collect();
            let placeholders = kind_strs
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", i + 2))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT * FROM edges WHERE source = ?1 AND kind IN ({})",
                placeholders
            );
            let all_params: Vec<String> = std::iter::once(source_id.to_string())
                .chain(kind_strs.iter().cloned())
                .collect();
            let mut stmt = conn.prepare(&sql)?;
            return collect_rows_from_stmt(
                &mut stmt,
                rusqlite::params_from_iter(all_params.iter()),
            );
        }
    }
    let mut stmt = conn.prepare_cached("SELECT * FROM edges WHERE source = ?1")?;
    collect_rows_from_stmt(&mut stmt, params![source_id])
}

pub fn get_incoming_edges(
    conn: &Connection,
    target_id: &str,
    kinds: Option<&[EdgeKind]>,
) -> Result<Vec<Edge>, CodeWikiError> {
    if let Some(kinds) = kinds {
        if !kinds.is_empty() {
            let kind_strs: Vec<String> = kinds.iter().map(kind_to_str).collect();
            let placeholders = kind_strs
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", i + 2))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT * FROM edges WHERE target = ?1 AND kind IN ({})",
                placeholders
            );
            let all_params: Vec<String> = std::iter::once(target_id.to_string())
                .chain(kind_strs.iter().cloned())
                .collect();
            let mut stmt = conn.prepare(&sql)?;
            return collect_rows_from_stmt(
                &mut stmt,
                rusqlite::params_from_iter(all_params.iter()),
            );
        }
    }
    let mut stmt = conn.prepare_cached("SELECT * FROM edges WHERE target = ?1")?;
    collect_rows_from_stmt(&mut stmt, params![target_id])
}

pub fn find_edges_between_nodes(
    conn: &Connection,
    node_ids: &[String],
    kinds: Option<&[EdgeKind]>,
) -> Result<Vec<Edge>, CodeWikiError> {
    if node_ids.is_empty() {
        return Ok(vec![]);
    }
    let ids_json = serde_json::to_string(node_ids)?;
    let base = r#"SELECT * FROM edges
        WHERE source IN (SELECT value FROM json_each(?1))
          AND target IN (SELECT value FROM json_each(?2))"#;

    if let Some(kinds) = kinds {
        if !kinds.is_empty() {
            let kind_strs: Vec<String> = kinds.iter().map(kind_to_str).collect();
            let placeholders = kind_strs
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", i + 3))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!("{} AND kind IN ({})", base, placeholders);
            let all_params: Vec<String> = std::iter::once(ids_json.clone())
                .chain(std::iter::once(ids_json.clone()))
                .chain(kind_strs.iter().cloned())
                .collect();
            let mut stmt = conn.prepare(&sql)?;
            return collect_rows_from_stmt(
                &mut stmt,
                rusqlite::params_from_iter(all_params.iter()),
            );
        }
    }
    let mut stmt = conn.prepare(base)?;
    collect_rows_from_stmt(&mut stmt, params![ids_json, ids_json])
}

/// Return the set of distinct file paths that contain nodes with edges
/// *targeting* any node in the given changed files.  These are "reverse
/// dependents" — files that import/call/reference symbols in the changed files
/// and whose unresolved refs may now need re-resolution.
///
/// Uses `idx_edges_target_kind` (via the target column) and
/// `idx_nodes_file_path` so the JOIN is index-driven even on large graphs.
///
/// OPT-9: used to scope incremental resolution to changed + dependent files.
/// Return the in-degree and out-degree for a given node across ALL edge kinds.
///
/// Used by the graph web server's `/api/node/:id` endpoint.
/// Uses `idx_edges_target_kind` and `idx_edges_source_kind` prefix scans.
///
/// Returns `(in_degree, out_degree)`.
pub fn get_node_degree(conn: &Connection, node_id: &str) -> Result<(u64, u64), CodeWikiError> {
    let (in_degree, out_degree): (i64, i64) = conn.query_row(
        "SELECT \
            (SELECT COUNT(*) FROM edges WHERE target = ?1) AS in_degree, \
            (SELECT COUNT(*) FROM edges WHERE source = ?1) AS out_degree",
        params![node_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((in_degree as u64, out_degree as u64))
}

pub fn get_dependent_files(
    conn: &Connection,
    changed_file_paths: &[String],
) -> Result<Vec<String>, CodeWikiError> {
    if changed_file_paths.is_empty() {
        return Ok(vec![]);
    }
    let n = changed_file_paths.len();

    // First IN-list: ?1 … ?n  (n_tgt.file_path IN changed files)
    let tgt_placeholders = (1..=n)
        .map(|i| format!("?{}", i))
        .collect::<Vec<_>>()
        .join(",");

    // Second IN-list: ?{n+1} … ?{2n}  (n_src.file_path NOT IN changed files)
    // Use distinct parameter numbers so SQLite binds them independently.
    let src_placeholders = (n + 1..=2 * n)
        .map(|i| format!("?{}", i))
        .collect::<Vec<_>>()
        .join(",");

    // Find nodes in the changed files, then find all edges whose target is
    // one of those nodes, then return the file_path of the *source* node.
    // The source file is the dependent that may have stale unresolved refs.
    let sql = format!(
        "SELECT DISTINCT n_src.file_path
         FROM edges e
         JOIN nodes n_tgt ON n_tgt.id = e.target
         JOIN nodes n_src ON n_src.id = e.source
         WHERE n_tgt.file_path IN ({tgt_placeholders})
           AND n_src.file_path NOT IN ({src_placeholders})",
        tgt_placeholders = tgt_placeholders,
        src_placeholders = src_placeholders,
    );
    // params = changed_file_paths twice (once for n_tgt IN, once for n_src NOT IN).
    let params_vec: Vec<String> = changed_file_paths
        .iter()
        .chain(changed_file_paths.iter())
        .cloned()
        .collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params_vec.iter()), |row| {
        row.get(0)
    })?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open_in_memory;
    use crate::queries::nodes::insert_node;
    use codewiki_core::{EdgeKind, Language, Node, NodeKind};

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

    fn make_edge(from: &str, to: &str, kind: EdgeKind) -> Edge {
        Edge {
            id: format!("{}->{}", from, to),
            source_id: from.to_string(),
            target_id: to.to_string(),
            kind,
            ..Default::default()
        }
    }

    #[test]
    fn get_node_degree_counts_correctly() {
        let conn = open_in_memory().unwrap();
        insert_node(&conn, &make_node("a")).unwrap();
        insert_node(&conn, &make_node("b")).unwrap();
        insert_node(&conn, &make_node("c")).unwrap();

        insert_edge(&conn, &make_edge("a", "b", EdgeKind::Calls)).unwrap();
        insert_edge(&conn, &make_edge("c", "a", EdgeKind::Calls)).unwrap();

        let (in_deg, out_deg) = super::get_node_degree(&conn, "a").unwrap();
        assert_eq!(in_deg, 1, "a has 1 incoming edge (from c)");
        assert_eq!(out_deg, 1, "a has 1 outgoing edge (to b)");

        let (in_deg_b, out_deg_b) = super::get_node_degree(&conn, "b").unwrap();
        assert_eq!(in_deg_b, 1);
        assert_eq!(out_deg_b, 0);
    }

    #[test]
    fn insert_and_query_edges() {
        let conn = open_in_memory().unwrap();
        insert_node(&conn, &make_node("a")).unwrap();
        insert_node(&conn, &make_node("b")).unwrap();
        insert_node(&conn, &make_node("c")).unwrap();

        let e1 = make_edge("a", "b", EdgeKind::Calls);
        let e2 = make_edge("a", "c", EdgeKind::Imports);
        insert_edge(&conn, &e1).unwrap();
        insert_edge(&conn, &e2).unwrap();

        let outgoing = get_outgoing_edges(&conn, "a", None).unwrap();
        assert_eq!(outgoing.len(), 2);

        let outgoing_calls = get_outgoing_edges(&conn, "a", Some(&[EdgeKind::Calls])).unwrap();
        assert_eq!(outgoing_calls.len(), 1);

        let incoming_b = get_incoming_edges(&conn, "b", None).unwrap();
        assert_eq!(incoming_b.len(), 1);
    }
}

use crate::search::normalize_for_fts;
/// Prepared-statement helpers for node CRUD operations.
use codewiki_core::{CodeWikiError, Node, NodeKind};
use rusqlite::{params, Connection};
use serde_json;

// ---------------------------------------------------------------------------
// Row → Node conversion
// ---------------------------------------------------------------------------

pub fn row_to_node(row: &rusqlite::Row<'_>) -> Result<Node, rusqlite::Error> {
    use codewiki_core::Language;

    let kind_str: String = row.get("kind")?;
    let lang_str: String = row.get("language")?;

    Ok(Node {
        id: row.get("id")?,
        name: row.get("name")?,
        qualified_name: row.get("qualified_name")?,
        kind: serde_json::from_value(serde_json::Value::String(kind_str))
            .unwrap_or(NodeKind::Unknown),
        language: serde_json::from_value(serde_json::Value::String(lang_str))
            .unwrap_or(Language::Unknown),
        file_path: row.get("file_path")?,
        start_line: row.get::<_, i64>("start_line")? as u32,
        end_line: row.get::<_, i64>("end_line")? as u32,
        start_col: row.get::<_, i64>("start_column")? as u32,
        end_col: row.get::<_, i64>("end_column")? as u32,
        is_exported: row.get::<_, i64>("is_exported")? != 0,
        docstring: row.get("docstring")?,
        signature: row.get("signature")?,
        metadata: row.get("decorators")?, // reuse metadata for decorators
    })
}

// ---------------------------------------------------------------------------
// Node → params helpers
// ---------------------------------------------------------------------------

fn node_kind_str(node: &Node) -> String {
    serde_json::to_value(&node.kind)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "unknown".to_string())
}

fn node_lang_str(node: &Node) -> String {
    serde_json::to_value(&node.language)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "unknown".to_string())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ---------------------------------------------------------------------------
// INSERT / UPDATE / DELETE
// ---------------------------------------------------------------------------

pub fn insert_node(conn: &Connection, node: &Node) -> Result<(), CodeWikiError> {
    // GAP-5: decode is_async from metadata JSON (set by extract_method when the
    // `async` modifier is detected).  Format: `{"is_async":true}`.
    let is_async: i64 = node
        .metadata
        .as_deref()
        .and_then(|m| {
            if m.contains("\"is_async\":true") {
                Some(1i64)
            } else {
                None
            }
        })
        .unwrap_or(0);

    let mut stmt = conn.prepare_cached(
        r#"INSERT OR REPLACE INTO nodes
            (id, kind, name, qualified_name, file_path, language,
             start_line, end_line, start_column, end_column,
             docstring, signature, visibility,
             is_exported, is_async, is_static, is_abstract,
             decorators, type_parameters, updated_at)
           VALUES
            (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,NULL,?13,?15,0,0,?16,NULL,?14)"#,
    )?;
    let norm_name = normalize_for_fts(&node.name);
    let norm_qname = normalize_for_fts(&node.qualified_name);
    let norm_doc = node.docstring.as_deref().map(normalize_for_fts);

    stmt.execute(params![
        node.id,
        node_kind_str(node),
        norm_name,
        norm_qname,
        node.file_path,
        node_lang_str(node),
        node.start_line as i64,
        node.end_line as i64,
        node.start_col as i64,
        node.end_col as i64,
        norm_doc,
        node.signature,
        node.is_exported as i64,
        now_ms(),
        is_async,
        node.metadata, // decorators column — stores the raw metadata JSON
    ])?;
    Ok(())
}

pub fn insert_nodes_batch(conn: &Connection, nodes: &[Node]) -> Result<(), CodeWikiError> {
    for node in nodes {
        insert_node(conn, node)?;
    }
    Ok(())
}

pub fn delete_node(conn: &Connection, id: &str) -> Result<(), CodeWikiError> {
    conn.execute("DELETE FROM nodes WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn delete_nodes_by_file(conn: &Connection, file_path: &str) -> Result<(), CodeWikiError> {
    conn.execute("DELETE FROM nodes WHERE file_path = ?1", params![file_path])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// SELECT
// ---------------------------------------------------------------------------

pub fn get_node_by_id(conn: &Connection, id: &str) -> Result<Option<Node>, CodeWikiError> {
    let mut stmt = conn.prepare_cached("SELECT * FROM nodes WHERE id = ?1")?;
    let mut rows = stmt.query(params![id])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row_to_node(row).map_err(CodeWikiError::Sqlite)?))
    } else {
        Ok(None)
    }
}

/// Batch lookup — chunks at 500 to respect SQLite parameter limits.
/// Return the subset of `ids` that exist in the nodes table as a `HashSet<String>`.
///
/// Used by `commit_resolved_batch` (OPT-13) to replace per-edge existence checks
/// (2 queries/edge × batch_size) with a single batched IN-query, reducing
/// resolution write time by ~4-8× on large corpora.
pub fn nodes_ids_exist_set(
    conn: &Connection,
    ids: &[&str],
) -> Result<std::collections::HashSet<String>, CodeWikiError> {
    const CHUNK: usize = 500;
    let mut result = std::collections::HashSet::with_capacity(ids.len());
    for chunk in ids.chunks(CHUNK) {
        let placeholders = chunk
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("SELECT id FROM nodes WHERE id IN ({})", placeholders);
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
            r.get::<_, String>(0)
        })?;
        for row in rows {
            result.insert(row.map_err(CodeWikiError::Sqlite)?);
        }
    }
    Ok(result)
}

pub fn get_nodes_by_ids(conn: &Connection, ids: &[String]) -> Result<Vec<Node>, CodeWikiError> {
    const CHUNK: usize = 500;
    let mut result = Vec::new();
    for chunk in ids.chunks(CHUNK) {
        let placeholders = chunk
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("SELECT * FROM nodes WHERE id IN ({})", placeholders);
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), row_to_node)?;
        for row in rows {
            result.push(row.map_err(CodeWikiError::Sqlite)?);
        }
    }
    Ok(result)
}

pub fn get_nodes_by_file(conn: &Connection, file_path: &str) -> Result<Vec<Node>, CodeWikiError> {
    let mut stmt =
        conn.prepare_cached("SELECT * FROM nodes WHERE file_path = ?1 ORDER BY start_line")?;
    let rows = stmt.query_map(params![file_path], row_to_node)?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

pub fn get_nodes_by_kind(conn: &Connection, kind: &NodeKind) -> Result<Vec<Node>, CodeWikiError> {
    let kind_str = serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "unknown".to_string());
    let mut stmt = conn.prepare_cached("SELECT * FROM nodes WHERE kind = ?1")?;
    let rows = stmt.query_map(params![kind_str], row_to_node)?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

pub fn get_nodes_by_name(conn: &Connection, name: &str) -> Result<Vec<Node>, CodeWikiError> {
    let mut stmt = conn.prepare_cached("SELECT * FROM nodes WHERE name = ?1")?;
    let rows = stmt.query_map(params![name], row_to_node)?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

pub fn get_nodes_by_qualified_name_exact(
    conn: &Connection,
    qualified_name: &str,
) -> Result<Vec<Node>, CodeWikiError> {
    let mut stmt = conn.prepare_cached("SELECT * FROM nodes WHERE qualified_name = ?1")?;
    let rows = stmt.query_map(params![qualified_name], row_to_node)?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

pub fn get_nodes_by_lower_name(
    conn: &Connection,
    lower_name: &str,
) -> Result<Vec<Node>, CodeWikiError> {
    let mut stmt = conn.prepare_cached("SELECT * FROM nodes WHERE lower(name) = ?1")?;
    let rows = stmt.query_map(params![lower_name], row_to_node)?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

/// Find nodes whose lowercased name matches any of `names`, optionally filtered by kind.
/// Port of TS `findNodesByExactName` from QueryBuilder.
/// WHERE lower(name) IN (...) [AND kind IN (...)]
pub fn find_nodes_by_exact_name(
    conn: &Connection,
    names: &[String],
    kinds: Option<&[NodeKind]>,
    limit: usize,
) -> Result<Vec<Node>, CodeWikiError> {
    if names.is_empty() {
        return Ok(vec![]);
    }

    let lower_names: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();

    // Build IN clause
    let name_placeholders = lower_names
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(",");

    let mut params: Vec<String> = lower_names.clone();

    let mut sql = format!(
        "SELECT * FROM nodes WHERE lower(name) IN ({})",
        name_placeholders
    );

    if let Some(ks) = kinds {
        if !ks.is_empty() {
            let n = params.len();
            let kind_strs: Vec<String> = ks
                .iter()
                .map(|k| {
                    serde_json::to_value(k)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_default()
                })
                .collect();
            sql += &format!(
                " AND kind IN ({})",
                kind_strs
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("?{}", n + i + 1))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            params.extend(kind_strs);
        }
    }

    sql += &format!(" ORDER BY name LIMIT {}", limit);

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), row_to_node)?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

/// Metric used to order nodes in `get_top_nodes_by_degree`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegreeMetric {
    /// Total degree (in + out).
    Total,
    /// In-degree only.
    In,
    /// Out-degree only.
    Out,
}

/// Return the top `limit` nodes ordered by the chosen degree metric.
///
/// Used by the graph web server's `/api/top-nodes` endpoint.
/// Returns `Vec<(Node, in_degree, out_degree)>`.
pub fn get_top_nodes_by_degree(
    conn: &Connection,
    metric: DegreeMetric,
    kind_filter: Option<&codewiki_core::NodeKind>,
    limit: usize,
) -> Result<Vec<(Node, u64, u64)>, codewiki_core::CodeWikiError> {
    use codewiki_core::CodeWikiError;

    let order_col = match metric {
        DegreeMetric::Total => "degree",
        DegreeMetric::In => "in_degree",
        DegreeMetric::Out => "out_degree",
    };

    let sql = if let Some(k) = kind_filter {
        let kind_str = serde_json::to_value(k)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "unknown".to_string());
        format!(
            "SELECT \
                n.id, n.kind, n.name, n.qualified_name, n.file_path, n.language, \
                n.start_line, n.end_line, n.start_column, n.end_column, \
                n.is_exported, n.docstring, n.signature, n.decorators, \
                COUNT(DISTINCT e_in.id)  AS in_degree, \
                COUNT(DISTINCT e_out.id) AS out_degree, \
                COUNT(DISTINCT e_in.id) + COUNT(DISTINCT e_out.id) AS degree \
             FROM nodes n \
             LEFT JOIN edges e_in  ON e_in.target  = n.id \
             LEFT JOIN edges e_out ON e_out.source = n.id \
             WHERE n.kind = '{}' \
             GROUP BY n.id \
             ORDER BY {} DESC \
             LIMIT {}",
            kind_str.replace('\'', "''"),
            order_col,
            limit
        )
    } else {
        format!(
            "SELECT \
                n.id, n.kind, n.name, n.qualified_name, n.file_path, n.language, \
                n.start_line, n.end_line, n.start_column, n.end_column, \
                n.is_exported, n.docstring, n.signature, n.decorators, \
                COUNT(DISTINCT e_in.id)  AS in_degree, \
                COUNT(DISTINCT e_out.id) AS out_degree, \
                COUNT(DISTINCT e_in.id) + COUNT(DISTINCT e_out.id) AS degree \
             FROM nodes n \
             LEFT JOIN edges e_in  ON e_in.target  = n.id \
             LEFT JOIN edges e_out ON e_out.source = n.id \
             GROUP BY n.id \
             ORDER BY {} DESC \
             LIMIT {}",
            order_col, limit
        )
    };

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        // row_to_node uses named column access — works with the aliased columns.
        // in_degree / out_degree are the AS-aliased aggregate columns.
        let node = row_to_node(row)?;
        let in_degree: i64 = row.get("in_degree")?;
        let out_degree: i64 = row.get("out_degree")?;
        Ok((node, in_degree as u64, out_degree as u64))
    })?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

pub fn get_all_node_names(conn: &Connection) -> Result<Vec<String>, CodeWikiError> {
    let mut stmt = conn.prepare_cached("SELECT DISTINCT name FROM nodes")?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

/// Fetch every node in the database in a single sequential scan.
/// Used by OPT-2 to build the in-memory name index once per resolution run.
pub fn get_all_nodes(conn: &Connection) -> Result<Vec<Node>, CodeWikiError> {
    let mut stmt = conn.prepare_cached("SELECT * FROM nodes")?;
    let rows = stmt.query_map([], row_to_node)?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

/// A lean node representation for the in-memory name index (Wave-3 OPT-12).
///
/// Stores only the fields consumed by the name matcher and resolution scorer,
/// skipping `docstring`, `signature`, and `metadata` — the three big `Option<String>`
/// fields that account for the majority of per-node heap cost.
///
/// `file_path` is stored as `Arc<str>` to allow cheap sharing across all nodes
/// in the same file without per-node String allocations.
#[derive(Debug, Clone)]
pub struct NodeRef {
    pub id: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: codewiki_core::NodeKind,
    pub language: codewiki_core::Language,
    /// Interned file path — shared across all nodes in the same source file.
    pub file_path: std::sync::Arc<str>,
    pub start_line: u32,
    pub is_exported: bool,
}

impl NodeRef {
    /// Convert to a minimal [`Node`] suitable for use by the name matcher and
    /// `make_name_match_edge`.  Heavy optional fields (`docstring`, `signature`,
    /// `metadata`) are left as `None`; line/col fields not used by the scorer
    /// are zeroed.
    #[inline]
    pub fn to_node(&self) -> Node {
        Node {
            id: self.id.clone(),
            name: self.name.clone(),
            qualified_name: self.qualified_name.clone(),
            kind: self.kind.clone(),
            language: self.language.clone(),
            file_path: self.file_path.to_string(),
            start_line: self.start_line,
            is_exported: self.is_exported,
            ..Default::default()
        }
    }
}

fn row_to_node_ref(row: &rusqlite::Row<'_>) -> Result<NodeRef, rusqlite::Error> {
    use codewiki_core::Language;
    let kind_str: String = row.get("kind")?;
    let lang_str: String = row.get("language")?;
    Ok(NodeRef {
        id: row.get("id")?,
        name: row.get("name")?,
        qualified_name: row.get("qualified_name")?,
        kind: serde_json::from_value(serde_json::Value::String(kind_str))
            .unwrap_or(codewiki_core::NodeKind::Unknown),
        language: serde_json::from_value(serde_json::Value::String(lang_str))
            .unwrap_or(Language::Unknown),
        // Temporary plain String — interning is applied in the caller.
        file_path: std::sync::Arc::from(row.get::<_, String>("file_path")?.as_str()),
        start_line: row.get::<_, i64>("start_line")? as u32,
        is_exported: row.get::<_, i64>("is_exported")? != 0,
    })
}

/// Fetch every node in the database, returning only the fields required by the
/// name-matcher (Wave-3 OPT-12).  Skips `docstring`, `signature`, `metadata`,
/// `end_line`, `start_column`, `end_column` — the fields that cause the bulk of
/// the per-node heap cost.  Caller is expected to intern `file_path` strings
/// across nodes in the same file.
pub fn get_all_node_refs(conn: &Connection) -> Result<Vec<NodeRef>, CodeWikiError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, kind, name, qualified_name, file_path, language, start_line, is_exported \
         FROM nodes",
    )?;
    let rows = stmt.query_map([], row_to_node_ref)?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open_in_memory;
    use codewiki_core::{Language, NodeKind};

    fn make_node(id: &str, name: &str, file: &str) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            qualified_name: name.to_string(),
            kind: NodeKind::Function,
            language: Language::TypeScript,
            file_path: file.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn insert_and_retrieve_by_id() {
        let conn = open_in_memory().unwrap();
        let node = make_node("abc", "myFunc", "src/index.ts");
        insert_node(&conn, &node).unwrap();
        let got = get_node_by_id(&conn, "abc").unwrap();
        assert!(got.is_some());
        assert_eq!(got.unwrap().name, "myFunc");
    }

    #[test]
    fn retrieve_by_name_and_file() {
        let conn = open_in_memory().unwrap();
        for i in 0..5 {
            let node = make_node(&format!("id{}", i), &format!("func{}", i), "src/foo.ts");
            insert_node(&conn, &node).unwrap();
        }
        let by_file = get_nodes_by_file(&conn, "src/foo.ts").unwrap();
        assert_eq!(by_file.len(), 5);
        let by_name = get_nodes_by_name(&conn, "func2").unwrap();
        assert_eq!(by_name.len(), 1);
    }

    #[test]
    fn batch_ids() {
        let conn = open_in_memory().unwrap();
        let ids: Vec<String> = (0..10).map(|i| format!("node{}", i)).collect();
        for id in &ids {
            let node = make_node(id, id, "src/x.ts");
            insert_node(&conn, &node).unwrap();
        }
        let got = get_nodes_by_ids(&conn, &ids).unwrap();
        assert_eq!(got.len(), 10);
    }

    #[test]
    fn get_top_nodes_by_degree_returns_ordered() {
        use super::DegreeMetric;
        use crate::queries::edges::insert_edge;
        use codewiki_core::{Edge, EdgeKind};

        let conn = open_in_memory().unwrap();
        for i in 0..3usize {
            let node = make_node(&format!("n{}", i), &format!("f{}", i), "src/x.ts");
            insert_node(&conn, &node).unwrap();
        }
        // n0 → n1 and n0 → n2 (n0 has out_degree 2, the others have 0)
        insert_edge(
            &conn,
            &Edge {
                id: "e1".into(),
                source_id: "n0".into(),
                target_id: "n1".into(),
                kind: EdgeKind::Calls,
                ..Default::default()
            },
        )
        .unwrap();
        insert_edge(
            &conn,
            &Edge {
                id: "e2".into(),
                source_id: "n0".into(),
                target_id: "n2".into(),
                kind: EdgeKind::Calls,
                ..Default::default()
            },
        )
        .unwrap();

        let rows = super::get_top_nodes_by_degree(&conn, DegreeMetric::Total, None, 5).unwrap();
        assert_eq!(rows.len(), 3);
        let (top_node, _in, _out) = &rows[0];
        // n0 has degree 2 (out) + 0 (in) = 2; n1/n2 have 1 each
        assert_eq!(top_node.id, "n0", "n0 should be first by total degree");
    }

    #[test]
    fn delete_by_file_removes_all() {
        let conn = open_in_memory().unwrap();
        for i in 0..3 {
            let node = make_node(&format!("n{}", i), &format!("f{}", i), "src/del.ts");
            insert_node(&conn, &node).unwrap();
        }
        delete_nodes_by_file(&conn, "src/del.ts").unwrap();
        let remaining = get_nodes_by_file(&conn, "src/del.ts").unwrap();
        assert!(remaining.is_empty());
    }
}

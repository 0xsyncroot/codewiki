/// Prepared-statement helpers for unresolved reference operations.
use codewiki_core::{CodeWikiError, UnresolvedRef};
use rusqlite::{Connection, params};

// ---------------------------------------------------------------------------
// Row → UnresolvedRef
// ---------------------------------------------------------------------------

pub fn row_to_unresolved_ref(row: &rusqlite::Row<'_>) -> Result<UnresolvedRef, rusqlite::Error> {
    let id: i64 = row.get("id")?;
    Ok(UnresolvedRef {
        id: id.to_string(),
        from_node_id: row.get("from_node_id")?,
        reference_name: row.get("reference_name")?,
        reference_kind: row.get("reference_kind")?,
        file_path: row.get("file_path")?,
        line: row.get::<_, Option<i64>>("line")?.map(|v| v as u32),
        col: row.get::<_, Option<i64>>("col")?.map(|v| v as u32),
        metadata: row.get("candidates")?,
    })
}

// ---------------------------------------------------------------------------
// INSERT
// ---------------------------------------------------------------------------

pub fn insert_unresolved_ref(
    conn: &Connection,
    uref: &UnresolvedRef,
) -> Result<(), CodeWikiError> {
    let mut stmt = conn.prepare_cached(
        r#"INSERT INTO unresolved_refs
            (from_node_id, reference_name, reference_kind, line, col, candidates, file_path, language)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'unknown')"#,
    )?;
    stmt.execute(params![
        uref.from_node_id,
        uref.reference_name,
        uref.reference_kind,
        uref.line.unwrap_or(0) as i64,
        uref.col.unwrap_or(0) as i64,
        uref.metadata,
        uref.file_path,
    ])?;
    Ok(())
}

pub fn insert_unresolved_refs_batch(
    conn: &Connection,
    refs: &[UnresolvedRef],
) -> Result<(), CodeWikiError> {
    for uref in refs {
        insert_unresolved_ref(conn, uref)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// DELETE
// ---------------------------------------------------------------------------

pub fn delete_unresolved_by_node(
    conn: &Connection,
    from_node_id: &str,
) -> Result<(), CodeWikiError> {
    conn.execute(
        "DELETE FROM unresolved_refs WHERE from_node_id = ?1",
        params![from_node_id],
    )?;
    Ok(())
}

/// Delete all unresolved refs originating from nodes in the given file.
pub fn delete_unresolved_refs_for_file(
    conn: &Connection,
    file_path: &str,
) -> Result<(), CodeWikiError> {
    conn.execute(
        "DELETE FROM unresolved_refs WHERE file_path = ?1",
        params![file_path],
    )?;
    Ok(())
}

pub fn clear_unresolved_refs(conn: &Connection) -> Result<(), CodeWikiError> {
    conn.execute_batch("DELETE FROM unresolved_refs")?;
    Ok(())
}

/// Delete specific resolved refs by (from_node_id, reference_name, reference_kind) tuple.
pub fn delete_specific_resolved_refs(
    conn: &Connection,
    refs: &[(&str, &str, &str)],
) -> Result<(), CodeWikiError> {
    let mut stmt = conn.prepare_cached(
        "DELETE FROM unresolved_refs WHERE from_node_id = ?1 AND reference_name = ?2 AND reference_kind = ?3",
    )?;
    for (from, name, kind) in refs {
        stmt.execute(params![from, name, kind])?;
    }
    Ok(())
}

/// OPT-14: Bulk-delete resolved refs by their row `id`.
///
/// Uses `DELETE FROM unresolved_refs WHERE id IN (…)` chunked at `chunk_size`
/// so SQLite can use the PRIMARY KEY index instead of a 3-column scan per row.
/// This reduces 114k individual 3-tuple DELETEs (django) to ~228 bulk statements.
///
/// Falls back to `delete_specific_resolved_refs` for any ref with `id == 0`
/// (id was not recorded).
pub fn delete_resolved_refs_by_ids(
    conn: &Connection,
    ids: &[i64],
    fallback_refs: &[(&str, &str, &str)],
) -> Result<(), CodeWikiError> {
    const CHUNK_SIZE: usize = 500;

    // Bulk-delete by PK id (fast path — covers refs where id was recorded).
    for chunk in ids.chunks(CHUNK_SIZE) {
        if chunk.is_empty() {
            continue;
        }
        let placeholders = chunk
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("DELETE FROM unresolved_refs WHERE id IN ({})", placeholders);
        let mut stmt = conn.prepare_cached(&sql)?;
        stmt.execute(rusqlite::params_from_iter(chunk.iter()))?;
    }

    // Fallback: delete any refs whose id was 0 (not recorded) using the
    // legacy 3-tuple match.  In practice this should be empty for a
    // correctly-populated ResolvedFromRef, but we keep it as a safety net.
    if !fallback_refs.is_empty() {
        delete_specific_resolved_refs(conn, fallback_refs)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// SELECT
// ---------------------------------------------------------------------------

pub fn get_unresolved_by_name(
    conn: &Connection,
    name: &str,
) -> Result<Vec<UnresolvedRef>, CodeWikiError> {
    let mut stmt =
        conn.prepare_cached("SELECT * FROM unresolved_refs WHERE reference_name = ?1")?;
    let rows = stmt.query_map(params![name], row_to_unresolved_ref)?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

pub fn get_unresolved_references(conn: &Connection) -> Result<Vec<UnresolvedRef>, CodeWikiError> {
    let mut stmt = conn.prepare_cached("SELECT * FROM unresolved_refs")?;
    let rows = stmt.query_map([], row_to_unresolved_ref)?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

pub fn get_unresolved_count(conn: &Connection) -> Result<usize, CodeWikiError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM unresolved_refs",
        [],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// Paginated batch — OFFSET-based (kept for incremental path + tests).
pub fn get_unresolved_batch(
    conn: &Connection,
    limit: usize,
    offset: usize,
) -> Result<Vec<UnresolvedRef>, CodeWikiError> {
    let mut stmt =
        conn.prepare_cached("SELECT * FROM unresolved_refs LIMIT ?1 OFFSET ?2")?;
    let rows = stmt.query_map(params![limit as i64, offset as i64], |row| {
        row_to_unresolved_ref(row)
    })?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

/// W6 cursor-based fetch: `WHERE id > last_id ORDER BY id LIMIT ?`.
///
/// Replaces the `OFFSET` scan for the full-index upfront-fetch path in
/// `run_until_empty_parallel`.  SQLite can satisfy this with an O(log n)
/// B-tree seek on the INTEGER PRIMARY KEY rather than scanning from row 0
/// each page — eliminating the O(offset) scan that grows to O(n²) at 100k.
///
/// Pass `after_id = 0` to start from the beginning.  Returns rows in
/// ascending `id` order; the caller records the last `id` seen and passes
/// it back as `after_id` for the next page.
pub fn get_unresolved_batch_after(
    conn: &Connection,
    after_id: i64,
    limit: usize,
) -> Result<Vec<UnresolvedRef>, CodeWikiError> {
    let mut stmt = conn.prepare_cached(
        "SELECT * FROM unresolved_refs WHERE id > ?1 ORDER BY id LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![after_id, limit as i64], |row| {
        row_to_unresolved_ref(row)
    })?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

pub fn get_unresolved_by_files(
    conn: &Connection,
    file_paths: &[String],
) -> Result<Vec<UnresolvedRef>, CodeWikiError> {
    if file_paths.is_empty() {
        return Ok(vec![]);
    }
    let placeholders = file_paths
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT * FROM unresolved_refs WHERE file_path IN ({})",
        placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(file_paths.iter()),
        row_to_unresolved_ref,
    )?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
}

/// Return all unresolved refs whose `reference_name` matches any of the
/// provided symbol names.  Uses `idx_unresolved_name`.  Intended for OPT-9:
/// find previously-unresolvable refs that may now resolve because the changed
/// files introduced symbols with these names.
pub fn get_unresolved_by_names(
    conn: &Connection,
    names: &[String],
) -> Result<Vec<UnresolvedRef>, CodeWikiError> {
    if names.is_empty() {
        return Ok(vec![]);
    }
    let placeholders = names
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT * FROM unresolved_refs WHERE reference_name IN ({})",
        placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(names.iter()),
        row_to_unresolved_ref,
    )?;
    rows.map(|r| r.map_err(CodeWikiError::Sqlite))
        .collect::<Result<Vec<_>, _>>()
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

    fn make_ref(from: &str, name: &str) -> UnresolvedRef {
        UnresolvedRef {
            id: String::new(),
            from_node_id: from.to_string(),
            reference_name: name.to_string(),
            reference_kind: "calls".to_string(),
            file_path: "src/x.ts".to_string(),
            line: Some(10),
            col: Some(5),
            metadata: None,
        }
    }

    #[test]
    fn insert_paginate_clear() {
        let conn = open_in_memory().unwrap();
        insert_node(&conn, &make_node("n1")).unwrap();

        for i in 0..100 {
            let r = make_ref("n1", &format!("ref{}", i));
            insert_unresolved_ref(&conn, &r).unwrap();
        }

        let count = get_unresolved_count(&conn).unwrap();
        assert_eq!(count, 100);

        let page1 = get_unresolved_batch(&conn, 10, 0).unwrap();
        assert_eq!(page1.len(), 10);

        let page2 = get_unresolved_batch(&conn, 10, 10).unwrap();
        assert_eq!(page2.len(), 10);
        assert_ne!(page1[0].reference_name, page2[0].reference_name);

        clear_unresolved_refs(&conn).unwrap();
        let count2 = get_unresolved_count(&conn).unwrap();
        assert_eq!(count2, 0);
    }

    /// W6: cursor pagination collects all rows in the correct order with no
    /// duplicates and no gaps, regardless of deletes in between pages.
    #[test]
    fn cursor_pagination_collects_all_rows() {
        let conn = open_in_memory().unwrap();
        insert_node(&conn, &make_node("n1")).unwrap();

        // Insert 100 refs.
        for i in 0..100 {
            let r = make_ref("n1", &format!("cref{}", i));
            insert_unresolved_ref(&conn, &r).unwrap();
        }

        // Collect via cursor pagination with page size 17 (not a factor of 100).
        let mut all_collected: Vec<String> = Vec::new();
        let mut last_id: i64 = 0;
        loop {
            let page = get_unresolved_batch_after(&conn, last_id, 17).unwrap();
            if page.is_empty() {
                break;
            }
            last_id = page.last().unwrap().id.parse::<i64>().unwrap();
            all_collected.extend(page.into_iter().map(|r| r.reference_name));
        }

        assert_eq!(
            all_collected.len(),
            100,
            "cursor pagination must collect all 100 rows"
        );

        // Ids must be strictly increasing (ORDER BY id guarantees this).
        let ids_seen: Vec<i64> = {
            let mut ids = Vec::new();
            let mut cur = 0i64;
            loop {
                let page = get_unresolved_batch_after(&conn, cur, 100).unwrap();
                if page.is_empty() { break; }
                for r in &page {
                    let id = r.id.parse::<i64>().unwrap();
                    assert!(id > cur, "ids must be strictly increasing");
                    ids.push(id);
                    cur = id;
                }
                if page.len() < 100 { break; }
            }
            ids
        };
        assert_eq!(ids_seen.len(), 100);
    }
}

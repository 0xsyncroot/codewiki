pub mod query_parser;
pub mod scoring;

pub use query_parser::{bounded_edit_distance, normalize_for_fts, parse_query, ParsedQuery};
pub use scoring::{
    extract_search_terms, extract_symbols_from_query, get_stem_variants, is_test_file, kind_bonus,
    name_match_bonus, score_path_relevance, HIGH_VALUE_NODE_KINDS,
};

use crate::queries::nodes::{get_all_node_names, row_to_node};
use crate::traits::query::SearchOptions;
use codewiki_core::{CodeWikiError, SearchResult};
use rusqlite::Connection;

/// FTS5 search with BM25 weights (0,20,5,1,2) and 5× over-fetch.
/// Falls back to LIKE if FTS returns 0 results.
/// Falls back to fuzzy edit-distance if LIKE also returns 0.
pub fn search_nodes_fts(
    conn: &Connection,
    query: &str,
    opts: &SearchOptions,
) -> Result<Vec<SearchResult>, CodeWikiError> {
    use query_parser::parse_query;

    let parsed = parse_query(query);

    // Merge kind/language filters
    let kinds = if !parsed.kinds.is_empty() {
        let mut merged = opts.kinds.clone().unwrap_or_default();
        for k in &parsed.kinds {
            if !merged.contains(k) {
                merged.push(k.clone());
            }
        }
        if merged.is_empty() {
            None
        } else {
            Some(merged)
        }
    } else {
        opts.kinds.clone()
    };

    let languages = if !parsed.languages.is_empty() {
        let mut merged = opts.languages.clone().unwrap_or_default();
        for l in &parsed.languages {
            if !merged.contains(l) {
                merged.push(l.clone());
            }
        }
        if merged.is_empty() {
            None
        } else {
            Some(merged)
        }
    } else {
        opts.languages.clone()
    };

    let text = parsed.text.clone();
    let limit = if opts.limit == 0 { 100 } else { opts.limit };

    let mut results = if !text.is_empty() {
        search_fts_raw(conn, &text, kinds.as_deref(), languages.as_deref(), limit)?
    } else {
        search_all_by_filters(conn, kinds.as_deref(), languages.as_deref(), limit * 5)?
    };

    // LIKE fallback
    if results.is_empty() && text.len() >= 2 {
        results = search_nodes_like(conn, &text, kinds.as_deref(), languages.as_deref(), limit)?;
    }

    // Fuzzy fallback
    if results.is_empty() && text.len() >= 3 {
        results = search_nodes_fuzzy(conn, &text, kinds.as_deref(), languages.as_deref(), limit)?;
    }

    // Supplement: add exact name matches not in FTS results
    if !results.is_empty() && !query.is_empty() {
        let existing_ids: std::collections::HashSet<String> =
            results.iter().map(|r| r.node.id.clone()).collect();
        let max_score = results.iter().map(|r| r.score).fold(0.0f64, f64::max);
        let terms: Vec<&str> = query.split_whitespace().filter(|t| t.len() >= 2).collect();
        for term in terms {
            let mut sql = "SELECT * FROM nodes WHERE name = ? COLLATE NOCASE".to_string();
            let mut param_vals: Vec<String> = vec![term.to_string()];
            if let Some(ref ks) = kinds {
                if !ks.is_empty() {
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
                            .map(|(i, _)| format!("?{}", param_vals.len() + i + 1))
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                    param_vals.extend(kind_strs);
                }
            }
            if let Some(ref ls) = languages {
                if !ls.is_empty() {
                    let lang_strs: Vec<String> = ls
                        .iter()
                        .map(|l| {
                            serde_json::to_value(l)
                                .ok()
                                .and_then(|v| v.as_str().map(String::from))
                                .unwrap_or_default()
                        })
                        .collect();
                    sql += &format!(
                        " AND language IN ({})",
                        lang_strs
                            .iter()
                            .enumerate()
                            .map(|(i, _)| format!("?{}", param_vals.len() + i + 1))
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                    param_vals.extend(lang_strs);
                }
            }
            sql += " LIMIT 20";
            let mut stmt = conn.prepare(&sql)?;
            let rows =
                stmt.query_map(rusqlite::params_from_iter(param_vals.iter()), row_to_node)?;
            for row in rows {
                let node = row.map_err(CodeWikiError::Sqlite)?;
                if !existing_ids.contains(&node.id) {
                    results.push(SearchResult {
                        node,
                        score: max_score,
                        snippet: None,
                    });
                }
            }
        }
    }

    // Post-scoring
    if !results.is_empty() && !query.is_empty() {
        let scoring_query = if !text.is_empty() {
            text.as_str()
        } else {
            query
        };
        for r in &mut results {
            r.score += kind_bonus(&r.node.kind) as f64
                + score_path_relevance(&r.node.file_path, scoring_query) as f64
                + name_match_bonus(&r.node.name, scoring_query) as f64;
        }
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
    }

    // Apply path: and name: hard filters
    if !parsed.path_filters.is_empty() {
        let lowered: Vec<String> = parsed
            .path_filters
            .iter()
            .map(|p| p.to_lowercase())
            .collect();
        results.retain(|r| {
            let fp = r.node.file_path.to_lowercase();
            lowered.iter().any(|p| fp.contains(p.as_str()))
        });
    }
    if !parsed.name_filters.is_empty() {
        let lowered: Vec<String> = parsed
            .name_filters
            .iter()
            .map(|n| n.to_lowercase())
            .collect();
        results.retain(|r| {
            let nm = r.node.name.to_lowercase();
            lowered.iter().any(|n| nm.contains(n.as_str()))
        });
    }

    Ok(results)
}

fn fts_query_string(text: &str) -> String {
    let prepared = text
        .replace("::", " ") // Rust/C++/Ruby scope operator
        .replace(|c: char| "\"*():^".contains(c), "")
        .split_whitespace()
        .filter(|t| !matches!(*t, "AND" | "OR" | "NOT" | "NEAR"))
        .map(|t| format!("\"{}\"*", t))
        .collect::<Vec<_>>()
        .join(" OR ");
    prepared
}

fn search_fts_raw(
    conn: &Connection,
    text: &str,
    kinds: Option<&[codewiki_core::NodeKind]>,
    languages: Option<&[codewiki_core::Language]>,
    limit: usize,
) -> Result<Vec<SearchResult>, CodeWikiError> {
    let fts_query = fts_query_string(text);
    if fts_query.is_empty() {
        return Ok(vec![]);
    }

    let fts_limit = std::cmp::max(limit * 5, 100);

    let mut sql = r#"SELECT nodes.*, bm25(nodes_fts, 0, 20, 5, 1, 2) as _score
        FROM nodes_fts
        JOIN nodes ON nodes_fts.id = nodes.id
        WHERE nodes_fts MATCH ?"#
        .to_string();

    let mut params: Vec<String> = vec![fts_query];

    if let Some(ks) = kinds {
        if !ks.is_empty() {
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
                " AND nodes.kind IN ({})",
                kind_strs
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("?{}", i + 2))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            params.extend(kind_strs);
        }
    }
    if let Some(ls) = languages {
        if !ls.is_empty() {
            let n = params.len();
            let lang_strs: Vec<String> = ls
                .iter()
                .map(|l| {
                    serde_json::to_value(l)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_default()
                })
                .collect();
            sql += &format!(
                " AND nodes.language IN ({})",
                lang_strs
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("?{}", n + i + 1))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            params.extend(lang_strs);
        }
    }
    sql += &format!(" ORDER BY _score LIMIT {}", fts_limit);

    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return Ok(vec![]),
    };
    let rows = match stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
        let node = row_to_node(row)?;
        let score: f64 = row.get("_score")?;
        Ok((node, score))
    }) {
        Ok(rows) => rows,
        Err(_) => return Ok(vec![]),
    };

    let mut results = Vec::new();
    for row in rows {
        let (node, score) = row.map_err(CodeWikiError::Sqlite)?;
        results.push(SearchResult {
            node,
            score: score.abs(),
            snippet: None,
        });
    }
    Ok(results)
}

fn search_nodes_like(
    conn: &Connection,
    text: &str,
    kinds: Option<&[codewiki_core::NodeKind]>,
    _languages: Option<&[codewiki_core::Language]>,
    limit: usize,
) -> Result<Vec<SearchResult>, CodeWikiError> {
    let contains = format!("%{}%", text);
    let starts = format!("{}%", text);

    let mut sql = r#"SELECT nodes.*,
        CASE
          WHEN name = ? THEN 1.0
          WHEN name LIKE ? THEN 0.9
          WHEN name LIKE ? THEN 0.8
          WHEN qualified_name LIKE ? THEN 0.7
          ELSE 0.5
        END as _score
        FROM nodes
        WHERE (name LIKE ? OR qualified_name LIKE ? OR name LIKE ?)"#
        .to_string();

    let mut params: Vec<String> = vec![
        text.to_string(),
        starts.clone(),
        contains.clone(),
        contains.clone(),
        contains.clone(),
        contains.clone(),
        starts.clone(),
    ];

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
    sql += &format!(" ORDER BY _score DESC, length(name) ASC LIMIT {}", limit);

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
        let node = row_to_node(row)?;
        let score: f64 = row.get("_score")?;
        Ok((node, score))
    })?;

    let mut results = Vec::new();
    for row in rows {
        let (node, score) = row.map_err(CodeWikiError::Sqlite)?;
        results.push(SearchResult {
            node,
            score,
            snippet: None,
        });
    }
    Ok(results)
}

fn search_nodes_fuzzy(
    conn: &Connection,
    text: &str,
    kinds: Option<&[codewiki_core::NodeKind]>,
    _languages: Option<&[codewiki_core::Language]>,
    limit: usize,
) -> Result<Vec<SearchResult>, CodeWikiError> {
    let lowered = text.to_lowercase();
    let max_dist = if lowered.len() <= 4 { 1 } else { 2 };

    let all_names = get_all_node_names(conn)?;
    let mut candidates: Vec<(String, usize)> = Vec::new();
    for name in &all_names {
        let dist = bounded_edit_distance(&name.to_lowercase(), &lowered, max_dist);
        if dist <= max_dist {
            candidates.push((name.clone(), dist));
        }
    }
    candidates.sort_by_key(|(_, d)| *d);

    let followup_cap = std::cmp::max(limit * 2, 50);
    let capped = candidates
        .into_iter()
        .take(followup_cap)
        .collect::<Vec<_>>();

    let mut results: Vec<SearchResult> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (name, dist) in capped {
        if results.len() >= limit {
            break;
        }
        let mut sql = "SELECT * FROM nodes WHERE name = ?".to_string();
        let mut params: Vec<String> = vec![name.clone()];
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
        sql += " LIMIT 5";
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), row_to_node)?;
        for row in rows {
            let node = row.map_err(CodeWikiError::Sqlite)?;
            if seen.contains(&node.id) {
                continue;
            }
            seen.insert(node.id.clone());
            let score = 1.0 / (1.0 + dist as f64);
            results.push(SearchResult {
                node,
                score,
                snippet: None,
            });
            if results.len() >= limit {
                break;
            }
        }
    }
    Ok(results)
}

fn search_all_by_filters(
    conn: &Connection,
    kinds: Option<&[codewiki_core::NodeKind]>,
    languages: Option<&[codewiki_core::Language]>,
    limit: usize,
) -> Result<Vec<SearchResult>, CodeWikiError> {
    let mut sql = "SELECT * FROM nodes WHERE 1=1".to_string();
    let mut params: Vec<String> = Vec::new();
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
    if let Some(ls) = languages {
        if !ls.is_empty() {
            let n = params.len();
            let lang_strs: Vec<String> = ls
                .iter()
                .map(|l| {
                    serde_json::to_value(l)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_default()
                })
                .collect();
            sql += &format!(
                " AND language IN ({})",
                lang_strs
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("?{}", n + i + 1))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            params.extend(lang_strs);
        }
    }
    sql += &format!(" ORDER BY name LIMIT {}", limit);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), row_to_node)?;
    let mut results = Vec::new();
    for row in rows {
        let node = row.map_err(CodeWikiError::Sqlite)?;
        results.push(SearchResult {
            node,
            score: 1.0,
            snippet: None,
        });
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open_in_memory;
    use crate::queries::nodes::insert_node;
    use crate::traits::query::SearchOptions;
    use codewiki_core::{Language, Node, NodeKind};

    fn make_node(id: &str, name: &str) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            qualified_name: name.to_string(),
            kind: NodeKind::Function,
            language: Language::TypeScript,
            file_path: "src/x.ts".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn fts_english_ascii_unaffected() {
        let conn = open_in_memory().unwrap();
        insert_node(&conn, &make_node("e1", "TransportService")).unwrap();
        let opts = SearchOptions {
            limit: 10,
            ..Default::default()
        };
        let results = search_nodes_fts(&conn, "TransportService", &opts).unwrap();
        assert!(!results.is_empty(), "ASCII search must still work");
        assert_eq!(results[0].node.id, "e1");
    }

    #[test]
    fn fts_vn_unaccented_matches_accented() {
        let conn = open_in_memory().unwrap();
        // Insert Vietnamese name — stored normalized (nguoi_dung) due to insert_node normalization
        insert_node(&conn, &make_node("vn1", "người_dùng")).unwrap();
        let opts = SearchOptions {
            limit: 10,
            ..Default::default()
        };
        // Query unaccented — parse_query also normalizes → "nguoi_dung"
        let results = search_nodes_fts(&conn, "nguoi_dung", &opts).unwrap();
        assert!(
            !results.is_empty(),
            "unaccented query must match accented stored name"
        );
    }

    #[test]
    fn fts_vn_d_stroke_preserves_raw_name() {
        // The canonical `nodes` columns must store the RAW identifier verbatim.
        // Previously `insert_node` folded the non-decomposable `đ` stroke
        // (U+0111 → 'd') at write time, corrupting the stored name to
        // `dang_nhap` and breaking edge resolution that matches on raw symbol
        // names. After the fix, the raw `đăng_nhập` is preserved exactly.
        //
        // Note: the FTS5 `unicode61 remove_diacritics 2` tokenizer strips
        // decomposable combining marks but does NOT fold the `đ` stroke, so
        // `đ`-stroke identifiers are not reachable via the ASCII form through
        // FTS — that is an accepted tradeoff of keeping canonical names intact.
        let conn = open_in_memory().unwrap();
        insert_node(&conn, &make_node("vn2", "đăng_nhập")).unwrap();
        let stored: String = conn
            .query_row("SELECT name FROM nodes WHERE id = ?1", ["vn2"], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            stored, "đăng_nhập",
            "canonical name column must store the raw đ-stroke identifier verbatim"
        );
    }

    #[test]
    fn fts_vn_docstring_searchable() {
        let conn = open_in_memory().unwrap();
        let mut node = make_node("vn3", "getOrders");
        node.docstring = Some("Trả về danh sách đơn hàng".to_string());
        insert_node(&conn, &node).unwrap();
        let opts = SearchOptions {
            limit: 10,
            ..Default::default()
        };
        // Search for normalized form
        let results = search_nodes_fts(&conn, "don hang", &opts).unwrap();
        assert!(
            !results.is_empty(),
            "Vietnamese docstring must be searchable via unaccented form"
        );
    }

    #[test]
    fn fts_finds_exact_name() {
        let conn = open_in_memory().unwrap();
        insert_node(&conn, &make_node("a1", "TransportService")).unwrap();
        insert_node(&conn, &make_node("a2", "OtherService")).unwrap();
        insert_node(&conn, &make_node("a3", "TransportFactory")).unwrap();

        let opts = SearchOptions {
            limit: 10,
            ..Default::default()
        };
        let results = search_nodes_fts(&conn, "TransportService", &opts).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].node.id, "a1");
    }

    #[test]
    fn like_fallback_works() {
        let conn = open_in_memory().unwrap();
        insert_node(&conn, &make_node("b1", "mySpecialFunc")).unwrap();

        let opts = SearchOptions {
            limit: 10,
            ..Default::default()
        };
        // Use a query that FTS won't match directly but LIKE will
        let results = search_nodes_fts(&conn, "SpecialFunc", &opts).unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn language_filter_applies_to_supplement_branch() {
        // The supplement branch (exact-name match not in FTS) must also respect
        // the language filter so that `lang:type_script getUser` never returns
        // Python nodes.
        let conn = open_in_memory().unwrap();

        // Insert getUser for TypeScript
        let mut ts_node = make_node("ts1", "getUser");
        ts_node.language = Language::TypeScript;
        insert_node(&conn, &ts_node).unwrap();

        // Insert getUser for Python — should NOT appear in TypeScript-filtered results
        let mut py_node = make_node("py1", "getUser");
        py_node.language = Language::Python;
        py_node.file_path = "src/service.py".to_string();
        insert_node(&conn, &py_node).unwrap();

        let opts = SearchOptions {
            limit: 20,
            languages: Some(vec![Language::TypeScript]),
            ..Default::default()
        };
        // Use a query term that will hit FTS and trigger the supplement path.
        let results = search_nodes_fts(&conn, "getUser", &opts).unwrap();

        for r in &results {
            assert_eq!(
                r.node.language,
                Language::TypeScript,
                "Python node leaked through language filter: {:?}",
                r.node
            );
        }
        assert!(
            !results.is_empty(),
            "expected at least the TypeScript getUser"
        );
    }
}

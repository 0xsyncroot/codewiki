//! EP-3: GET /api/node/:id
//!
//! Returns node metadata + inline source code + degree counts.
//! FLAG-4: MutexGuard + GraphTraverser constructed INSIDE spawn_blocking.

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;
use tokio::task::spawn_blocking;

use super::AppState;
use codewiki_storage::queries::{edges as eq, nodes as nq};

#[derive(Serialize)]
pub struct NodeResponse {
    pub node: codewiki_core::Node,
    pub code: Option<String>,
    pub caller_count: usize,
    pub callee_count: usize,
    pub in_degree: u64,
    pub out_degree: u64,
}

pub async fn handle(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let db = state.db.clone();
    let result = spawn_blocking(move || {
        let conn = db.lock().map_err(|_| anyhow::anyhow!("mutex poisoned"))?;

        let node = nq::get_node_by_id(&conn, &id).map_err(|e| anyhow::anyhow!("{e}"))?;
        let node = match node {
            Some(n) => n,
            None => return Err(anyhow::anyhow!("NOT_FOUND")),
        };

        // Read source code inline
        let code = read_code_for_node(&node);

        // Caller count (calls edges only — incoming)
        let callers = eq::get_incoming_edges(&conn, &id, Some(&[codewiki_core::EdgeKind::Calls]))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let caller_count = callers.len();

        // Callee count (calls edges only — outgoing)
        let callees = eq::get_outgoing_edges(&conn, &id, Some(&[codewiki_core::EdgeKind::Calls]))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let callee_count = callees.len();

        // Degree (all edge kinds)
        let (in_degree, out_degree) = codewiki_storage::queries::edges::get_node_degree(&conn, &id)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        Ok(NodeResponse {
            node,
            code,
            caller_count,
            callee_count,
            in_degree,
            out_degree,
        })
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking panic: {e}")));

    match result {
        Ok(resp) => Json(resp).into_response(),
        Err(e) if e.to_string() == "NOT_FOUND" => {
            super::ApiError::not_found("node not found").into_response()
        }
        Err(e) => super::ApiError::internal(e.to_string()).into_response(),
    }
}

fn read_code_for_node(node: &codewiki_core::Node) -> Option<String> {
    let path = std::path::PathBuf::from(&node.file_path);
    let content = std::fs::read_to_string(&path).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    let start = (node.start_line as usize).saturating_sub(1);
    let end = (node.end_line as usize).min(lines.len());
    if start >= lines.len() {
        return None;
    }
    Some(lines[start..end].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request;
    use axum::Router;
    use codewiki_core::{Edge, EdgeKind, Language, Node, NodeKind};
    use codewiki_storage::connection::open_in_memory;
    use codewiki_storage::queries::edges::insert_edge;
    use codewiki_storage::queries::nodes::insert_node;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    #[tokio::test]
    async fn node_returns_degree_fields() {
        let conn = open_in_memory().unwrap();
        let node_a = Node {
            id: "a".into(),
            name: "funcA".into(),
            qualified_name: "funcA".into(),
            kind: NodeKind::Function,
            language: Language::Rust,
            file_path: "src/a.rs".into(),
            ..Default::default()
        };
        let node_b = Node {
            id: "b".into(),
            name: "funcB".into(),
            qualified_name: "funcB".into(),
            kind: NodeKind::Function,
            language: Language::Rust,
            file_path: "src/b.rs".into(),
            ..Default::default()
        };
        insert_node(&conn, &node_a).unwrap();
        insert_node(&conn, &node_b).unwrap();
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

        let db = Arc::new(Mutex::new(conn));
        let state = AppState { db, max_nodes: 200 };
        let app = Router::new()
            .route("/api/node/{id}", axum::routing::get(handle))
            .with_state(state);

        let req = Request::builder()
            .uri("/api/node/a")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.get("in_degree").is_some());
        assert!(v.get("out_degree").is_some());
        assert_eq!(v["out_degree"], 1);
        assert_eq!(v["in_degree"], 0);
    }
}

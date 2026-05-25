//! EP-4: GET /api/neighborhood/:id
//!
//! Primary canvas endpoint — BFS neighborhood around a node.
//! FLAG-4: MutexGuard + GraphTraverser constructed INSIDE spawn_blocking.

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use std::collections::HashSet;
use tokio::task::spawn_blocking;

use super::{build_subgraph_response, AppState};
use codewiki_core::{EdgeKind, NodeKind};
use codewiki_storage::graph::traversal::{GraphTraverser, TraversalDirection, TraversalOptions};

#[derive(Deserialize)]
pub struct NeighborhoodParams {
    pub depth: Option<usize>,
    pub edge_kinds: Option<String>,
    pub node_kinds: Option<String>,
    pub direction: Option<String>,
    pub limit: Option<usize>,
    pub exclude: Option<String>,
}

pub async fn handle(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<NeighborhoodParams>,
) -> impl IntoResponse {
    let depth = params.depth.unwrap_or(1).min(4);
    let limit = params.limit.unwrap_or(200).min(500);
    let max_nodes = state.max_nodes;
    let effective_limit = limit.min(max_nodes);

    let edge_kinds = parse_edge_kinds(params.edge_kinds.as_deref());
    let node_kinds = parse_node_kinds(params.node_kinds.as_deref());
    let direction = parse_direction(params.direction.as_deref());
    let exclude = parse_exclude(params.exclude.as_deref());

    let db = state.db.clone();
    let result = spawn_blocking(move || {
        // FLAG-4: acquire MutexGuard and construct GraphTraverser INSIDE the closure
        let conn = db.lock().map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
        let traverser = GraphTraverser::new(&conn);
        let opts = TraversalOptions {
            max_depth: depth,
            edge_kinds,
            direction,
            limit: effective_limit + 1, // +1 to detect truncation
            include_start: true,
        };
        traverser
            .traverse_bfs(&id, &opts)
            .map_err(|e| anyhow::anyhow!("{e}"))
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking panic: {e}")));

    match result {
        Ok(subgraph) => {
            let node_kinds_filter = if node_kinds.is_empty() {
                None
            } else {
                Some(node_kinds.as_slice())
            };
            let resp =
                build_subgraph_response(subgraph, effective_limit, &exclude, node_kinds_filter);
            Json(resp).into_response()
        }
        Err(e) => super::ApiError::internal(e.to_string()).into_response(),
    }
}

fn parse_edge_kinds(s: Option<&str>) -> Vec<EdgeKind> {
    let Some(s) = s else { return vec![] };
    s.split(',')
        .filter_map(|k| {
            serde_json::from_value(serde_json::Value::String(k.trim().to_string())).ok()
        })
        .collect()
}

fn parse_node_kinds(s: Option<&str>) -> Vec<NodeKind> {
    let Some(s) = s else { return vec![] };
    s.split(',')
        .filter_map(|k| {
            serde_json::from_value(serde_json::Value::String(k.trim().to_string())).ok()
        })
        .collect()
}

fn parse_direction(s: Option<&str>) -> TraversalDirection {
    match s {
        Some("outgoing") | Some("out") => TraversalDirection::Outgoing,
        Some("incoming") | Some("in") => TraversalDirection::Incoming,
        _ => TraversalDirection::Both,
    }
}

fn parse_exclude(s: Option<&str>) -> HashSet<String> {
    let Some(s) = s else { return HashSet::new() };
    s.split(',')
        .filter(|id| !id.trim().is_empty())
        .take(500)
        .map(|id| id.trim().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request;
    use axum::Router;
    use codewiki_core::{Language, Node, NodeKind};
    use codewiki_storage::connection::open_in_memory;
    use codewiki_storage::queries::nodes::insert_node;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    #[tokio::test]
    async fn neighborhood_returns_truncated_field() {
        let conn = open_in_memory().unwrap();
        let node = Node {
            id: "root".into(),
            name: "root".into(),
            qualified_name: "root".into(),
            kind: NodeKind::Function,
            language: Language::Rust,
            file_path: "src/root.rs".into(),
            ..Default::default()
        };
        insert_node(&conn, &node).unwrap();

        let db = Arc::new(Mutex::new(conn));
        let state = AppState { db, max_nodes: 200 };
        let app = Router::new()
            .route(
                "/api/neighborhood/{id}",
                axum::routing::get(handle),
            )
            .with_state(state);

        let req = Request::builder()
            .uri("/api/neighborhood/root?depth=1")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.get("truncated").is_some());
        let node_count = v["node_count"].as_u64().unwrap_or(0);
        assert!(node_count <= 200);
    }
}

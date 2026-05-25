//! EP-8: GET /api/file-graph
//!
//! Subgraph of all nodes in a file plus their neighborhood.
//! Composes get_nodes_by_file + traverse_bfs — no new storage query.
//! FLAG-4: MutexGuard + GraphTraverser constructed INSIDE spawn_blocking.

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use tokio::task::spawn_blocking;

use super::{build_subgraph_response, AppState};
use codewiki_core::{Node, Subgraph};
use codewiki_storage::graph::traversal::{GraphTraverser, TraversalDirection, TraversalOptions};
use codewiki_storage::queries::nodes as nq;

#[derive(Deserialize)]
pub struct FileGraphParams {
    pub path: Option<String>,
    pub limit: Option<usize>,
}

pub async fn handle(
    State(state): State<AppState>,
    Query(params): Query<FileGraphParams>,
) -> impl IntoResponse {
    let path = match params.path {
        Some(p) if !p.trim().is_empty() => p,
        _ => {
            return super::ApiError::bad_request("query parameter `path` is required")
                .into_response()
        }
    };
    let limit = params.limit.unwrap_or(200).min(500);
    let max_nodes = state.max_nodes;
    let effective_limit = limit.min(max_nodes);

    let db = state.db.clone();
    let result = spawn_blocking(move || {
        // FLAG-4: acquire MutexGuard and construct GraphTraverser INSIDE the closure
        let conn = db.lock().map_err(|_| anyhow::anyhow!("mutex poisoned"))?;

        // Get all nodes in the file
        let file_nodes = nq::get_nodes_by_file(&conn, &path)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        if file_nodes.is_empty() {
            return Ok(Subgraph {
                nodes: HashMap::new(),
                edges: vec![],
                roots: vec![],
            });
        }

        let traverser = GraphTraverser::new(&conn);
        let opts = TraversalOptions {
            max_depth: 1,
            edge_kinds: vec![],
            direction: TraversalDirection::Both,
            limit: effective_limit + 1,
            include_start: true,
        };

        // BFS from each file node, merging results
        let mut merged_nodes: HashMap<String, Node> = HashMap::new();
        let mut merged_edges: Vec<codewiki_core::Edge> = Vec::new();
        let roots: Vec<String> = file_nodes.iter().map(|n| n.id.clone()).collect();

        for seed in &file_nodes {
            if merged_nodes.len() >= effective_limit {
                break;
            }
            let sub = traverser
                .traverse_bfs(&seed.id, &opts)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            merged_nodes.extend(sub.nodes);
            merged_edges.extend(sub.edges);
        }

        Ok(Subgraph {
            nodes: merged_nodes,
            edges: merged_edges,
            roots,
        })
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking panic: {e}")));

    match result {
        Ok(subgraph) => {
            let resp =
                build_subgraph_response(subgraph, effective_limit, &HashSet::new(), None);
            Json(resp).into_response()
        }
        Err(e) => super::ApiError::internal(e.to_string()).into_response(),
    }
}

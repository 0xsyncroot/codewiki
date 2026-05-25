//! EP-5: GET /api/impact/:id
//!
//! Blast-radius view — all nodes that would be affected if this node changed.
//! FLAG-4: MutexGuard + GraphTraverser constructed INSIDE spawn_blocking.

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use std::collections::HashSet;
use tokio::task::spawn_blocking;

use super::{build_subgraph_response, AppState};
use codewiki_storage::graph::traversal::{GraphTraverser, TraversalDirection, TraversalOptions};

#[derive(Deserialize)]
pub struct ImpactParams {
    pub depth: Option<usize>,
    pub limit: Option<usize>,
}

pub async fn handle(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<ImpactParams>,
) -> impl IntoResponse {
    let depth = params.depth.unwrap_or(3).min(6);
    let limit = params.limit.unwrap_or(200).min(500);
    let max_nodes = state.max_nodes;
    let effective_limit = limit.min(max_nodes);

    let db = state.db.clone();
    let result = spawn_blocking(move || {
        // FLAG-4: acquire MutexGuard and construct GraphTraverser INSIDE the closure
        let conn = db.lock().map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
        let traverser = GraphTraverser::new(&conn);
        let opts = TraversalOptions {
            max_depth: depth,
            edge_kinds: vec![],
            direction: TraversalDirection::Incoming,
            limit: effective_limit + 1,
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
            let resp =
                build_subgraph_response(subgraph, effective_limit, &HashSet::new(), None);
            Json(resp).into_response()
        }
        Err(e) => super::ApiError::internal(e.to_string()).into_response(),
    }
}

//! EP-6: GET /api/callers/:id
//!
//! FLAG-4: MutexGuard + GraphTraverser constructed INSIDE spawn_blocking.

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio::task::spawn_blocking;

use super::AppState;
use codewiki_storage::graph::traversal::GraphTraverser;

#[derive(Deserialize)]
pub struct CallersParams {
    pub depth: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct CallersResponse {
    pub items: Vec<CallerItem>,
    pub truncated: bool,
    pub total: usize,
}

#[derive(Serialize)]
pub struct CallerItem {
    pub node: codewiki_core::Node,
    pub edge: codewiki_core::Edge,
}

pub async fn handle(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<CallersParams>,
) -> impl IntoResponse {
    let depth = params.depth.unwrap_or(1).min(3);
    let limit = params.limit.unwrap_or(50).min(200);

    let db = state.db.clone();
    let result = spawn_blocking(move || {
        // FLAG-4: acquire MutexGuard and construct GraphTraverser INSIDE the closure
        let conn = db.lock().map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
        let traverser = GraphTraverser::new(&conn);
        traverser
            .get_callers(&id, depth)
            .map_err(|e| anyhow::anyhow!("{e}"))
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking panic: {e}")));

    match result {
        Ok(pairs) => {
            let total = pairs.len();
            let truncated = total > limit;
            let items: Vec<CallerItem> = pairs
                .into_iter()
                .take(limit)
                .map(|(node, edge)| CallerItem { node, edge })
                .collect();
            Json(CallersResponse {
                items,
                truncated,
                total,
            })
            .into_response()
        }
        Err(e) => super::ApiError::internal(e.to_string()).into_response(),
    }
}

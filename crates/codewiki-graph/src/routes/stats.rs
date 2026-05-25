//! EP-1: GET /api/stats

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};
use tokio::task::spawn_blocking;

use super::AppState;
use codewiki_storage::queries::meta as mq;

pub async fn handle(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.db.clone();
    let result: anyhow::Result<Value> = spawn_blocking(move || {
        let conn = db.lock().map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
        let stats = mq::get_stats(&conn).map_err(|e| anyhow::anyhow!("{e}"))?;
        let edges_by_kind = mq::get_edges_by_kind(&conn).map_err(|e| anyhow::anyhow!("{e}"))?;
        let v = json!({
            "node_count": stats.node_count,
            "edge_count": stats.edge_count,
            "file_count": stats.file_count,
            "unresolved_ref_count": stats.unresolved_ref_count,
            "db_size_bytes": stats.db_size_bytes,
            "journal_mode": stats.journal_mode,
            "nodes_by_kind": stats.nodes_by_kind,
            "files_by_language": stats.files_by_language,
            "edges_by_kind": edges_by_kind,
        });
        Ok(v)
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking panic: {e}")));

    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => super::ApiError::internal(e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request;
    use axum::Router;
    use codewiki_storage::connection::open_in_memory;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    #[tokio::test]
    async fn stats_returns_expected_keys() {
        let conn = open_in_memory().unwrap();
        let db = Arc::new(Mutex::new(conn));
        let state = AppState { db, max_nodes: 200 };
        let app = Router::new()
            .route("/api/stats", axum::routing::get(handle))
            .with_state(state);

        let req = Request::builder()
            .uri("/api/stats")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.get("node_count").is_some());
        assert!(v.get("edge_count").is_some());
        assert!(v.get("edges_by_kind").is_some());
    }
}

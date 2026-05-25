//! EP-12: GET /api/health

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;
use tokio::task::spawn_blocking;

use super::AppState;
use codewiki_storage::queries::meta as mq;

pub async fn handle(State(state): State<AppState>) -> impl IntoResponse {
    let db = state.db.clone();
    let result = spawn_blocking(move || {
        let conn = db.lock().map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
        let stats = mq::get_stats(&conn).map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(stats.db_size_bytes)
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking panic: {e}")));

    match result {
        Ok(db_size_bytes) => {
            Json(json!({ "ok": true, "db_size_bytes": db_size_bytes })).into_response()
        }
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

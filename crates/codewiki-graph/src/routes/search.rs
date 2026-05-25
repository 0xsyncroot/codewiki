//! EP-2: GET /api/search

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio::task::spawn_blocking;

use super::AppState;
use codewiki_storage::search::search_nodes_fts;
use codewiki_storage::SearchOptions;

#[derive(Deserialize)]
pub struct SearchParams {
    pub q: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResultItem>,
}

#[derive(Serialize)]
pub struct SearchResultItem {
    pub node: codewiki_core::Node,
    pub score: f64,
    pub snippet: Option<String>,
}

pub async fn handle(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> impl IntoResponse {
    let q = match params.q {
        Some(q) if !q.trim().is_empty() => q,
        _ => {
            return super::ApiError::bad_request("query parameter `q` is required").into_response()
        }
    };
    let limit = params.limit.unwrap_or(20).min(100);

    let db = state.db.clone();
    let result = spawn_blocking(move || {
        let conn = db.lock().map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
        let opts = SearchOptions {
            limit,
            kinds: None,
            languages: None,
            path_filter: None,
        };
        let results = search_nodes_fts(&conn, &q, &opts).map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok::<Vec<codewiki_core::SearchResult>, anyhow::Error>(results)
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking panic: {e}")));

    match result {
        Ok(results) => {
            let items: Vec<SearchResultItem> = results
                .into_iter()
                .map(|r| SearchResultItem {
                    node: r.node,
                    score: r.score,
                    snippet: r.snippet,
                })
                .collect();
            Json(SearchResponse { results: items }).into_response()
        }
        Err(e) => super::ApiError::internal(e.to_string()).into_response(),
    }
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
    async fn search_returns_results_array() {
        let conn = open_in_memory().unwrap();
        let node = Node {
            id: "test-id".into(),
            name: "traverse_bfs".into(),
            qualified_name: "graph::traverse_bfs".into(),
            kind: NodeKind::Function,
            language: Language::Rust,
            file_path: "src/graph.rs".into(),
            ..Default::default()
        };
        insert_node(&conn, &node).unwrap();

        let db = Arc::new(Mutex::new(conn));
        let state = AppState { db, max_nodes: 200 };
        let app = Router::new()
            .route("/api/search", axum::routing::get(handle))
            .with_state(state);

        let req = Request::builder()
            .uri("/api/search?q=traverse")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.get("results").is_some());
    }
}

//! EP-10: GET /api/top-nodes

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio::task::spawn_blocking;

use super::AppState;
use codewiki_storage::queries::nodes::{get_top_nodes_by_degree, DegreeMetric};

#[derive(Deserialize)]
pub struct TopNodesParams {
    pub metric: Option<String>,
    pub kind: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct TopNodesResponse {
    pub nodes: Vec<TopNodeEntry>,
}

#[derive(Serialize)]
pub struct TopNodeEntry {
    pub node: codewiki_core::Node,
    pub in_degree: u64,
    pub out_degree: u64,
    pub degree: u64,
}

pub async fn handle(
    State(state): State<AppState>,
    Query(params): Query<TopNodesParams>,
) -> impl IntoResponse {
    let metric = match params.metric.as_deref() {
        Some("in_degree") | Some("in") => DegreeMetric::In,
        Some("out_degree") | Some("out") => DegreeMetric::Out,
        _ => DegreeMetric::Total,
    };
    let limit = params.limit.unwrap_or(20).min(50);
    let kind_filter: Option<codewiki_core::NodeKind> = params
        .kind
        .as_deref()
        .and_then(|k| serde_json::from_value(serde_json::Value::String(k.to_string())).ok());

    let db = state.db.clone();
    let result = spawn_blocking(move || {
        let conn = db.lock().map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
        get_top_nodes_by_degree(&conn, metric, kind_filter.as_ref(), limit)
            .map_err(|e| anyhow::anyhow!("{e}"))
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking panic: {e}")));

    match result {
        Ok(rows) => {
            let nodes: Vec<TopNodeEntry> = rows
                .into_iter()
                .map(|(node, in_degree, out_degree)| TopNodeEntry {
                    degree: in_degree + out_degree,
                    node,
                    in_degree,
                    out_degree,
                })
                .collect();
            Json(TopNodesResponse { nodes }).into_response()
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
    use codewiki_core::{Edge, EdgeKind, Language, Node, NodeKind};
    use codewiki_storage::connection::open_in_memory;
    use codewiki_storage::queries::edges::insert_edge;
    use codewiki_storage::queries::nodes::insert_node;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    #[tokio::test]
    async fn top_nodes_returns_ordered_by_degree() {
        let conn = open_in_memory().unwrap();
        // Insert 3 nodes with different connectivity
        for i in 0..3usize {
            let node = Node {
                id: format!("n{}", i),
                name: format!("node{}", i),
                qualified_name: format!("node{}", i),
                kind: NodeKind::Function,
                language: Language::Rust,
                file_path: "src/x.rs".into(),
                ..Default::default()
            };
            insert_node(&conn, &node).unwrap();
        }
        // n0 calls n1 and n2 → n0 has out_degree=2, n1/n2 have in_degree=1
        for target in ["n1", "n2"] {
            insert_edge(
                &conn,
                &Edge {
                    id: format!("e-n0-{}", target),
                    source_id: "n0".into(),
                    target_id: target.into(),
                    kind: EdgeKind::Calls,
                    ..Default::default()
                },
            )
            .unwrap();
        }

        let db = Arc::new(Mutex::new(conn));
        let state = AppState { db, max_nodes: 200 };
        let app = Router::new()
            .route("/api/top-nodes", axum::routing::get(handle))
            .with_state(state);

        let req = Request::builder()
            .uri("/api/top-nodes?limit=5")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let nodes = v["nodes"].as_array().unwrap();
        assert!(!nodes.is_empty());
        // First node should have highest degree
        let first_degree = nodes[0]["degree"].as_u64().unwrap_or(0);
        assert!(first_degree >= 2);
    }
}

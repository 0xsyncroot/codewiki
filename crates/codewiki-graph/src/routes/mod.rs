//! Shared types and utilities for route handlers.

pub mod callees;
pub mod callers;
pub mod clusters;
pub mod file_graph;
pub mod files;
pub mod health;
pub mod impact;
pub mod neighborhood;
pub mod node;
pub mod search;
pub mod stats;
pub mod top_nodes;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::json;

/// Application state shared across route handlers.
#[derive(Clone)]
pub struct AppState {
    pub db: crate::db::ReadDb,
    pub max_nodes: usize,
}

/// A JSON error response.
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
    pub code: &'static str,
}

impl ApiError {
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
            code: "NOT_FOUND",
        }
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
            code: "BAD_REQUEST",
        }
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
            code: "INTERNAL",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({ "error": self.message, "code": self.code });
        (self.status, axum::Json(body)).into_response()
    }
}

impl<E: std::fmt::Display> From<E> for ApiError {
    fn from(e: E) -> Self {
        ApiError::internal(e.to_string())
    }
}

/// Wrapper for bounded subgraph responses.
#[derive(Serialize)]
pub struct SubgraphResponse {
    pub subgraph: SubgraphData,
    pub truncated: bool,
    pub node_count: usize,
    pub edge_count: usize,
}

#[derive(Serialize)]
pub struct SubgraphData {
    pub nodes: std::collections::HashMap<String, codewiki_core::Node>,
    pub edges: Vec<codewiki_core::Edge>,
    pub roots: Vec<String>,
}

/// Deduplicate edges by id (traverse_bfs with Both direction can emit
/// the same edge twice — once as outgoing from A, once as incoming to B).
pub fn dedup_edges(edges: Vec<codewiki_core::Edge>) -> Vec<codewiki_core::Edge> {
    let mut seen = std::collections::HashSet::new();
    edges
        .into_iter()
        .filter(|e| seen.insert(e.id.clone()))
        .collect()
}

/// Build a `SubgraphResponse` from a `Subgraph`, applying node cap and
/// optional node-kind filter.
pub fn build_subgraph_response(
    subgraph: codewiki_core::Subgraph,
    limit: usize,
    exclude: &std::collections::HashSet<String>,
    node_kinds: Option<&[codewiki_core::NodeKind]>,
) -> SubgraphResponse {
    // Filter excluded + optional kind filter
    let mut nodes: std::collections::HashMap<String, codewiki_core::Node> = subgraph
        .nodes
        .into_iter()
        .filter(|(id, node)| {
            !exclude.contains(id)
                && node_kinds
                    .map(|kinds| kinds.contains(&node.kind))
                    .unwrap_or(true)
        })
        .collect();

    // Apply hard cap
    let truncated = nodes.len() > limit;
    if truncated {
        let to_remove: Vec<String> = nodes.keys().skip(limit).cloned().collect();
        for k in to_remove {
            nodes.remove(&k);
        }
    }

    let kept_ids: std::collections::HashSet<&str> =
        nodes.keys().map(String::as_str).collect();

    let edges = dedup_edges(subgraph.edges)
        .into_iter()
        .filter(|e| kept_ids.contains(e.source_id.as_str()) && kept_ids.contains(e.target_id.as_str()))
        .collect::<Vec<_>>();

    let node_count = nodes.len();
    let edge_count = edges.len();

    SubgraphResponse {
        subgraph: SubgraphData {
            nodes,
            edges,
            roots: subgraph.roots,
        },
        truncated,
        node_count,
        edge_count,
    }
}

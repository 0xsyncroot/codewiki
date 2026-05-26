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

/// Select up to `limit` nodes that form a CONNECTED core of the subgraph.
///
/// Walks BFS outward from `roots` (or, if no root survives the kind/exclude
/// filter, from an arbitrary surviving node) following the undirected edge set,
/// keeping nodes in discovery order. This guarantees the truncated result is a
/// connected, edge-rich neighborhood rather than a scatter of isolated nodes.
fn connected_core(
    nodes: &std::collections::HashMap<String, codewiki_core::Node>,
    edges: &[codewiki_core::Edge],
    roots: &[String],
    limit: usize,
) -> std::collections::HashSet<String> {
    use std::collections::{HashMap, HashSet, VecDeque};

    let mut kept: HashSet<String> = HashSet::with_capacity(limit);
    if limit == 0 {
        return kept;
    }

    // Undirected adjacency over only the surviving (filtered) nodes.
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in edges {
        let (s, t) = (e.source_id.as_str(), e.target_id.as_str());
        if nodes.contains_key(s) && nodes.contains_key(t) {
            adj.entry(s).or_default().push(t);
            adj.entry(t).or_default().push(s);
        }
    }

    let mut queue: VecDeque<String> = VecDeque::new();

    // Seed from surviving roots first so the user's focus node anchors the view.
    for r in roots {
        if kept.len() >= limit {
            break;
        }
        if nodes.contains_key(r) && kept.insert(r.clone()) {
            queue.push_back(r.clone());
        }
    }

    while kept.len() < limit {
        // BFS the current frontier.
        while let Some(cur) = queue.pop_front() {
            if kept.len() >= limit {
                break;
            }
            if let Some(neighbors) = adj.get(cur.as_str()) {
                for &n in neighbors {
                    if kept.len() >= limit {
                        break;
                    }
                    if kept.insert(n.to_string()) {
                        queue.push_back(n.to_string());
                    }
                }
            }
        }
        if kept.len() >= limit {
            break;
        }
        // Frontier exhausted but cap not reached: the subgraph is disconnected.
        // Seed the next unvisited component so we still fill up to the cap with
        // connected clusters rather than stopping short.
        match nodes.keys().find(|id| !kept.contains(*id)) {
            Some(id) => {
                let id = id.clone();
                kept.insert(id.clone());
                queue.push_back(id);
            }
            None => break,
        }
    }

    kept
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

    // Dedup edges up front so both the cap (which BFS-walks the edges to keep a
    // connected core) and the final filter operate on the same set.
    let deduped_edges = dedup_edges(subgraph.edges);

    // Apply hard cap. CRITICAL: don't evict arbitrary HashMap keys — that
    // shreds connectivity (an arbitrary 200-of-2110 cut keeps almost no edges,
    // yielding an "edgeless dust cloud"). Instead keep a CONNECTED core: BFS
    // outward from the roots following the surviving edges, retaining nodes in
    // discovery (distance) order until we hit the cap. Edges between kept nodes
    // come along for free, so the truncated subgraph stays edge-rich.
    let truncated = nodes.len() > limit;
    if truncated {
        let kept = connected_core(&nodes, &deduped_edges, &subgraph.roots, limit);
        nodes.retain(|id, _| kept.contains(id));
    }

    let kept_ids: std::collections::HashSet<&str> = nodes.keys().map(String::as_str).collect();

    let edges = deduped_edges
        .into_iter()
        .filter(|e| {
            kept_ids.contains(e.source_id.as_str()) && kept_ids.contains(e.target_id.as_str())
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use codewiki_core::{Edge, EdgeKind, Node, Subgraph};
    use std::collections::{HashMap, HashSet};

    fn node(id: &str) -> Node {
        Node {
            id: id.into(),
            name: id.into(),
            qualified_name: id.into(),
            ..Default::default()
        }
    }

    fn edge(s: &str, t: &str) -> Edge {
        Edge {
            id: format!("e-{s}-{t}"),
            source_id: s.into(),
            target_id: t.into(),
            kind: EdgeKind::Calls,
            ..Default::default()
        }
    }

    /// A line graph 0->1->2->...->N rooted at 0. Capping to `limit` must keep a
    /// CONNECTED prefix (limit-1 edges), never an edgeless cloud.
    #[test]
    fn cap_keeps_connected_core_not_edgeless_cloud() {
        let n = 50usize;
        let mut nodes = HashMap::new();
        let mut edges = Vec::new();
        for i in 0..n {
            nodes.insert(format!("n{i}"), node(&format!("n{i}")));
            if i + 1 < n {
                edges.push(edge(&format!("n{i}"), &format!("n{}", i + 1)));
            }
        }
        let sub = Subgraph {
            nodes,
            edges,
            roots: vec!["n0".into()],
        };

        let limit = 10;
        let resp = build_subgraph_response(sub, limit, &HashSet::new(), None);

        assert!(resp.truncated);
        assert_eq!(resp.node_count, limit);
        // A connected line prefix of `limit` nodes has `limit - 1` edges.
        // The old arbitrary-eviction code would have kept ~0-1 here.
        assert_eq!(
            resp.edge_count,
            limit - 1,
            "truncated core must stay connected"
        );
        // Root must survive and anchor the view.
        assert!(resp.subgraph.nodes.contains_key("n0"));
    }

    /// Roots seed the retained core: BFS starts from the root's neighborhood.
    #[test]
    fn cap_anchors_on_roots() {
        // Two disjoint chains; root is in the second chain.
        let mut nodes = HashMap::new();
        let mut edges = Vec::new();
        for i in 0..20 {
            nodes.insert(format!("a{i}"), node(&format!("a{i}")));
            if i + 1 < 20 {
                edges.push(edge(&format!("a{i}"), &format!("a{}", i + 1)));
            }
        }
        for i in 0..20 {
            nodes.insert(format!("b{i}"), node(&format!("b{i}")));
            if i + 1 < 20 {
                edges.push(edge(&format!("b{i}"), &format!("b{}", i + 1)));
            }
        }
        let sub = Subgraph {
            nodes,
            edges,
            roots: vec!["b0".into()],
        };

        let resp = build_subgraph_response(sub, 5, &HashSet::new(), None);
        assert!(resp.truncated);
        assert_eq!(resp.node_count, 5);
        // The root's chain (b*) should be retained, fully connected.
        assert!(resp.subgraph.nodes.contains_key("b0"));
        assert_eq!(resp.edge_count, 4);
    }

    /// No truncation needed → everything is returned untouched.
    #[test]
    fn no_cap_keeps_all() {
        let mut nodes = HashMap::new();
        let mut edges = Vec::new();
        for i in 0..5 {
            nodes.insert(format!("n{i}"), node(&format!("n{i}")));
            if i + 1 < 5 {
                edges.push(edge(&format!("n{i}"), &format!("n{}", i + 1)));
            }
        }
        let sub = Subgraph {
            nodes,
            edges,
            roots: vec!["n0".into()],
        };
        let resp = build_subgraph_response(sub, 100, &HashSet::new(), None);
        assert!(!resp.truncated);
        assert_eq!(resp.node_count, 5);
        assert_eq!(resp.edge_count, 4);
    }
}

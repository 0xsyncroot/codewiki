//! EP-11: GET /api/clusters
//!
//! Groups files by directory depth — no new SQL; Rust-side grouping.

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::task::spawn_blocking;

use super::AppState;
use codewiki_storage::queries::files as fq;

#[derive(Deserialize)]
pub struct ClustersParams {
    pub depth: Option<usize>,
    pub prefix: Option<String>,
}

#[derive(Serialize)]
pub struct ClustersResponse {
    pub clusters: Vec<ClusterEntry>,
}

#[derive(Serialize)]
pub struct ClusterEntry {
    pub dir: String,
    pub node_count: u64,
    pub file_count: usize,
    pub languages: Vec<String>,
}

pub async fn handle(
    State(state): State<AppState>,
    Query(params): Query<ClustersParams>,
) -> impl IntoResponse {
    let depth = params.depth.unwrap_or(2).max(1);
    let prefix = params.prefix;

    let db = state.db.clone();
    let result = spawn_blocking(move || {
        let conn = db.lock().map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
        let all = fq::get_all_files(&conn).map_err(|e| anyhow::anyhow!("{e}"))?;

        // Group by first `depth` path segments
        let mut map: HashMap<String, (u64, usize, std::collections::HashSet<String>)> =
            HashMap::new();

        for f in &all {
            let path_str = f.path.to_string_lossy();
            // Apply prefix filter
            if let Some(p) = &prefix {
                if !path_str.contains(p.as_str()) {
                    continue;
                }
            }
            // Truncate to first `depth` components
            let parts: Vec<&str> = path_str.split('/').collect();
            let dir = if parts.len() > depth {
                parts[..depth].join("/")
            } else {
                parts[..parts.len().saturating_sub(1)].join("/")
            };

            let entry = map
                .entry(dir)
                .or_insert((0, 0, std::collections::HashSet::new()));
            entry.0 += f.node_count as u64;
            entry.1 += 1;
            entry.2.insert(f.language.clone());
        }

        let mut clusters: Vec<ClusterEntry> = map
            .into_iter()
            .map(|(dir, (node_count, file_count, langs))| ClusterEntry {
                dir,
                node_count,
                file_count,
                languages: langs.into_iter().collect(),
            })
            .collect();
        clusters.sort_by_key(|c| std::cmp::Reverse(c.node_count));

        Ok(clusters)
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking panic: {e}")));

    match result {
        Ok(clusters) => Json(ClustersResponse { clusters }).into_response(),
        Err(e) => super::ApiError::internal(e.to_string()).into_response(),
    }
}

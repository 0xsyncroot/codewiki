//! EP-9: GET /api/files

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio::task::spawn_blocking;

use super::AppState;
use codewiki_storage::queries::files as fq;

#[derive(Deserialize)]
pub struct FilesParams {
    pub prefix: Option<String>,
    pub lang: Option<String>,
}

#[derive(Serialize)]
pub struct FilesResponse {
    pub files: Vec<FileEntry>,
    pub total: usize,
}

#[derive(Serialize)]
pub struct FileEntry {
    pub path: String,
    pub language: String,
    pub node_count: u32,
    pub size: u64,
    pub modified_at: i64,
}

pub async fn handle(
    State(state): State<AppState>,
    Query(params): Query<FilesParams>,
) -> impl IntoResponse {
    let prefix = params.prefix;
    let lang = params.lang;

    let db = state.db.clone();
    let result = spawn_blocking(move || {
        let conn = db.lock().map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
        let all = fq::get_all_files(&conn).map_err(|e| anyhow::anyhow!("{e}"))?;
        let filtered: Vec<_> = all
            .into_iter()
            .filter(|f| {
                let path_str = f.path.to_string_lossy();
                let prefix_ok = prefix
                    .as_deref()
                    .map(|p| path_str.contains(p))
                    .unwrap_or(true);
                let lang_ok = lang
                    .as_deref()
                    .map(|l| f.language.eq_ignore_ascii_case(l))
                    .unwrap_or(true);
                prefix_ok && lang_ok
            })
            .collect();
        Ok(filtered)
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking panic: {e}")));

    match result {
        Ok(records) => {
            let total = records.len();
            let files: Vec<FileEntry> = records
                .into_iter()
                .map(|f| FileEntry {
                    path: f.path.to_string_lossy().to_string(),
                    language: f.language,
                    node_count: f.node_count,
                    size: f.size,
                    modified_at: f.modified_at,
                })
                .collect();
            Json(FilesResponse { files, total }).into_response()
        }
        Err(e) => super::ApiError::internal(e.to_string()).into_response(),
    }
}

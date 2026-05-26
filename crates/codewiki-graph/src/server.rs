//! axum HTTP server for the codewiki-graph web UI.

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tower_http::cors::{Any, CorsLayer};

use crate::assets::Assets;
use crate::db::{open_readonly, ReadDb};
use crate::routes;
use crate::routes::AppState;

/// Builder for the graph HTTP server.
pub struct GraphServer {
    db_path: PathBuf,
    port: u16,
    no_open: bool,
    max_nodes: usize,
}

impl GraphServer {
    /// Create a new server pointing at the given DB path.
    pub fn new(db_path: PathBuf) -> Self {
        Self {
            db_path,
            port: 7007,
            no_open: false,
            // Generous default so a typical repo renders as one connected graph
            // rather than a 200-node stub. build_subgraph_response still keeps a
            // CONNECTED core when a huge repo exceeds this, and the CLI
            // `--max-nodes` flag can raise it further for very large indexes.
            max_nodes: 2000,
        }
    }

    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn no_open(mut self, no_open: bool) -> Self {
        self.no_open = no_open;
        self
    }

    pub fn max_nodes(mut self, max_nodes: usize) -> Self {
        self.max_nodes = max_nodes;
        self
    }

    /// Start the axum server and block until Ctrl-C.
    pub async fn serve(self) -> anyhow::Result<()> {
        let conn = open_readonly(&self.db_path)
            .map_err(|e| anyhow::anyhow!("Cannot open DB at {}: {e}", self.db_path.display()))?;

        let db: ReadDb = Arc::new(Mutex::new(conn));
        let state = AppState {
            db,
            max_nodes: self.max_nodes,
        };

        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);

        let api = Router::new()
            .route("/api/stats", get(routes::stats::handle))
            .route("/api/search", get(routes::search::handle))
            .route("/api/node/{id}", get(routes::node::handle))
            .route("/api/neighborhood/{id}", get(routes::neighborhood::handle))
            .route("/api/impact/{id}", get(routes::impact::handle))
            .route("/api/callers/{id}", get(routes::callers::handle))
            .route("/api/callees/{id}", get(routes::callees::handle))
            .route("/api/file-graph", get(routes::file_graph::handle))
            .route("/api/files", get(routes::files::handle))
            .route("/api/top-nodes", get(routes::top_nodes::handle))
            .route("/api/clusters", get(routes::clusters::handle))
            .route("/api/health", get(routes::health::handle));

        let app = Router::new()
            .merge(api)
            .route("/", get(serve_index))
            .route("/{*path}", get(serve_static))
            .layer(cors)
            .with_state(state);

        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        let url = format!("http://{addr}");
        println!("codewiki graph UI  →  {url}");

        if !self.no_open {
            // Best-effort browser open; ignore errors
            let _ = open::that(&url);
        }

        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!(addr = %addr, "graph server listening");

        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await?;

        Ok(())
    }
}

async fn serve_index(State(_state): State<AppState>) -> impl IntoResponse {
    serve_asset("index.html")
}

async fn serve_static(
    State(_state): State<AppState>,
    axum::extract::Path(path): axum::extract::Path<String>,
) -> impl IntoResponse {
    serve_asset(&path)
}

fn serve_asset(path: &str) -> Response {
    match Assets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime.as_ref())
                .body(Body::from(content.data.to_vec()))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received");
}

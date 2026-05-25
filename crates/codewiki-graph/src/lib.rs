//! codewiki-graph — interactive web UI for browsing the CodeWiki knowledge graph.
//!
//! The `web` feature gates the axum HTTP server, rust-embed static assets,
//! and all dependencies that would otherwise bloat the base binary.
//!
//! Without `web`:    zero addition to the binary.
//! With `web`:       axum + hyper + force-graph frontend (~1.35 MB).

#[cfg(feature = "web")]
pub mod assets;
#[cfg(feature = "web")]
pub mod db;
#[cfg(feature = "web")]
pub mod routes;
#[cfg(feature = "web")]
pub mod server;

#[cfg(feature = "web")]
pub use server::GraphServer;

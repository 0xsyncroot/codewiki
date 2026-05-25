//! Embedded static assets for the graph web UI.
//!
//! In debug builds, `rust-embed` reads from disk automatically (no `debug-embed`
//! feature needed) — live reload by refreshing the browser.
//! In release builds, bytes are compiled in.

#[derive(rust_embed::Embed)]
#[folder = "src/assets/"]
pub struct Assets;

//! .codewiki-extraction` — tree-sitter extraction pipeline for CodeWiki.
//!
//! # Overview
//!
//! This crate implements all of the P2 extraction tasks (T-019, T-201..T-224).
//! It provides:
//!
//! - `extract_file` — parse a single source file and return an `ExtractionBatch`.
//! - `ExtractionOrchestratorImpl` — rayon-parallel orchestrator for bulk indexing.
//! - `IncrementalParser` trait — incremental reparse support.
//! - `FileEdit` — byte-level edit descriptor.
//! - Per-language extractors for 14 natively supported languages + 4 special extractors.
//! - WASM grammar loader behind the `wasmtime-grammars` feature.

pub mod ast_walker;
pub mod config;
pub mod file_reader;
pub mod incremental;
pub mod language_detector;
pub mod languages;
pub mod orchestrator;
pub mod parser_pool;
pub mod path_norm;
pub mod types;
pub mod wasm_grammars;
pub mod wasm_parser;

// Re-export public API.
pub use ast_walker::{extract_file, generate_node_id, LanguageConfig, LanguageExtractor};
pub use config::{EXTRACTION_CHANNEL_DEPTH, INCREMENTAL_LOC_THRESHOLD, MAX_FILE_SIZE_BYTES};
pub use file_reader::read_source_file;
pub use incremental::IncrementalParser;
pub use language_detector::{detect_language, is_source_file};
pub use orchestrator::{ChangedFiles, ExtractionOrchestratorImpl, ExtractionStore, IndexCounts};
pub use path_norm::normalize_path;
pub use types::{FileEdit, ParsedTree};
pub use wasm_grammars::{wasm_grammar_bytes, WasmGrammar};

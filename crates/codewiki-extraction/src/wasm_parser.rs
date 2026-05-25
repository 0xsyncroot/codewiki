//! T-203 — WASM grammar parser pool (behind `wasmtime-grammars` feature).
//!
//! When the feature is enabled this module owns:
//! - A single `wasmtime::Engine` (shared across threads via `OnceLock`).
//! - A per-thread `WasmParserPool` stored in `thread_local! WASM_PARSERS`,
//!   mirroring the native `THREAD_PARSERS` in `parser_pool.rs`.
//!
//! The public entry-point is `extract_wasm(source, path, grammar)` which
//! parses `source` via the bundled WASM grammar and calls the existing
//! `extract_with_config` pipeline, producing nodes/edges in the same shape
//! as all native extractors.

// When the feature is off, these are needed by the no-op stubs below.
// When the feature is on, they are re-imported inside the `inner` module.
#[cfg(not(feature = "wasmtime-grammars"))]
use {crate::wasm_grammars::WasmGrammar, codewiki_core::ExtractionBatch, std::path::Path};

// ── Error type ─────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum WasmExtractionError {
    #[error("wasmtime-grammars feature is disabled")]
    FeatureDisabled,
    #[error("failed to create WasmStore: {0}")]
    StoreInit(String),
    #[error("failed to load grammar '{0}': {1}")]
    GrammarLoad(&'static str, String),
    #[error("failed to set WASM store on parser: {0}")]
    SetWasmStore(String),
    #[error("failed to set language on parser: {0}")]
    SetLanguage(String),
    #[error("parser returned no tree for {0}")]
    ParseFailed(String),
}

// ── Feature-enabled implementation ─────────────────────────────────────────────

#[cfg(feature = "wasmtime-grammars")]
mod inner {
    use super::WasmExtractionError;
    use crate::ast_walker::extract_with_config;
    use crate::languages::{
        lua::LuaExtractor, luau::LuauExtractor, pascal::PascalExtractor, scala::ScalaExtractor,
    };
    use crate::wasm_grammars::{WasmGrammar, LUAU_WASM, LUA_WASM, PASCAL_WASM, SCALA_WASM};
    use codewiki_core::ExtractionBatch;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::OnceLock;
    // Use tree-sitter's re-exported wasmtime (v29) to avoid the version mismatch
    // between the workspace wasmtime (v20) and tree-sitter's internal wasmtime.
    use tree_sitter::{wasmtime, WasmStore};

    // ── Shared engine (one per process) ──────────────────────────────────────────

    /// Shared `wasmtime::Engine`. Created once; cloned cheaply (it is Arc-backed).
    static WASM_ENGINE: OnceLock<wasmtime::Engine> = OnceLock::new();

    fn get_engine() -> &'static wasmtime::Engine {
        WASM_ENGINE.get_or_init(wasmtime::Engine::default)
    }

    // ── Per-thread pool ───────────────────────────────────────────────────────────

    /// How many WASM parses before recycling the parser (lower than native due to
    /// larger WASM heap footprint).
    const WASM_PARSER_RECYCLE_INTERVAL: u32 = 200;

    struct WasmEntry {
        parser: tree_sitter::Parser,
        // Language is kept alive here; the parser holds a raw pointer to it.
        _language: tree_sitter::Language,
        parse_count: u32,
    }

    struct WasmParserPool {
        entries: HashMap<WasmGrammar, WasmEntry>,
    }

    impl WasmParserPool {
        fn new() -> Self {
            Self {
                entries: HashMap::new(),
            }
        }

        /// Return a mutable reference to the parser for `grammar`, initialising
        /// the `WasmStore` and loading the language on first use per thread.
        #[allow(clippy::map_entry)]
        fn parser_for(
            &mut self,
            grammar: WasmGrammar,
        ) -> Result<&mut tree_sitter::Parser, WasmExtractionError> {
            // Recycle the parser every N parses.
            if let Some(entry) = self.entries.get(&grammar) {
                if entry.parse_count >= WASM_PARSER_RECYCLE_INTERVAL {
                    self.entries.remove(&grammar);
                }
            }

            if !self.entries.contains_key(&grammar) {
                // WasmStore::new takes &Engine (not owned Engine).
                let engine = get_engine();
                let mut store = WasmStore::new(engine).map_err(|e: tree_sitter::WasmError| {
                    WasmExtractionError::StoreInit(e.to_string())
                })?;

                let name = grammar.grammar_name();
                let bytes: &[u8] = match grammar {
                    WasmGrammar::Lua => LUA_WASM,
                    WasmGrammar::Luau => LUAU_WASM,
                    WasmGrammar::Pascal => PASCAL_WASM,
                    WasmGrammar::Scala => SCALA_WASM,
                };

                let load_start = std::time::Instant::now();
                let language =
                    store
                        .load_language(name, bytes)
                        .map_err(|e: tree_sitter::WasmError| {
                            WasmExtractionError::GrammarLoad(name, e.to_string())
                        })?;
                tracing::info!(
                    grammar = name,
                    elapsed_ms = load_start.elapsed().as_millis(),
                    "WASM grammar loaded (first use on this thread)"
                );

                let mut parser = tree_sitter::Parser::new();
                // set_wasm_store takes ownership of the store.
                parser
                    .set_wasm_store(store)
                    .map_err(|e: tree_sitter::LanguageError| {
                        WasmExtractionError::SetWasmStore(e.to_string())
                    })?;
                parser
                    .set_language(&language)
                    .map_err(|e: tree_sitter::LanguageError| {
                        WasmExtractionError::SetLanguage(e.to_string())
                    })?;

                self.entries.insert(
                    grammar,
                    WasmEntry {
                        parser,
                        _language: language,
                        parse_count: 0,
                    },
                );
            }

            let entry = self.entries.get_mut(&grammar).unwrap();
            entry.parse_count += 1;
            Ok(&mut entry.parser)
        }
    }

    thread_local! {
        static WASM_PARSERS: RefCell<WasmParserPool> = RefCell::new(WasmParserPool::new());
    }

    // ── Public extract_wasm ───────────────────────────────────────────────────────

    /// Extract symbols from `source` using the bundled WASM grammar for `grammar`.
    ///
    /// On success returns a full `ExtractionBatch` (file node + symbol nodes + edges).
    /// On failure returns `Err` so the caller can fall back to `file_only_batch`.
    pub fn extract_wasm(
        source: &str,
        path: &Path,
        grammar: WasmGrammar,
    ) -> Result<ExtractionBatch, WasmExtractionError> {
        // Parse using the per-thread WASM parser pool.
        let tree = WASM_PARSERS.with(|pool| {
            let mut pool = pool.borrow_mut();
            pool.parser_for(grammar).and_then(|parser| {
                parser
                    .parse(source, None)
                    .ok_or_else(|| WasmExtractionError::ParseFailed(path.display().to_string()))
            })
        })?;

        // Dispatch to the per-language extractor (same extract_with_config path as native).
        let batch = match grammar {
            WasmGrammar::Lua => extract_with_config(&LuaExtractor, &tree, source, path),
            WasmGrammar::Luau => extract_with_config(&LuauExtractor, &tree, source, path),
            WasmGrammar::Pascal => extract_with_config(&PascalExtractor, &tree, source, path),
            WasmGrammar::Scala => extract_with_config(&ScalaExtractor, &tree, source, path),
        };

        Ok(batch)
    }
}

// ── Feature-disabled stub ─────────────────────────────────────────────────────

#[cfg(not(feature = "wasmtime-grammars"))]
pub fn extract_wasm(
    _source: &str,
    _path: &Path,
    _grammar: WasmGrammar,
) -> Result<ExtractionBatch, WasmExtractionError> {
    Err(WasmExtractionError::FeatureDisabled)
}

#[cfg(feature = "wasmtime-grammars")]
pub use inner::extract_wasm;

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "wasmtime-grammars"))]
mod tests {
    use crate::wasm_grammars::{LUAU_WASM, LUA_WASM, PASCAL_WASM, SCALA_WASM};
    use tree_sitter::{wasmtime, WasmStore};

    /// Verifies ABI compat and that grammar_name() values match exported WASM symbols.
    #[test]
    fn wasm_grammars_load_without_error() {
        let engine = wasmtime::Engine::default();
        let mut store = WasmStore::new(&engine).unwrap();
        store.load_language("lua", LUA_WASM).unwrap();
        store.load_language("luau", LUAU_WASM).unwrap();
        store.load_language("pascal", PASCAL_WASM).unwrap();
        store.load_language("scala", SCALA_WASM).unwrap();
    }
}

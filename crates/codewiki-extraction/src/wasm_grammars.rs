//! T-203 — WASM grammar loader (gated on `wasmtime-grammars` feature).
//!
//! When the feature is enabled, the four bundled `.wasm` grammar files for
//! Lua, Luau, Pascal, and Scala are embedded via `include_bytes!` and made
//! available for loading into a `wasmtime::Engine`.
//!
//! When the feature is **disabled**, `load_wasm_grammar` returns `None` and
//! logs a `tracing::warn!` so callers know to skip those languages.

#[cfg(feature = "wasmtime-grammars")]
pub static LUA_WASM: &[u8] =
    include_bytes!("../grammars/tree-sitter-lua.wasm");

#[cfg(feature = "wasmtime-grammars")]
pub static LUAU_WASM: &[u8] =
    include_bytes!("../grammars/tree-sitter-luau.wasm");

#[cfg(feature = "wasmtime-grammars")]
pub static PASCAL_WASM: &[u8] =
    include_bytes!("../grammars/tree-sitter-pascal.wasm");

#[cfg(feature = "wasmtime-grammars")]
pub static SCALA_WASM: &[u8] =
    include_bytes!("../grammars/tree-sitter-scala.wasm");

/// Identify which WASM grammar to load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WasmGrammar {
    Lua,
    Luau,
    Pascal,
    Scala,
}

impl WasmGrammar {
    /// Map a codewiki `Language` to its `WasmGrammar` variant.
    /// Returns `None` for all non-WASM languages.
    pub fn from_language(lang: &codewiki_core::Language) -> Option<Self> {
        match lang {
            codewiki_core::Language::Lua    => Some(Self::Lua),
            codewiki_core::Language::Luau   => Some(Self::Luau),
            codewiki_core::Language::Pascal => Some(Self::Pascal),
            codewiki_core::Language::Scala  => Some(Self::Scala),
            _ => None,
        }
    }

    /// The grammar name string as expected by `WasmStore::load_language`.
    /// Must match the exported symbol name in the `.wasm` binary.
    pub fn grammar_name(self) -> &'static str {
        match self {
            Self::Lua    => "lua",
            Self::Luau   => "luau",
            Self::Pascal => "pascal",
            Self::Scala  => "scala",
        }
    }
}

/// Return the embedded bytes for a WASM grammar.
///
/// Returns `None` when the `wasmtime-grammars` feature is disabled.
#[allow(unused_variables)]
pub fn wasm_grammar_bytes(grammar: WasmGrammar) -> Option<&'static [u8]> {
    #[cfg(feature = "wasmtime-grammars")]
    {
        Some(match grammar {
            WasmGrammar::Lua => LUA_WASM,
            WasmGrammar::Luau => LUAU_WASM,
            WasmGrammar::Pascal => PASCAL_WASM,
            WasmGrammar::Scala => SCALA_WASM,
        })
    }
    #[cfg(not(feature = "wasmtime-grammars"))]
    {
        tracing::warn!(
            ?grammar,
            "WASM grammar requested but wasmtime-grammars feature is disabled; skipping"
        );
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "wasmtime-grammars")]
    #[test]
    fn wasm_bytes_non_empty() {
        assert!(!LUA_WASM.is_empty());
        assert!(!LUAU_WASM.is_empty());
        assert!(!PASCAL_WASM.is_empty());
        assert!(!SCALA_WASM.is_empty());
    }

    #[cfg(not(feature = "wasmtime-grammars"))]
    #[test]
    fn no_feature_returns_none() {
        assert!(wasm_grammar_bytes(WasmGrammar::Lua).is_none());
    }
}

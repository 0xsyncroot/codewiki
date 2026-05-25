//! Language extractor registry — returns the correct `LanguageExtractor` for each language.

pub mod c;
pub mod cpp;
pub mod csharp;
pub mod dart;
pub mod dfm;
pub mod go;
pub mod java;
pub mod javascript;
pub mod kotlin;
pub mod liquid;
pub mod lua;
pub mod luau;
pub mod pascal;
pub mod php;
pub mod python;
pub mod razor;
pub mod ruby;
pub mod rust_lang;
pub mod scala;
pub mod svelte;
pub mod swift;
pub mod typescript;
pub mod vue;

use crate::ast_walker::{GetExtractorResult, LanguageExtractor};
use codewiki_core::{ExtractionBatch, FileRecord, Language, Node, NodeKind};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Dispatch for a language file.
///
/// - Special languages (Svelte, Vue, Liquid, DFM) run their own regex/sub-extractor
///   and return `GetExtractorResult::Special(batch)`.
/// - The 14 native tree-sitter languages return `GetExtractorResult::UseTreeSitter(extractor)`.
/// - WASM-backed languages (Lua, Luau, Pascal, Scala) and `Unknown` return
///   `GetExtractorResult::None`.
pub fn get_extractor_and_use_special(
    lang: &Language,
    path: &Path,
    source: &str,
) -> GetExtractorResult {
    match lang {
        Language::Svelte => GetExtractorResult::Special(svelte::extract_special(source, path)),
        Language::Vue => GetExtractorResult::Special(vue::extract_special(source, path)),
        Language::Razor => GetExtractorResult::Special(razor::extract_special(source, path)),
        Language::Liquid => GetExtractorResult::Special(liquid::extract_special(source, path)),
        Language::Dfm => GetExtractorResult::Special(dfm::extract_special(source, path)),
        Language::Lua | Language::Luau | Language::Pascal | Language::Scala => {
            use crate::wasm_grammars::WasmGrammar;
            use crate::wasm_parser::extract_wasm;

            let grammar = match lang {
                Language::Lua => WasmGrammar::Lua,
                Language::Luau => WasmGrammar::Luau,
                Language::Pascal => WasmGrammar::Pascal,
                Language::Scala => WasmGrammar::Scala,
                _ => unreachable!(),
            };

            GetExtractorResult::Special(extract_wasm(source, path, grammar).unwrap_or_else(|e| {
                tracing::warn!(?lang, %e, "WASM extraction failed; falling back to file-only");
                file_only_batch(source, path)
            }))
        }
        _ => match get_extractor(lang) {
            Some(ext) => GetExtractorResult::UseTreeSitter(ext),
            None => GetExtractorResult::None,
        },
    }
}

/// Return a boxed extractor for the given language, or `None` if the language
/// is handled by a WASM runtime (Lua, Luau, Pascal, Scala) or is a special
/// extractor (Svelte, Vue, Liquid, DFM) or is unknown.
pub fn get_extractor(lang: &Language) -> Option<Box<dyn LanguageExtractor>> {
    match lang {
        Language::TypeScript => Some(Box::new(typescript::TypeScriptExtractor)),
        Language::JavaScript => Some(Box::new(javascript::JavaScriptExtractor)),
        Language::Python => Some(Box::new(python::PythonExtractor)),
        Language::Go => Some(Box::new(go::GoExtractor)),
        Language::Rust => Some(Box::new(rust_lang::RustExtractor)),
        Language::Java => Some(Box::new(java::JavaExtractor)),
        Language::CSharp => Some(Box::new(csharp::CSharpExtractor)),
        Language::Php => Some(Box::new(php::PhpExtractor)),
        Language::Ruby => Some(Box::new(ruby::RubyExtractor)),
        Language::Swift => Some(Box::new(swift::SwiftExtractor)),
        Language::Kotlin => Some(Box::new(kotlin::KotlinExtractor)),
        Language::Dart => Some(Box::new(dart::DartExtractor)),
        Language::Cpp => Some(Box::new(cpp::CppExtractor)),
        Language::C => Some(Box::new(c::CExtractor)),
        // Special extractors — handled via get_extractor_and_use_special
        Language::Svelte => Some(Box::new(svelte::SvelteExtractor)),
        Language::Vue => Some(Box::new(vue::VueExtractor)),
        Language::Razor => Some(Box::new(razor::RazorExtractor)),
        Language::Liquid => Some(Box::new(liquid::LiquidExtractor)),
        Language::Dfm => Some(Box::new(dfm::DfmExtractor)),
        // WASM-backed languages are handled via get_extractor_and_use_special (not here).
        Language::Lua | Language::Luau | Language::Pascal | Language::Scala => None,
        Language::Unknown => None,
    }
}

/// Build a minimal `ExtractionBatch` with only the file node, used for WASM-backed
/// languages whose tree-sitter grammar is not yet wired into the pipeline.
pub fn file_only_batch(source: &str, file_path: &Path) -> ExtractionBatch {
    use crate::ast_walker::timestamps_for_file;
    use crate::language_detector::detect_language;

    let file_path_str = file_path.to_string_lossy().to_string();
    let language = detect_language(file_path).unwrap_or(Language::Unknown);
    let file_id = format!("file:{file_path_str}");
    let line_count = source.lines().count() as u32;
    let content_hash = hex::encode(Sha256::digest(source.as_bytes()));
    let (modified_at, indexed_at) = timestamps_for_file(file_path);

    let nodes = vec![Node {
        id: file_id,
        name: file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<unknown>")
            .to_string(),
        qualified_name: file_path_str.clone(),
        kind: NodeKind::File,
        language: language.clone(),
        file_path: file_path_str.clone(),
        start_line: 1,
        end_line: line_count.max(1),
        start_col: 0,
        end_col: 0,
        is_exported: false,
        signature: None,
        docstring: None,
        metadata: None,
    }];

    ExtractionBatch {
        file: FileRecord {
            path: file_path.to_path_buf(),
            content_hash,
            language: format!("{language:?}"),
            size: source.len() as u64,
            modified_at,
            indexed_at,
            node_count: nodes.len() as u32,
            errors: Vec::new(),
        },
        nodes,
        edges: Vec::new(),
        unresolved_refs: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use crate::ast_walker::extract_file;
    use codewiki_core::NodeKind;
    use std::io::Write;

    /// Bug 8 fix: WASM-backed languages (Lua/Luau/Pascal/Scala) must emit at least
    /// a file node rather than returning zero nodes (GetExtractorResult::None).
    #[test]
    fn lua_extractor_emits_at_least_file_node() {
        let source = "-- Lua script\nlocal x = 1\nprint(x)\n";
        let mut f = tempfile::NamedTempFile::with_suffix(".lua").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);

        assert!(
            !batch.nodes.is_empty(),
            "Lua extractor returned zero nodes — expected at least a file node"
        );
        assert!(
            batch.nodes.iter().any(|n| n.kind == NodeKind::File),
            "No file node emitted for Lua source"
        );
    }
}

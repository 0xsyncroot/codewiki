//! T-407 — `codewiki_search` tool handler.
//!
//! Quick symbol search by name. Returns locations only (no code).

use crate::input_limits::validate_query;
use crate::tools::MAX_OUTPUT_LENGTH;
use codewiki_core::{CodeWikiError, NodeKind};
use codewiki_storage::{QueryHandle, SearchOptions};
use std::sync::Arc;

#[tracing::instrument(skip(handle), fields(query_len = %query.len(), limit))]
pub async fn handle_search(
    handle: Arc<dyn QueryHandle>,
    query: String,
    kind: Option<String>,
    limit: usize,
) -> Result<String, CodeWikiError> {
    validate_query(&query)?;

    let limit = limit.clamp(1, 100);

    let kinds: Option<Vec<NodeKind>> = kind
        .as_deref()
        .map(|k| parse_kind_filter(k).map(|nk| vec![nk]).unwrap_or_default());

    let opts = SearchOptions {
        limit,
        kinds,
        ..Default::default()
    };

    let results = handle.search_nodes(&query, opts)?;

    if results.is_empty() {
        return Ok(format!("No results found for '{query}'."));
    }

    let root = handle.root_path();
    let root_ref = root.as_deref();

    let mut out = format!("## Search Results for '{query}'\n\n");
    out.push_str(&crate::tools::root_header(root_ref));
    for (i, sr) in results.iter().enumerate() {
        let n = &sr.node;
        out.push_str(&format!(
            "{}. **{}** ({} {})\n   `{}` — line {}\n",
            i + 1,
            n.name,
            format_kind(&n.kind),
            format_language(&n.language),
            crate::tools::rel(&n.file_path, root_ref),
            n.start_line,
        ));
        if let Some(sig) = &n.signature {
            if !sig.is_empty() {
                out.push_str(&format!("   Signature: `{sig}`\n"));
            }
        }
        if let Some(doc) = &n.docstring {
            if !doc.is_empty() {
                let short = doc.lines().next().unwrap_or("").trim();
                out.push_str(&format!("   Doc: {short}\n"));
            }
        }
        out.push('\n');
    }

    Ok(crate::tools::truncate_output(out, MAX_OUTPUT_LENGTH))
}

fn parse_kind_filter(kind: &str) -> Option<NodeKind> {
    // Keep this in lockstep with the `kind` enum advertised in tools/mod.rs and
    // with `format_kind` below — an enum value with no arm here silently no-ops.
    match kind {
        "function" => Some(NodeKind::Function),
        "method" => Some(NodeKind::Method),
        "class" => Some(NodeKind::Class),
        "struct" => Some(NodeKind::Struct),
        "enum" => Some(NodeKind::Enum),
        "trait" => Some(NodeKind::Trait),
        "interface" => Some(NodeKind::Interface),
        "type" => Some(NodeKind::Type),
        "namespace" => Some(NodeKind::Namespace),
        "variable" => Some(NodeKind::Variable),
        "constant" => Some(NodeKind::Constant),
        "property" => Some(NodeKind::Property),
        "route" => Some(NodeKind::Route),
        "component" => Some(NodeKind::Component),
        _ => None,
    }
}

pub fn format_kind(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Function => "function",
        NodeKind::Method => "method",
        NodeKind::Class => "class",
        NodeKind::Interface => "interface",
        NodeKind::Enum => "enum",
        NodeKind::EnumMember => "enum_member",
        NodeKind::Struct => "struct",
        NodeKind::Trait => "trait",
        NodeKind::Type => "type",
        NodeKind::Variable => "variable",
        NodeKind::Constant => "constant",
        NodeKind::Module => "module",
        NodeKind::Namespace => "namespace",
        NodeKind::Property => "property",
        NodeKind::Field => "field",
        NodeKind::Constructor => "constructor",
        NodeKind::Getter => "getter",
        NodeKind::Setter => "setter",
        NodeKind::File => "file",
        NodeKind::Component => "component",
        NodeKind::Route => "route",
        NodeKind::Unknown => "unknown",
    }
}

pub fn format_language(lang: &codewiki_core::Language) -> &'static str {
    use codewiki_core::Language::*;
    match lang {
        TypeScript => "TypeScript",
        JavaScript => "JavaScript",
        Python => "Python",
        Go => "Go",
        Rust => "Rust",
        Java => "Java",
        Cpp => "C++",
        C => "C",
        CSharp => "C#",
        Php => "PHP",
        Ruby => "Ruby",
        Swift => "Swift",
        Kotlin => "Kotlin",
        Dart => "Dart",
        Scala => "Scala",
        Lua => "Lua",
        Luau => "Luau",
        Pascal => "Pascal",
        Svelte => "Svelte",
        Vue => "Vue",
        Razor => "Razor",
        Liquid => "Liquid",
        Dfm => "Delphi Form",
        Unknown => "unknown",
    }
}

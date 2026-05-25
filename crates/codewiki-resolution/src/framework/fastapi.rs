// T-307 — FastAPI framework resolver (full port)

use super::{make_resolved_edge, FrameworkExtractionResult, FrameworkResolver, ResolutionContext};
use crate::strip_comments::{strip_comments, CommentLang};
use codewiki_core::{CodeWikiError, Language, Node, NodeKind, UnresolvedRef};
use codewiki_storage::traits::ResolvedEdge;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

fn decorator_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // @x.get/post/put/patch/delete/options/head('/path')
        Regex::new(
            r#"@(\w+)\.(get|post|put|patch|delete|options|head)\s*\(\s*['"]([^'"]+)['"]"#,
        )
        .expect("fastapi decorator_regex")
    })
}

fn next_def_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?m)\n\s*(?:async\s+)?def\s+(\w+)"#).unwrap())
}

pub struct FastApiResolver;

impl FrameworkResolver for FastApiResolver {
    fn name(&self) -> &'static str { "fastapi" }
    fn languages(&self) -> Option<&'static [Language]> {
        static LANGS: &[Language] = &[Language::Python];
        Some(LANGS)
    }

    fn detect(&self, context: &ResolutionContext<'_>) -> bool {
        for f in &["requirements.txt", "pyproject.toml"] {
            if let Some(c) = context.read_file(f) {
                if c.to_lowercase().contains("fastapi") { return true; }
            }
        }
        for f in &["app.py", "main.py", "api.py"] {
            if let Some(c) = context.read_file(f) {
                if c.contains("FastAPI(") { return true; }
            }
        }
        false
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &ResolutionContext<'_>,
    ) -> Result<Option<ResolvedEdge>, CodeWikiError> {
        let name = &reference.reference_name;
        if name.ends_with("_router") || name == "router" {
            let candidates = context.get_nodes_by_name(name);
            if let Some(n) = candidates.first() {
                return Ok(Some(make_resolved_edge(reference, n.id.clone(), 0.8, self.name())));
            }
        }
        if name.starts_with("get_") {
            let candidates = context.get_nodes_by_name(name);
            let fn_node = candidates.iter().find(|n| n.kind == NodeKind::Function);
            if let Some(n) = fn_node {
                return Ok(Some(make_resolved_edge(reference, n.id.clone(), 0.75, self.name())));
            }
        }
        Ok(None)
    }

    fn extract(
        &self,
        file_path: &Path,
        content: &str,
        _context: &ResolutionContext<'_>,
    ) -> Result<FrameworkExtractionResult, CodeWikiError> {
        let file_str = file_path.to_string_lossy();
        if !file_str.ends_with(".py") { return Ok(FrameworkExtractionResult::empty()); }
        let safe = strip_comments(content, CommentLang::Python);
        let mut nodes = Vec::new();
        let mut unresolved_refs = Vec::new();

        for cap in decorator_regex().captures_iter(&safe) {
            let method = cap[2].to_uppercase();
            let route_path = &cap[3];
            let match_end = cap.get(0).unwrap().end();
            let line = safe[..cap.get(0).unwrap().start()].lines().count() as u32 + 1;

            // Find the handler function immediately after the decorator
            let tail = &safe[match_end..];
            let handler_name = next_def_regex()
                .captures(tail)
                .map(|c| c[1].to_string());

            let route_id = format!("route:{}:{}:{}:{}", file_str, line, method, route_path);
            nodes.push(Node {
                id: route_id.clone(),
                name: format!("{} {}", method, route_path),
                qualified_name: format!("{}::{}:{}", file_str, method, route_path),
                kind: NodeKind::Route,
                language: Language::Python,
                file_path: file_str.to_string(),
                start_line: line,
                end_line: line,
                ..Default::default()
            });

            if let Some(handler) = handler_name {
                unresolved_refs.push(UnresolvedRef {
                    id: format!("{}->{}", route_id, handler),
                    from_node_id: route_id,
                    reference_name: handler,
                    reference_kind: "references".to_string(),
                    file_path: file_str.to_string(),
                    line: Some(line),
                    ..Default::default()
                });
            }
        }

        Ok(FrameworkExtractionResult { nodes, edges: Vec::new(), unresolved_refs })
    }
}

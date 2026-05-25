// T-307 — Flask framework resolver (full port)

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
        // @x.route('/path', methods=[...])
        // followed by: def handler_name
        Regex::new(
            r#"@(\w+)\.route\s*\(\s*['"]([^'"]+)['"](?:[^)]*)\)\s*\n\s*(?:async\s+)?def\s+(\w+)"#,
        )
        .expect("flask decorator_regex")
    })
}

pub struct FlaskResolver;

impl FrameworkResolver for FlaskResolver {
    fn name(&self) -> &'static str {
        "flask"
    }
    fn languages(&self) -> Option<&'static [Language]> {
        static LANGS: &[Language] = &[Language::Python];
        Some(LANGS)
    }

    fn detect(&self, context: &ResolutionContext<'_>) -> bool {
        for f in &["requirements.txt", "pyproject.toml"] {
            if let Some(c) = context.read_file(f) {
                if c.to_lowercase().contains("flask") {
                    return true;
                }
            }
        }
        for f in &["app.py", "application.py", "main.py", "__init__.py"] {
            if let Some(c) = context.read_file(f) {
                if c.contains("Flask(__name__)") {
                    return true;
                }
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
        if name.ends_with("_bp") || name.ends_with("_blueprint") {
            let candidates = context.get_nodes_by_name(name);
            if let Some(n) = candidates.first() {
                return Ok(Some(make_resolved_edge(
                    reference,
                    n.id.clone(),
                    0.8,
                    self.name(),
                )));
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
        if !file_str.ends_with(".py") {
            return Ok(FrameworkExtractionResult::empty());
        }
        let safe = strip_comments(content, CommentLang::Python);
        let mut nodes = Vec::new();
        let mut unresolved_refs = Vec::new();

        for cap in decorator_regex().captures_iter(&safe) {
            let route_path = &cap[2];
            let handler_name = &cap[3];
            let line = safe[..cap.get(0).unwrap().start()].lines().count() as u32 + 1;
            let route_id = format!("route:{}:{}:GET:{}", file_str, line, route_path);
            nodes.push(Node {
                id: route_id.clone(),
                name: format!("GET {}", route_path),
                qualified_name: format!("{}::GET:{}", file_str, route_path),
                kind: NodeKind::Route,
                language: Language::Python,
                file_path: file_str.to_string(),
                start_line: line,
                end_line: line,
                ..Default::default()
            });
            unresolved_refs.push(UnresolvedRef {
                id: format!("{}->{}", route_id, handler_name),
                from_node_id: route_id,
                reference_name: handler_name.to_string(),
                reference_kind: "references".to_string(),
                file_path: file_str.to_string(),
                line: Some(line),
                ..Default::default()
            });
        }

        Ok(FrameworkExtractionResult {
            nodes,
            edges: Vec::new(),
            unresolved_refs,
        })
    }
}

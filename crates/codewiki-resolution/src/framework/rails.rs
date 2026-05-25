// T-307 — Rails framework resolver (full port)

use super::{make_resolved_edge, FrameworkExtractionResult, FrameworkResolver, ResolutionContext};
use crate::strip_comments::{strip_comments, CommentLang};
use codewiki_core::{CodeWikiError, Language, Node, NodeKind, UnresolvedRef};
use codewiki_storage::traits::ResolvedEdge;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

fn route_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // get '/path', to: 'controller#action'
        // get '/path' => 'controller#action'
        Regex::new(
            r#"\b(get|post|put|patch|delete|match)\s+['"]([^'"]+)['"]\s*(?:,\s*to:\s*|=>\s*)['"]([^#'"]+)#([^'"]+)['"]\s*"#,
        )
        .expect("rails route_regex")
    })
}

fn camel_to_snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_lowercase().next().unwrap_or(c));
    }
    out
}

pub struct RailsResolver;

impl FrameworkResolver for RailsResolver {
    fn name(&self) -> &'static str { "rails" }
    fn languages(&self) -> Option<&'static [Language]> {
        static LANGS: &[Language] = &[Language::Ruby];
        Some(LANGS)
    }

    fn detect(&self, context: &ResolutionContext<'_>) -> bool {
        if let Some(gemfile) = context.read_file("Gemfile") {
            if gemfile.contains("'rails'") || gemfile.contains("\"rails\"") {
                return true;
            }
        }
        context.file_exists("config/application.rb")
            || context.file_exists("app/controllers/application_controller.rb")
            || context.file_exists("config/routes.rb")
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &ResolutionContext<'_>,
    ) -> Result<Option<ResolvedEdge>, CodeWikiError> {
        let name = &reference.reference_name;

        // PascalCase → model lookup
        if name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
            && !name.contains('_')
        {
            if let Some(id) = resolve_rails_model(name, context) {
                return Ok(Some(make_resolved_edge(reference, id, 0.8, self.name())));
            }
        }

        if name.ends_with("Controller") {
            let candidates = context.get_nodes_by_name(name);
            let ctrl = candidates.iter().find(|n| {
                n.kind == NodeKind::Class && n.file_path.contains("controllers/")
            });
            if let Some(n) = ctrl {
                return Ok(Some(make_resolved_edge(reference, n.id.clone(), 0.85, self.name())));
            }
        }

        if name.ends_with("Helper") || name.ends_with("Service") || name.ends_with("Job") {
            let candidates = context.get_nodes_by_name(name);
            if let Some(n) = candidates.first() {
                return Ok(Some(make_resolved_edge(reference, n.id.clone(), 0.8, self.name())));
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
        if !file_str.ends_with(".rb") { return Ok(FrameworkExtractionResult::empty()); }
        let safe = strip_comments(content, CommentLang::Ruby);
        let mut nodes = Vec::new();
        let mut unresolved_refs = Vec::new();

        for cap in route_regex().captures_iter(&safe) {
            let method = &cap[1];
            let route_path = &cap[2];
            let _controller = &cap[3];
            let action = &cap[4];
            let line = safe[..cap.get(0).unwrap().start()].lines().count() as u32 + 1;
            let upper = method.to_uppercase();
            let route_id = format!("route:{}:{}:{}:{}", file_str, line, upper, route_path);
            nodes.push(Node {
                id: route_id.clone(),
                name: format!("{} {}", upper, route_path),
                qualified_name: format!("{}::route:{}", file_str, route_path),
                kind: NodeKind::Route,
                language: Language::Ruby,
                file_path: file_str.to_string(),
                start_line: line,
                end_line: line,
                ..Default::default()
            });
            unresolved_refs.push(UnresolvedRef {
                id: format!("{}->{}", route_id, action),
                from_node_id: route_id,
                reference_name: action.to_string(),
                reference_kind: "references".to_string(),
                file_path: file_str.to_string(),
                line: Some(line),
                ..Default::default()
            });
        }

        Ok(FrameworkExtractionResult { nodes, edges: Vec::new(), unresolved_refs })
    }
}

fn resolve_rails_model(name: &str, context: &ResolutionContext<'_>) -> Option<String> {
    let snake = camel_to_snake(name);
    for path in &[
        format!("app/models/{}.rb", snake),
        format!("app/models/concerns/{}.rb", snake),
    ] {
        if context.file_exists(path) {
            let nodes = context.get_nodes_in_file(path);
            if let Some(n) = nodes.iter().find(|n| n.kind == NodeKind::Class && n.name == name) {
                return Some(n.id.clone());
            }
        }
    }
    let candidates = context.get_nodes_by_name(name);
    candidates.iter().find(|n| n.kind == NodeKind::Class && n.file_path.contains("app/models/"))
        .map(|n| n.id.clone())
}

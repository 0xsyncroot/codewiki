// T-307 — Django framework resolver (full port)
//
// Detects: requirements.txt / setup.py / pyproject.toml contains "django",
//          or manage.py exists.
// Extracts: path('url', handler, ...) / re_path / url() → route nodes.
// Resolves: XxxModel, XxxView, XxxViewSet, XxxForm → class lookup.

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
        // path('url', handler, name=...) / re_path(r'...', handler) / url(r'...', handler)
        // Handler may contain a single balanced () pair (e.g. View.as_view(), include('x.y'))
        Regex::new(
            r#"\b(?:path|re_path|url)\s*\(\s*r?['"]([^'"]+)['"]\s*,\s*([\w.]+(?:\s*\([^)]*\))?)"#,
        )
        .expect("django route_regex")
    })
}

fn resolve_handler_name(expr: &str) -> Option<(String, &'static str)> {
    // include('module.path')
    let include_re: &Regex = {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r#"^include\s*\(\s*['"]([^'"]+)['"]\s*\)"#).unwrap())
    };
    if let Some(cap) = include_re.captures(expr) {
        return Some((cap[1].to_string(), "imports"));
    }

    // Strip .as_view(...) and trailing method calls
    let mut head = expr.trim().to_string();
    // Remove trailing .as_view(...)
    if let Some(pos) = head.rfind(".as_view") {
        head.truncate(pos);
    } else {
        // Remove any trailing .method_call(...)
        let method_re: &Regex = {
            static RE: OnceLock<Regex> = OnceLock::new();
            RE.get_or_init(|| Regex::new(r#"\.\w+\s*\([^)]*\)\s*$"#).unwrap())
        };
        if let Some(m) = method_re.find(&head.clone()) {
            head.truncate(m.start());
        }
    }

    let parts: Vec<&str> = head.split('.').filter(|s| !s.is_empty()).collect();
    let last = parts.last()?;
    if last.chars().all(|c| c.is_alphanumeric() || c == '_') {
        Some((last.to_string(), "references"))
    } else {
        None
    }
}

pub struct DjangoResolver;

impl FrameworkResolver for DjangoResolver {
    fn name(&self) -> &'static str {
        "django"
    }

    fn languages(&self) -> Option<&'static [Language]> {
        static LANGS: &[Language] = &[Language::Python];
        Some(LANGS)
    }

    fn detect(&self, context: &ResolutionContext<'_>) -> bool {
        for f in &["requirements.txt", "setup.py", "pyproject.toml"] {
            if let Some(content) = context.read_file(f) {
                if content.to_lowercase().contains("django") {
                    return true;
                }
            }
        }
        context.file_exists("manage.py")
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &ResolutionContext<'_>,
    ) -> Result<Option<ResolvedEdge>, CodeWikiError> {
        let name = &reference.reference_name;

        if name.ends_with("Model") || name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
            if let Some(id) = resolve_by_name_kind(name, &[NodeKind::Class], &["models", "app/models"], context) {
                return Ok(Some(make_resolved_edge(reference, id, 0.8, self.name())));
            }
        }

        if name.ends_with("View") || name.ends_with("ViewSet") {
            if let Some(id) = resolve_by_name_kind(
                name,
                &[NodeKind::Class, NodeKind::Function],
                &["views", "app/views", "api/views"],
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.8, self.name())));
            }
        }

        if name.ends_with("Form") {
            if let Some(id) = resolve_by_name_kind(name, &[NodeKind::Class], &["forms", "app/forms"], context) {
                return Ok(Some(make_resolved_edge(reference, id, 0.8, self.name())));
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

        for cap in route_regex().captures_iter(&safe) {
            let url_path = &cap[1];
            let handler_expr = &cap[2];
            let line = safe[..cap.get(0).unwrap().start()].lines().count() as u32 + 1;

            let route_id = format!("route:{}:{}:{}", file_str, line, url_path);
            let route_node = Node {
                id: route_id.clone(),
                name: url_path.to_string(),
                qualified_name: format!("{}::route:{}", file_str, url_path),
                kind: NodeKind::Route,
                language: Language::Python,
                file_path: file_str.to_string(),
                start_line: line,
                end_line: line,
                ..Default::default()
            };
            nodes.push(route_node);

            if let Some((target, kind)) = resolve_handler_name(handler_expr.trim()) {
                unresolved_refs.push(UnresolvedRef {
                    id: format!("{}->{}", route_id, target),
                    from_node_id: route_id,
                    reference_name: target,
                    reference_kind: kind.to_string(),
                    file_path: file_str.to_string(),
                    line: Some(line),
                    ..Default::default()
                });
            }
        }

        Ok(FrameworkExtractionResult { nodes, edges: Vec::new(), unresolved_refs })
    }
}

// ─── Shared helper ───────────────────────────────────────────────────────────

pub(crate) fn resolve_by_name_kind(
    name: &str,
    kinds: &[NodeKind],
    preferred_dirs: &[&str],
    context: &ResolutionContext<'_>,
) -> Option<String> {
    let candidates = context.get_nodes_by_name(name);
    let kind_filtered: Vec<_> = candidates.iter().filter(|n| kinds.contains(&n.kind)).collect();
    if kind_filtered.is_empty() {
        return None;
    }
    if !preferred_dirs.is_empty() {
        for dir in preferred_dirs {
            if let Some(node) = kind_filtered.iter().find(|n| n.file_path.contains(dir)) {
                return Some(node.id.clone());
            }
        }
    }
    kind_filtered.first().map(|n| n.id.clone())
}

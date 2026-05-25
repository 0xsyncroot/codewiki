// T-307 — Express framework resolver (full port)
//
// Handles Express and general Node.js route patterns.
// Detects: package.json contains "express", "fastify", "koa", or "hapi".
// Extracts: (app|router).METHOD('/path', ...handlers) → route nodes + refs.
// Resolves: middleware names, Controller.method patterns, Service.method patterns.

use super::{make_resolved_edge, FrameworkExtractionResult, FrameworkResolver, ResolutionContext};
use crate::strip_comments::{strip_comments, CommentLang};
use codewiki_core::{CodeWikiError, Language, Node, NodeKind, UnresolvedRef};
use codewiki_storage::traits::ResolvedEdge;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

// ─── Regexes ──────────────────────────────────────────────────────────────────

fn route_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // (app|router).METHOD('/path', ...handlers)
        // Handler is the last comma-separated argument before the closing paren.
        Regex::new(
            r#"(?:app|router)\.(get|post|put|patch|delete|all|use)\s*\(\s*['"`]([^'"`]+)['"`]\s*,\s*([^)]+)\)"#,
        )
        .expect("route_regex")
    })
}

fn controller_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"^(\w+)Controller\.(\w+)$"#).expect("controller_regex"))
}

fn service_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"^(\w+)(Service|Helper|Utils?)\.(\w+)$"#).expect("service_regex")
    })
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Extract the trailing identifier from a handler expression.
/// e.g. `getUsers`, `userController.list`, `userController.list.bind(this)` → `list`.
fn extract_tail_ident(expr: &str) -> Option<String> {
    let cleaned = expr.trim().trim_end_matches("()").trim();
    let re: &Regex = {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r#"(?:\.|^)([A-Za-z_][A-Za-z0-9_]*)$"#).unwrap())
    };
    re.captures(cleaned).map(|c| c[1].to_string())
}

fn detect_language_from_path(file_path: &str) -> CommentLang {
    if file_path.ends_with(".ts") || file_path.ends_with(".tsx") {
        CommentLang::TypeScript
    } else {
        CommentLang::JavaScript
    }
}

fn is_middleware_name(name: &str) -> bool {
    let patterns = [
        "auth",
        "authenticate",
        "authorization",
        "validate",
        "sanitize",
        "rateLimit",
        "cors",
        "helmet",
        "logger",
        "errorHandler",
        "notFound",
    ];
    let lower = name.to_lowercase();
    patterns.iter().any(|p| lower == p.to_lowercase()) || lower.ends_with("middleware")
}

// ─── Resolver ─────────────────────────────────────────────────────────────────

pub struct ExpressResolver;

impl FrameworkResolver for ExpressResolver {
    fn name(&self) -> &'static str {
        "express"
    }

    fn languages(&self) -> Option<&'static [Language]> {
        static LANGS: &[Language] = &[Language::TypeScript, Language::JavaScript];
        Some(LANGS)
    }

    fn detect(&self, context: &ResolutionContext<'_>) -> bool {
        // Check package.json for known web framework packages
        if let Some(pkg_raw) = context.read_file("package.json") {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&pkg_raw) {
                let deps = {
                    let mut d = serde_json::Map::new();
                    if let Some(x) = pkg.get("dependencies").and_then(|v| v.as_object()) {
                        d.extend(x.clone());
                    }
                    if let Some(x) = pkg.get("devDependencies").and_then(|v| v.as_object()) {
                        d.extend(x.clone());
                    }
                    d
                };
                if deps.contains_key("express")
                    || deps.contains_key("fastify")
                    || deps.contains_key("koa")
                    || deps.contains_key("hapi")
                {
                    return true;
                }
            }
        }

        // Scan common route-containing files
        for file in context.known_files {
            if file.contains("routes")
                || file.contains("controllers")
                || file.contains("middleware")
            {
                if let Some(content) = context.read_file(file) {
                    if content.contains("express")
                        || content.contains("app.get")
                        || content.contains("router.get")
                    {
                        return true;
                    }
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

        // Pattern 1: middleware names
        if is_middleware_name(name) {
            let candidates = context.get_nodes_by_name(name);
            let target = candidates.iter().find(|n| {
                n.name.to_lowercase() == name.to_lowercase()
                    || n.name
                        .to_lowercase()
                        .ends_with(&format!("{}middleware", name.to_lowercase()))
            });
            if let Some(node) = target {
                return Ok(Some(make_resolved_edge(
                    reference,
                    node.id.clone(),
                    0.8,
                    self.name(),
                )));
            }
        }

        // Pattern 2: XxxController.method
        if let Some(cap) = controller_regex().captures(name) {
            let controller = &cap[1];
            let method = &cap[2];
            // Find method in files matching controller name
            let method_candidates = context.get_nodes_by_name(method);
            let target = method_candidates.iter().find(|n| {
                (n.kind == NodeKind::Method || n.kind == NodeKind::Function)
                    && n.file_path
                        .to_lowercase()
                        .contains(&controller.to_lowercase())
            });
            if let Some(node) = target {
                return Ok(Some(make_resolved_edge(
                    reference,
                    node.id.clone(),
                    0.85,
                    self.name(),
                )));
            }
        }

        // Pattern 3: XxxService.method / XxxHelper.method / XxxUtils.method
        if let Some(cap) = service_regex().captures(name) {
            let service_base = format!("{}{}", &cap[1], &cap[2]);
            let method = &cap[3];
            let method_candidates = context.get_nodes_by_name(method);
            let stripped_lower = service_base
                .to_lowercase()
                .replace("service", "")
                .replace("helper", "")
                .replace("utils", "")
                .replace("util", "");
            let target = method_candidates.iter().find(|n| {
                (n.kind == NodeKind::Method || n.kind == NodeKind::Function)
                    && n.file_path.to_lowercase().contains(&stripped_lower)
            });
            if let Some(node) = target {
                return Ok(Some(make_resolved_edge(
                    reference,
                    node.id.clone(),
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

        // Only process JS/TS files
        if !file_str.ends_with(".js")
            && !file_str.ends_with(".ts")
            && !file_str.ends_with(".jsx")
            && !file_str.ends_with(".tsx")
            && !file_str.ends_with(".mjs")
            && !file_str.ends_with(".cjs")
        {
            return Ok(FrameworkExtractionResult::empty());
        }

        let lang = detect_language_from_path(&file_str);
        let safe = strip_comments(content, lang);

        let mut nodes = Vec::new();
        let mut unresolved_refs = Vec::new();

        for cap in route_regex().captures_iter(&safe) {
            let method = &cap[1];
            let route_path = &cap[2];
            let handlers_raw = &cap[3];

            // Skip non-path `use` calls (e.g. app.use(middleware))
            if method == "use" && !route_path.starts_with('/') {
                continue;
            }

            let line = safe[..cap.get(0).unwrap().start()].lines().count() as u32 + 1;

            let route_id = format!(
                "route:{}:{}:{}:{}",
                file_str,
                line,
                method.to_uppercase(),
                route_path
            );

            let route_node = Node {
                id: route_id.clone(),
                name: format!("{} {}", method.to_uppercase(), route_path),
                qualified_name: format!("{}::{}:{}", file_str, method.to_uppercase(), route_path),
                kind: NodeKind::Route,
                language: if file_str.ends_with(".ts") || file_str.ends_with(".tsx") {
                    Language::TypeScript
                } else {
                    Language::JavaScript
                },
                file_path: file_str.to_string(),
                start_line: line,
                end_line: line,
                ..Default::default()
            };
            nodes.push(route_node);

            // Handler is the LAST comma-separated argument
            let parts: Vec<&str> = handlers_raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            if let Some(last) = parts.last() {
                if let Some(handler_name) = extract_tail_ident(last) {
                    unresolved_refs.push(UnresolvedRef {
                        id: format!("{}->{}", route_id, handler_name),
                        from_node_id: route_id,
                        reference_name: handler_name,
                        reference_kind: "references".to_string(),
                        file_path: file_str.to_string(),
                        line: Some(line),
                        ..Default::default()
                    });
                }
            }
        }

        Ok(FrameworkExtractionResult {
            nodes,
            edges: Vec::new(),
            unresolved_refs,
        })
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caches::ResolverCaches;
    use crate::path_aliases::PathAliasMap;
    use std::path::Path;

    fn make_ctx<'a>(
        caches: &'a ResolverCaches,
        aliases: &'a PathAliasMap,
        known_files: &'a [String],
    ) -> ResolutionContext<'a> {
        ResolutionContext {
            project_root: Path::new("/project"),
            project_languages: &[Language::TypeScript, Language::JavaScript],
            caches,
            path_aliases: aliases,
            known_files,
        }
    }

    #[test]
    fn express_extracts_route_and_handler_ref() {
        // T-311: express resolver detects routes in `app.get('/foo', handler)` snippet
        let content = "app.get('/foo', getUsers);\napp.post('/bar', createUser);";
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);

        let resolver = ExpressResolver;
        let result = resolver
            .extract(Path::new("routes.ts"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 2, "two route nodes");
        assert!(result.nodes[0].name.contains("GET"), "method in node name");
        assert!(result.nodes[0].name.contains("/foo"), "path in node name");

        let refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(refs.contains(&"getUsers"), "getUsers ref emitted");
        assert!(refs.contains(&"createUser"), "createUser ref emitted");
    }

    #[test]
    fn express_detect_via_package_json() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let pkg = dir.path().join("package.json");
        fs::write(&pkg, r#"{"dependencies":{"express":"^4.18.0"}}"#).unwrap();

        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = ResolutionContext {
            project_root: dir.path(),
            project_languages: &[Language::TypeScript],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &[],
        };

        let resolver = ExpressResolver;
        assert!(resolver.detect(&ctx), "must detect express project");
    }
}

// T-307 — Laravel framework resolver (full port)
//
// Detects: artisan, app/Http/Controllers/Controller.php, or routes/web.php.
// Extracts:
//   Pattern A: Route::METHOD('/path', handler) → route node + handler UnresolvedRef
//   Pattern B: Route::resource/apiResource('name', Controller::class) → route node + ref
// Resolves: Controller@method, Model::method, and class-suffix patterns.

use super::{make_resolved_edge, FrameworkExtractionResult, FrameworkResolver, ResolutionContext};
use crate::strip_comments::{strip_comments, CommentLang};
use codewiki_core::{CodeWikiError, Language, Node, NodeKind, UnresolvedRef};
use codewiki_storage::traits::ResolvedEdge;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

// ─── Regexes ─────────────────────────────────────────────────────────────────

/// Route::METHOD('/path', handler-expr)
fn route_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"Route::(get|post|put|patch|delete|options|any)\s*\(\s*['"]([^'"]+)['"]\s*,\s*([^)]+)\)"#,
        )
        .expect("laravel route_regex")
    })
}

/// Route::resource('name', Controller::class)
fn resource_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"Route::(resource|apiResource)\s*\(\s*['"]([^'"]+)['"]\s*(?:,\s*([^)]+))?\)"#)
            .expect("laravel resource_regex")
    })
}

/// Controller@method pattern used in resolve()
fn controller_at_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"^([A-Za-z_][A-Za-z0-9_]*)@([A-Za-z_][A-Za-z0-9_]*)$"#)
            .expect("laravel controller_at_regex")
    })
}

/// ClassName::method pattern used in resolve()
fn model_call_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"^([A-Za-z_][A-Za-z0-9_]*)::([A-Za-z_][A-Za-z0-9_]*)$"#)
            .expect("laravel model_call_regex")
    })
}

// ─── Handler expression parser ────────────────────────────────────────────────

/// Parse a Laravel handler expression to the symbol name that should be linked.
/// Returns `(name, reference_kind)`:
///   - `[Controller::class, 'method']`  → `("method", "references")`
///   - `'Controller@method'`            → `("method", "references")`
///   - `Controller::class`              → `("Controller", "imports")`
///   - closure / unrecognized           → `None`
fn extract_laravel_handler(expr: &str) -> Option<(String, &'static str)> {
    let trimmed = expr.trim();

    // [ClassName::class, 'method'] or [ClassName::class, "method"]
    static TUPLE_RE: OnceLock<Regex> = OnceLock::new();
    let tuple_re = TUPLE_RE.get_or_init(|| {
        Regex::new(r#"^\[\s*[^,]+,\s*['"]([^'"]+)['"]\s*\]"#).expect("laravel tuple_re")
    });
    if let Some(cap) = tuple_re.captures(trimmed) {
        return Some((cap[1].to_string(), "references"));
    }

    // 'Controller@method' or "Controller@method"
    static AT_RE: OnceLock<Regex> = OnceLock::new();
    let at_re =
        AT_RE.get_or_init(|| Regex::new(r#"^['"][^'"@]+@([^'"]+)['"]$"#).expect("laravel at_re"));
    if let Some(cap) = at_re.captures(trimmed) {
        return Some((cap[1].to_string(), "references"));
    }

    // ClassName::class
    static CLASS_RE: OnceLock<Regex> = OnceLock::new();
    let class_re = CLASS_RE.get_or_init(|| {
        Regex::new(r#"^([A-Za-z_][A-Za-z0-9_]*)::class"#).expect("laravel class_re")
    });
    if let Some(cap) = class_re.captures(trimmed) {
        return Some((cap[1].to_string(), "imports"));
    }

    None
}

// ─── Resolver ─────────────────────────────────────────────────────────────────

pub struct LaravelResolver;

impl FrameworkResolver for LaravelResolver {
    fn name(&self) -> &'static str {
        "laravel"
    }

    fn languages(&self) -> Option<&'static [Language]> {
        static LANGS: &[Language] = &[Language::Php];
        Some(LANGS)
    }

    fn detect(&self, context: &ResolutionContext<'_>) -> bool {
        context.file_exists("artisan")
            || context.file_exists("app/Http/Controllers/Controller.php")
            || context.file_exists("routes/web.php")
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &ResolutionContext<'_>,
    ) -> Result<Option<ResolvedEdge>, CodeWikiError> {
        let name = &reference.reference_name;

        // Pattern: Controller@method → look up the method in the controller file
        if let Some(cap) = controller_at_regex().captures(name) {
            let controller = &cap[1];
            let method = &cap[2];
            let method_candidates = context.get_nodes_by_name(method);
            let ctrl_lower = controller.to_lowercase();
            let target = method_candidates.iter().find(|n| {
                (n.kind == NodeKind::Method || n.kind == NodeKind::Function)
                    && n.file_path.to_lowercase().contains(&ctrl_lower)
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

        // Pattern: Model::staticMethod → look up in model files
        if let Some(cap) = model_call_regex().captures(name) {
            let class_name = &cap[1];
            let method = &cap[2];
            let class_lower = class_name.to_lowercase();
            // Try method node first
            let method_candidates = context.get_nodes_by_name(method);
            let target = method_candidates.iter().find(|n| {
                (n.kind == NodeKind::Method || n.kind == NodeKind::Function)
                    && (n.file_path.to_lowercase().contains("models")
                        || n.file_path.to_lowercase().contains(&class_lower))
            });
            if let Some(node) = target {
                return Ok(Some(make_resolved_edge(
                    reference,
                    node.id.clone(),
                    0.80,
                    self.name(),
                )));
            }
            // Fall back to class node
            let class_candidates = context.get_nodes_by_name(class_name);
            let target = class_candidates.iter().find(|n| n.kind == NodeKind::Class);
            if let Some(node) = target {
                return Ok(Some(make_resolved_edge(
                    reference,
                    node.id.clone(),
                    0.70,
                    self.name(),
                )));
            }
        }

        // Pattern: plain class name ending in Controller or class reference
        if name.ends_with("Controller") || reference.reference_kind == "imports" {
            let candidates = context.get_nodes_by_name(name);
            if let Some(n) = candidates.iter().find(|n| n.kind == NodeKind::Class) {
                return Ok(Some(make_resolved_edge(
                    reference,
                    n.id.clone(),
                    0.80,
                    self.name(),
                )));
            }
        }

        // Pattern: plain method name — look up in controller files
        if !name.contains("::") && !name.contains('@') {
            let candidates = context.get_nodes_by_name(name);
            let target = candidates.iter().find(|n| {
                (n.kind == NodeKind::Method || n.kind == NodeKind::Function)
                    && n.file_path.to_lowercase().contains("controller")
            });
            if let Some(node) = target {
                return Ok(Some(make_resolved_edge(
                    reference,
                    node.id.clone(),
                    0.75,
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
        if !file_str.ends_with(".php") {
            return Ok(FrameworkExtractionResult::empty());
        }

        let safe = strip_comments(content, CommentLang::Php);
        let mut nodes = Vec::new();
        let mut unresolved_refs = Vec::new();

        // Pattern A: Route::METHOD('/path', handler)
        for cap in route_regex().captures_iter(&safe) {
            let method = cap[1].to_uppercase();
            let route_path = &cap[2];
            let handler_expr = cap[3].to_string();
            let line = safe[..cap.get(0).unwrap().start()].lines().count() as u32 + 1;

            let route_id = format!("route:{}:{}:{}:{}", file_str, line, method, route_path);

            let route_node = Node {
                id: route_id.clone(),
                name: format!("{} {}", method, route_path),
                // F6: qualified_name matches express.rs — "{file}::{METHOD}:{path}"
                qualified_name: format!("{}::{}:{}", file_str, method, route_path),
                kind: NodeKind::Route,
                language: Language::Php,
                file_path: file_str.to_string(),
                start_line: line,
                end_line: line,
                ..Default::default()
            };
            nodes.push(route_node);

            if let Some((handler_name, ref_kind)) = extract_laravel_handler(&handler_expr) {
                // F7: UnresolvedRef.id = "{route_id}->{reference_name}"
                unresolved_refs.push(UnresolvedRef {
                    id: format!("{}->{}", route_id, handler_name),
                    from_node_id: route_id,
                    reference_name: handler_name,
                    reference_kind: ref_kind.to_string(),
                    file_path: file_str.to_string(),
                    line: Some(line),
                    ..Default::default()
                });
            }
        }

        // Pattern B: Route::resource/apiResource
        for cap in resource_regex().captures_iter(&safe) {
            let resource_name = &cap[2];
            let handler_expr = cap
                .get(3)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let line = safe[..cap.get(0).unwrap().start()].lines().count() as u32 + 1;

            let route_id = format!("route:{}:{}:RESOURCE:{}", file_str, line, resource_name);

            let route_node = Node {
                id: route_id.clone(),
                name: format!("resource:{}", resource_name),
                qualified_name: format!("{}::RESOURCE:{}", file_str, resource_name),
                kind: NodeKind::Route,
                language: Language::Php,
                file_path: file_str.to_string(),
                start_line: line,
                end_line: line,
                ..Default::default()
            };
            nodes.push(route_node);

            if !handler_expr.is_empty() {
                if let Some((handler_name, ref_kind)) = extract_laravel_handler(&handler_expr) {
                    unresolved_refs.push(UnresolvedRef {
                        id: format!("{}->{}", route_id, handler_name),
                        from_node_id: route_id,
                        reference_name: handler_name,
                        reference_kind: ref_kind.to_string(),
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
            project_languages: &[Language::Php],
            caches,
            path_aliases: aliases,
            known_files,
        }
    }

    #[test]
    fn laravel_extracts_array_tuple_route() {
        let content = "Route::get('/users', [UserController::class, 'index']);";
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let resolver = LaravelResolver;
        let result = resolver
            .extract(Path::new("routes/web.php"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 1, "one route node");
        assert_eq!(result.nodes[0].name, "GET /users");
        assert_eq!(result.nodes[0].qualified_name, "routes/web.php::GET:/users");
        assert_eq!(result.unresolved_refs.len(), 1, "one handler ref");
        assert_eq!(result.unresolved_refs[0].reference_name, "index");
        assert_eq!(result.unresolved_refs[0].reference_kind, "references");
        // F7: check id format
        assert!(result.unresolved_refs[0].id.ends_with("->index"));
    }

    #[test]
    fn laravel_extracts_at_syntax_route() {
        let content = "Route::get('/users/{id}', 'UserController@show');";
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let resolver = LaravelResolver;
        let result = resolver
            .extract(Path::new("routes/web.php"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "GET /users/{id}");
        assert_eq!(result.unresolved_refs.len(), 1);
        assert_eq!(result.unresolved_refs[0].reference_name, "show");
        assert_eq!(result.unresolved_refs[0].reference_kind, "references");
    }

    #[test]
    fn laravel_extracts_resource_route() {
        let content = "Route::resource('photos', PhotoController::class);";
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let resolver = LaravelResolver;
        let result = resolver
            .extract(Path::new("routes/web.php"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "resource:photos");
        assert_eq!(result.unresolved_refs.len(), 1);
        assert_eq!(result.unresolved_refs[0].reference_name, "PhotoController");
        assert_eq!(result.unresolved_refs[0].reference_kind, "imports");
    }

    #[test]
    fn laravel_extracts_api_resource_route() {
        let content = "Route::apiResource('users', UserController::class);";
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let resolver = LaravelResolver;
        let result = resolver
            .extract(Path::new("routes/api.php"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "resource:users");
        assert_eq!(result.unresolved_refs[0].reference_name, "UserController");
    }

    #[test]
    fn laravel_extracts_post_route_with_class_ref() {
        let content = "Route::post('/users', [UserController::class, 'store']);";
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let resolver = LaravelResolver;
        let result = resolver
            .extract(Path::new("routes/web.php"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes[0].name, "POST /users");
        assert_eq!(result.unresolved_refs[0].reference_name, "store");
    }

    #[test]
    fn laravel_skips_non_php_files() {
        let content = "Route::get('/users', [UserController::class, 'index']);";
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let resolver = LaravelResolver;
        let result = resolver
            .extract(Path::new("routes/web.js"), content, &ctx)
            .expect("extract ok");

        assert!(
            result.nodes.is_empty(),
            "non-PHP file must produce no nodes"
        );
    }

    #[test]
    fn laravel_detect_via_artisan() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("artisan"), "#!/usr/bin/env php").unwrap();

        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = ResolutionContext {
            project_root: dir.path(),
            project_languages: &[Language::Php],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &[],
        };

        let resolver = LaravelResolver;
        assert!(resolver.detect(&ctx), "artisan file must trigger detect");
    }

    #[test]
    fn laravel_handler_tuple_extraction() {
        let result = extract_laravel_handler("[UserController::class, 'index']");
        assert_eq!(result, Some(("index".to_string(), "references")));
    }

    #[test]
    fn laravel_handler_at_syntax_extraction() {
        let result = extract_laravel_handler("'UserController@show'");
        assert_eq!(result, Some(("show".to_string(), "references")));
    }

    #[test]
    fn laravel_handler_class_extraction() {
        let result = extract_laravel_handler("PhotoController::class");
        assert_eq!(result, Some(("PhotoController".to_string(), "imports")));
    }

    #[test]
    fn laravel_handler_closure_returns_none() {
        let result = extract_laravel_handler("function() { return 'hello'; }");
        assert!(result.is_none(), "closure must not produce a handler name");
    }
}

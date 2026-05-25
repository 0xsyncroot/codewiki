// T-308 — SvelteKit file-based routing resolver (full port)
//
// Detection: svelte / @sveltejs/kit in deps/devDeps; fallback .svelte file scan.
// Extraction: full SvelteKit route-file table (12 variants); +server.ts per-method nodes.
// Resolution: rune self-refs, store $subscriptions, $lib/ alias, PascalCase components.
//
// Review fixes applied:
//   F3  — known_files element type verified as &[String]
//   F6  — qualified_name = "{file}::{METHOD}:{path}" for server method routes
//   F7  — UnresolvedRef.id = "{route_id}->{reference_name}"
//
// Boundary: do NOT emit Component nodes — extraction-side svelte.rs already does that.

use super::{make_resolved_edge, FrameworkExtractionResult, FrameworkResolver, ResolutionContext};
use codewiki_core::{CodeWikiError, Language, Node, NodeKind, UnresolvedRef};
use codewiki_storage::traits::ResolvedEdge;
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

// ─── Statics ──────────────────────────────────────────────────────────────────

/// Svelte 5 runes (compiler-provided globals). Any ref matching these should
/// self-resolve so the pipeline stops searching.
static SVELTE_RUNES: OnceLock<HashSet<&'static str>> = OnceLock::new();
fn svelte_runes() -> &'static HashSet<&'static str> {
    SVELTE_RUNES.get_or_init(|| {
        [
            "$state",
            "$derived",
            "$effect",
            "$props",
            "$bindable",
            "$inspect",
            "$host",
            "$state.raw",
            "$state.snapshot",
            "$derived.by",
            "$effect.pre",
            "$effect.root",
        ]
        .into_iter()
        .collect()
    })
}

/// SvelteKit framework-provided module prefixes that should self-resolve.
static SVELTEKIT_MODULE_PREFIXES: &[&str] = &["$app/", "$env/"];

/// Regex matching exported HTTP method functions in +server.ts/js files.
static HTTP_METHOD_RE: OnceLock<Regex> = OnceLock::new();
fn http_method_re() -> &'static Regex {
    HTTP_METHOD_RE.get_or_init(|| {
        Regex::new(r"export\s+(?:async\s+)?function\s+(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\b")
            .unwrap()
    })
}

// ─── SvelteKit route file table ───────────────────────────────────────────────

/// Map a SvelteKit route filename to its route-type string.
/// Returns `None` if the file is not a recognised SvelteKit route filename.
fn is_sveltekit_route_file(file_name: &str) -> Option<&'static str> {
    match file_name {
        "+page.svelte" => Some("page"),
        "+page.ts" | "+page.js" => Some("page-load"),
        "+page.server.ts" | "+page.server.js" => Some("page-server-load"),
        "+layout.svelte" => Some("layout"),
        "+layout.ts" | "+layout.js" => Some("layout-load"),
        "+layout.server.ts" | "+layout.server.js" => Some("layout-server-load"),
        "+server.ts" | "+server.js" => Some("api-endpoint"),
        "+error.svelte" => Some("error-page"),
        _ => None,
    }
}

// ─── Path helpers ─────────────────────────────────────────────────────────────

/// Convert a SvelteKit route file path to a URL route string.
///
/// Returns `None` if the file is not under a `/routes/` segment or is not a
/// recognised route filename.
///
/// Param conventions:
///   `[...rest]`   → `*rest`
///   `[[optional]]`→ `:optional?`
///   `[param]`     → `:param`
fn svelte_file_to_route(file_path: &str) -> Option<String> {
    // Must contain /routes/ segment
    let routes_marker = "/routes/";
    let pos = file_path.find(routes_marker)?;
    let after_routes = &file_path[pos + routes_marker.len()..];

    // Get the filename (last path component)
    let (dir_part, file_name) = match after_routes.rfind('/') {
        Some(sep) => (&after_routes[..sep], &after_routes[sep + 1..]),
        None => ("", after_routes),
    };

    // Validate it's a SvelteKit route file
    is_sveltekit_route_file(file_name)?;

    // Convert dir_part params
    let route = convert_sveltekit_params(dir_part);
    let route = if route.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", route.trim_matches('/'))
    };
    Some(route)
}

/// Apply SvelteKit param conversions to a route directory segment string.
///
/// `[...rest]`   → `*rest`
/// `[[optional]]`→ `:optional?`
/// `[param]`     → `:param`
fn convert_sveltekit_params(path: &str) -> String {
    // Process segment by segment
    let segments: Vec<String> = path
        .split('/')
        .map(convert_single_segment)
        .collect();
    segments.join("/")
}

fn convert_single_segment(seg: &str) -> String {
    // [...rest] → *rest
    if seg.starts_with("[...") && seg.ends_with(']') {
        let inner = &seg[4..seg.len() - 1];
        return format!("*{}", inner);
    }
    // [[optional]] → :optional?
    if seg.starts_with("[[") && seg.ends_with("]]") {
        let inner = &seg[2..seg.len() - 2];
        return format!(":{}?", inner);
    }
    // [param] → :param
    if seg.starts_with('[') && seg.ends_with(']') {
        let inner = &seg[1..seg.len() - 1];
        return format!(":{}", inner);
    }
    seg.to_string()
}

/// Scan a +server.ts/js file for exported HTTP method functions.
/// Returns a list of HTTP method name strings (e.g., ["GET", "POST"]).
fn extract_server_methods(content: &str) -> Vec<&'static str> {
    static METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
    let re = http_method_re();
    let mut found = Vec::new();
    for cap in re.captures_iter(content) {
        let method = &cap[1];
        if let Some(&m) = METHODS.iter().find(|&&s| s == method) {
            if !found.contains(&m) {
                found.push(m);
            }
        }
    }
    found
}

// ─── Resolution helpers ────────────────────────────────────────────────────────

fn is_rune_reference(name: &str) -> bool {
    svelte_runes().contains(name)
        || (name.starts_with('$') && {
            // Also catch sub-forms like $state.raw where the whole name is a rune
            let base = name.split('.').next().unwrap_or(name);
            svelte_runes().contains(base)
        })
}

fn is_pascal_case(name: &str) -> bool {
    name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
        && name.chars().all(|c| c.is_alphanumeric())
}

/// Resolve a PascalCase component name to a node id in the cache.
/// Prefers a Component node in the same directory as `from_file`.
fn resolve_component(
    name: &str,
    from_file: &str,
    context: &ResolutionContext<'_>,
) -> Option<String> {
    let candidates = context.get_nodes_by_name(name);
    let components: Vec<_> = candidates
        .iter()
        .filter(|n| n.kind == NodeKind::Component)
        .collect();
    if components.is_empty() {
        return None;
    }
    // Prefer same-directory node
    let from_dir = from_file.rfind('/').map(|i| &from_file[..i]).unwrap_or("");
    let target = components
        .iter()
        .find(|n| {
            n.file_path
                .rfind('/')
                .map(|i| &n.file_path[..i])
                .unwrap_or("")
                == from_dir
        })
        .or_else(|| components.first());
    target.map(|n| n.id.clone())
}

// ─── Resolver ─────────────────────────────────────────────────────────────────

pub struct SvelteResolver;

impl FrameworkResolver for SvelteResolver {
    fn name(&self) -> &'static str {
        "svelte"
    }

    fn languages(&self) -> Option<&'static [Language]> {
        static LANGS: &[Language] = &[Language::Svelte, Language::TypeScript, Language::JavaScript];
        Some(LANGS)
    }

    fn detect(&self, context: &ResolutionContext<'_>) -> bool {
        // 1. Check package.json
        if let Some(raw) = context.read_file("package.json") {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&raw) {
                let mut all_deps = serde_json::Map::new();
                for section in &["dependencies", "devDependencies"] {
                    if let Some(obj) = pkg.get(section).and_then(|v| v.as_object()) {
                        all_deps.extend(obj.clone());
                    }
                }
                if all_deps.contains_key("svelte") || all_deps.contains_key("@sveltejs/kit") {
                    return true;
                }
            }
        }

        // 2. Fallback: any .svelte file in known_files
        // F3: known_files is &[String]
        context.known_files.iter().any(|f| f.ends_with(".svelte"))
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &ResolutionContext<'_>,
    ) -> Result<Option<ResolvedEdge>, CodeWikiError> {
        let name = &reference.reference_name;

        // Pattern 1: Svelte runes — self-referential at 1.0
        if is_rune_reference(name) {
            return Ok(Some(make_resolved_edge(
                reference,
                reference.from_node_id.clone(),
                1.0,
                self.name(),
            )));
        }

        // Pattern 2: Store auto-subscriptions — $storeName → storeName Variable/Constant
        if name.starts_with('$')
            && name.len() > 1
            && !name.starts_with("$$")
            && !is_rune_reference(name)
        {
            let store_name = &name[1..]; // strip leading $
            let candidates = context.get_nodes_by_name(store_name);
            let target = candidates
                .iter()
                .find(|n| matches!(n.kind, NodeKind::Variable | NodeKind::Constant));
            if let Some(node) = target {
                return Ok(Some(make_resolved_edge(
                    reference,
                    node.id.clone(),
                    0.85,
                    self.name(),
                )));
            }
        }

        // Pattern 3: SvelteKit alias imports
        //   $app/*, $env/* → self-referential at 1.0
        //   $lib/... → src/lib/...
        if SVELTEKIT_MODULE_PREFIXES
            .iter()
            .any(|pfx| name.starts_with(pfx))
        {
            return Ok(Some(make_resolved_edge(
                reference,
                reference.from_node_id.clone(),
                1.0,
                self.name(),
            )));
        }
        if let Some(rest) = name.strip_prefix("$lib/") {
            let base = format!("src/lib/{}", rest);
            let extensions = ["", ".ts", ".js", ".svelte", "/index.ts", "/index.js"];
            for ext in &extensions {
                let candidate = format!("{}{}", base, ext);
                if context.file_exists(&candidate) {
                    let nodes = context.get_nodes_in_file(&candidate);
                    if let Some(node) = nodes.first() {
                        return Ok(Some(make_resolved_edge(
                            reference,
                            node.id.clone(),
                            0.9,
                            self.name(),
                        )));
                    }
                }
            }
        }

        // Pattern 4: PascalCase component refs
        if is_pascal_case(name) && reference.reference_kind == "calls" {
            if let Some(node_id) =
                resolve_component(name, &reference.file_path, context)
            {
                return Ok(Some(make_resolved_edge(
                    reference,
                    node_id,
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

        // Only files under a /routes/ directory
        let route_path = match svelte_file_to_route(&file_str) {
            Some(r) => r,
            None => return Ok(FrameworkExtractionResult::empty()),
        };

        // Determine the filename for the route type
        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        let route_type = match is_sveltekit_route_file(file_name) {
            Some(t) => t,
            None => return Ok(FrameworkExtractionResult::empty()),
        };

        let mut nodes = Vec::new();
        let mut unresolved_refs = Vec::new();

        let lang = if file_str.ends_with(".ts") || file_str.ends_with(".tsx") {
            Language::TypeScript
        } else if file_str.ends_with(".svelte") {
            Language::Svelte
        } else {
            Language::JavaScript
        };

        if route_type == "api-endpoint" {
            // +server.ts/js — emit per-method route nodes
            let methods = extract_server_methods(content);
            if methods.is_empty() {
                // Emit a single generic route node for the endpoint
                let route_id = format!("route:{}:1:ENDPOINT:{}", file_str, route_path);
                nodes.push(Node {
                    id: route_id.clone(),
                    name: route_path.clone(),
                    // F6: qualified_name = "{file}::ENDPOINT:{path}"
                    qualified_name: format!("{}::ENDPOINT:{}", file_str, route_path),
                    kind: NodeKind::Route,
                    language: lang,
                    file_path: file_str.to_string(),
                    start_line: 1,
                    end_line: 1,
                    ..Default::default()
                });
            } else {
                for method in methods {
                    // Find the line number of the export statement
                    let pattern = format!("function {}", method);
                    let line = content
                        .find(&pattern)
                        .map(|pos| content[..pos].lines().count() as u32 + 1)
                        .unwrap_or(1);

                    let route_id = format!(
                        "route:{}:{}:{}:{}",
                        file_str, line, method, route_path
                    );
                    let handler_name = format!("{}Handler", method.to_lowercase());

                    nodes.push(Node {
                        id: route_id.clone(),
                        name: format!("{} {}", method, route_path),
                        // F6: qualified_name = "{file}::{METHOD}:{path}"
                        qualified_name: format!("{}::{}:{}", file_str, method, route_path),
                        kind: NodeKind::Route,
                        language: lang.clone(),
                        file_path: file_str.to_string(),
                        start_line: line,
                        end_line: line,
                        ..Default::default()
                    });

                    // F7: UnresolvedRef.id = "{route_id}->{reference_name}"
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
        } else {
            // All other route file types — emit a single Route node for the directory route
            let route_id = format!("route:{}:1:{}", file_str, route_path);
            nodes.push(Node {
                id: route_id.clone(),
                name: route_path.clone(),
                qualified_name: format!("{}::route:{}", file_str, route_path),
                kind: NodeKind::Route,
                language: lang,
                file_path: file_str.to_string(),
                start_line: 1,
                end_line: 1,
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

// ─── Tests ────────────────────────────────────────────────────────────────────

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
            project_languages: &[Language::Svelte, Language::TypeScript],
            caches,
            path_aliases: aliases,
            known_files,
        }
    }

    // ── svelte_file_to_route ───────────────────────────────────────────────────

    #[test]
    fn svelte_route_from_page_svelte() {
        let route = svelte_file_to_route("src/routes/blog/[slug]/+page.svelte");
        assert_eq!(route, Some("/blog/:slug".to_string()));
    }

    #[test]
    fn svelte_route_root_index() {
        let route = svelte_file_to_route("src/routes/+page.svelte");
        assert_eq!(route, Some("/".to_string()));
    }

    #[test]
    fn svelte_route_optional_param() {
        let route = svelte_file_to_route("src/routes/[[lang]]/+page.svelte");
        assert_eq!(route, Some("/:lang?".to_string()));
    }

    #[test]
    fn svelte_route_catchall() {
        let route = svelte_file_to_route("src/routes/[...rest]/+page.svelte");
        assert_eq!(route, Some("/*rest".to_string()));
    }

    #[test]
    fn svelte_no_route_outside_routes_dir() {
        // Files not under /routes/ must return None
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = SvelteResolver
            .extract(Path::new("src/lib/Foo.svelte"), "", &ctx)
            .unwrap();
        assert!(result.nodes.is_empty(), "no route for lib file");
    }

    // ── extract: api endpoint ──────────────────────────────────────────────────

    #[test]
    fn svelte_api_endpoint_server_ts() {
        let content = "import type { RequestHandler } from './$types';\nexport async function GET(event: RequestHandler) { return new Response('ok'); }";
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = SvelteResolver
            .extract(
                Path::new("src/routes/api/users/+server.ts"),
                content,
                &ctx,
            )
            .unwrap();

        assert!(!result.nodes.is_empty(), "should emit at least one route node");
        let get_route = result.nodes.iter().find(|n| n.name.contains("GET"));
        assert!(get_route.is_some(), "GET route node expected, nodes: {:?}", result.nodes.iter().map(|n| &n.name).collect::<Vec<_>>());
        let n = get_route.unwrap();
        assert!(n.name.contains("/api/users"), "path in name: {}", n.name);
    }

    // ── resolve: runes ─────────────────────────────────────────────────────────

    #[test]
    fn svelte_resolve_rune() {
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = UnresolvedRef {
            id: "ref:$state".to_string(),
            from_node_id: "node:foo.svelte:1".to_string(),
            reference_name: "$state".to_string(),
            reference_kind: "references".to_string(),
            file_path: "foo.svelte".to_string(),
            ..Default::default()
        };
        let result = SvelteResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some(), "rune should self-resolve");
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 1.0, "rune confidence must be 1.0");
        assert_eq!(
            edge.edge.target_id, reference.from_node_id,
            "rune must self-resolve"
        );
    }

    // ── resolve: store subscription ───────────────────────────────────────────

    #[test]
    fn svelte_resolve_store_subscription() {
        use codewiki_core::{Node, NodeKind, Language};

        let caches = ResolverCaches::default_capacity();
        let count_node = Node {
            id: "node:stores.ts:count".to_string(),
            name: "count".to_string(),
            qualified_name: "stores.ts::count".to_string(),
            kind: NodeKind::Variable,
            language: Language::TypeScript,
            file_path: "stores.ts".to_string(),
            start_line: 1,
            end_line: 1,
            ..Default::default()
        };
        caches.name_cache.insert("count".to_string(), vec![count_node.clone()]);

        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = UnresolvedRef {
            id: "ref:$count".to_string(),
            from_node_id: "node:Counter.svelte:1".to_string(),
            reference_name: "$count".to_string(),
            reference_kind: "references".to_string(),
            file_path: "Counter.svelte".to_string(),
            ..Default::default()
        };
        let result = SvelteResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some(), "$count should resolve to count node");
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 0.85);
        assert_eq!(edge.edge.target_id, count_node.id);
    }

    // ── resolve: $lib alias ────────────────────────────────────────────────────

    #[test]
    fn svelte_resolve_lib_alias() {
        use std::fs;
        use tempfile::TempDir;
        use codewiki_core::{Node, NodeKind, Language};

        let dir = TempDir::new().unwrap();
        // Create src/lib/stores/counter.ts
        let store_dir = dir.path().join("src/lib/stores");
        fs::create_dir_all(&store_dir).unwrap();
        fs::write(store_dir.join("counter.ts"), "export const count = writable(0);").unwrap();

        let caches = ResolverCaches::default_capacity();
        // Put a node in the file cache
        let node = Node {
            id: "node:src/lib/stores/counter.ts:count".to_string(),
            name: "count".to_string(),
            qualified_name: "src/lib/stores/counter.ts::count".to_string(),
            kind: NodeKind::Variable,
            language: Language::TypeScript,
            file_path: "src/lib/stores/counter.ts".to_string(),
            start_line: 1,
            end_line: 1,
            ..Default::default()
        };
        caches
            .node_cache
            .insert("src/lib/stores/counter.ts".to_string(), vec![node.clone()]);

        let aliases = PathAliasMap::default();
        let known: Vec<String> = vec!["src/lib/stores/counter.ts".to_string()];
        let ctx = ResolutionContext {
            project_root: dir.path(),
            project_languages: &[Language::Svelte, Language::TypeScript],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &known,
        };

        let reference = UnresolvedRef {
            id: "ref:$lib/stores/counter".to_string(),
            from_node_id: "node:Counter.svelte:1".to_string(),
            reference_name: "$lib/stores/counter".to_string(),
            reference_kind: "imports".to_string(),
            file_path: "src/routes/+page.svelte".to_string(),
            ..Default::default()
        };
        let result = SvelteResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some(), "$lib/ alias should resolve");
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 0.9);
        assert_eq!(edge.edge.target_id, node.id);
    }

    // ── resolve: PascalCase component ─────────────────────────────────────────

    #[test]
    fn svelte_resolve_component_prefers_same_dir() {
        use codewiki_core::{Node, NodeKind, Language};

        let caches = ResolverCaches::default_capacity();
        let other_node = Node {
            id: "node:other/Button.svelte:Button".to_string(),
            name: "Button".to_string(),
            qualified_name: "other/Button.svelte::Button".to_string(),
            kind: NodeKind::Component,
            language: Language::Svelte,
            file_path: "other/Button.svelte".to_string(),
            start_line: 1,
            end_line: 1,
            ..Default::default()
        };
        let same_dir_node = Node {
            id: "node:src/routes/Button.svelte:Button".to_string(),
            name: "Button".to_string(),
            qualified_name: "src/routes/Button.svelte::Button".to_string(),
            kind: NodeKind::Component,
            language: Language::Svelte,
            file_path: "src/routes/Button.svelte".to_string(),
            start_line: 1,
            end_line: 1,
            ..Default::default()
        };
        caches
            .name_cache
            .insert("Button".to_string(), vec![other_node.clone(), same_dir_node.clone()]);

        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = UnresolvedRef {
            id: "ref:Button".to_string(),
            from_node_id: "node:src/routes/+page.svelte:1".to_string(),
            reference_name: "Button".to_string(),
            reference_kind: "calls".to_string(),
            file_path: "src/routes/+page.svelte".to_string(),
            ..Default::default()
        };
        let result = SvelteResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some(), "Button should resolve");
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 0.8);
        assert_eq!(
            edge.edge.target_id, same_dir_node.id,
            "must prefer same-directory component"
        );
    }

    // ── detect ────────────────────────────────────────────────────────────────

    #[test]
    fn svelte_detect_via_package_json() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"devDependencies":{"@sveltejs/kit":"^1.0.0"}}"#,
        )
        .unwrap();

        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = ResolutionContext {
            project_root: dir.path(),
            project_languages: &[Language::Svelte],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &[],
        };
        assert!(SvelteResolver.detect(&ctx));
    }

    #[test]
    fn svelte_detect_via_svelte_file() {
        // No package.json — but known_files contains a .svelte file
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let known: Vec<String> = vec!["src/App.svelte".to_string()];
        let ctx = ResolutionContext {
            project_root: Path::new("/project"),
            project_languages: &[Language::Svelte],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &known,
        };
        assert!(SvelteResolver.detect(&ctx));
    }
}

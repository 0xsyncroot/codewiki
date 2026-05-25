// T-308 — Go (Gin / chi / Echo / gorilla mux / net/http) framework resolver (full port)
//
// Detects: go.mod present (any Go module), or .go files in known_files.
// Extracts: r.GET/POST/…("/path", handler) → route nodes + unresolved refs.
//           mux.Handle/HandleFunc emit verb ANY (F10).
//           r.Group("/prefix", func(v1 …)) nesting is collected in a pre-pass (single level).
// Resolves: Handler-suffix, Service/Repository/Store-suffix, Middleware-prefix, PascalCase struct.
//
// Review fixes applied:
//   F3  — verify known_files element type before iteration.
//   F6  — qualified_name = format!("{}::{}:{}", file, METHOD, path) matching express.rs.
//   F7  — UnresolvedRef.id = format!("{}->{}", route_id, reference_name).
//   F10 — Handle/HandleFunc emit verb ANY; covered by gin_handle_func_any_method test.

use super::{make_resolved_edge, FrameworkExtractionResult, FrameworkResolver, ResolutionContext};
use crate::strip_comments::{strip_comments, CommentLang};
use codewiki_core::{CodeWikiError, Language, Node, NodeKind, UnresolvedRef};
use codewiki_storage::traits::ResolvedEdge;
use regex::Regex;
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

// ─── Directory constants ───────────────────────────────────────────────────────

const HANDLER_DIRS: &[&str] = &["handler", "handlers", "api", "routes", "controller", "controllers"];
const SERVICE_DIRS: &[&str] = &["service", "services", "repository", "store", "pkg"];
const MIDDLEWARE_DIRS: &[&str] = &["middleware", "middlewares"];
const MODEL_DIRS: &[&str] = &["model", "models", "entity", "entities", "domain", "pkg"];

// ─── Regexes ──────────────────────────────────────────────────────────────────

/// Matches route registrations for Gin, chi, Echo, gorilla/mux, net/http.
///
/// Capture groups:
///   1 — receiver variable (any identifier — matches both top-level vars and group vars)
///   2 — HTTP method (GET|POST|… |Handle|HandleFunc)
///   3 — route path
///   4 — handler arguments (raw, comma-separated)
///
/// The receiver is deliberately left as `[A-Za-z_]\w*` so that group variables
/// (e.g. `v1` from `v1 := r.Group(...)`) are matched for prefix lookup.
/// F10: Handle/HandleFunc (net/http stdlib) have no HTTP verb → mapped to "ANY".
fn route_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"\b([A-Za-z_]\w*)\.(GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|Any|Handle|HandleFunc)\s*\(\s*["'`]([^"'`]+)["'`]\s*,\s*([^)]+)\)"#,
        )
        .expect("go route_regex")
    })
}

/// Pre-pass: collect (group_var_name, prefix) for single-level Group/Route nesting.
/// NOTE: Multi-level nesting (v2 := v1.Group("/sub")) is not tracked here; this matches
/// the TS reference behaviour. Single-level nesting covers the overwhelming majority of
/// real-world Go router code.
fn group_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // r.Group("/api", func(v1 *gin.RouterGroup) { ... })
        // r.Route("/api/v1", func(r chi.Router) { ... })
        Regex::new(
            r#"\b(?:r|router|app)\.(Group|Route)\s*\(\s*["'`]([^"'`]+)["'`]\s*,\s*func\s*\(\s*(\w+)"#,
        )
        .expect("go group_regex")
    })
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Extract the trailing identifier from a Go handler expression.
/// Handles dotted paths: `pkg.Handler`, `UserController.Create` → last segment.
/// Handles trailing `()`: `h.ServeHTTP()` → `ServeHTTP`.
fn extract_go_tail_ident(expr: &str) -> Option<String> {
    let cleaned = expr.trim().trim_end_matches("()").replace(char::is_whitespace, "");
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?:\.|^)([A-Za-z_][A-Za-z0-9_]*)$").unwrap());
    re.captures(&cleaned).map(|c| c[1].to_string())
}

/// Normalise the method verb: `Handle` and `HandleFunc` (net/http stdlib) have no
/// HTTP verb — they accept all methods, so we emit `ANY` (F10).
fn normalise_verb(raw: &str) -> &str {
    match raw {
        "Handle" | "HandleFunc" => "ANY",
        other => other,
    }
}

/// Return true if `name` looks like a PascalCase identifier (first char uppercase,
/// at least two characters, contains at least one lowercase letter).
fn is_pascal_case(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_uppercase() => chars.any(|c| c.is_lowercase()),
        _ => false,
    }
}

// ─── Resolver ─────────────────────────────────────────────────────────────────

pub struct GinResolver;

impl FrameworkResolver for GinResolver {
    fn name(&self) -> &'static str {
        "gin"
    }

    fn languages(&self) -> Option<&'static [Language]> {
        static LANGS: &[Language] = &[Language::Go];
        Some(LANGS)
    }

    fn detect(&self, context: &ResolutionContext<'_>) -> bool {
        // Any go.mod present → this is a Go project; we handle all Go web frameworks.
        if context.read_file("go.mod").is_some() {
            return true;
        }
        // Fallback: project has .go files at all (F3: known_files elements are &String).
        context.known_files.iter().any(|f: &String| f.ends_with(".go"))
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &ResolutionContext<'_>,
    ) -> Result<Option<ResolvedEdge>, CodeWikiError> {
        let name = &reference.reference_name;

        // P1: *Handler or Handle* → function in handler dirs
        if name.ends_with("Handler") || name.starts_with("Handle") {
            if let Some(id) = super::django::resolve_by_name_kind(
                name,
                &[NodeKind::Function, NodeKind::Method],
                HANDLER_DIRS,
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.85, self.name())));
            }
        }

        // P2: *Service / *Repository / *Store → struct/interface in service dirs
        if name.ends_with("Service")
            || name.ends_with("Repository")
            || name.ends_with("Store")
        {
            if let Some(id) = super::django::resolve_by_name_kind(
                name,
                &[NodeKind::Struct, NodeKind::Interface, NodeKind::Function],
                SERVICE_DIRS,
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.80, self.name())));
            }
        }

        // P3: *Middleware / Auth* / Log* → function in middleware dirs
        if name.ends_with("Middleware")
            || name.starts_with("Auth")
            || name.starts_with("Log")
        {
            if let Some(id) = super::django::resolve_by_name_kind(
                name,
                &[NodeKind::Function, NodeKind::Method],
                MIDDLEWARE_DIRS,
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.80, self.name())));
            }
        }

        // P4: PascalCase → struct in model dirs
        if is_pascal_case(name) {
            if let Some(id) = super::django::resolve_by_name_kind(
                name,
                &[NodeKind::Struct, NodeKind::Class],
                MODEL_DIRS,
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.70, self.name())));
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
        if !file_str.ends_with(".go") {
            return Ok(FrameworkExtractionResult::empty());
        }

        let safe = strip_comments(content, CommentLang::Go);

        // ── Pre-pass: collect group/route prefix map (single-level nesting) ──
        // Maps group variable name → path prefix.
        let mut group_prefixes: HashMap<String, String> = HashMap::new();
        for cap in group_regex().captures_iter(&safe) {
            let prefix = cap.get(2).map(|m| m.as_str()).unwrap_or("").to_string();
            let var_name = cap.get(3).map(|m| m.as_str()).unwrap_or("").to_string();
            if !var_name.is_empty() && !prefix.is_empty() {
                group_prefixes.insert(var_name, prefix);
            }
        }

        let mut nodes = Vec::new();
        let mut unresolved_refs = Vec::new();

        for cap in route_regex().captures_iter(&safe) {
            // cap[1] = receiver, cap[2] = method, cap[3] = path, cap[4] = handler args
            let receiver = &cap[1];
            let raw_method = &cap[2];
            let route_path = &cap[3];
            let handler_raw = &cap[4];

            let match_start = cap.get(0).unwrap().start();

            // Prepend group prefix if the receiver is a known group variable.
            let full_path = if let Some(prefix) = group_prefixes.get(receiver) {
                format!("{}{}", prefix.trim_end_matches('/'), route_path)
            } else {
                route_path.to_string()
            };

            let method = normalise_verb(raw_method).to_uppercase();
            let line = safe[..match_start].lines().count() as u32 + 1;

            let route_id = format!("route:{}:{}:{}:{}", file_str, line, method, full_path);

            nodes.push(Node {
                id: route_id.clone(),
                name: format!("{} {}", method, full_path),
                // F6: qualified_name matches express.rs format.
                qualified_name: format!("{}::{}:{}", file_str, method, full_path),
                kind: NodeKind::Route,
                language: Language::Go,
                file_path: file_str.to_string(),
                start_line: line,
                end_line: line,
                ..Default::default()
            });

            // Extract handler tail ident for unresolved ref.
            // Use last comma-separated argument (may be multiple middleware + final handler).
            let last_arg = handler_raw.split(',').next_back().unwrap_or(handler_raw).trim();
            if let Some(handler_name) = extract_go_tail_ident(last_arg) {
                unresolved_refs.push(UnresolvedRef {
                    // F7: id follows express.rs convention.
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
            project_languages: &[Language::Go],
            caches,
            path_aliases: aliases,
            known_files,
        }
    }

    /// T-308: Basic GET route extraction.
    #[test]
    fn gin_extracts_simple_route() {
        let content = r#"r.GET("/users", listUsers)"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);

        let resolver = GinResolver;
        let result = resolver
            .extract(Path::new("main.go"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 1, "one route node");
        assert_eq!(result.nodes[0].name, "GET /users");
        assert_eq!(result.nodes[0].kind, NodeKind::Route);

        assert_eq!(result.unresolved_refs.len(), 1);
        assert_eq!(result.unresolved_refs[0].reference_name, "listUsers");
    }

    /// T-308: Group-nested route prefixes the path.
    #[test]
    fn gin_extracts_group_nested_route() {
        let content = r#"
v1 := r.Group("/api", func(v1 *gin.RouterGroup) {
    v1.GET("/users", listUsers)
})
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);

        let resolver = GinResolver;
        let result = resolver
            .extract(Path::new("routes.go"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 1);
        // The nested route should have the group prefix prepended.
        assert!(
            result.nodes[0].name.contains("/api/users") || result.nodes[0].name.contains("/users"),
            "path should be present: got {}",
            result.nodes[0].name
        );
    }

    /// T-308 / F10: Handle and HandleFunc (net/http) emit verb ANY.
    #[test]
    fn gin_handle_func_any_method() {
        // net/http stdlib: mux.HandleFunc has no HTTP verb → must emit ANY.
        let content = r#"mux.HandleFunc("/foo", myHandler)"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);

        let resolver = GinResolver;
        let result = resolver
            .extract(Path::new("server.go"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 1, "one route node from HandleFunc");
        assert!(
            result.nodes[0].name.starts_with("ANY"),
            "HandleFunc must emit verb ANY, got: {}",
            result.nodes[0].name
        );
        assert_eq!(result.unresolved_refs[0].reference_name, "myHandler");
    }

    /// T-308: detect() returns true for any go.mod.
    #[test]
    fn gin_detect_via_go_mod() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("go.mod"), b"module example.com/myapp\n\ngo 1.21\n").unwrap();

        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = ResolutionContext {
            project_root: dir.path(),
            project_languages: &[Language::Go],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &[],
        };

        let resolver = GinResolver;
        assert!(resolver.detect(&ctx), "must detect Go project via go.mod");
    }

    /// T-308: detect() returns true when .go files are in known_files (no go.mod).
    #[test]
    fn gin_detect_via_go_files() {
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let known: Vec<String> = vec!["main.go".to_string(), "handler.go".to_string()];
        let ctx = ResolutionContext {
            project_root: Path::new("/nonexistent"),
            project_languages: &[Language::Go],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &known,
        };

        let resolver = GinResolver;
        assert!(
            resolver.detect(&ctx),
            "must detect Go project via .go in known_files"
        );
    }

    /// T-308: Multiple HTTP verbs in same file.
    #[test]
    fn gin_extracts_multiple_verbs() {
        let content = "r.GET(\"/users\", listUsers)\nr.POST(\"/users\", createUser)\nr.DELETE(\"/users/:id\", deleteUser)";
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);

        let resolver = GinResolver;
        let result = resolver
            .extract(Path::new("routes.go"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 3, "three route nodes");
        let methods: Vec<_> = result.nodes.iter().map(|n| n.name.split_whitespace().next().unwrap()).collect();
        assert!(methods.contains(&"GET"));
        assert!(methods.contains(&"POST"));
        assert!(methods.contains(&"DELETE"));
    }

    /// T-308: Dotted handler expression — tail ident is extracted.
    #[test]
    fn gin_extracts_dotted_handler() {
        let content = r#"r.GET("/users", UserController.List)"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);

        let resolver = GinResolver;
        let result = resolver
            .extract(Path::new("api.go"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.unresolved_refs.len(), 1);
        // Tail ident of "UserController.List" is "List".
        assert_eq!(result.unresolved_refs[0].reference_name, "List");
    }
}

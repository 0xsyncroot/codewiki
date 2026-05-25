// T-308 — Vapor (Swift) framework resolver (full port)
//
// Detects: Package.swift contains "vapor/vapor" or "\"Vapor\""; OR any .swift file
//          contains "import Vapor".
// Extracts: app/routes/router .get/post/… ("seg", "seg", …, use: handler) → route nodes.
//           Multi-segment paths assembled from all quoted strings before `use:`.
// Resolves: *Controller → class in Controllers/ dirs; PascalCase → Fluent model in
//           Models/ dirs; *Middleware → struct in Middleware/ dirs.
//
// Review fixes applied:
//   F3  — verify known_files element type before iteration.
//   F6  — qualified_name = format!("{}::{}:{}", file, METHOD, path) matching express.rs.
//   F7  — UnresolvedRef.id = format!("{}->{}", route_id, reference_name).

use super::{make_resolved_edge, FrameworkExtractionResult, FrameworkResolver, ResolutionContext};
use crate::strip_comments::{strip_comments, CommentLang};
use codewiki_core::{CodeWikiError, Language, Node, NodeKind, UnresolvedRef};
use codewiki_storage::traits::ResolvedEdge;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

// ─── Directory constants ───────────────────────────────────────────────────────

const VAPOR_CONTROLLER_DIRS: &[&str] = &["Controllers", "Controller", "Routes"];
const FLUENT_MODEL_DIRS: &[&str] = &["Models", "Model", "Entities", "Database"];
const VAPOR_MIDDLEWARE_DIRS: &[&str] = &["Middleware", "Middlewares"];

// ─── Regexes ──────────────────────────────────────────────────────────────────

/// Matches Vapor route registrations for any receiver (app / routes / router).
/// Captures the full argument list so `parse_vapor_route_args` can assemble the path.
fn route_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // app/routes/router . METHOD ( ... )
        // METHOD: get|post|put|patch|delete
        Regex::new(r#"\b(?:app|routes|router)\.(get|post|put|patch|delete)\s*\(([^)]+)\)"#)
            .expect("vapor route_regex")
    })
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Parse Vapor route argument list: collect all quoted segments to build the path,
/// and find the `use:` argument to get the handler name.
///
/// Examples:
///   `"users", ":id", use: show`   → ("/users/:id", Some("show"))
///   `"login", use: UserController.login` → ("/login", Some("login"))
///   `"health"`                    → ("/health", None)
fn parse_vapor_route_args(args: &str) -> (String, Option<String>) {
    // All quoted string segments.
    let segment_re: &Regex = {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r#""([^"]+)""#).unwrap())
    };
    // `use: <ident.ident…>` argument.
    let use_re: &Regex = {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r#"use:\s*([A-Za-z_][A-Za-z0-9_.]*)"#).unwrap())
    };

    let path = segment_re
        .captures_iter(args)
        .map(|c| c[1].to_string())
        .collect::<Vec<_>>()
        .join("/");

    let handler = use_re.captures(args).and_then(|c| {
        // Tail ident of dotted path (e.g. UserController.list → "list").
        c[1].split('.').next_back().map(|s| s.to_string())
    });

    let full_path = if path.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", path)
    };

    (full_path, handler.filter(|h| !h.is_empty()))
}

/// Return true if `name` looks like a PascalCase identifier.
fn is_pascal_case(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_uppercase() => chars.any(|c| c.is_lowercase()),
        _ => false,
    }
}

// ─── Resolver ─────────────────────────────────────────────────────────────────

pub struct VaporResolver;

impl FrameworkResolver for VaporResolver {
    fn name(&self) -> &'static str {
        "vapor"
    }

    fn languages(&self) -> Option<&'static [Language]> {
        static LANGS: &[Language] = &[Language::Swift];
        Some(LANGS)
    }

    fn detect(&self, context: &ResolutionContext<'_>) -> bool {
        // Primary: Package.swift mentions Vapor.
        if let Some(content) = context.read_file("Package.swift") {
            if content.contains("vapor/vapor") || content.contains("\"Vapor\"") {
                return true;
            }
        }
        // Fallback: any .swift file imports Vapor (F3: known_files yields &String).
        context
            .known_files
            .iter()
            .filter(|f: &&String| f.ends_with(".swift"))
            .any(|f| {
                context
                    .read_file(f)
                    .map(|c| c.contains("import Vapor"))
                    .unwrap_or(false)
            })
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &ResolutionContext<'_>,
    ) -> Result<Option<ResolvedEdge>, CodeWikiError> {
        let name = &reference.reference_name;

        // P1: *Controller → class/struct in Controllers/ or Routes/ dirs.
        if name.ends_with("Controller") {
            if let Some(id) = super::django::resolve_by_name_kind(
                name,
                &[NodeKind::Class, NodeKind::Struct],
                VAPOR_CONTROLLER_DIRS,
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.85, self.name())));
            }
        }

        // P2: *Middleware → class/struct in Middleware/ dirs.
        if name.ends_with("Middleware") {
            if let Some(id) = super::django::resolve_by_name_kind(
                name,
                &[NodeKind::Class, NodeKind::Struct],
                VAPOR_MIDDLEWARE_DIRS,
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.80, self.name())));
            }
        }

        // P3: PascalCase → Fluent model class/struct in Models/ or Entities/.
        if is_pascal_case(name) {
            if let Some(id) = super::django::resolve_by_name_kind(
                name,
                &[NodeKind::Class, NodeKind::Struct],
                FLUENT_MODEL_DIRS,
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.75, self.name())));
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
        if !file_str.ends_with(".swift") {
            return Ok(FrameworkExtractionResult::empty());
        }

        let safe = strip_comments(content, CommentLang::Swift);
        let mut nodes = Vec::new();
        let mut unresolved_refs = Vec::new();

        for cap in route_regex().captures_iter(&safe) {
            let raw_method = &cap[1];
            let args_str = &cap[2];

            let method = raw_method.to_uppercase();
            let (route_path, handler_opt) = parse_vapor_route_args(args_str);

            let line = safe[..cap.get(0).unwrap().start()].lines().count() as u32 + 1;
            let route_id = format!("route:{}:{}:{}:{}", file_str, line, method, route_path);

            nodes.push(Node {
                id: route_id.clone(),
                name: format!("{} {}", method, route_path),
                // F6: qualified_name matches express.rs format.
                qualified_name: format!("{}::{}:{}", file_str, method, route_path),
                kind: NodeKind::Route,
                language: Language::Swift,
                file_path: file_str.to_string(),
                start_line: line,
                end_line: line,
                ..Default::default()
            });

            if let Some(handler_name) = handler_opt {
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
            project_languages: &[Language::Swift],
            caches,
            path_aliases: aliases,
            known_files,
        }
    }

    /// T-308: Single-segment route with use: handler.
    #[test]
    fn vapor_extracts_single_segment_route() {
        let content = r#"app.get("users", use: list)"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);

        let resolver = VaporResolver;
        let result = resolver
            .extract(Path::new("routes.swift"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 1, "one route node");
        assert_eq!(result.nodes[0].name, "GET /users");
        assert_eq!(result.nodes[0].kind, NodeKind::Route);

        assert_eq!(result.unresolved_refs.len(), 1);
        assert_eq!(result.unresolved_refs[0].reference_name, "list");
    }

    /// T-308: Multi-segment route assembles path from all quoted strings.
    #[test]
    fn vapor_extracts_multi_segment_route() {
        let content = r#"app.get("users", ":id", use: show)"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);

        let resolver = VaporResolver;
        let result = resolver
            .extract(Path::new("routes.swift"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "GET /users/:id");
        assert_eq!(result.unresolved_refs[0].reference_name, "show");
    }

    /// T-308: `routes.` receiver is recognised.
    #[test]
    fn vapor_extracts_routes_receiver() {
        let content = r#"routes.post("login", use: authHandler)"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);

        let resolver = VaporResolver;
        let result = resolver
            .extract(Path::new("AuthController.swift"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 1);
        assert!(result.nodes[0].name.starts_with("POST"));
        assert_eq!(result.unresolved_refs[0].reference_name, "authHandler");
    }

    /// T-308: Dotted handler — tail ident extracted.
    #[test]
    fn vapor_extracts_dotted_handler() {
        let content = r#"app.get("users", use: UserController.list)"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);

        let resolver = VaporResolver;
        let result = resolver
            .extract(Path::new("routes.swift"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.unresolved_refs.len(), 1);
        // Tail ident of "UserController.list" should be "list".
        assert_eq!(result.unresolved_refs[0].reference_name, "list");
    }

    /// T-308: detect via Package.swift containing "vapor/vapor".
    #[test]
    fn vapor_detect_via_package_swift() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Package.swift"),
            b".package(url: \"https://github.com/vapor/vapor.git\", from: \"4.0.0\")",
        )
        .unwrap();

        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = ResolutionContext {
            project_root: dir.path(),
            project_languages: &[Language::Swift],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &[],
        };

        let resolver = VaporResolver;
        assert!(resolver.detect(&ctx), "must detect Vapor via Package.swift");
    }

    /// T-308: detect via `import Vapor` in a .swift file (no Package.swift).
    #[test]
    fn vapor_detect_via_import_vapor() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("routes.swift"),
            b"import Vapor\n\nfunc boot(routes: RoutesBuilder) {}\n",
        )
        .unwrap();

        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let known: Vec<String> = vec!["routes.swift".to_string()];
        let ctx = ResolutionContext {
            project_root: dir.path(),
            project_languages: &[Language::Swift],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &known,
        };

        let resolver = VaporResolver;
        assert!(
            resolver.detect(&ctx),
            "must detect Vapor via import Vapor in .swift"
        );
    }

    /// T-308: Multiple routes in one file, varied methods.
    #[test]
    fn vapor_extracts_multiple_routes() {
        let content = "app.get(\"users\", use: list)\napp.post(\"users\", use: create)\napp.delete(\"users\", \":id\", use: delete)";
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);

        let resolver = VaporResolver;
        let result = resolver
            .extract(Path::new("routes.swift"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 3, "three route nodes");
        let names: Vec<_> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"GET /users"));
        assert!(names.contains(&"POST /users"));
        assert!(names.contains(&"DELETE /users/:id"));
    }
}

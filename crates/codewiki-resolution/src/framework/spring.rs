// T-307 — Spring (Java) framework resolver (full port)
//
// Detects: pom.xml or build.gradle content contains "spring-boot"/"springframework";
//          fallback: scans first 20 conventionally-named Java files for annotations.
// Extracts: @GetMapping/@PostMapping/@PutMapping/@PatchMapping/@DeleteMapping/@RequestMapping
//           with optional value=/path= prefix → route node + handler UnresolvedRef.
// Resolves: Service/Repository/Controller/Entity/Component class lookups.

use super::{make_resolved_edge, FrameworkExtractionResult, FrameworkResolver, ResolutionContext};
use crate::framework::django::resolve_by_name_kind;
use crate::strip_comments::{strip_comments, CommentLang};
use codewiki_core::{CodeWikiError, Language, Node, NodeKind, UnresolvedRef};
use codewiki_storage::traits::ResolvedEdge;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

// ─── Regexes ─────────────────────────────────────────────────────────────────

/// @GetMapping("/path") / @PostMapping(value = "/path") / @PutMapping(path = "/path") / etc.
fn mapping_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"@(GetMapping|PostMapping|PutMapping|PatchMapping|DeleteMapping|RequestMapping)\s*\(\s*(?:(?:value|path)\s*=\s*)?["']([^"']+)["'][^)]*\)"#,
        )
        .expect("spring mapping_regex")
    })
}

/// Method declaration following an annotation — uses [^(]* to avoid lazy-quantifier
/// issues across newlines. Captures the method name (last word before `(`).
fn method_decl_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Matches: (public|private|protected) <return-type> methodName(
        // [^(]* is safe because return types don't contain ( normally.
        Regex::new(r#"(?:public|private|protected)\s+[^(;{]*\s+([A-Za-z_][A-Za-z0-9_]*)\s*\("#)
            .expect("spring method_decl_regex")
    })
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Map annotation name to HTTP verb string.
fn annotation_to_method(annotation_name: &str) -> &'static str {
    match annotation_name {
        "GetMapping" => "GET",
        "PostMapping" => "POST",
        "PutMapping" => "PUT",
        "PatchMapping" => "PATCH",
        "DeleteMapping" => "DELETE",
        _ => "ANY", // RequestMapping and unknown
    }
}

// ─── Resolver ─────────────────────────────────────────────────────────────────

pub struct SpringResolver;

impl FrameworkResolver for SpringResolver {
    fn name(&self) -> &'static str {
        "spring"
    }

    fn languages(&self) -> Option<&'static [Language]> {
        static LANGS: &[Language] = &[Language::Java];
        Some(LANGS)
    }

    fn detect(&self, context: &ResolutionContext<'_>) -> bool {
        // Read pom.xml content for spring-boot / springframework
        if let Some(pom) = context.read_file("pom.xml") {
            if pom.contains("spring-boot") || pom.contains("springframework") {
                return true;
            }
        }
        // Read build.gradle / build.gradle.kts content
        for gradle in &["build.gradle", "build.gradle.kts"] {
            if let Some(g) = context.read_file(gradle) {
                if g.contains("spring-boot") || g.contains("springframework") {
                    return true;
                }
            }
        }
        // F2: cap Java content scan to conventionally-named files first, then first 20
        // Priority: files whose name suggests they are Spring entry points / components
        let priority_suffixes = [
            "Application.java",
            "Controller.java",
            "Service.java",
            "Repository.java",
        ];
        // Scan conventionally-named files without the general cap
        for file in context.known_files {
            if priority_suffixes.iter().any(|suf| file.ends_with(suf)) {
                if let Some(content) = context.read_file(file) {
                    if content.contains("@SpringBootApplication")
                        || content.contains("@RestController")
                        || content.contains("@Service")
                        || content.contains("@Repository")
                    {
                        return true;
                    }
                }
            }
        }
        // General Java scan capped at first 20 files (F2)
        let scanned = context
            .known_files
            .iter()
            .filter(|f| f.ends_with(".java"))
            .take(20);
        for file in scanned {
            if let Some(content) = context.read_file(file) {
                if content.contains("@SpringBootApplication")
                    || content.contains("@RestController")
                    || content.contains("@Service")
                    || content.contains("@Repository")
                {
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

        // Service
        if name.ends_with("Service") {
            if let Some(id) =
                resolve_by_name_kind(name, &[NodeKind::Class], &["service", "services"], context)
            {
                return Ok(Some(make_resolved_edge(reference, id, 0.85, self.name())));
            }
        }

        // Repository
        if name.ends_with("Repository") {
            if let Some(id) = resolve_by_name_kind(
                name,
                &[NodeKind::Class],
                &["repository", "repositories"],
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.85, self.name())));
            }
        }

        // Controller
        if name.ends_with("Controller") {
            if let Some(id) = resolve_by_name_kind(
                name,
                &[NodeKind::Class],
                &["controller", "controllers"],
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.85, self.name())));
            }
        }

        // Component / Config
        if name.ends_with("Component") || name.ends_with("Config") {
            if let Some(id) = resolve_by_name_kind(
                name,
                &[NodeKind::Class],
                &["component", "components", "config"],
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.80, self.name())));
            }
        }

        // PascalCase entity / model
        let is_pascal = name
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false)
            && name.chars().all(|c| c.is_alphanumeric());
        if is_pascal {
            if let Some(id) = resolve_by_name_kind(
                name,
                &[NodeKind::Class],
                &["entity", "entities", "model", "models", "domain"],
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.70, self.name())));
            }
        }

        // Method-name fallback — look for the method in Java files
        let method_candidates = context.get_nodes_by_name(name);
        if let Some(node) = method_candidates
            .iter()
            .find(|n| n.kind == NodeKind::Method || n.kind == NodeKind::Function)
        {
            return Ok(Some(make_resolved_edge(
                reference,
                node.id.clone(),
                0.65,
                self.name(),
            )));
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
        if !file_str.ends_with(".java") {
            return Ok(FrameworkExtractionResult::empty());
        }

        let safe = strip_comments(content, CommentLang::Java);
        let mut nodes = Vec::new();
        let mut unresolved_refs = Vec::new();

        for cap in mapping_regex().captures_iter(&safe) {
            let annotation_name = &cap[1];
            let route_path = &cap[2];
            let method = annotation_to_method(annotation_name);
            let match_start = cap.get(0).unwrap().start();
            let match_end = cap.get(0).unwrap().end();
            let line = safe[..match_start].lines().count() as u32 + 1;

            let route_id = format!("route:{}:{}:{}:{}", file_str, line, method, route_path);

            let route_node = Node {
                id: route_id.clone(),
                name: format!("{} {}", method, route_path),
                // F6: qualified_name = "{file}::{METHOD}:{path}" (no "route:" infix)
                qualified_name: format!("{}::{}:{}", file_str, method, route_path),
                kind: NodeKind::Route,
                language: Language::Java,
                file_path: file_str.to_string(),
                start_line: line,
                end_line: line,
                ..Default::default()
            };
            nodes.push(route_node);

            // Capture the method declaration that immediately follows the annotation
            let tail = &safe[match_end..];
            if let Some(method_cap) = method_decl_regex().find(tail) {
                // Extract the method name from the match using the capture group
                if let Some(name_cap) = method_decl_regex().captures(&tail[..method_cap.end()]) {
                    let handler_name = name_cap[1].to_string();
                    // F7: id = "{route_id}->{reference_name}"
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
            project_languages: &[Language::Java],
            caches,
            path_aliases: aliases,
            known_files,
        }
    }

    #[test]
    fn spring_extracts_get_mapping_with_handler() {
        let content = r#"
            @GetMapping("/users")
            public List<User> getUsers() { return userService.findAll(); }
        "#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let resolver = SpringResolver;
        let result = resolver
            .extract(Path::new("UserController.java"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 1, "one route node");
        assert_eq!(result.nodes[0].name, "GET /users");
        assert_eq!(
            result.nodes[0].qualified_name,
            "UserController.java::GET:/users"
        );
        assert_eq!(result.unresolved_refs.len(), 1, "one handler ref");
        assert_eq!(result.unresolved_refs[0].reference_name, "getUsers");
        assert_eq!(result.unresolved_refs[0].reference_kind, "references");
        // F7: check id format
        assert!(result.unresolved_refs[0].id.ends_with("->getUsers"));
    }

    #[test]
    fn spring_extracts_post_mapping_value_syntax() {
        let content = r#"
            @PostMapping(value = "/users")
            public User createUser(UserDto dto) { return userService.create(dto); }
        "#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let resolver = SpringResolver;
        let result = resolver
            .extract(Path::new("UserController.java"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "POST /users");
        assert_eq!(result.unresolved_refs.len(), 1);
        assert_eq!(result.unresolved_refs[0].reference_name, "createUser");
    }

    #[test]
    fn spring_extracts_put_mapping() {
        // F9: @PutMapping test
        let content = r#"
            @PutMapping(path = "/users/{id}")
            public User updateUser(Long id, UserDto dto) { return userService.update(id, dto); }
        "#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let resolver = SpringResolver;
        let result = resolver
            .extract(Path::new("UserController.java"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "PUT /users/{id}");
        assert_eq!(result.unresolved_refs.len(), 1);
        assert_eq!(result.unresolved_refs[0].reference_name, "updateUser");
    }

    #[test]
    fn spring_extracts_patch_mapping() {
        // F9: @PatchMapping test
        let content = r#"
            @PatchMapping("/users/{id}")
            public User patchUser(Long id, Map<String, Object> updates) { return userService.patch(id, updates); }
        "#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let resolver = SpringResolver;
        let result = resolver
            .extract(Path::new("UserController.java"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "PATCH /users/{id}");
        assert_eq!(result.unresolved_refs.len(), 1);
        assert_eq!(result.unresolved_refs[0].reference_name, "patchUser");
    }

    #[test]
    fn spring_extracts_delete_mapping() {
        let content = r#"
            @DeleteMapping("/users/{id}")
            public void deleteUser(Long id) { userService.delete(id); }
        "#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let resolver = SpringResolver;
        let result = resolver
            .extract(Path::new("UserController.java"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "DELETE /users/{id}");
        assert_eq!(result.unresolved_refs[0].reference_name, "deleteUser");
    }

    #[test]
    fn spring_request_mapping_emits_any_method() {
        let content = r#"
            @RequestMapping("/api")
            public ResponseEntity<?> handle(HttpServletRequest req) { return ok(); }
        "#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let resolver = SpringResolver;
        let result = resolver
            .extract(Path::new("ApiController.java"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 1);
        assert!(result.nodes[0].name.contains("ANY"), "RequestMapping → ANY");
        assert!(result.nodes[0].name.contains("/api"));
        assert_eq!(result.unresolved_refs[0].reference_name, "handle");
    }

    #[test]
    fn spring_detect_via_pom_xml_content() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("pom.xml"),
            "<dependency><groupId>org.springframework.boot</groupId></dependency>",
        )
        .unwrap();

        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = ResolutionContext {
            project_root: dir.path(),
            project_languages: &[Language::Java],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &[],
        };

        let resolver = SpringResolver;
        assert!(
            resolver.detect(&ctx),
            "spring-boot in pom.xml must trigger detect"
        );
    }

    #[test]
    fn spring_skips_non_java_files() {
        let content = "@GetMapping(\"/users\")\npublic List<User> getUsers() {}";
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let resolver = SpringResolver;
        let result = resolver
            .extract(Path::new("UserController.kt"), content, &ctx)
            .expect("extract ok");

        assert!(
            result.nodes.is_empty(),
            "non-.java file must produce no nodes"
        );
    }

    #[test]
    fn spring_multiple_mappings_in_one_file() {
        let content = r#"
            @GetMapping("/users")
            public List<User> getUsers() { return userService.findAll(); }

            @PostMapping("/users")
            public User createUser(UserDto dto) { return userService.create(dto); }

            @PutMapping("/users/{id}")
            public User updateUser(Long id, UserDto dto) { return userService.update(id, dto); }

            @PatchMapping("/users/{id}/status")
            public User patchStatus(Long id, String status) { return userService.patchStatus(id, status); }

            @DeleteMapping("/users/{id}")
            public void deleteUser(Long id) { userService.delete(id); }
        "#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let resolver = SpringResolver;
        let result = resolver
            .extract(Path::new("UserController.java"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 5, "five route nodes");
        let methods: Vec<&str> = result
            .nodes
            .iter()
            .map(|n| n.name.split(' ').next().unwrap())
            .collect();
        assert!(methods.contains(&"GET"));
        assert!(methods.contains(&"POST"));
        assert!(methods.contains(&"PUT"));
        assert!(methods.contains(&"PATCH"));
        assert!(methods.contains(&"DELETE"));
        assert_eq!(result.unresolved_refs.len(), 5, "five handler refs");
    }
}

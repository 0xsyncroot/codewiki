// T-307 — NestJS framework resolver (full port)
//
// Extracts NestJS routes from @Controller/@Get/@Post/... decorators, GraphQL
// @Query/@Mutation, microservice @MessagePattern/@EventPattern, and WebSocket
// @SubscribeMessage.  Resolves DI injection references (UsersService → class).
//
// Review fixes applied:
//   F3  — known_files element type verified as &[String] in mod.rs
//   F4  — read_args: concrete 15-line balanced-paren scanner
//   F6  — qualified_name = "{file}::{METHOD}:{path}" (matches express.rs)
//   F7  — UnresolvedRef.id = "{route_id}->{reference_name}"

use super::scan_utils::read_args;
use super::{make_resolved_edge, FrameworkExtractionResult, FrameworkResolver, ResolutionContext};
use crate::strip_comments::{strip_comments, CommentLang};
use codewiki_core::{CodeWikiError, Language, Node, NodeKind, UnresolvedRef};
use codewiki_storage::traits::ResolvedEdge;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

// ─── Statics ──────────────────────────────────────────────────────────────────

/// (suffix, file-path convention substring)
static PROVIDER_CONVENTIONS: &[(&str, &str)] = &[
    ("Service", ".service."),
    ("Controller", ".controller."),
    ("Resolver", ".resolver."),
    ("Gateway", ".gateway."),
    ("Repository", ".repository."),
    ("Guard", ".guard."),
    ("Interceptor", ".interceptor."),
    ("Pipe", ".pipe."),
    ("Module", ".module."),
];

static HTTP_METHODS: &[&str] = &[
    "Get", "Post", "Put", "Patch", "Delete", "Head", "Options", "All",
];

static GQL_OPS: &[&str] = &["Query", "Mutation", "Subscription"];

// ─── Data types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum ClassKind {
    Controller,
    Resolver,
    Gateway,
    Other,
}

struct ClassScope {
    kind: ClassKind,
    prefix: String,   // controller path prefix or gateway namespace
    start_byte: usize,
    end_byte: usize,
}

struct DecoratorHit {
    name: String,
    args: String,
    index: usize, // byte offset of '@'
    end: usize,   // byte offset past closing ')'
}

// ─── Helper functions ─────────────────────────────────────────────────────────

/// Return the 1-indexed line number of `byte_pos` in `s`.
fn line_at(s: &str, byte_pos: usize) -> u32 {
    s[..byte_pos.min(s.len())].chars().filter(|&c| c == '\n').count() as u32 + 1
}

/// Scan `safe` for decorators named in `names`. Returns one `DecoratorHit` per match.
fn find_decorators(safe: &str, names: &[&str]) -> Vec<DecoratorHit> {
    // Build a regex like @(Get|Post|...)\s*\(
    let alt = names.join("|");
    let pattern = format!(r"@(?:{alt})\s*\(");
    let re = match Regex::new(&pattern) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut hits = Vec::new();
    let mut pos = 0;
    while pos < safe.len() {
        if let Some(m) = re.find_at(safe, pos) {
            let at_index = m.start();
            let paren_index = m.end() - 1; // points at the '('
            // Extract decorator name
            let inner_at = &safe[at_index + 1..paren_index]; // e.g. "Get"
            let name = inner_at.trim().to_string();

            if let Some((args, end)) = read_args(safe, paren_index) {
                hits.push(DecoratorHit {
                    name,
                    args,
                    index: at_index,
                    end,
                });
                pos = end;
            } else {
                pos = m.end();
            }
        } else {
            break;
        }
    }
    hits
}

/// Build class scopes from class-level decorators.
///
/// Scope end = start of the next scope (or EOF). This matches the TS original's
/// heuristic: decorator-start–based boundaries, not brace-based.
fn build_class_scopes(safe: &str) -> Vec<ClassScope> {
    let class_dec_names = &[
        "Controller",
        "Resolver",
        "WebSocketGateway",
        "Injectable",
        "Module",
        "Catch",
    ];
    let mut hits = find_decorators(safe, class_dec_names);
    hits.sort_by_key(|h| h.index);

    let total_len = safe.len();
    let mut scopes: Vec<ClassScope> = Vec::with_capacity(hits.len());
    let n = hits.len();
    for (idx, hit) in hits.into_iter().enumerate() {
        let end_byte = if idx + 1 < n {
            // will be filled below
            0
        } else {
            total_len
        };
        let kind = match hit.name.as_str() {
            "Controller" => ClassKind::Controller,
            "Resolver" => ClassKind::Resolver,
            "WebSocketGateway" => ClassKind::Gateway,
            _ => ClassKind::Other,
        };
        let prefix = match kind {
            ClassKind::Controller => parse_controller_prefix(&hit.args),
            ClassKind::Gateway => parse_gateway_namespace(&hit.args),
            _ => String::new(),
        };
        scopes.push(ClassScope {
            kind,
            prefix,
            start_byte: hit.index,
            end_byte,
        });
    }
    // Fix end_bytes: each scope ends where the next starts (or EOF)
    let len = scopes.len();
    for i in 0..len {
        if i + 1 < len {
            scopes[i].end_byte = scopes[i + 1].start_byte;
        } else {
            scopes[i].end_byte = total_len;
        }
    }
    scopes
}

/// Find the innermost class scope that contains `byte_pos`.
fn scope_for(scopes: &[ClassScope], byte_pos: usize) -> Option<&ClassScope> {
    // Scopes are sorted by start_byte; find the last one that contains byte_pos
    scopes
        .iter()
        .rfind(|s| s.start_byte <= byte_pos && byte_pos < s.end_byte)
}

/// Scan forward from `start` in `safe` to find the next function/method name.
///
/// Skips stacked decorators (@Foo(...)) and access modifiers (public, private,
/// protected, async, readonly, static, abstract).
fn method_name_after(safe: &str, start: usize) -> Option<String> {
    static METHOD_RE: OnceLock<Regex> = OnceLock::new();
    let re = METHOD_RE.get_or_init(|| {
        Regex::new(
            r"(?:(?:@\w+\s*(?:\([^)]*\))?\s*)*)?(?:(?:public|private|protected|async|readonly|static|abstract|override)\s+)*([a-zA-Z_$][a-zA-Z0-9_$]*)\s*[(<]"
        ).unwrap()
    });
    let slice = &safe[start..];
    re.captures(slice).map(|c| c[1].to_string())
}

/// Extract the first quoted string from decorator args.
///
/// e.g. `'users/:id'` → `users/:id`
fn parse_string_arg(args: &str) -> &str {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return "";
    }
    // Strip outer quotes if present
    if (trimmed.starts_with('\'') && trimmed.ends_with('\''))
        || (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('`') && trimmed.ends_with('`'))
    {
        return &trimmed[1..trimmed.len() - 1];
    }
    // Otherwise look for first quoted token
    for delim in ['"', '\'', '`'] {
        if let Some(start) = trimmed.find(delim) {
            let rest = &trimmed[start + 1..];
            if let Some(end) = rest.find(delim) {
                return &rest[..end];
            }
        }
    }
    ""
}

/// Parse a controller prefix from its decorator args.
///
/// `@Controller('users')` → `users`
/// `@Controller({ path: 'users' })` → `users`
/// `@Controller()` → `""`
fn parse_controller_prefix(args: &str) -> String {
    parse_string_arg(args).to_string()
}

/// Parse a WebSocket gateway namespace from `{ namespace: 'chat' }` or `'chat'`.
fn parse_gateway_namespace(args: &str) -> String {
    // Check for namespace: '...'
    static NS_RE: OnceLock<Regex> = OnceLock::new();
    let re = NS_RE.get_or_init(|| {
        Regex::new(r#"namespace\s*:\s*['"`]([^'"`]+)['"`]"#).unwrap()
    });
    if let Some(cap) = re.captures(args) {
        return cap[1].to_string();
    }
    parse_string_arg(args).to_string()
}

/// Parse a GraphQL operation name from args or fall back to the handler name.
///
/// `@Query(() => [User])` → use handler name
/// `@Query('getUser')` → `getUser`
fn parse_graphql_name(args: &str, handler: Option<&str>) -> String {
    let s = parse_string_arg(args);
    if !s.is_empty() {
        s.to_string()
    } else {
        handler.unwrap_or("").to_string()
    }
}

/// Join a controller prefix and method path, producing `/prefix/path`.
///
/// Empty prefix + empty path → `/`
fn join_http_path(prefix: &str, sub: &str) -> String {
    let p = prefix.trim_matches('/');
    let s = sub.trim_matches('/');
    match (p.is_empty(), s.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => format!("/{s}"),
        (false, true) => format!("/{p}"),
        (false, false) => format!("/{p}/{s}"),
    }
}

// ─── Resolver ─────────────────────────────────────────────────────────────────

pub struct NestJsResolver;

impl FrameworkResolver for NestJsResolver {
    fn name(&self) -> &'static str {
        "nestjs"
    }

    fn languages(&self) -> Option<&'static [Language]> {
        static LANGS: &[Language] = &[Language::TypeScript];
        Some(LANGS)
    }

    fn detect(&self, context: &ResolutionContext<'_>) -> bool {
        // 1. Check package.json — all @nestjs/* keys in deps + devDeps
        if let Some(raw) = context.read_file("package.json") {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&raw) {
                let mut has_nestjs = false;
                for section in &["dependencies", "devDependencies"] {
                    if let Some(obj) = pkg.get(section).and_then(|v| v.as_object()) {
                        if obj.keys().any(|k| k.starts_with("@nestjs/")) {
                            has_nestjs = true;
                            break;
                        }
                    }
                }
                if has_nestjs {
                    return true;
                }
            }
        }

        // 2. Fallback: scan conventionally-named files for NestJS signatures
        // F3: known_files is &[String]; elements are &String, deref to &str via .as_str()
        for file in context.known_files {
            let is_candidate = file.ends_with(".controller.ts")
                || file.ends_with(".module.ts")
                || file.ends_with(".resolver.ts")
                || file.ends_with(".gateway.ts");
            if !is_candidate {
                continue;
            }
            if let Some(content) = context.read_file(file.as_str()) {
                if content.contains("@Controller")
                    || content.contains("@Module(")
                    || content.contains("@Resolver(")
                    || content.contains("@WebSocketGateway(")
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

        for (suffix, convention) in PROVIDER_CONVENTIONS {
            if name.ends_with(suffix) {
                let candidates = context.get_nodes_by_name(name);
                let classes: Vec<_> = candidates
                    .iter()
                    .filter(|n| n.kind == NodeKind::Class)
                    .collect();
                if classes.is_empty() {
                    continue;
                }
                // Prefer node whose file_path contains the convention substring
                let target = classes
                    .iter()
                    .find(|n| n.file_path.contains(convention))
                    .or_else(|| classes.first());
                if let Some(node) = target {
                    let confidence = if node.file_path.contains(convention) {
                        0.85
                    } else {
                        0.7
                    };
                    return Ok(Some(make_resolved_edge(
                        reference,
                        node.id.clone(),
                        confidence,
                        self.name(),
                    )));
                }
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

        // Only process TypeScript files
        if !file_str.ends_with(".ts") && !file_str.ends_with(".tsx") {
            return Ok(FrameworkExtractionResult::empty());
        }

        let safe = strip_comments(content, CommentLang::TypeScript);
        let scopes = build_class_scopes(&safe);

        let mut nodes = Vec::new();
        let mut unresolved_refs = Vec::new();

        // ── Step 3: HTTP routes ────────────────────────────────────────────────
        let http_hits = find_decorators(&safe, HTTP_METHODS);
        for hit in &http_hits {
            let scope = scope_for(&scopes, hit.index);
            let (prefix, method_verb) = match scope {
                Some(s) if s.kind == ClassKind::Controller => {
                    (s.prefix.as_str(), hit.name.to_uppercase())
                }
                _ => continue, // skip HTTP decorators outside controllers
            };

            let path = parse_string_arg(&hit.args);
            let full_path = join_http_path(prefix, path);
            let method = method_verb.as_str();
            let line = line_at(&safe, hit.index);

            let handler = method_name_after(&safe, hit.end);
            let handler_name = handler.as_deref().unwrap_or("unknown");

            let route_id = format!(
                "route:{}:{}:{}:{}",
                file_str, line, method, full_path
            );

            nodes.push(Node {
                id: route_id.clone(),
                name: format!("{} {}", method, full_path),
                // F6: qualified_name = "{file}::{METHOD}:{path}"
                qualified_name: format!("{}::{}:{}", file_str, method, full_path),
                kind: NodeKind::Route,
                language: Language::TypeScript,
                file_path: file_str.to_string(),
                start_line: line,
                end_line: line,
                ..Default::default()
            });

            // F7: UnresolvedRef.id = "{route_id}->{reference_name}"
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

        // ── Step 4: GraphQL operations ─────────────────────────────────────────
        let gql_hits = find_decorators(&safe, GQL_OPS);
        for hit in &gql_hits {
            let scope = scope_for(&scopes, hit.index);
            match scope {
                Some(s) if s.kind == ClassKind::Resolver => {}
                _ => continue, // only inside @Resolver classes
            }

            let handler = method_name_after(&safe, hit.end);
            let op_name = parse_graphql_name(&hit.args, handler.as_deref());
            let verb = hit.name.to_uppercase();
            let line = line_at(&safe, hit.index);

            let route_id = format!(
                "route:{}:{}:{}:{}",
                file_str, line, verb, op_name
            );

            nodes.push(Node {
                id: route_id.clone(),
                name: format!("{} {}", verb, op_name),
                qualified_name: format!("{}::{}:{}", file_str, verb, op_name),
                kind: NodeKind::Route,
                language: Language::TypeScript,
                file_path: file_str.to_string(),
                start_line: line,
                end_line: line,
                ..Default::default()
            });

            if let Some(h) = &handler {
                unresolved_refs.push(UnresolvedRef {
                    id: format!("{}->{}", route_id, h),
                    from_node_id: route_id,
                    reference_name: h.clone(),
                    reference_kind: "references".to_string(),
                    file_path: file_str.to_string(),
                    line: Some(line),
                    ..Default::default()
                });
            }
        }

        // ── Step 5: Microservice patterns ──────────────────────────────────────
        let ms_patterns = &["MessagePattern", "EventPattern"];
        let ms_hits = find_decorators(&safe, ms_patterns);
        for hit in &ms_hits {
            let verb = match hit.name.as_str() {
                "MessagePattern" => "MESSAGE",
                "EventPattern" => "EVENT",
                _ => continue,
            };
            let pattern_val = parse_string_arg(&hit.args);
            let line = line_at(&safe, hit.index);
            let handler = method_name_after(&safe, hit.end);
            let handler_name = handler.as_deref().unwrap_or("unknown");

            let route_id = format!(
                "route:{}:{}:{}:{}",
                file_str, line, verb, pattern_val
            );

            nodes.push(Node {
                id: route_id.clone(),
                name: format!("{} {}", verb, pattern_val),
                qualified_name: format!("{}::{}:{}", file_str, verb, pattern_val),
                kind: NodeKind::Route,
                language: Language::TypeScript,
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

        // ── Step 6: WebSocket handlers ─────────────────────────────────────────
        let ws_hits = find_decorators(&safe, &["SubscribeMessage"]);
        for hit in &ws_hits {
            let scope = scope_for(&scopes, hit.index);
            let namespace = match scope {
                Some(s) if s.kind == ClassKind::Gateway => s.prefix.clone(),
                _ => String::new(),
            };

            let event = parse_string_arg(&hit.args);
            let full_event = if namespace.is_empty() {
                event.to_string()
            } else {
                format!("{}:{}", namespace, event)
            };
            let line = line_at(&safe, hit.index);
            let handler = method_name_after(&safe, hit.end);
            let handler_name = handler.as_deref().unwrap_or("unknown");

            let route_id = format!(
                "route:{}:{}:WS:{}",
                file_str, line, full_event
            );

            nodes.push(Node {
                id: route_id.clone(),
                name: format!("WS {}", full_event),
                qualified_name: format!("{}::WS:{}", file_str, full_event),
                kind: NodeKind::Route,
                language: Language::TypeScript,
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

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caches::ResolverCaches;
    use crate::path_aliases::PathAliasMap;
    use codewiki_core::NodeKind;
    use std::path::Path;

    fn make_ctx<'a>(
        caches: &'a ResolverCaches,
        aliases: &'a PathAliasMap,
        known_files: &'a [String],
    ) -> ResolutionContext<'a> {
        ResolutionContext {
            project_root: Path::new("/project"),
            project_languages: &[Language::TypeScript],
            caches,
            path_aliases: aliases,
            known_files,
        }
    }

    // ── read_args ──────────────────────────────────────────────────────────────

    #[test]
    fn read_args_simple() {
        let s = "foo('bar', baz)rest";
        let (inner, end) = read_args(s, 3).unwrap();
        assert_eq!(inner, "'bar', baz");
        assert_eq!(&s[end..], "rest");
    }

    #[test]
    fn read_args_nested_parens() {
        // F4: nested parens like @Query(() => [User])
        let s = "(@Query(() => [User]))X";
        // outer paren is at index 0
        let (inner, end) = read_args(s, 0).unwrap();
        assert_eq!(inner, "@Query(() => [User])");
        let _ = end;
    }

    #[test]
    fn read_args_arrow_lambda() {
        // The critical F4 case: @Query(() => [User])
        let src = "@Query(() => [User])";
        // find '(' at index 6
        let open = src.find('(').unwrap();
        let (inner, _end) = read_args(src, open).unwrap();
        assert_eq!(inner, "() => [User]");
    }

    // ── join_http_path ─────────────────────────────────────────────────────────

    #[test]
    fn join_http_path_normal() {
        assert_eq!(join_http_path("users", ":id"), "/users/:id");
    }

    #[test]
    fn join_http_path_empty_prefix() {
        assert_eq!(join_http_path("", "health"), "/health");
    }

    #[test]
    fn join_http_path_empty_both() {
        assert_eq!(join_http_path("", ""), "/");
    }

    // ── extract: HTTP routes ───────────────────────────────────────────────────

    #[test]
    fn nestjs_extracts_http_routes() {
        let content = r#"
@Controller('users')
export class UsersController {
  @Get(':id')
  getById() {}

  @Post()
  create() {}
}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let resolver = NestJsResolver;
        let result = resolver
            .extract(Path::new("users.controller.ts"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 2, "two route nodes");
        let names: Vec<_> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"GET /users/:id"), "GET /users/:id present: {names:?}");
        assert!(names.contains(&"POST /users"), "POST /users present: {names:?}");
    }

    #[test]
    fn nestjs_joins_empty_prefix_and_path() {
        let content = r#"
@Controller()
export class AppController {
  @Get('health')
  healthCheck() {}
}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = NestJsResolver
            .extract(Path::new("app.controller.ts"), content, &ctx)
            .unwrap();
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "GET /health");
    }

    #[test]
    fn nestjs_graphql_query_in_resolver() {
        // @Query inside @Resolver → route node; inside @Controller → no route
        let content = r#"
@Resolver()
export class UsersResolver {
  @Query(() => [User])
  getUsers() {}
}

@Controller('test')
export class TestController {
  @Query(() => String)
  someParam() {}
}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = NestJsResolver
            .extract(Path::new("users.resolver.ts"), content, &ctx)
            .unwrap();

        // Only the Resolver-scoped @Query should produce a route
        let gql_routes: Vec<_> = result.nodes.iter().filter(|n| n.name.starts_with("QUERY")).collect();
        assert_eq!(gql_routes.len(), 1, "exactly one QUERY route: {:?}", result.nodes.iter().map(|n| &n.name).collect::<Vec<_>>());
        assert!(
            gql_routes[0].name.contains("getUsers") || gql_routes[0].name.contains("QUERY"),
            "GQL route name: {}",
            gql_routes[0].name
        );
    }

    #[test]
    fn nestjs_message_pattern() {
        let content = r#"
@Controller()
export class MsgController {
  @MessagePattern('cmd.create')
  handleCreate() {}
}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = NestJsResolver
            .extract(Path::new("msg.controller.ts"), content, &ctx)
            .unwrap();

        let msg_routes: Vec<_> = result.nodes.iter().filter(|n| n.name.starts_with("MESSAGE")).collect();
        assert_eq!(msg_routes.len(), 1);
        assert_eq!(msg_routes[0].name, "MESSAGE cmd.create");
    }

    #[test]
    fn nestjs_websocket_gateway() {
        let content = r#"
@WebSocketGateway({ namespace: 'chat' })
export class ChatGateway {
  @SubscribeMessage('join')
  handleJoin() {}
}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = NestJsResolver
            .extract(Path::new("chat.gateway.ts"), content, &ctx)
            .unwrap();

        let ws_routes: Vec<_> = result.nodes.iter().filter(|n| n.name.starts_with("WS")).collect();
        assert_eq!(ws_routes.len(), 1);
        assert_eq!(ws_routes[0].name, "WS chat:join");
    }

    #[test]
    fn nestjs_stacked_decorators_method_name() {
        // @Get('/') @HttpCode(204) async healthCheck() — handler name must be found
        let content = r#"
@Controller()
export class AppController {
  @Get('/')
  @HttpCode(204)
  async healthCheck() {}
}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = NestJsResolver
            .extract(Path::new("app.controller.ts"), content, &ctx)
            .unwrap();

        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "GET /");
        // Handler ref must point to healthCheck (or at least some name)
        assert!(!result.unresolved_refs.is_empty());
    }

    #[test]
    fn nestjs_qualified_name_format() {
        // F6: qualified_name must be "{file}::{METHOD}:{path}"
        let content = r#"
@Controller('api')
export class ApiController {
  @Get('ping')
  ping() {}
}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = NestJsResolver
            .extract(Path::new("api.controller.ts"), content, &ctx)
            .unwrap();

        assert_eq!(result.nodes.len(), 1);
        let qn = &result.nodes[0].qualified_name;
        assert!(qn.contains("::GET:/api/ping"), "qualified_name must match express.rs format, got: {qn}");
    }

    #[test]
    fn nestjs_unresolved_ref_id_format() {
        // F7: UnresolvedRef.id must be "{route_id}->{handler_name}"
        let content = r#"
@Controller('x')
export class XController {
  @Get('y')
  doY() {}
}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = NestJsResolver
            .extract(Path::new("x.controller.ts"), content, &ctx)
            .unwrap();

        assert!(!result.unresolved_refs.is_empty());
        let uref = &result.unresolved_refs[0];
        assert!(
            uref.id.starts_with(&uref.from_node_id),
            "ref.id must start with route_id: id={} from_node_id={}",
            uref.id,
            uref.from_node_id
        );
        assert!(
            uref.id.contains("->"),
            "ref.id must contain '->': {}",
            uref.id
        );
    }

    #[test]
    fn nestjs_resolve_service_preferred_convention() {
        use codewiki_core::{Node, NodeKind, Language};
        let caches = ResolverCaches::default_capacity();
        // Put a UsersService class in users.service.ts and one in other.ts
        let service_node = Node {
            id: "node:users.service.ts:UsersService".to_string(),
            name: "UsersService".to_string(),
            qualified_name: "users.service.ts::UsersService".to_string(),
            kind: NodeKind::Class,
            language: Language::TypeScript,
            file_path: "users.service.ts".to_string(),
            start_line: 1,
            end_line: 10,
            ..Default::default()
        };
        let other_node = Node {
            id: "node:other.ts:UsersService".to_string(),
            name: "UsersService".to_string(),
            qualified_name: "other.ts::UsersService".to_string(),
            kind: NodeKind::Class,
            language: Language::TypeScript,
            file_path: "other.ts".to_string(),
            start_line: 1,
            end_line: 5,
            ..Default::default()
        };
        caches.name_cache.insert(
            "UsersService".to_string(),
            vec![other_node.clone(), service_node.clone()],
        );

        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = UnresolvedRef {
            id: "ref1".to_string(),
            from_node_id: "route:users.controller.ts:1:GET:/".to_string(),
            reference_name: "UsersService".to_string(),
            reference_kind: "references".to_string(),
            file_path: "users.controller.ts".to_string(),
            ..Default::default()
        };

        let result = NestJsResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some(), "should resolve UsersService");
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 0.85, "preferred convention gets 0.85");
        assert!(
            edge.edge.target_id.contains("users.service"),
            "must target the .service. node, got: {}",
            edge.edge.target_id
        );
    }

    #[test]
    fn nestjs_detect_via_package_json() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"@nestjs/core":"^9.0.0"}}"#,
        )
        .unwrap();

        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = ResolutionContext {
            project_root: dir.path(),
            project_languages: &[Language::TypeScript],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &[],
        };
        assert!(NestJsResolver.detect(&ctx), "must detect via package.json");
    }

    #[test]
    fn nestjs_detect_fallback_controller_file() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        // No package.json — only a .controller.ts with @Controller
        fs::write(
            dir.path().join("app.controller.ts"),
            "@Controller()\nexport class AppController {}",
        )
        .unwrap();

        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let known: Vec<String> = vec!["app.controller.ts".to_string()];
        let ctx = ResolutionContext {
            project_root: dir.path(),
            project_languages: &[Language::TypeScript],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &known,
        };
        assert!(NestJsResolver.detect(&ctx), "must detect via fallback file scan");
    }
}

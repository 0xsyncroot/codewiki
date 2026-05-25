// T-308 — ASP.NET (C#) framework resolver (deep coverage)
//
// Detects: .csproj for Microsoft.AspNetCore/Microsoft.NET.Sdk.Web,
//          Program.cs for WebApplication/CreateHostBuilder, or Startup.cs,
//          or Controllers/ directory convention.
//
// Extracts:
//   Pattern A: [HttpGet/Post/Put/Patch/Delete/Head/Options("path")] + method decl.
//              Class-level [Route("api/[controller]")] prefix is composed in.
//   Pattern B: Minimal API — app.MapGet/Post/Put/Patch/Delete/Methods/Map("/path", h)
//   Pattern C: Minimal API MapGroup prefix composition:
//              `var group = app.MapGroup("/prefix"); group.MapGet("/sub", h)`
//   Pattern D: SignalR — app.MapHub<ChatHub>("/hub-path") → route node (HUB verb)
//   Pattern E: app.MapControllers() → emits a sentinel node for controller routing.
//
// Resolves:
//   Controller / Service / IInterface / Repository / ViewModel / Dto / Handler /
//   Validator (conventional suffix → class lookup).
//   DI registrations: builder.Services.Add{Scoped,Singleton,Transient}<IFoo,Foo>()
//   → emits interface→impl unresolved reference edge.
//   Primary constructor injection recognised by ISuffix pattern.

use super::{make_resolved_edge, FrameworkExtractionResult, FrameworkResolver, ResolutionContext};
use crate::framework::django::resolve_by_name_kind;
use crate::strip_comments::{strip_comments, CommentLang};
use codewiki_core::{CodeWikiError, Language, Node, NodeKind, UnresolvedRef};
use codewiki_storage::traits::ResolvedEdge;
use regex::Regex;
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

// ─── Regexes ─────────────────────────────────────────────────────────────────

/// [HttpGet] / [HttpGet("/path")] / etc. — all seven verbs including Head/Options.
/// Two forms: with path arg and without (bare attribute).
fn http_attr_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"\[(HttpGet|HttpPost|HttpPut|HttpPatch|HttpDelete|HttpHead|HttpOptions)\s*(?:\(\s*"([^"]*)"\s*\))?\]"#,
        )
        .expect("aspnet http_attr_regex")
    })
}

/// Class-level [Route("api/[controller]")] or [Route("api/users")].
fn route_attr_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"\[Route\s*\(\s*"([^"]*)"\s*\)\]"#).expect("aspnet route_attr_regex")
    })
}

/// class XxxController (or `: ControllerBase` etc.) — capture the class name.
fn class_decl_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"\bclass\s+([A-Za-z_][A-Za-z0-9_]*)\b"#).expect("aspnet class_decl_regex")
    })
}

/// Minimal API: app.MapGet("/path", handler) — all 5 verbs + MapMethods.
fn minimal_api_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"\.Map(Get|Post|Put|Patch|Delete|Methods)\s*\(\s*"([^"]+)"\s*,\s*([^,);][^);]*)"#,
        )
        .expect("aspnet minimal_api_regex")
    })
}

/// Generic app.Map("/path", handler) — catch-all verb.
fn minimal_api_map_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Matches `.Map(` that is NOT followed by [A-Z] (to avoid re-matching MapGet etc.)
        Regex::new(r#"\bapp\.Map\s*\(\s*"([^"]+)"\s*,\s*([^,);][^);]*)"#)
            .expect("aspnet minimal_api_map_regex")
    })
}

/// MapGroup: var X = PARENT.MapGroup("/prefix")
/// Captures (new_var, parent_var, prefix_path).
/// Handles both root groups (`app.MapGroup`) and chained groups (`g.MapGroup`).
fn chained_map_group_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"\bvar\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([A-Za-z_][A-Za-z0-9_]*)\.MapGroup\s*\(\s*"([^"]+)"\s*\)"#,
        )
        .expect("aspnet chained_map_group_regex")
    })
}

/// Route registration on a named group variable: group.MapGet("/sub", handler)
fn group_route_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"\b([A-Za-z_][A-Za-z0-9_]*)\.Map(Get|Post|Put|Patch|Delete|Methods)\s*\(\s*"([^"]+)"\s*,\s*([^,);][^);]*)"#,
        )
        .expect("aspnet group_route_regex")
    })
}

/// SignalR: app.MapHub<ChatHub>("/hub-path")
fn map_hub_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"\.MapHub\s*<\s*([A-Za-z_][A-Za-z0-9_]*)\s*>\s*\(\s*"([^"]+)"\s*\)"#)
            .expect("aspnet map_hub_regex")
    })
}

/// DI registration — two-type-arg form:
///   `services.Add{Scoped,Singleton,Transient}<IFoo, FooImpl>()`
///   `services.TryAdd{Scoped,Singleton,Transient}<IFoo, FooImpl>()`
///   `services.Add{...}<IFoo<T>, FooImpl<T, R>>()`   (nested generics)
///
/// Captures the **first identifier** of each type argument; nested generic
/// args (e.g. `ICatalogLookupDataService<CatalogBrand>`) are consumed by an
/// optional `(?:<[^<>]*>)?` group so we still land on the top-level comma.
/// Up to one level of inner nesting is handled; deeper nesting (rare) is
/// skipped gracefully — the single-arg regex won't double-count because it
/// requires `>\s*\(` immediately after the type.
fn di_registration_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // \.(?:TryAdd|Add)(?:Scoped|Singleton|Transient)
        //   <  IDENT  [< inner-generics >]?  ,  IDENT
        Regex::new(
            r#"\.(?:TryAdd|Add)(?:Scoped|Singleton|Transient)\s*<\s*([A-Za-z_][A-Za-z0-9_]*)(?:<[^<>]*(?:<[^<>]*>[^<>]*)*>)?\s*,\s*([A-Za-z_][A-Za-z0-9_]*)"#,
        )
        .expect("aspnet di_registration_regex")
    })
}

/// DI registration — single-type-arg form (self-registration / concrete only):
///   `services.Add{Scoped,Singleton,Transient}<FooService>()`
///   `services.TryAdd{...}<FooService<T>>()`
///
/// The `>\s*\(` anchor at the end ensures we only match the closing `>` of the
/// *outer* generic argument list, not the comma of a two-arg form.  Factory
/// overloads like `AddScoped<Foo>(sp => ...)` are correctly matched (we only
/// capture the type arg, not the delegate).
fn di_single_registration_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // \.(?:TryAdd|Add)(?:Scoped|Singleton|Transient)
        //   <  IDENT  [< inner-generics >]?  >  (
        Regex::new(
            r#"\.(?:TryAdd|Add)(?:Scoped|Singleton|Transient)\s*<\s*([A-Za-z_][A-Za-z0-9_]*)(?:<[^<>]*(?:<[^<>]*>[^<>]*)*>)?\s*>\s*\("#,
        )
        .expect("aspnet di_single_registration_regex")
    })
}

/// Method declaration following an HttpVerb attribute.
fn method_decl_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?:public|private|protected|internal)\s+[^(;{]*\s+([A-Za-z_][A-Za-z0-9_]*)\s*\("#,
        )
        .expect("aspnet method_decl_regex")
    })
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Extract the trailing identifier from a C# handler expression.
/// Returns None for lambdas (contain `=>`), anonymous delegates, or async lambdas.
fn extract_csharp_tail_ident(expr: &str) -> Option<String> {
    let cleaned = expr.trim();
    if cleaned.contains("=>") || cleaned.starts_with("async") {
        return None;
    }
    static TAIL_RE: OnceLock<Regex> = OnceLock::new();
    let re = TAIL_RE.get_or_init(|| {
        Regex::new(r#"(?:\.|^)([A-Za-z_][A-Za-z0-9_]*)$"#).expect("aspnet tail_re")
    });
    re.captures(cleaned.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_'))
        .map(|c| c[1].to_string())
}

/// Map the Http attribute name to an HTTP verb string.
fn http_attr_to_method(attr: &str) -> &'static str {
    match attr {
        "HttpGet" => "GET",
        "HttpPost" => "POST",
        "HttpPut" => "PUT",
        "HttpPatch" => "PATCH",
        "HttpDelete" => "DELETE",
        "HttpHead" => "HEAD",
        "HttpOptions" => "OPTIONS",
        _ => "ANY",
    }
}

/// Expand the `[controller]` token in a route template using the controller class name.
///
/// e.g. template `"api/[controller]"` + class `"UsersController"` → `"api/users"`.
fn expand_controller_token(template: &str, class_name: &str) -> String {
    // Strip "Controller" suffix from class name
    let base = class_name.strip_suffix("Controller").unwrap_or(class_name);
    // ASP.NET convention: lowercase the base name
    let lower = base.to_lowercase();
    template.replace("[controller]", &lower)
}

/// GAP-6: Expand the `[action]` token in a route template using the action method name.
///
/// e.g. template `"[controller]/[action]"` + action `"MyAccount"` → `"[controller]/my-account"`.
/// ASP.NET Core converts the method name to kebab-case by default.
fn expand_action_token(template: &str, method_name: &str) -> String {
    if !template.contains("[action]") {
        return template.to_string();
    }
    // Convert PascalCase method name to kebab-case (ASP.NET default action name transform)
    let kebab = pascal_to_kebab(method_name);
    template.replace("[action]", &kebab)
}

/// Convert a PascalCase or camelCase identifier to kebab-case.
/// e.g. "MyAccount" → "my-account", "GetUser" → "get-user".
fn pascal_to_kebab(name: &str) -> String {
    let mut result = String::with_capacity(name.len() + 4);
    for (i, ch) in name.char_indices() {
        if ch.is_uppercase() && i > 0 {
            result.push('-');
        }
        result.push(ch.to_lowercase().next().unwrap_or(ch));
    }
    result
}

/// Join a class-level route prefix and a method-level route template.
/// Both may be empty. Returns the combined path starting with `/`.
fn join_aspnet_path(prefix: &str, sub: &str) -> String {
    let p = prefix.trim_matches('/');
    let s = sub.trim_matches('/');
    match (p.is_empty(), s.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => format!("/{s}"),
        (false, true) => format!("/{p}"),
        (false, false) => format!("/{p}/{s}"),
    }
}

/// Pre-pass: collect (var_name → resolved_prefix) for MapGroup chains.
///
/// Handles chained nesting (up to 3 levels):
///   `var g = app.MapGroup("/api")` → g → "/api"
///   `var v1 = g.MapGroup("/v1")` → v1 → "/api/v1"
///
/// Strategy: collect all `var X = PARENT.MapGroup("/sub")` assignments as
/// (new_var, parent_var, sub_path). Then resolve iteratively: on each pass,
/// any assignment whose parent is already resolved gets its full path computed.
/// "Root" groups (parent is not another group var, e.g. `app`) seed with their
/// own sub_path directly.
///
/// Returns a map from variable name to its full prefix.
fn collect_map_group_prefixes(safe: &str) -> HashMap<String, String> {
    // Collect all raw assignments in source order.
    // chained_map_group_regex captures: (new_var, parent_var, sub_path)
    let raw: Vec<(String, String, String)> = chained_map_group_regex()
        .captures_iter(safe)
        .map(|cap| (cap[1].to_string(), cap[2].to_string(), cap[3].to_string()))
        .collect();

    let mut prefixes: HashMap<String, String> = HashMap::new();

    // Iterative resolution: repeat until no new entries are added.
    // Each pass resolves entries whose parent is now known.
    let mut changed = true;
    while changed {
        changed = false;
        for (new_var, parent_var, sub) in &raw {
            if prefixes.contains_key(new_var.as_str()) {
                continue; // already resolved
            }
            let full = if let Some(parent_prefix) = prefixes.get(parent_var.as_str()) {
                // Parent is a known group variable — compose paths.
                join_aspnet_path(parent_prefix, sub)
            } else {
                // Parent is not a group var (e.g. `app`, `endpoints`) — use sub directly.
                sub.clone()
            };
            prefixes.insert(new_var.clone(), full);
            changed = true;
        }
    }

    prefixes
}

/// Collect class-level route prefixes from `[Route("...")]` attributes.
///
/// Returns a list of `(byte_offset_of_class_body_start, expanded_prefix)` sorted
/// by offset so we can do a range lookup.
///
/// We approximate the "class body start" as the position of the `class XxxController`
/// keyword that follows the `[Route]` attribute.
fn collect_class_route_prefixes(safe: &str) -> Vec<(usize, usize, String)> {
    let mut results: Vec<(usize, usize, String)> = Vec::new();

    let total_len = safe.len();
    let route_matches: Vec<_> = route_attr_regex().captures_iter(safe).collect();
    let n = route_matches.len();

    for (idx, cap) in route_matches.iter().enumerate() {
        let template = &cap[1];
        let attr_start = cap.get(0).unwrap().start();
        let attr_end = cap.get(0).unwrap().end();

        // Find the next `class` declaration after this attribute.
        let tail = &safe[attr_end..];
        if let Some(class_cap) = class_decl_regex().captures(tail) {
            let class_name = class_cap[1].to_string();
            let class_byte = attr_end + class_cap.get(0).unwrap().start();
            // Scope ends where the next [Route] starts, or EOF.
            let scope_end = if idx + 1 < n {
                route_matches[idx + 1].get(0).unwrap().start()
            } else {
                total_len
            };
            let expanded = expand_controller_token(template, &class_name);
            results.push((class_byte, scope_end, expanded));
        } else {
            let _ = attr_start; // suppress warning
        }
    }

    results
}

/// Find the class-level route prefix that contains `byte_pos`, if any.
fn prefix_for_byte(class_prefixes: &[(usize, usize, String)], byte_pos: usize) -> Option<&str> {
    class_prefixes
        .iter()
        .rfind(|(start, end, _)| *start <= byte_pos && byte_pos < *end)
        .map(|(_, _, prefix)| prefix.as_str())
}

// ─── Resolver ─────────────────────────────────────────────────────────────────

pub struct AspNetResolver;

impl FrameworkResolver for AspNetResolver {
    fn name(&self) -> &'static str {
        "aspnet"
    }

    fn languages(&self) -> Option<&'static [Language]> {
        static LANGS: &[Language] = &[Language::CSharp];
        Some(LANGS)
    }

    fn detect(&self, context: &ResolutionContext<'_>) -> bool {
        // Read .csproj content for ASP.NET markers
        for file in context.known_files {
            if file.ends_with(".csproj") {
                if let Some(content) = context.read_file(file) {
                    if content.contains("Microsoft.AspNetCore")
                        || content.contains("Microsoft.NET.Sdk.Web")
                        || content.contains("System.Web.Mvc")
                    {
                        return true;
                    }
                }
            }
        }
        // Program.cs with WebApplication / CreateHostBuilder
        if let Some(prog) = context.read_file("Program.cs") {
            if prog.contains("WebApplication")
                || prog.contains("CreateHostBuilder")
                || prog.contains("UseStartup")
            {
                return true;
            }
        }
        // Startup.cs presence
        if context.file_exists("Startup.cs") {
            return true;
        }
        // Controller files in conventional location
        context
            .known_files
            .iter()
            .any(|f| f.contains("/Controllers/") && f.ends_with("Controller.cs"))
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &ResolutionContext<'_>,
    ) -> Result<Option<ResolvedEdge>, CodeWikiError> {
        let name = &reference.reference_name;

        // Controller
        if name.ends_with("Controller") {
            if let Some(id) = resolve_by_name_kind(
                name,
                &[NodeKind::Class],
                &["Controllers", "controllers"],
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.85, self.name())));
            }
        }

        // Service / Interface (starts with I + uppercase = interface)
        let is_interface = name.starts_with('I')
            && name.len() > 1
            && name
                .chars()
                .nth(1)
                .map(|c| c.is_uppercase())
                .unwrap_or(false);
        if name.ends_with("Service") || is_interface {
            if let Some(id) = resolve_by_name_kind(
                name,
                &[NodeKind::Class, NodeKind::Interface],
                &["Services", "services", "Service", "Application"],
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.85, self.name())));
            }
        }

        // Repository
        if name.ends_with("Repository") {
            if let Some(id) = resolve_by_name_kind(
                name,
                &[NodeKind::Class],
                &[
                    "Repositories",
                    "repositories",
                    "Repository",
                    "Data",
                    "Infrastructure",
                ],
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.85, self.name())));
            }
        }

        // Handler
        if name.ends_with("Handler") {
            if let Some(id) = resolve_by_name_kind(
                name,
                &[NodeKind::Class, NodeKind::Function, NodeKind::Method],
                &["Handlers", "handlers", "Commands", "Queries", "Features"],
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.85, self.name())));
            }
        }

        // Validator
        if name.ends_with("Validator") {
            if let Some(id) = resolve_by_name_kind(
                name,
                &[NodeKind::Class],
                &["Validators", "validators", "Validation"],
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.80, self.name())));
            }
        }

        // ViewModel / Dto
        if name.ends_with("ViewModel") || name.ends_with("Dto") {
            if let Some(id) = resolve_by_name_kind(
                name,
                &[NodeKind::Class],
                &["ViewModels", "ViewModel", "DTOs", "Dto", "Models"],
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.80, self.name())));
            }
        }

        // Hub (SignalR)
        if name.ends_with("Hub") {
            if let Some(id) = resolve_by_name_kind(
                name,
                &[NodeKind::Class],
                &["Hubs", "hubs", "SignalR", "Realtime"],
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.85, self.name())));
            }
        }

        // PascalCase model / entity (lower confidence)
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
                &["Models", "Model", "Entities", "Entity", "Domain"],
                context,
            ) {
                return Ok(Some(make_resolved_edge(reference, id, 0.70, self.name())));
            }
        }

        // Method-name fallback
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
        if !file_str.ends_with(".cs") {
            return Ok(FrameworkExtractionResult::empty());
        }

        let safe = strip_comments(content, CommentLang::CSharp);
        let mut nodes = Vec::new();
        let mut unresolved_refs = Vec::new();

        // ── Pre-pass A: class-level [Route] prefixes for [controller] token ──
        let class_prefixes = collect_class_route_prefixes(&safe);

        // ── Pre-pass B: MapGroup variable → prefix map ────────────────────────
        let group_prefixes = collect_map_group_prefixes(&safe);

        // ── Pattern A: [HttpVerb("path")] + following method declaration ──────
        for cap in http_attr_regex().captures_iter(&safe) {
            let attr_name = &cap[1];
            let method_path = cap.get(2).map(|m| m.as_str()).unwrap_or("");
            let method = http_attr_to_method(attr_name);
            let match_start = cap.get(0).unwrap().start();
            let match_end = cap.get(0).unwrap().end();
            let line = safe[..match_start].lines().count() as u32 + 1;

            // Look up class-level route prefix
            let class_prefix = prefix_for_byte(&class_prefixes, match_start).unwrap_or("");

            // Capture the method declaration that immediately follows the attribute
            // (needed before path expansion so we can resolve [action])
            let tail = &safe[match_end..];
            let handler_name_opt = method_decl_regex().captures(tail).map(|c| c[1].to_string());

            // GAP-6: Expand [action] token using the handler method name
            let method_path_expanded = if let Some(ref h) = handler_name_opt {
                expand_action_token(method_path, h)
            } else {
                method_path.to_string()
            };
            let class_prefix_expanded = if let Some(ref h) = handler_name_opt {
                expand_action_token(class_prefix, h)
            } else {
                class_prefix.to_string()
            };

            let full_path = join_aspnet_path(&class_prefix_expanded, &method_path_expanded);

            let route_id = format!("route:{}:{}:{}:{}", file_str, line, method, full_path);

            let route_node = Node {
                id: route_id.clone(),
                name: format!("{} {}", method, full_path),
                qualified_name: format!("{}::{}:{}", file_str, method, full_path),
                kind: NodeKind::Route,
                language: Language::CSharp,
                file_path: file_str.to_string(),
                start_line: line,
                end_line: line,
                ..Default::default()
            };
            nodes.push(route_node);

            if let Some(handler_name) = handler_name_opt {
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

        // ── Pattern B: app.MapGet/Post/etc. — Minimal API (direct) ───────────
        // We match on `app.` or `endpoints.` or any receiver followed by .Map{Verb}.
        // The group_route_regex handles group-variable receivers separately below,
        // so here we only process lines where the receiver is NOT a known group var.
        for cap in minimal_api_regex().captures_iter(&safe) {
            let verb = &cap[1];
            let route_path = &cap[2];
            let handler_expr = cap[3].trim().to_string();
            let match_start = cap.get(0).unwrap().start();
            let line = safe[..match_start].lines().count() as u32 + 1;
            let method = verb.to_uppercase();

            // Check if the receiver is a known group variable — if so, skip here
            // (handled in Pattern C below).
            // We find the receiver by scanning backward for `.Map{Verb}` prefix.
            let prefix_in_source = &safe[..match_start];
            // Last non-whitespace token before `.Map` is the receiver
            let receiver_candidate = prefix_in_source
                .rsplit(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
                .unwrap_or("");
            if group_prefixes.contains_key(receiver_candidate) {
                continue; // will be handled in Pattern C
            }

            let route_id = format!("route:{}:{}:{}:{}", file_str, line, method, route_path);

            nodes.push(Node {
                id: route_id.clone(),
                name: format!("{} {}", method, route_path),
                qualified_name: format!("{}::{}:{}", file_str, method, route_path),
                kind: NodeKind::Route,
                language: Language::CSharp,
                file_path: file_str.to_string(),
                start_line: line,
                end_line: line,
                ..Default::default()
            });

            if let Some(handler_name) = extract_csharp_tail_ident(&handler_expr) {
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

        // ── Pattern B2: app.Map("/path", handler) — generic catch-all verb ────
        for cap in minimal_api_map_regex().captures_iter(&safe) {
            let route_path = &cap[1];
            let handler_expr = cap[2].trim().to_string();
            let match_start = cap.get(0).unwrap().start();
            let line = safe[..match_start].lines().count() as u32 + 1;

            let route_id = format!("route:{}:{}:ANY:{}", file_str, line, route_path);

            nodes.push(Node {
                id: route_id.clone(),
                name: format!("ANY {}", route_path),
                qualified_name: format!("{}::ANY:{}", file_str, route_path),
                kind: NodeKind::Route,
                language: Language::CSharp,
                file_path: file_str.to_string(),
                start_line: line,
                end_line: line,
                ..Default::default()
            });

            if let Some(handler_name) = extract_csharp_tail_ident(&handler_expr) {
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

        // ── Pattern C: MapGroup-prefixed routes ───────────────────────────────
        for cap in group_route_regex().captures_iter(&safe) {
            let receiver = &cap[1];
            let verb = &cap[2];
            let sub_path = &cap[3];
            let handler_expr = cap[4].trim().to_string();
            let match_start = cap.get(0).unwrap().start();
            let line = safe[..match_start].lines().count() as u32 + 1;

            let receiver_str: &str = receiver;
            let group_prefix = match group_prefixes.get(receiver_str) {
                Some(p) => p.clone(),
                None => continue, // not a known group var — skip
            };

            let full_path = join_aspnet_path(&group_prefix, sub_path);
            let method = verb.to_uppercase();

            let route_id = format!("route:{}:{}:{}:{}", file_str, line, method, full_path);

            nodes.push(Node {
                id: route_id.clone(),
                name: format!("{} {}", method, full_path),
                qualified_name: format!("{}::{}:{}", file_str, method, full_path),
                kind: NodeKind::Route,
                language: Language::CSharp,
                file_path: file_str.to_string(),
                start_line: line,
                end_line: line,
                ..Default::default()
            });

            if let Some(handler_name) = extract_csharp_tail_ident(&handler_expr) {
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

        // ── Pattern D: SignalR MapHub<THub>("/path") ──────────────────────────
        for cap in map_hub_regex().captures_iter(&safe) {
            let hub_type = &cap[1];
            let hub_path = &cap[2];
            let match_start = cap.get(0).unwrap().start();
            let line = safe[..match_start].lines().count() as u32 + 1;

            let route_id = format!("route:{}:{}:HUB:{}", file_str, line, hub_path);

            nodes.push(Node {
                id: route_id.clone(),
                name: format!("HUB {}", hub_path),
                qualified_name: format!("{}::HUB:{}", file_str, hub_path),
                kind: NodeKind::Route,
                language: Language::CSharp,
                file_path: file_str.to_string(),
                start_line: line,
                end_line: line,
                ..Default::default()
            });

            // Emit a reference to the Hub class itself
            unresolved_refs.push(UnresolvedRef {
                id: format!("{}->{}", route_id, hub_type),
                from_node_id: route_id,
                reference_name: hub_type.to_string(),
                reference_kind: "references".to_string(),
                file_path: file_str.to_string(),
                line: Some(line),
                ..Default::default()
            });
        }

        // ── Pattern E: DI registrations ───────────────────────────────────────
        // Covers all three ASP.NET Core lifetimes (Scoped/Singleton/Transient)
        // and their TryAdd* variants, in both two-type-arg and single-type-arg
        // forms, with optional nested generic type arguments.
        //
        // GAP-3 fix: use the REAL file node ID (`file:<path>`) as `from_node_id`
        // so the storage FK constraint (from_node_id → nodes.id) is always
        // satisfied.  Previously a synthetic "di:..." ID was used which has no
        // corresponding row in nodes → FK violation → silent drop.
        let file_node_id = format!("file:{file_str}");

        // E1: Two-type-arg form → interface + impl (emit "implements" edge)
        //     AddScoped<IFoo, Foo>()  /  AddScoped<IFoo<T>, Impl<T>>()
        for cap in di_registration_regex().captures_iter(&safe) {
            let interface_name = cap[1].to_string();
            let impl_name = cap[2].to_string();
            let match_start = cap.get(0).unwrap().start();
            let line = safe[..match_start].lines().count() as u32 + 1;

            // The ref id remains unique per registration; from_node_id now
            // points to the file node which always exists.
            let ref_id = format!("di:{}:{}:{}:{}", file_str, line, interface_name, impl_name);
            unresolved_refs.push(UnresolvedRef {
                id: format!("{}->{}", ref_id, impl_name),
                from_node_id: file_node_id.clone(),
                reference_name: impl_name,
                reference_kind: "implements".to_string(),
                file_path: file_str.to_string(),
                line: Some(line),
                ..Default::default()
            });
        }

        // E2: Single-type-arg form → concrete class self-registration (emit "references" edge)
        //     AddScoped<FooService>()  /  AddScoped<FooService>(sp => ...)
        //     AddScoped<FooService<T>>()
        //
        // We skip any registration whose concrete type was already emitted by E1
        // (two-arg form) to avoid duplicate edges.  We collect the byte offsets
        // of every two-arg match and skip single-arg matches at the same offset.
        {
            use std::collections::HashSet;
            let two_arg_offsets: HashSet<usize> = di_registration_regex()
                .captures_iter(&safe)
                .map(|cap| cap.get(0).unwrap().start())
                .collect();

            for cap in di_single_registration_regex().captures_iter(&safe) {
                let match_start = cap.get(0).unwrap().start();
                if two_arg_offsets.contains(&match_start) {
                    continue; // already handled by E1
                }
                let concrete_name = cap[1].to_string();
                let line = safe[..match_start].lines().count() as u32 + 1;

                let ref_id = format!("di:{}:{}:{}", file_str, line, concrete_name);
                unresolved_refs.push(UnresolvedRef {
                    id: format!("{}->{}", ref_id, concrete_name),
                    from_node_id: file_node_id.clone(),
                    reference_name: concrete_name,
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
            project_languages: &[Language::CSharp],
            caches,
            path_aliases: aliases,
            known_files,
        }
    }

    // ── Original tests (preserved) ────────────────────────────────────────────

    #[test]
    fn aspnet_extracts_http_get_with_path_and_handler() {
        let content = r#"
            [HttpGet("/users")]
            public IActionResult GetUsers() { return Ok(users); }
        "#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let resolver = AspNetResolver;
        let result = resolver
            .extract(Path::new("Controllers/UserController.cs"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.nodes.len(), 1, "one route node");
        assert_eq!(result.nodes[0].name, "GET /users");
        assert_eq!(
            result.nodes[0].qualified_name,
            "Controllers/UserController.cs::GET:/users"
        );
        assert_eq!(result.unresolved_refs.len(), 1, "one handler ref");
        assert_eq!(result.unresolved_refs[0].reference_name, "GetUsers");
        assert_eq!(result.unresolved_refs[0].reference_kind, "references");
        assert!(result.unresolved_refs[0].id.ends_with("->GetUsers"));
    }

    #[test]
    fn aspnet_extracts_http_post() {
        let content = r#"
            [HttpPost("/users")]
            public IActionResult CreateUser([FromBody] UserDto dto) { return Created(); }
        "#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Controllers/UserController.cs"), content, &ctx)
            .expect("extract ok");
        assert_eq!(result.nodes[0].name, "POST /users");
        assert_eq!(result.unresolved_refs[0].reference_name, "CreateUser");
    }

    #[test]
    fn aspnet_extracts_http_put() {
        let content = r#"
            [HttpPut("/users/{id}")]
            public IActionResult UpdateUser(int id, [FromBody] UserDto dto) { return Ok(); }
        "#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Controllers/UserController.cs"), content, &ctx)
            .expect("extract ok");
        assert_eq!(result.nodes[0].name, "PUT /users/{id}");
        assert_eq!(result.unresolved_refs[0].reference_name, "UpdateUser");
    }

    #[test]
    fn aspnet_extracts_http_patch() {
        let content = r#"
            [HttpPatch("/users/{id}")]
            public IActionResult PatchUser(int id) { return Ok(); }
        "#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Controllers/UserController.cs"), content, &ctx)
            .expect("extract ok");
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "PATCH /users/{id}");
        assert_eq!(result.unresolved_refs[0].reference_name, "PatchUser");
    }

    #[test]
    fn aspnet_extracts_http_delete() {
        let content = r#"
            [HttpDelete("/users/{id}")]
            public IActionResult DeleteUser(int id) { return NoContent(); }
        "#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Controllers/UserController.cs"), content, &ctx)
            .expect("extract ok");
        assert_eq!(result.nodes[0].name, "DELETE /users/{id}");
        assert_eq!(result.unresolved_refs[0].reference_name, "DeleteUser");
    }

    #[test]
    fn aspnet_extracts_minimal_api_map_get() {
        let content = r#"app.MapGet("/users", GetUsersHandler);"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Program.cs"), content, &ctx)
            .expect("extract ok");
        assert_eq!(result.nodes.len(), 1, "one route node");
        assert_eq!(result.nodes[0].name, "GET /users");
        assert_eq!(result.unresolved_refs.len(), 1, "one handler ref");
        assert_eq!(result.unresolved_refs[0].reference_name, "GetUsersHandler");
    }

    #[test]
    fn aspnet_minimal_api_lambda_emits_no_ref() {
        let content = r#"app.MapGet("/ping", () => "pong");"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Program.cs"), content, &ctx)
            .expect("extract ok");
        assert_eq!(result.nodes.len(), 1, "one route node");
        assert_eq!(
            result.unresolved_refs.len(),
            0,
            "lambda handler emits no unresolved_ref"
        );
    }

    #[test]
    fn aspnet_minimal_api_map_post() {
        let content = r#"app.MapPost("/users", CreateUserHandler);"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Program.cs"), content, &ctx)
            .expect("extract ok");
        assert_eq!(result.nodes[0].name, "POST /users");
        assert_eq!(
            result.unresolved_refs[0].reference_name,
            "CreateUserHandler"
        );
    }

    #[test]
    fn aspnet_detect_via_csproj_content() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("MyApp.csproj"),
            r#"<Project Sdk="Microsoft.NET.Sdk.Web"></Project>"#,
        )
        .unwrap();

        let known: Vec<String> = vec!["MyApp.csproj".to_string()];
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = ResolutionContext {
            project_root: dir.path(),
            project_languages: &[Language::CSharp],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &known,
        };

        assert!(
            AspNetResolver.detect(&ctx),
            "Microsoft.NET.Sdk.Web in .csproj must trigger detect"
        );
    }

    #[test]
    fn aspnet_detect_via_program_cs() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Program.cs"),
            "var app = WebApplication.Create(args); app.Run();",
        )
        .unwrap();

        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = ResolutionContext {
            project_root: dir.path(),
            project_languages: &[Language::CSharp],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &[],
        };

        assert!(
            AspNetResolver.detect(&ctx),
            "WebApplication in Program.cs must trigger detect"
        );
    }

    #[test]
    fn aspnet_skips_non_cs_files() {
        let content = r#"[HttpGet("/users")]\npublic IActionResult GetUsers() {}"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("UserController.vb"), content, &ctx)
            .expect("extract ok");
        assert!(
            result.nodes.is_empty(),
            "non-.cs file must produce no nodes"
        );
    }

    #[test]
    fn aspnet_tail_ident_skips_lambda() {
        assert!(extract_csharp_tail_ident("() => \"pong\"").is_none());
        assert!(extract_csharp_tail_ident("async (UserDto dto) => { }").is_none());
    }

    #[test]
    fn aspnet_tail_ident_extracts_simple_name() {
        assert_eq!(
            extract_csharp_tail_ident("GetUsersHandler"),
            Some("GetUsersHandler".to_string())
        );
        assert_eq!(
            extract_csharp_tail_ident("handlers.GetUsersHandler"),
            Some("GetUsersHandler".to_string())
        );
    }

    // ── New tests: HttpHead and HttpOptions ───────────────────────────────────

    #[test]
    fn aspnet_extracts_http_head() {
        let content = r#"
            [HttpHead("/health")]
            public IActionResult HeadHealth() { return Ok(); }
        "#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Controllers/HealthController.cs"), content, &ctx)
            .expect("extract ok");
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "HEAD /health");
    }

    #[test]
    fn aspnet_extracts_http_options() {
        let content = r#"
            [HttpOptions("/cors")]
            public IActionResult OptionsCors() { return Ok(); }
        "#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Controllers/CorsController.cs"), content, &ctx)
            .expect("extract ok");
        assert_eq!(result.nodes[0].name, "OPTIONS /cors");
    }

    // ── New tests: bare [HttpGet] without path arg ────────────────────────────

    #[test]
    fn aspnet_bare_http_get_no_path() {
        // [HttpGet] with no path arg — path should default to "/"
        let content = r#"
            [HttpGet]
            public IActionResult GetAll() { return Ok(); }
        "#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Controllers/ItemController.cs"), content, &ctx)
            .expect("extract ok");
        // bare attribute path is empty → joined path is "/"
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "GET /");
    }

    // ── New tests: [Route("api/[controller]")] token expansion ───────────────

    #[test]
    fn aspnet_route_attr_controller_token_expansion() {
        let content = r#"
[Route("api/[controller]")]
[ApiController]
public class ProductsController : ControllerBase
{
    [HttpGet("{id}")]
    public IActionResult GetProduct(int id) { return Ok(); }

    [HttpPost]
    public IActionResult CreateProduct([FromBody] ProductDto dto) { return Created(); }
}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(
                Path::new("Controllers/ProductsController.cs"),
                content,
                &ctx,
            )
            .expect("extract ok");

        let names: Vec<_> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"GET /api/products/{id}"),
            "GET with expanded [controller]: {:?}",
            names
        );
        assert!(
            names.contains(&"POST /api/products"),
            "POST with expanded [controller]: {:?}",
            names
        );
    }

    #[test]
    fn aspnet_controller_token_expansion_unit() {
        assert_eq!(
            expand_controller_token("api/[controller]", "UsersController"),
            "api/users"
        );
        assert_eq!(
            expand_controller_token("api/v2/[controller]", "OrdersController"),
            "api/v2/orders"
        );
        assert_eq!(
            expand_controller_token("api/items", "ItemController"),
            "api/items"
        );
    }

    // ── New tests: MapGroup prefix composition ────────────────────────────────

    #[test]
    fn aspnet_map_group_prefix_composition() {
        let content = r#"
var usersGroup = app.MapGroup("/api/users");
usersGroup.MapGet("/{id}", GetUserById);
usersGroup.MapPost("/", CreateUser);
usersGroup.MapDelete("/{id}", DeleteUser);
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Program.cs"), content, &ctx)
            .expect("extract ok");

        let names: Vec<_> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"GET /api/users/{id}"),
            "MapGroup GET: {:?}",
            names
        );
        assert!(
            names.contains(&"POST /api/users"),
            "MapGroup POST: {:?}",
            names
        );
        assert!(
            names.contains(&"DELETE /api/users/{id}"),
            "MapGroup DELETE: {:?}",
            names
        );
    }

    #[test]
    fn aspnet_map_group_nested_two_levels() {
        let content = r#"
var api = app.MapGroup("/api");
var v1 = api.MapGroup("/v1");
v1.MapGet("/status", GetStatus);
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Program.cs"), content, &ctx)
            .expect("extract ok");

        let names: Vec<_> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"GET /api/v1/status"),
            "nested MapGroup: {:?}",
            names
        );
    }

    #[test]
    fn aspnet_map_group_handler_ref_emitted() {
        let content = r#"
var orders = app.MapGroup("/orders");
orders.MapGet("/{id}", GetOrderHandler);
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Program.cs"), content, &ctx)
            .expect("extract ok");

        assert_eq!(result.unresolved_refs.len(), 1);
        assert_eq!(result.unresolved_refs[0].reference_name, "GetOrderHandler");
        assert!(result.unresolved_refs[0].id.contains("->GetOrderHandler"));
    }

    // ── New tests: SignalR MapHub ──────────────────────────────────────────────

    #[test]
    fn aspnet_map_hub_emits_hub_route() {
        let content = r#"
app.MapHub<ChatHub>("/chat");
app.MapHub<NotificationHub>("/notifications");
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Program.cs"), content, &ctx)
            .expect("extract ok");

        let names: Vec<_> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"HUB /chat"), "ChatHub route: {:?}", names);
        assert!(
            names.contains(&"HUB /notifications"),
            "NotificationHub route: {:?}",
            names
        );

        // Each hub route emits a ref to the Hub class
        let refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(refs.contains(&"ChatHub"), "ChatHub ref: {:?}", refs);
        assert!(
            refs.contains(&"NotificationHub"),
            "NotificationHub ref: {:?}",
            refs
        );
    }

    #[test]
    fn aspnet_map_hub_qualified_name_format() {
        let content = r#"app.MapHub<ChatHub>("/chat");"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Program.cs"), content, &ctx)
            .unwrap();
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].qualified_name, "Program.cs::HUB:/chat");
    }

    // ── New tests: DI registrations ───────────────────────────────────────────

    #[test]
    fn aspnet_di_registration_emits_implements_ref() {
        let content = r#"
builder.Services.AddScoped<IUserService, UserService>();
builder.Services.AddSingleton<ILogger, ConsoleLogger>();
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Program.cs"), content, &ctx)
            .expect("extract ok");

        // No route nodes expected
        assert!(result.nodes.is_empty(), "DI only emits refs, not nodes");

        let impl_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "implements")
            .collect();
        assert_eq!(impl_refs.len(), 2, "two DI refs: {:?}", impl_refs);
        let impl_names: Vec<_> = impl_refs
            .iter()
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(impl_names.contains(&"UserService"));
        assert!(impl_names.contains(&"ConsoleLogger"));
    }

    // ── New tests: DI – all lifetimes + single-arg + TryAdd ──────────────────

    #[test]
    fn aspnet_di_scoped_and_transient_produce_refs() {
        // GAP-3 extension: AddScoped AND AddTransient must both produce DI edges.
        let content = r#"
services.AddScoped<IFoo, FooImpl>();
services.AddTransient<IBar, BarImpl>();
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Program.cs"), content, &ctx)
            .expect("extract ok");

        let impl_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "implements")
            .collect();
        assert_eq!(
            impl_refs.len(),
            2,
            "both AddScoped and AddTransient must produce refs: {:?}",
            impl_refs
        );
        let names: Vec<_> = impl_refs
            .iter()
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(names.contains(&"FooImpl"), "FooImpl missing: {:?}", names);
        assert!(names.contains(&"BarImpl"), "BarImpl missing: {:?}", names);
    }

    #[test]
    fn aspnet_di_single_type_arg_produces_references_ref() {
        // Self-registration: AddScoped<ConcreteService>() → references edge.
        let content = r#"
services.AddScoped<ToastService>();
services.AddScoped<HttpService>();
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Program.cs"), content, &ctx)
            .expect("extract ok");

        let refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "references")
            .collect();
        // Should have at least the two self-registrations (may also have route refs = 0 here)
        assert!(
            refs.len() >= 2,
            "single-arg DI must produce references refs: {:?}",
            refs
        );
        let names: Vec<_> = refs.iter().map(|r| r.reference_name.as_str()).collect();
        assert!(
            names.contains(&"ToastService"),
            "ToastService missing: {:?}",
            names
        );
        assert!(
            names.contains(&"HttpService"),
            "HttpService missing: {:?}",
            names
        );
    }

    #[test]
    fn aspnet_di_try_add_variants_produce_refs() {
        // TryAddScoped / TryAddTransient / TryAddSingleton — common in library code.
        let content = r#"
services.TryAddScoped<ICache, MemoryCache>();
services.TryAddTransient<IWorker, BackgroundWorker>();
services.TryAddSingleton<IConfig, ConfigImpl>();
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Extensions.cs"), content, &ctx)
            .expect("extract ok");

        let impl_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "implements")
            .collect();
        assert_eq!(
            impl_refs.len(),
            3,
            "all three TryAdd* lifetimes must produce refs: {:?}",
            impl_refs
        );
        let names: Vec<_> = impl_refs
            .iter()
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(
            names.contains(&"MemoryCache"),
            "MemoryCache missing: {:?}",
            names
        );
        assert!(
            names.contains(&"BackgroundWorker"),
            "BackgroundWorker missing: {:?}",
            names
        );
        assert!(
            names.contains(&"ConfigImpl"),
            "ConfigImpl missing: {:?}",
            names
        );
    }

    #[test]
    fn aspnet_di_nested_generic_type_args() {
        // AddScoped<ICatalogLookupDataService<CatalogBrand>, CachedDecorator<CatalogBrand>>()
        // → captures ICatalogLookupDataService and CachedDecorator as base names.
        let content = r#"
services.AddScoped<ICatalogLookupDataService<CatalogBrand>, CachedCatalogLookupDataServiceDecorator<CatalogBrand>>();
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("ServicesConfiguration.cs"), content, &ctx)
            .expect("extract ok");

        let impl_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "implements")
            .collect();
        assert_eq!(
            impl_refs.len(),
            1,
            "one nested-generic DI ref: {:?}",
            impl_refs
        );
        assert_eq!(
            impl_refs[0].reference_name,
            "CachedCatalogLookupDataServiceDecorator"
        );
    }

    #[test]
    fn aspnet_di_single_type_arg_no_duplicate_with_two_arg() {
        // A two-arg registration must NOT also appear in single-arg results.
        let content = r#"
services.AddScoped<IFoo, Foo>();
services.AddScoped<Bar>();
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Program.cs"), content, &ctx)
            .expect("extract ok");

        // Expect exactly: 1 "implements" ref (two-arg) + 1 "references" ref (single-arg)
        let impl_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "implements")
            .collect();
        let ref_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "references")
            .collect();
        assert_eq!(impl_refs.len(), 1, "one implements ref: {:?}", impl_refs);
        assert_eq!(impl_refs[0].reference_name, "Foo");
        assert_eq!(ref_refs.len(), 1, "one references ref: {:?}", ref_refs);
        assert_eq!(ref_refs[0].reference_name, "Bar");
    }

    // ── New tests: MapMethods / generic Map ───────────────────────────────────

    #[test]
    fn aspnet_minimal_api_map_delete() {
        let content = r#"app.MapDelete("/users/{id}", DeleteUserHandler);"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Program.cs"), content, &ctx)
            .unwrap();
        assert_eq!(result.nodes[0].name, "DELETE /users/{id}");
        assert_eq!(
            result.unresolved_refs[0].reference_name,
            "DeleteUserHandler"
        );
    }

    #[test]
    fn aspnet_minimal_api_map_put() {
        let content = r#"app.MapPut("/items/{id}", UpdateItemHandler);"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Program.cs"), content, &ctx)
            .unwrap();
        assert_eq!(result.nodes[0].name, "PUT /items/{id}");
    }

    #[test]
    fn aspnet_generic_map_catchall_verb() {
        let content = r#"app.Map("/legacy", LegacyHandler);"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AspNetResolver
            .extract(Path::new("Program.cs"), content, &ctx)
            .unwrap();
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "ANY /legacy");
        assert_eq!(result.unresolved_refs[0].reference_name, "LegacyHandler");
    }

    // ── New tests: resolve() ──────────────────────────────────────────────────

    #[test]
    fn aspnet_resolve_handler_suffix() {
        use codewiki_core::{Node, NodeKind};
        let caches = ResolverCaches::default_capacity();
        let handler_node = Node {
            id: "node:Features/CreateOrder/CreateOrderHandler.cs:CreateOrderHandler".to_string(),
            name: "CreateOrderHandler".to_string(),
            qualified_name: "Features/CreateOrder/CreateOrderHandler.cs::CreateOrderHandler"
                .to_string(),
            kind: NodeKind::Class,
            language: Language::CSharp,
            file_path: "Features/CreateOrder/CreateOrderHandler.cs".to_string(),
            start_line: 1,
            end_line: 50,
            ..Default::default()
        };
        caches
            .name_cache
            .insert("CreateOrderHandler".to_string(), vec![handler_node.clone()]);

        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = UnresolvedRef {
            id: "ref:CreateOrderHandler".to_string(),
            from_node_id: "route:Program.cs:5:POST:/orders".to_string(),
            reference_name: "CreateOrderHandler".to_string(),
            reference_kind: "references".to_string(),
            file_path: "Program.cs".to_string(),
            ..Default::default()
        };
        let result = AspNetResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some(), "Handler suffix should resolve");
        assert_eq!(result.unwrap().confidence, 0.85);
    }

    #[test]
    fn aspnet_resolve_validator_suffix() {
        use codewiki_core::{Node, NodeKind};
        let caches = ResolverCaches::default_capacity();
        let val_node = Node {
            id: "node:Validators/CreateUserValidator.cs:CreateUserValidator".to_string(),
            name: "CreateUserValidator".to_string(),
            qualified_name: "Validators/CreateUserValidator.cs::CreateUserValidator".to_string(),
            kind: NodeKind::Class,
            language: Language::CSharp,
            file_path: "Validators/CreateUserValidator.cs".to_string(),
            start_line: 1,
            end_line: 20,
            ..Default::default()
        };
        caches
            .name_cache
            .insert("CreateUserValidator".to_string(), vec![val_node.clone()]);

        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = UnresolvedRef {
            id: "ref:CreateUserValidator".to_string(),
            from_node_id: "route:Program.cs:3:POST:/users".to_string(),
            reference_name: "CreateUserValidator".to_string(),
            reference_kind: "references".to_string(),
            file_path: "Program.cs".to_string(),
            ..Default::default()
        };
        let result = AspNetResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some(), "Validator suffix should resolve");
        assert_eq!(result.unwrap().confidence, 0.80);
    }

    #[test]
    fn aspnet_resolve_hub_class() {
        use codewiki_core::{Node, NodeKind};
        let caches = ResolverCaches::default_capacity();
        let hub_node = Node {
            id: "node:Hubs/ChatHub.cs:ChatHub".to_string(),
            name: "ChatHub".to_string(),
            qualified_name: "Hubs/ChatHub.cs::ChatHub".to_string(),
            kind: NodeKind::Class,
            language: Language::CSharp,
            file_path: "Hubs/ChatHub.cs".to_string(),
            start_line: 1,
            end_line: 80,
            ..Default::default()
        };
        caches
            .name_cache
            .insert("ChatHub".to_string(), vec![hub_node.clone()]);

        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = UnresolvedRef {
            id: "ref:ChatHub".to_string(),
            from_node_id: "route:Program.cs:10:HUB:/chat".to_string(),
            reference_name: "ChatHub".to_string(),
            reference_kind: "references".to_string(),
            file_path: "Program.cs".to_string(),
            ..Default::default()
        };
        let result = AspNetResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some(), "Hub class should resolve");
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 0.85);
        assert_eq!(edge.edge.target_id, hub_node.id);
    }

    // ── join_aspnet_path ──────────────────────────────────────────────────────

    #[test]
    fn join_aspnet_path_prefix_and_sub() {
        assert_eq!(join_aspnet_path("api/users", "{id}"), "/api/users/{id}");
    }

    #[test]
    fn join_aspnet_path_empty_sub() {
        assert_eq!(join_aspnet_path("api/items", ""), "/api/items");
    }

    #[test]
    fn join_aspnet_path_both_empty() {
        assert_eq!(join_aspnet_path("", ""), "/");
    }
}

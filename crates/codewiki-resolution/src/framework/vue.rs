// T-308 — Vue/Nuxt framework resolver (deep coverage)
//
// Detection: vue/nuxt/@nuxt/kit in deps+devDeps; fallback .vue file scan.
//
// Extraction:
//   A  — Nuxt pages/ file-system routing (including catch-all, optional, nested)
//        + definePageMeta path override, alias, and middleware edges
//   B  — Nuxt server/api/ endpoint routes (HTTP verb from filename suffix)
//   C  — Nuxt middleware/ function nodes
//   D  — vue-router 4 createRouter({routes:[...]}) config → route nodes
//        (nested children path composition, lazy component imports)
//   E  — Pinia defineStore('id', ...) → store function nodes
//   F  — Nuxt composables/ directory → function nodes (auto-import)
//
// Resolution:
//   1  — Vue compiler macros (defineProps, defineEmits, etc.) → self-ref 1.0
//   2  — Nuxt auto-imported composables → self-ref 1.0
//   3  — Nuxt virtual modules (#imports, etc.) → self-ref 1.0
//   4  — @/ alias → src/
//   5  — ~/ alias → src/
//   6  — PascalCase component refs (reference_kind = "calls")
//   7  — kebab-case component refs → resolved PascalCase (e.g. <my-button> → MyButton)
//   8  — useXxx composable refs → composables/ dir lookup
//   9  — Pinia store ref (useXStore) → store node lookup
//  10  — middleware ref from definePageMeta → middleware/ function node (0.85)
//
// Boundary: do NOT emit Component nodes — extraction-side vue.rs already does that.
//
// Review fixes applied:
//   F3  — known_files element type verified as &[String]
//   F6  — qualified_name = "{file}::{METHOD}:{path}" for route nodes
//   F7  — UnresolvedRef.id = "{route_id}->{reference_name}"

use super::scan_utils::{read_args, read_bracket_array, read_object};
use super::{make_resolved_edge, FrameworkExtractionResult, FrameworkResolver, ResolutionContext};
use codewiki_core::{CodeWikiError, Language, Node, NodeKind, UnresolvedRef};
use codewiki_storage::traits::ResolvedEdge;
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

// ─── Statics ──────────────────────────────────────────────────────────────────

static VUE_COMPILER_MACROS: OnceLock<HashSet<&'static str>> = OnceLock::new();
fn vue_compiler_macros() -> &'static HashSet<&'static str> {
    VUE_COMPILER_MACROS.get_or_init(|| {
        [
            "defineProps",
            "defineEmits",
            "defineExpose",
            "defineOptions",
            "defineSlots",
            "defineModel",
            "withDefaults",
            // Additional macros not in initial pass:
            "defineAsyncComponent",
            "defineComponent",
            "defineNuxtComponent",
        ]
        .into_iter()
        .collect()
    })
}

static NUXT_AUTO_IMPORTS: OnceLock<HashSet<&'static str>> = OnceLock::new();
fn nuxt_auto_imports() -> &'static HashSet<&'static str> {
    NUXT_AUTO_IMPORTS.get_or_init(|| {
        [
            "useRoute",
            "useRouter",
            "navigateTo",
            "useFetch",
            "useAsyncData",
            "useState",
            "useHead",
            "useSeoMeta",
            "useRuntimeConfig",
            "useNuxtApp",
            "useCookie",
            "useError",
            "createError",
            "showError",
            "clearError",
            "definePageMeta",
            "defineNuxtConfig",
            "defineNuxtPlugin",
            "defineNuxtRouteMiddleware",
            "useRequestHeaders",
            "useRequestEvent",
            "useLazyFetch",
            "useLazyAsyncData",
            "useAppConfig",
            "updateAppConfig",
            "useNuxtData",
            "refreshNuxtData",
            "clearNuxtData",
            "useHydration",
            "callOnce",
            "defineRouteRules",
            "preloadComponents",
            "prefetchComponents",
            "isPrerendered",
        ]
        .into_iter()
        .collect()
    })
}

static NUXT_VIRTUAL_MODULES: &[&str] = &["#imports", "#components", "#app", "#build", "#head"];

// ─── Regexes ─────────────────────────────────────────────────────────────────

/// vue-router 4 route object path: `path: '/foo'`
fn vue_router_path_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"\bpath\s*:\s*['"`]([^'"`]*)['"`]"#).expect("vue_router_path_regex")
    })
}

/// `component: Identifier` (PascalCase or any identifier form)
fn vue_router_component_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"\bcomponent\s*:\s*([A-Za-z_][A-Za-z0-9_]*)"#)
            .expect("vue_router_component_regex")
    })
}

/// `component: () => import('./path/to/File.vue')` — lazy import form.
/// Capture group 1: the import path string (single/double/backtick quotes).
fn vue_router_lazy_component_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"\bcomponent\s*:\s*\(\s*\)\s*=>\s*import\s*\(\s*['"`]([^'"`]+)['"`]\s*\)"#)
            .expect("vue_router_lazy_component_regex")
    })
}

/// defineStore('store-id', ...) — Pinia store definition.
fn pinia_define_store_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"\bdefineStore\s*\(\s*['"`]([^'"`]+)['"`]"#).expect("pinia_define_store_regex")
    })
}

/// Detect `createRouter({` to scope vue-router detection.
fn create_router_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"\bcreateRouter\s*\("#).expect("create_router_regex"))
}

/// `definePageMeta(` — locates the call in page content.
fn define_page_meta_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bdefinePageMeta\s*\(").expect("define_page_meta_regex"))
}

/// `path: '/custom'` inside a definePageMeta body.
fn page_meta_path_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"\bpath\s*:\s*['"`]([^'"`]*)['"`]"#).expect("page_meta_path_regex")
    })
}

/// `alias:` key inside a definePageMeta body (used to locate the start of the value).
fn page_meta_alias_start_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\balias\s*:").expect("page_meta_alias_start_regex"))
}

/// `middleware:` key inside a definePageMeta body.
fn page_meta_middleware_start_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bmiddleware\s*:").expect("page_meta_middleware_start_regex"))
}

/// `routes\s*:\s*[` — locates the routes array inside a createRouter config.
fn routes_array_start_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\broutes\s*:\s*\[").expect("routes_array_start_regex"))
}

/// `children\s*:\s*[` — locates a children sub-array inside a route object.
fn children_array_start_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bchildren\s*:\s*\[").expect("children_array_start_regex"))
}

// ─── Path helpers ─────────────────────────────────────────────────────────────

/// Convert a Nuxt `pages/` file path to its URL route string.
fn file_path_to_nuxt_route(normalized: &str, after_pages_start: usize) -> Option<String> {
    let rest = &normalized[after_pages_start..];
    let rest = rest
        .trim_end_matches(".vue")
        .trim_end_matches(".ts")
        .trim_end_matches(".js");
    let rest = rest.trim_end_matches("/index");
    let rest = if rest.is_empty() || rest == "index" {
        "/"
    } else {
        rest
    };
    let route = convert_nuxt_params(rest);
    let route = if route.is_empty() || route == "/" {
        "/".to_string()
    } else {
        format!("/{}", route.trim_matches('/'))
    };
    Some(route)
}

/// Apply Nuxt dynamic param conventions to a path string.
fn convert_nuxt_params(path: &str) -> String {
    path.split('/')
        .map(convert_nuxt_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// Convert a single path segment from Nuxt file-system convention to route param syntax.
pub(crate) fn convert_nuxt_segment(seg: &str) -> String {
    if seg.starts_with("[...") && seg.ends_with(']') {
        let inner = &seg[4..seg.len() - 1];
        return format!("*{}", inner);
    }
    if seg.starts_with("[[") && seg.ends_with("]]") {
        let inner = &seg[2..seg.len() - 2];
        return format!(":{}?", inner);
    }
    if seg.starts_with('[') && seg.ends_with(']') {
        let inner = &seg[1..seg.len() - 1];
        return format!(":{}", inner);
    }
    seg.to_string()
}

/// Infer HTTP method from a Nuxt server/api/ filename suffix.
fn nuxt_api_verb_from_filename(filename: &str) -> &'static str {
    let stem = filename
        .trim_end_matches(".ts")
        .trim_end_matches(".js")
        .trim_end_matches(".vue");
    if let Some(pos) = stem.rfind('.') {
        match &stem[pos + 1..] {
            "get" => return "GET",
            "post" => return "POST",
            "put" => return "PUT",
            "patch" => return "PATCH",
            "delete" => return "DELETE",
            "head" => return "HEAD",
            "options" => return "OPTIONS",
            _ => {}
        }
    }
    "ENDPOINT"
}

/// Try to resolve an alias-transformed path by trying several extensions.
fn try_alias_resolve(alias_path: &str, context: &ResolutionContext<'_>) -> Option<String> {
    let extensions = [
        "",
        ".ts",
        ".js",
        ".vue",
        "/index.ts",
        "/index.js",
        "/index.vue",
    ];
    for ext in &extensions {
        let candidate = format!("{}{}", alias_path, ext);
        if context.file_exists(&candidate) {
            let nodes = context.get_nodes_in_file(&candidate);
            if let Some(node) = nodes.first() {
                return Some(node.id.clone());
            }
        }
    }
    None
}

// ─── Resolution helpers ────────────────────────────────────────────────────────

fn is_pascal_case(name: &str) -> bool {
    name.chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
        && name.chars().all(|c| c.is_alphanumeric())
}

fn kebab_to_pascal(kebab: &str) -> String {
    kebab
        .split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

fn is_kebab_case(name: &str) -> bool {
    name.contains('-')
        && name
            .chars()
            .all(|c| c.is_lowercase() || c.is_numeric() || c == '-')
}

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

// ─── definePageMeta helpers ───────────────────────────────────────────────────

/// Extract the body (inner text) of `definePageMeta(...)` from a page file's content.
fn extract_define_page_meta_args(content: &str) -> Option<String> {
    let m = define_page_meta_regex().find(content)?;
    // The regex ends with `(`, so `m.end() - 1` is the offset of `(`.
    let open = m.end() - 1;
    let (inner, _) = read_args(content, open)?;
    Some(inner)
}

/// Extract all quoted strings that are the value of an `alias:` key.
/// Handles both `alias: '/old'` and `alias: ['/a', '/b']` forms.
fn extract_aliases(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let m = match page_meta_alias_start_regex().find(body) {
        Some(m) => m,
        None => return out,
    };
    collect_quoted_strings_from(&body[m.end()..], &mut out);
    out
}

/// Extract all quoted middleware names from a `middleware:` value.
/// Handles both `middleware: 'auth'` and `middleware: ['auth', 'admin']` forms.
fn extract_middleware_names(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let m = match page_meta_middleware_start_regex().find(body) {
        Some(m) => m,
        None => return out,
    };
    collect_quoted_strings_from(&body[m.end()..], &mut out);
    out
}

/// Scan `rest` (text after a key like `alias:` or `middleware:`) and collect
/// all quoted strings until the first top-level comma or closing brace/bracket
/// that would end the value.
///
/// Stops when it sees a `,` or `}` at depth 0 (outside any brackets/strings).
fn collect_quoted_strings_from(rest: &str, out: &mut Vec<String>) {
    let bytes = rest.as_bytes();
    let mut i = 0;
    let mut depth = 0usize; // bracket/brace nesting inside the value
    let mut found_any = false;

    while i < bytes.len() {
        match bytes[i] {
            b'[' | b'(' | b'{' => {
                depth += 1;
                i += 1;
            }
            b']' | b')' | b'}' => {
                if depth == 0 {
                    break; // end of value
                }
                depth -= 1;
                i += 1;
            }
            b',' if depth == 0 => {
                // Top-level comma: if we already found strings (array or single
                // string), stop. If we haven't started, this comma is between
                // properties of the parent object — also stop.
                if found_any {
                    break;
                }
                // Haven't found anything yet — this comma ends whatever the
                // value was (e.g. a non-string value we don't care about).
                break;
            }
            b'\'' | b'"' | b'`' => {
                let q = bytes[i];
                i += 1;
                let start = i;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                    } else if bytes[i] == q {
                        break;
                    } else {
                        i += 1;
                    }
                }
                if let Ok(s) = std::str::from_utf8(&bytes[start..i]) {
                    out.push(s.to_string());
                    found_any = true;
                }
                if i < bytes.len() {
                    i += 1; // skip closing quote
                }
            }
            _ => {
                i += 1;
            }
        }
    }
}

// ─── vue-router component ref enum ───────────────────────────────────────────

/// How a vue-router route references its component.
enum ComponentRef {
    /// `component: FooPage` — resolved by name lookup.
    Identifier(String),
    /// `component: () => import('./pages/Foo.vue')` — resolved by import path.
    ImportPath(String),
}

/// Try to find a component reference in a route object body.
fn find_component_ref(obj_body: &str) -> Option<ComponentRef> {
    // Try lazy import form first (more specific).
    if let Some(cap) = vue_router_lazy_component_regex().captures(obj_body) {
        return Some(ComponentRef::ImportPath(cap[1].to_string()));
    }
    // Fall back to identifier form.
    if let Some(cap) = vue_router_component_regex().captures(obj_body) {
        // Skip `import` keyword if somehow matched (shouldn't happen with the regex, but guard).
        let name = cap[1].to_string();
        if name != "import" {
            return Some(ComponentRef::Identifier(name));
        }
    }
    None
}

// ─── Route path composition ───────────────────────────────────────────────────

/// Compose a parent route path with a child path segment.
///
/// - Absolute child path (starts with `/`) overrides parent entirely.
/// - Empty child inherits parent unchanged.
/// - Otherwise: `{parent}/{child}` with duplicate slashes collapsed.
fn compose_route_path(parent: &str, child: &str) -> String {
    if child.starts_with('/') {
        return child.to_string();
    }
    if child.is_empty() {
        return parent.to_string();
    }
    if parent.is_empty() || parent == "/" {
        format!("/{}", child.trim_matches('/'))
    } else {
        format!(
            "{}/{}",
            parent.trim_end_matches('/'),
            child.trim_matches('/')
        )
    }
}

/// Extract the body of a `children: [...]` array from a route object body.
fn extract_children_array(obj_body: &str) -> Option<String> {
    let m = children_array_start_regex().find(obj_body)?;
    // The match ends with `[`, so `m.end() - 1` is the offset of `[`.
    let bracket_open = m.end() - 1;
    read_bracket_array(obj_body, bracket_open)
}

// ─── Recursive route array parser ────────────────────────────────────────────

/// Parse a routes array body recursively, composing nested child paths.
///
/// `array_body` — text between the `[` and `]` of a routes array.
/// `prefix`     — accumulated parent path (empty string at top level).
///
/// Returns a list of `(full_path, Option<ComponentRef>)`.
///
/// Advisory (C-ADVISORY-1): if a route object has no `path` field (layout
/// routes), we skip emitting a node for that object but still recurse into its
/// `children:` array using the current `prefix` unchanged.
fn parse_routes_array(array_body: &str, prefix: &str) -> Vec<(String, Option<ComponentRef>)> {
    let mut results = Vec::new();
    let bytes = array_body.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'{' {
            match read_object(array_body, i) {
                Some((obj_body, end)) => {
                    // Extract the `path:` value from this route object.
                    let full_path = if let Some(cap) = vue_router_path_regex().captures(&obj_body) {
                        let rel_path = &cap[1];
                        compose_route_path(prefix, rel_path)
                    } else {
                        // No path field — layout route; use current prefix for children.
                        prefix.to_string()
                    };

                    // Only emit a route node if we have an explicit path.
                    let has_explicit_path = vue_router_path_regex().is_match(&obj_body);
                    if has_explicit_path {
                        let component = find_component_ref(&obj_body);
                        results.push((full_path.clone(), component));
                    }

                    // Recurse into children regardless.
                    if let Some(children_body) = extract_children_array(&obj_body) {
                        let child_results = parse_routes_array(&children_body, &full_path);
                        results.extend(child_results);
                    }

                    i = end;
                    continue;
                }
                None => {
                    i += 1;
                }
            }
        } else {
            i += 1;
        }
    }

    results
}

// ─── vue-router route extraction (top-level) ─────────────────────────────────

/// Parse vue-router 4 `createRouter({ routes: [...] })` from a JS/TS file.
///
/// Uses a brace/bracket-aware recursive descent to handle nested `children:`
/// arrays and both identifier and lazy-import component forms.
fn parse_vue_router_routes(content: &str) -> Vec<(String, Option<ComponentRef>)> {
    if !create_router_regex().is_match(content) {
        return Vec::new();
    }

    // Find createRouter( and extract its argument object.
    let m = match create_router_regex().find(content) {
        Some(m) => m,
        None => return Vec::new(),
    };
    let open_paren = m.end() - 1; // offset of `(`
    let (config_body, _) = match read_args(content, open_paren) {
        Some(r) => r,
        None => return Vec::new(),
    };

    // Find `routes: [` inside the config body and extract the array.
    let routes_m = match routes_array_start_regex().find(&config_body) {
        Some(m) => m,
        None => return Vec::new(),
    };
    let bracket_open = routes_m.end() - 1; // offset of `[`
    let routes_body = match read_bracket_array(&config_body, bracket_open) {
        Some(b) => b,
        None => return Vec::new(),
    };

    parse_routes_array(&routes_body, "")
}

// ─── Resolver ─────────────────────────────────────────────────────────────────

pub struct VueResolver;

impl FrameworkResolver for VueResolver {
    fn name(&self) -> &'static str {
        "vue"
    }

    fn languages(&self) -> Option<&'static [Language]> {
        static LANGS: &[Language] = &[Language::Vue, Language::TypeScript, Language::JavaScript];
        Some(LANGS)
    }

    fn detect(&self, context: &ResolutionContext<'_>) -> bool {
        if let Some(raw) = context.read_file("package.json") {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&raw) {
                let mut all_deps = serde_json::Map::new();
                for section in &["dependencies", "devDependencies"] {
                    if let Some(obj) = pkg.get(section).and_then(|v| v.as_object()) {
                        all_deps.extend(obj.clone());
                    }
                }
                if all_deps.contains_key("vue")
                    || all_deps.contains_key("nuxt")
                    || all_deps.contains_key("@nuxt/kit")
                    || all_deps.contains_key("vue-router")
                    || all_deps.contains_key("pinia")
                {
                    return true;
                }
            }
        }
        context.known_files.iter().any(|f| f.ends_with(".vue"))
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &ResolutionContext<'_>,
    ) -> Result<Option<ResolvedEdge>, CodeWikiError> {
        let name = &reference.reference_name;

        // Pattern 1: Vue compiler macros → self-referential at 1.0
        if vue_compiler_macros().contains(name.as_str()) {
            return Ok(Some(make_resolved_edge(
                reference,
                reference.from_node_id.clone(),
                1.0,
                self.name(),
            )));
        }

        // Pattern 2: Nuxt auto-imported composables → self-referential at 1.0
        if nuxt_auto_imports().contains(name.as_str()) {
            return Ok(Some(make_resolved_edge(
                reference,
                reference.from_node_id.clone(),
                1.0,
                self.name(),
            )));
        }

        // Pattern 3: Nuxt virtual module imports (#imports, #components, etc.)
        if NUXT_VIRTUAL_MODULES
            .iter()
            .any(|&m| name == m || name.starts_with(&format!("{}/", m)))
        {
            return Ok(Some(make_resolved_edge(
                reference,
                reference.from_node_id.clone(),
                1.0,
                self.name(),
            )));
        }

        // Pattern 4: @/ alias → src/
        if let Some(rest) = name.strip_prefix("@/") {
            let alias_path = format!("src/{}", rest);
            if let Some(node_id) = try_alias_resolve(&alias_path, context) {
                return Ok(Some(make_resolved_edge(
                    reference,
                    node_id,
                    0.9,
                    self.name(),
                )));
            }
        }

        // Pattern 5: ~/ alias → src/
        if let Some(rest) = name.strip_prefix("~/") {
            let alias_path = format!("src/{}", rest);
            if let Some(node_id) = try_alias_resolve(&alias_path, context) {
                return Ok(Some(make_resolved_edge(
                    reference,
                    node_id,
                    0.9,
                    self.name(),
                )));
            }
        }

        // Pattern 6: PascalCase component refs (reference_kind = "calls")
        if is_pascal_case(name) && reference.reference_kind == "calls" {
            if let Some(node_id) = resolve_component(name, &reference.file_path, context) {
                return Ok(Some(make_resolved_edge(
                    reference,
                    node_id,
                    0.8,
                    self.name(),
                )));
            }
        }

        // Pattern 7: kebab-case component refs → PascalCase lookup
        if is_kebab_case(name) && reference.reference_kind == "calls" {
            let pascal_name = kebab_to_pascal(name);
            if let Some(node_id) = resolve_component(&pascal_name, &reference.file_path, context) {
                return Ok(Some(make_resolved_edge(
                    reference,
                    node_id,
                    0.75,
                    self.name(),
                )));
            }
        }

        // Pattern 8: useXxx() composable → composables/ directory lookup
        if name.starts_with("use")
            && name.len() > 3
            && name
                .chars()
                .nth(3)
                .map(|c| c.is_uppercase())
                .unwrap_or(false)
        {
            let candidates = context.get_nodes_by_name(name);
            let target = candidates.iter().find(|n| {
                (n.kind == NodeKind::Function || n.kind == NodeKind::Variable)
                    && (n.file_path.contains("composables/")
                        || n.file_path.contains("composables\\"))
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

        // Pattern 9: Pinia useXStore() → store node
        if name.starts_with("use") && name.ends_with("Store") && name.len() > 8 {
            let candidates = context.get_nodes_by_name(name);
            let target = candidates
                .iter()
                .find(|n| n.kind == NodeKind::Function || n.kind == NodeKind::Variable);
            if let Some(node) = target {
                return Ok(Some(make_resolved_edge(
                    reference,
                    node.id.clone(),
                    0.85,
                    self.name(),
                )));
            }
            let store_candidates = context.get_nodes_by_name(name);
            if let Some(node) = store_candidates.first() {
                return Ok(Some(make_resolved_edge(
                    reference,
                    node.id.clone(),
                    0.80,
                    self.name(),
                )));
            }
        }

        // Pattern 10: middleware ref from definePageMeta → middleware/ function node
        if reference.reference_kind == "references" {
            let candidates = context.get_nodes_by_name(name);
            let target = candidates.iter().find(|n| {
                n.kind == NodeKind::Function
                    && (n.file_path.contains("middleware/") || n.file_path.contains("middleware\\"))
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

        Ok(None)
    }

    fn extract(
        &self,
        file_path: &Path,
        content: &str,
        _context: &ResolutionContext<'_>,
    ) -> Result<FrameworkExtractionResult, CodeWikiError> {
        let file_str = file_path.to_string_lossy();
        let normalized = file_str.replace('\\', "/");

        let mut nodes = Vec::new();
        let mut unresolved_refs = Vec::new();

        // ── Sub-case A: Nuxt pages/ ───────────────────────────────────────────
        let pages_marker = if let Some(pos) = normalized.find("/pages/") {
            Some((pos, pos + "/pages/".len()))
        } else if normalized.starts_with("pages/") {
            Some((0, "pages/".len()))
        } else {
            None
        };
        if let Some((_, after_pages)) = pages_marker {
            if let Some(fs_route_path) = file_path_to_nuxt_route(&normalized, after_pages) {
                let lang = if normalized.ends_with(".ts") {
                    Language::TypeScript
                } else if normalized.ends_with(".vue") {
                    Language::Vue
                } else {
                    Language::JavaScript
                };

                // Try to read definePageMeta override from content.
                let (effective_path, alias_paths, middleware_names) =
                    if let Some(body) = extract_define_page_meta_args(content) {
                        let overridden = page_meta_path_regex()
                            .captures(&body)
                            .map(|c| c[1].to_string())
                            .unwrap_or_else(|| fs_route_path.clone());
                        let aliases = extract_aliases(&body);
                        let middleware = extract_middleware_names(&body);
                        (overridden, aliases, middleware)
                    } else {
                        (fs_route_path, Vec::new(), Vec::new())
                    };

                // Emit primary route node.
                let route_id = format!("route:{}:1:GET:{}", file_str, effective_path);
                nodes.push(Node {
                    id: route_id.clone(),
                    name: effective_path.clone(),
                    qualified_name: format!("{}::GET:{}", file_str, effective_path),
                    kind: NodeKind::Route,
                    language: lang.clone(),
                    file_path: file_str.to_string(),
                    start_line: 1,
                    end_line: 1,
                    ..Default::default()
                });

                // Emit middleware UnresolvedRefs.
                for mw_name in &middleware_names {
                    unresolved_refs.push(UnresolvedRef {
                        id: format!("{}->mw:{}", route_id, mw_name),
                        from_node_id: route_id.clone(),
                        reference_name: mw_name.clone(),
                        reference_kind: "references".to_string(),
                        file_path: file_str.to_string(),
                        line: Some(1),
                        ..Default::default()
                    });
                }

                // Emit alias route nodes.
                for alias_path in &alias_paths {
                    let alias_route_id = format!("route:{}:1:GET:{}", file_str, alias_path);
                    nodes.push(Node {
                        id: alias_route_id,
                        name: alias_path.clone(),
                        qualified_name: format!("{}::GET:{}", file_str, alias_path),
                        kind: NodeKind::Route,
                        language: lang.clone(),
                        file_path: file_str.to_string(),
                        start_line: 1,
                        end_line: 1,
                        ..Default::default()
                    });
                }

                return Ok(FrameworkExtractionResult {
                    nodes,
                    edges: Vec::new(),
                    unresolved_refs,
                });
            }
        }

        // ── Sub-case B: Nuxt server/api/ ──────────────────────────────────────
        let server_api_after = if let Some(pos) = normalized.find("/server/api/") {
            Some(pos + "/server/api/".len())
        } else if normalized.starts_with("server/api/") {
            Some("server/api/".len())
        } else {
            None
        };
        if let Some(after_api) = server_api_after {
            let rest = &normalized[after_api..];
            let rest_clean = rest
                .trim_end_matches(".ts")
                .trim_end_matches(".js")
                .trim_end_matches(".vue");

            let filename = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let verb = nuxt_api_verb_from_filename(filename);

            let path_rest = strip_api_verb_suffix(rest_clean);
            let path_rest = path_rest.trim_end_matches("/index");
            let route_path = if path_rest.is_empty() || path_rest == "index" {
                "/api/".to_string()
            } else {
                format!("/api/{}", convert_nuxt_params(path_rest).trim_matches('/'))
            };

            let lang = if normalized.ends_with(".ts") {
                Language::TypeScript
            } else if normalized.ends_with(".vue") {
                Language::Vue
            } else {
                Language::JavaScript
            };

            let route_id = format!("route:{}:1:{}:{}", file_str, verb, route_path);
            nodes.push(Node {
                id: route_id.clone(),
                name: route_path.clone(),
                qualified_name: format!("{}::{}:{}", file_str, verb, route_path),
                kind: NodeKind::Route,
                language: lang,
                file_path: file_str.to_string(),
                start_line: 1,
                end_line: 1,
                ..Default::default()
            });
            return Ok(FrameworkExtractionResult {
                nodes,
                edges: Vec::new(),
                unresolved_refs,
            });
        }

        // ── Sub-case C: Nuxt middleware/ ──────────────────────────────────────
        let in_middleware =
            normalized.contains("/middleware/") || normalized.starts_with("middleware/");
        if in_middleware {
            let file_name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let mw_name = file_name
                .trim_end_matches(".ts")
                .trim_end_matches(".js")
                .trim_end_matches(".vue");
            if !mw_name.is_empty() {
                let lang = if normalized.ends_with(".ts") {
                    Language::TypeScript
                } else {
                    Language::JavaScript
                };
                let node_id = format!("function:{}:1:{}", file_str, mw_name);
                nodes.push(Node {
                    id: node_id,
                    name: mw_name.to_string(),
                    qualified_name: format!("{}::{}", file_str, mw_name),
                    kind: NodeKind::Function,
                    language: lang,
                    file_path: file_str.to_string(),
                    start_line: 1,
                    end_line: 1,
                    ..Default::default()
                });
                return Ok(FrameworkExtractionResult {
                    nodes,
                    edges: Vec::new(),
                    unresolved_refs,
                });
            }
        }

        // ── Sub-case D: vue-router 4 createRouter config ──────────────────────
        let is_ts_js = normalized.ends_with(".ts")
            || normalized.ends_with(".js")
            || normalized.ends_with(".mjs");
        if is_ts_js {
            let lang = if normalized.ends_with(".ts") {
                Language::TypeScript
            } else {
                Language::JavaScript
            };

            // vue-router routes from createRouter config (recursive, nesting-aware).
            let router_routes = parse_vue_router_routes(content);
            if !router_routes.is_empty() {
                for (idx, (path, component_ref)) in router_routes.into_iter().enumerate() {
                    let line = (idx + 1) as u32;
                    let route_id = format!("route:{}:{}:GET:{}", file_str, line, path);
                    nodes.push(Node {
                        id: route_id.clone(),
                        name: path.clone(),
                        qualified_name: format!("{}::GET:{}", file_str, path),
                        kind: NodeKind::Route,
                        language: lang.clone(),
                        file_path: file_str.to_string(),
                        start_line: line,
                        end_line: line,
                        ..Default::default()
                    });

                    match component_ref {
                        Some(ComponentRef::Identifier(name)) => {
                            unresolved_refs.push(UnresolvedRef {
                                id: format!("{}->{}", route_id, name),
                                from_node_id: route_id,
                                reference_name: name,
                                reference_kind: "references".to_string(),
                                file_path: file_str.to_string(),
                                line: Some(line),
                                ..Default::default()
                            });
                        }
                        Some(ComponentRef::ImportPath(import_path)) => {
                            unresolved_refs.push(UnresolvedRef {
                                id: format!("{}->{}", route_id, import_path),
                                from_node_id: route_id,
                                reference_name: import_path,
                                reference_kind: "imports".to_string(),
                                file_path: file_str.to_string(),
                                line: Some(line),
                                ..Default::default()
                            });
                        }
                        None => {}
                    }
                }
            }

            // Pinia defineStore('id', ...) → store function node
            for cap in pinia_define_store_regex().captures_iter(content) {
                let store_id = &cap[1];
                let match_start = cap.get(0).unwrap().start();
                let line = content[..match_start].lines().count() as u32 + 1;

                let camel_id = store_id_to_camel(store_id);
                let use_fn_name = format!("use{}Store", camel_id);

                let node_id = format!("function:{}:{}:{}", file_str, line, use_fn_name);
                nodes.push(Node {
                    id: node_id,
                    name: use_fn_name.clone(),
                    qualified_name: format!("{}::{}", file_str, use_fn_name),
                    kind: NodeKind::Function,
                    language: lang.clone(),
                    file_path: file_str.to_string(),
                    start_line: line,
                    end_line: line,
                    ..Default::default()
                });
            }

            // Nuxt composables/ directory: emit function node for each composable file
            if normalized.contains("/composables/") || normalized.starts_with("composables/") {
                if let Some(stem) = file_path.file_stem().and_then(|s| s.to_str()) {
                    if stem.starts_with("use")
                        && stem.len() > 3
                        && stem
                            .chars()
                            .nth(3)
                            .map(|c| c.is_uppercase())
                            .unwrap_or(false)
                    {
                        let node_id = format!("function:{}:1:{}", file_str, stem);
                        nodes.push(Node {
                            id: node_id,
                            name: stem.to_string(),
                            qualified_name: format!("{}::{}", file_str, stem),
                            kind: NodeKind::Function,
                            language: lang.clone(),
                            file_path: file_str.to_string(),
                            start_line: 1,
                            end_line: 1,
                            ..Default::default()
                        });
                    }
                }
            }

            // TODO: routeRules — nuxt.config.ts `routeRules: { '/admin/**': {...} }`
            // Concrete keys → route node qualified_name = "{file}::RULE:{key}".
            // Glob patterns emitted with raw string as name, no UnresolvedRef.
            // Low ROI: most routeRules are redirect/cache directives, not API surface.

            if !nodes.is_empty() || !unresolved_refs.is_empty() {
                return Ok(FrameworkExtractionResult {
                    nodes,
                    edges: Vec::new(),
                    unresolved_refs,
                });
            }
        }

        Ok(FrameworkExtractionResult::empty())
    }
}

// ─── Private helpers ──────────────────────────────────────────────────────────

fn strip_api_verb_suffix(path: &str) -> &str {
    let verbs = [
        ".get", ".post", ".put", ".patch", ".delete", ".head", ".options",
    ];
    for v in &verbs {
        if let Some(stripped) = path.strip_suffix(v) {
            return stripped;
        }
    }
    path
}

fn store_id_to_camel(id: &str) -> String {
    id.split(['-', '_', ' '])
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
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
            project_languages: &[Language::Vue, Language::TypeScript],
            caches,
            path_aliases: aliases,
            known_files,
        }
    }

    // ── file_path_to_nuxt_route (original) ────────────────────────────────────

    #[test]
    fn vue_route_from_pages_vue() {
        let route = file_path_to_nuxt_route("src/pages/products/[id].vue", "src/pages/".len());
        assert_eq!(route, Some("/products/:id".to_string()));
    }

    #[test]
    fn vue_route_index_page() {
        let route = file_path_to_nuxt_route("src/pages/index.vue", "src/pages/".len());
        assert_eq!(route, Some("/".to_string()));
    }

    #[test]
    fn vue_route_catchall_nuxt() {
        let route = file_path_to_nuxt_route("src/pages/[...slug].vue", "src/pages/".len());
        assert_eq!(route, Some("/*slug".to_string()));
    }

    #[test]
    fn vue_route_optional_param() {
        let route = file_path_to_nuxt_route("pages/users/[[id]].vue", "pages/".len());
        assert_eq!(route, Some("/users/:id?".to_string()));
    }

    #[test]
    fn vue_route_nested_folder() {
        let route = file_path_to_nuxt_route("pages/blog/posts/[slug].vue", "pages/".len());
        assert_eq!(route, Some("/blog/posts/:slug".to_string()));
    }

    #[test]
    fn vue_route_deep_catchall() {
        let route = file_path_to_nuxt_route("pages/[...path].vue", "pages/".len());
        assert_eq!(route, Some("/*path".to_string()));
    }

    #[test]
    fn vue_segment_conversions() {
        assert_eq!(convert_nuxt_segment("[id]"), ":id");
        assert_eq!(convert_nuxt_segment("[...slug]"), "*slug");
        assert_eq!(convert_nuxt_segment("[[optional]]"), ":optional?");
        assert_eq!(convert_nuxt_segment("about"), "about");
    }

    // ── extract: pages ────────────────────────────────────────────────────────

    #[test]
    fn vue_extract_pages_route() {
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("src/pages/products/[id].vue"), "", &ctx)
            .unwrap();
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "/products/:id");
        assert_eq!(result.nodes[0].kind, NodeKind::Route);
    }

    #[test]
    fn vue_extract_pages_catchall() {
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("pages/[...slug].vue"), "", &ctx)
            .unwrap();
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "/*slug");
        assert_eq!(result.nodes[0].kind, NodeKind::Route);
    }

    #[test]
    fn vue_extract_pages_optional() {
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("pages/users/[[id]].vue"), "", &ctx)
            .unwrap();
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "/users/:id?");
    }

    // ── extract: server/api ───────────────────────────────────────────────────

    #[test]
    fn vue_api_route_server_api() {
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("server/api/users/index.ts"), "", &ctx)
            .unwrap();
        assert_eq!(result.nodes.len(), 1);
        assert!(
            result.nodes[0].name.contains("/api/users"),
            "api route path: {}",
            result.nodes[0].name
        );
        assert_eq!(result.nodes[0].kind, NodeKind::Route);
    }

    #[test]
    fn vue_api_route_verb_from_filename() {
        assert_eq!(nuxt_api_verb_from_filename("users.get.ts"), "GET");
        assert_eq!(nuxt_api_verb_from_filename("users.post.ts"), "POST");
        assert_eq!(nuxt_api_verb_from_filename("users.delete.ts"), "DELETE");
        assert_eq!(nuxt_api_verb_from_filename("users.ts"), "ENDPOINT");
        assert_eq!(nuxt_api_verb_from_filename("index.ts"), "ENDPOINT");
    }

    #[test]
    fn vue_api_route_get_verb_in_node() {
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("server/api/users.get.ts"), "", &ctx)
            .unwrap();
        assert_eq!(result.nodes.len(), 1);
        assert!(
            result.nodes[0].id.contains("GET"),
            "GET verb in route id: {}",
            result.nodes[0].id
        );
        assert!(result.nodes[0].qualified_name.contains("::GET:"));
    }

    // ── extract: middleware ────────────────────────────────────────────────────

    #[test]
    fn vue_middleware_emits_function_node() {
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("middleware/auth.ts"), "", &ctx)
            .unwrap();
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "auth");
        assert_eq!(result.nodes[0].kind, NodeKind::Function);
    }

    // ── extract: vue-router 4 createRouter ────────────────────────────────────

    #[test]
    fn vue_router_create_router_basic() {
        let content = r#"
import { createRouter, createWebHistory } from 'vue-router'
import Users from './pages/Users.vue'
import UserDetail from './pages/UserDetail.vue'

const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/users', component: Users },
    { path: '/users/:id', component: UserDetail },
    { path: '/', component: Home },
  ],
})
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("src/router/index.ts"), content, &ctx)
            .unwrap();

        let route_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Route)
            .collect();
        assert!(
            route_nodes.len() >= 3,
            "expected >=3 route nodes, got {}: {:?}",
            route_nodes.len(),
            route_nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
        );

        let paths: Vec<_> = route_nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(paths.contains(&"/users"), "/users route: {:?}", paths);
        assert!(
            paths.contains(&"/users/:id"),
            "/users/:id route: {:?}",
            paths
        );
        assert!(paths.contains(&"/"), "/ route: {:?}", paths);
    }

    #[test]
    fn vue_router_component_ref_emitted() {
        let content = r#"
const router = createRouter({
  routes: [
    { path: '/dashboard', component: Dashboard },
  ],
})
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("src/router.ts"), content, &ctx)
            .unwrap();

        let route_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Route)
            .collect();
        assert_eq!(route_nodes.len(), 1);
        assert_eq!(route_nodes[0].name, "/dashboard");

        assert_eq!(result.unresolved_refs.len(), 1);
        assert_eq!(result.unresolved_refs[0].reference_name, "Dashboard");
        assert_eq!(result.unresolved_refs[0].reference_kind, "references");
        assert!(result.unresolved_refs[0].id.ends_with("->Dashboard"));
    }

    #[test]
    fn vue_router_qualified_name_format() {
        let content = r#"
const r = createRouter({
  routes: [{ path: '/about', component: About }],
})
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("router/index.ts"), content, &ctx)
            .unwrap();

        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .unwrap();
        assert!(
            route.qualified_name.contains("::GET:/about"),
            "F6 format: {}",
            route.qualified_name
        );
    }

    // ── NEW: lazy-loaded component ref ────────────────────────────────────────

    #[test]
    fn vue_router_lazy_component_ref_emitted() {
        let content = r#"
const router = createRouter({
  routes: [
    { path: '/admin', component: () => import('./pages/Admin.vue') },
  ],
})
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("src/router.ts"), content, &ctx)
            .unwrap();

        assert_eq!(
            result
                .nodes
                .iter()
                .filter(|n| n.kind == NodeKind::Route)
                .count(),
            1,
            "one route node for /admin"
        );
        assert_eq!(result.unresolved_refs.len(), 1);
        let uref = &result.unresolved_refs[0];
        assert_eq!(uref.reference_kind, "imports");
        assert!(
            uref.reference_name.contains("Admin.vue"),
            "import path should contain Admin.vue: {}",
            uref.reference_name
        );
    }

    // ── NEW: nested children path composition ─────────────────────────────────

    #[test]
    fn vue_router_nested_children_compose_path() {
        let content = r#"
const router = createRouter({
  routes: [
    {
      path: '/user',
      component: User,
      children: [
        { path: 'profile', component: UserProfile },
        { path: 'settings', component: UserSettings },
      ],
    },
  ],
})
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("router/index.ts"), content, &ctx)
            .unwrap();

        let route_names: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Route)
            .map(|n| n.name.as_str())
            .collect();
        assert!(
            route_names.contains(&"/user"),
            "missing /user: {:?}",
            route_names
        );
        assert!(
            route_names.contains(&"/user/profile"),
            "missing /user/profile: {:?}",
            route_names
        );
        assert!(
            route_names.contains(&"/user/settings"),
            "missing /user/settings: {:?}",
            route_names
        );
    }

    #[test]
    fn vue_router_absolute_child_path_overrides_parent() {
        let content = r#"
const router = createRouter({
  routes: [
    {
      path: '/parent',
      component: Parent,
      children: [
        { path: '/absolute', component: Absolute },
      ],
    },
  ],
})
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("router/index.ts"), content, &ctx)
            .unwrap();

        let names: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Route)
            .map(|n| n.name.as_str())
            .collect();
        assert!(
            names.contains(&"/absolute"),
            "absolute child path should override parent: {:?}",
            names
        );
    }

    // ── NEW: definePageMeta path override ─────────────────────────────────────

    #[test]
    fn vue_define_page_meta_path_override() {
        let content = r#"
<script setup>
definePageMeta({ path: '/custom-dashboard', layout: 'admin' })
</script>
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("pages/dashboard.vue"), content, &ctx)
            .unwrap();

        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "/custom-dashboard");
        assert!(
            result.nodes[0].id.contains("GET:/custom-dashboard"),
            "id: {}",
            result.nodes[0].id
        );
    }

    #[test]
    fn vue_define_page_meta_alias() {
        let content = r#"
<script setup>
definePageMeta({ path: '/new', alias: ['/old', '/legacy'] })
</script>
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("pages/new.vue"), content, &ctx)
            .unwrap();

        let route_names: Vec<_> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            route_names.contains(&"/new"),
            "missing /new: {:?}",
            route_names
        );
        assert!(
            route_names.contains(&"/old"),
            "missing /old: {:?}",
            route_names
        );
        assert!(
            route_names.contains(&"/legacy"),
            "missing /legacy: {:?}",
            route_names
        );
    }

    #[test]
    fn vue_define_page_meta_single_alias_string() {
        let content = r#"
<script setup>
definePageMeta({ alias: '/old-path' })
</script>
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("pages/about.vue"), content, &ctx)
            .unwrap();

        let names: Vec<_> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"/about"),
            "fs-derived path present: {:?}",
            names
        );
        assert!(
            names.contains(&"/old-path"),
            "single alias string emitted: {:?}",
            names
        );
    }

    // ── NEW: definePageMeta middleware refs ───────────────────────────────────

    #[test]
    fn vue_define_page_meta_middleware_refs() {
        let content = r#"
<script setup>
definePageMeta({ middleware: ['auth', 'admin'] })
</script>
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("pages/secret.vue"), content, &ctx)
            .unwrap();

        let ref_names: Vec<_> = result
            .unresolved_refs
            .iter()
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(
            ref_names.contains(&"auth"),
            "auth middleware ref: {:?}",
            ref_names
        );
        assert!(
            ref_names.contains(&"admin"),
            "admin middleware ref: {:?}",
            ref_names
        );
        assert!(
            result
                .unresolved_refs
                .iter()
                .all(|r| r.reference_kind == "references"),
            "all refs should have kind=references"
        );
    }

    #[test]
    fn vue_define_page_meta_single_middleware() {
        let content = r#"
<script setup>
definePageMeta({ middleware: 'auth' })
</script>
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("pages/protected.vue"), content, &ctx)
            .unwrap();

        let ref_names: Vec<_> = result
            .unresolved_refs
            .iter()
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(
            ref_names.contains(&"auth"),
            "single middleware string: {:?}",
            ref_names
        );
    }

    // ── extract: Pinia defineStore ────────────────────────────────────────────

    #[test]
    fn vue_pinia_define_store_emits_function_node() {
        let content = r#"
import { defineStore } from 'pinia'

export const useCounterStore = defineStore('counter', () => {
  const count = ref(0)
  function increment() { count.value++ }
  return { count, increment }
})
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("stores/counter.ts"), content, &ctx)
            .unwrap();

        let store_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .collect();
        assert!(!store_nodes.is_empty(), "expected store function node");
        let names: Vec<_> = store_nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names
                .iter()
                .any(|n| n.contains("Counter") || n.contains("counter")),
            "store name should contain Counter/counter: {:?}",
            names
        );
    }

    #[test]
    fn vue_pinia_store_id_to_camel() {
        assert_eq!(store_id_to_camel("counter"), "Counter");
        assert_eq!(store_id_to_camel("user-profile"), "UserProfile");
        assert_eq!(store_id_to_camel("auth_store"), "AuthStore");
        assert_eq!(store_id_to_camel("cart"), "Cart");
    }

    #[test]
    fn vue_pinia_multi_store_same_file() {
        let content = r#"
export const useCartStore = defineStore('cart', () => ({ items: ref([]) }))
export const useWishlistStore = defineStore('wishlist', () => ({ ids: ref([]) }))
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("stores/shop.ts"), content, &ctx)
            .unwrap();
        let fn_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .collect();
        assert_eq!(fn_nodes.len(), 2, "two store nodes: {:?}", fn_nodes);
    }

    // ── extract: composables/ ─────────────────────────────────────────────────

    #[test]
    fn vue_composables_dir_emits_function_node() {
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("composables/useAuth.ts"), "", &ctx)
            .unwrap();
        let fn_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .collect();
        assert_eq!(fn_nodes.len(), 1, "one composable function node");
        assert_eq!(fn_nodes[0].name, "useAuth");
    }

    #[test]
    fn vue_composables_non_use_not_emitted() {
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = VueResolver
            .extract(Path::new("composables/helpers.ts"), "", &ctx)
            .unwrap();
        assert!(
            result.nodes.iter().all(|n| n.kind != NodeKind::Function),
            "non-useXxx file should not emit function node"
        );
    }

    // ── resolve: compiler macros ──────────────────────────────────────────────

    #[test]
    fn vue_resolve_compiler_macro() {
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = UnresolvedRef {
            id: "ref:defineProps".to_string(),
            from_node_id: "node:Foo.vue:1".to_string(),
            reference_name: "defineProps".to_string(),
            reference_kind: "references".to_string(),
            file_path: "Foo.vue".to_string(),
            ..Default::default()
        };
        let result = VueResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some());
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 1.0);
        assert_eq!(edge.edge.target_id, reference.from_node_id);
    }

    #[test]
    fn vue_resolve_define_async_component() {
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = UnresolvedRef {
            id: "ref:defineAsyncComponent".to_string(),
            from_node_id: "node:App.vue:1".to_string(),
            reference_name: "defineAsyncComponent".to_string(),
            reference_kind: "references".to_string(),
            file_path: "App.vue".to_string(),
            ..Default::default()
        };
        let result = VueResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some(), "defineAsyncComponent should resolve");
        assert_eq!(result.unwrap().confidence, 1.0);
    }

    #[test]
    fn vue_resolve_define_component() {
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = UnresolvedRef {
            id: "ref:defineComponent".to_string(),
            from_node_id: "node:Button.vue:1".to_string(),
            reference_name: "defineComponent".to_string(),
            reference_kind: "references".to_string(),
            file_path: "Button.vue".to_string(),
            ..Default::default()
        };
        let result = VueResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some(), "defineComponent should resolve");
    }

    // ── resolve: Nuxt composable ──────────────────────────────────────────────

    #[test]
    fn vue_resolve_nuxt_composable() {
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = UnresolvedRef {
            id: "ref:useFetch".to_string(),
            from_node_id: "node:pages/index.vue:1".to_string(),
            reference_name: "useFetch".to_string(),
            reference_kind: "references".to_string(),
            file_path: "pages/index.vue".to_string(),
            ..Default::default()
        };
        let result = VueResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some());
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 1.0);
    }

    // ── resolve: @/ alias ─────────────────────────────────────────────────────

    #[test]
    fn vue_resolve_at_alias() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let comp_dir = dir.path().join("src/components");
        fs::create_dir_all(&comp_dir).unwrap();
        fs::write(
            comp_dir.join("Button.vue"),
            "<template><button /></template>",
        )
        .unwrap();

        let caches = ResolverCaches::default_capacity();
        let node = Node {
            id: "node:src/components/Button.vue:Button".to_string(),
            name: "Button".to_string(),
            qualified_name: "src/components/Button.vue::Button".to_string(),
            kind: NodeKind::Component,
            language: Language::Vue,
            file_path: "src/components/Button.vue".to_string(),
            start_line: 1,
            end_line: 1,
            ..Default::default()
        };
        caches
            .node_cache
            .insert("src/components/Button.vue".to_string(), vec![node.clone()]);

        let aliases = PathAliasMap::default();
        let known: Vec<String> = vec!["src/components/Button.vue".to_string()];
        let ctx = ResolutionContext {
            project_root: dir.path(),
            project_languages: &[Language::Vue],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &known,
        };

        let reference = UnresolvedRef {
            id: "ref:@/components/Button".to_string(),
            from_node_id: "node:pages/index.vue:1".to_string(),
            reference_name: "@/components/Button".to_string(),
            reference_kind: "imports".to_string(),
            file_path: "pages/index.vue".to_string(),
            ..Default::default()
        };
        let result = VueResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some(), "@/ alias should resolve");
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 0.9);
        assert_eq!(edge.edge.target_id, node.id);
    }

    // ── resolve: ~/ alias ─────────────────────────────────────────────────────

    #[test]
    fn vue_resolve_tilde_alias() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let util_dir = dir.path().join("src/utils");
        fs::create_dir_all(&util_dir).unwrap();
        fs::write(util_dir.join("format.ts"), "export function format() {}").unwrap();

        let caches = ResolverCaches::default_capacity();
        let node = Node {
            id: "node:src/utils/format.ts:format".to_string(),
            name: "format".to_string(),
            qualified_name: "src/utils/format.ts::format".to_string(),
            kind: NodeKind::Function,
            language: Language::TypeScript,
            file_path: "src/utils/format.ts".to_string(),
            start_line: 1,
            end_line: 1,
            ..Default::default()
        };
        caches
            .node_cache
            .insert("src/utils/format.ts".to_string(), vec![node.clone()]);

        let aliases = PathAliasMap::default();
        let known: Vec<String> = vec!["src/utils/format.ts".to_string()];
        let ctx = ResolutionContext {
            project_root: dir.path(),
            project_languages: &[Language::Vue],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &known,
        };

        let reference = UnresolvedRef {
            id: "ref:~/utils/format".to_string(),
            from_node_id: "node:pages/index.vue:1".to_string(),
            reference_name: "~/utils/format".to_string(),
            reference_kind: "imports".to_string(),
            file_path: "pages/index.vue".to_string(),
            ..Default::default()
        };
        let result = VueResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some(), "~/ alias should resolve");
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 0.9);
        assert_eq!(edge.edge.target_id, node.id);
    }

    // ── resolve: PascalCase component ─────────────────────────────────────────

    #[test]
    fn vue_resolve_component_pascalcase() {
        let caches = ResolverCaches::default_capacity();
        let card_node = Node {
            id: "node:components/MyCard.vue:MyCard".to_string(),
            name: "MyCard".to_string(),
            qualified_name: "components/MyCard.vue::MyCard".to_string(),
            kind: NodeKind::Component,
            language: Language::Vue,
            file_path: "components/MyCard.vue".to_string(),
            start_line: 1,
            end_line: 1,
            ..Default::default()
        };
        caches
            .name_cache
            .insert("MyCard".to_string(), vec![card_node.clone()]);

        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = UnresolvedRef {
            id: "ref:MyCard".to_string(),
            from_node_id: "node:pages/index.vue:1".to_string(),
            reference_name: "MyCard".to_string(),
            reference_kind: "calls".to_string(),
            file_path: "pages/index.vue".to_string(),
            ..Default::default()
        };
        let result = VueResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some(), "MyCard should resolve");
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 0.8);
        assert_eq!(edge.edge.target_id, card_node.id);
    }

    // ── resolve: kebab-case component ─────────────────────────────────────────

    #[test]
    fn vue_resolve_kebab_case_component() {
        let caches = ResolverCaches::default_capacity();
        let btn_node = Node {
            id: "node:components/MyButton.vue:MyButton".to_string(),
            name: "MyButton".to_string(),
            qualified_name: "components/MyButton.vue::MyButton".to_string(),
            kind: NodeKind::Component,
            language: Language::Vue,
            file_path: "components/MyButton.vue".to_string(),
            start_line: 1,
            end_line: 1,
            ..Default::default()
        };
        caches
            .name_cache
            .insert("MyButton".to_string(), vec![btn_node.clone()]);

        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = UnresolvedRef {
            id: "ref:my-button".to_string(),
            from_node_id: "node:pages/home.vue:1".to_string(),
            reference_name: "my-button".to_string(),
            reference_kind: "calls".to_string(),
            file_path: "pages/home.vue".to_string(),
            ..Default::default()
        };
        let result = VueResolver.resolve(&reference, &ctx).unwrap();
        assert!(
            result.is_some(),
            "kebab-case my-button should resolve to MyButton"
        );
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 0.75);
        assert_eq!(edge.edge.target_id, btn_node.id);
    }

    #[test]
    fn kebab_to_pascal_conversions() {
        assert_eq!(kebab_to_pascal("my-button"), "MyButton");
        assert_eq!(kebab_to_pascal("user-profile-card"), "UserProfileCard");
        assert_eq!(kebab_to_pascal("app"), "App");
        assert_eq!(kebab_to_pascal("base-input"), "BaseInput");
    }

    // ── resolve: composable useXxx ────────────────────────────────────────────

    #[test]
    fn vue_resolve_composable_use_xxx() {
        let caches = ResolverCaches::default_capacity();
        let composable_node = Node {
            id: "function:composables/useAuth.ts:1:useAuth".to_string(),
            name: "useAuth".to_string(),
            qualified_name: "composables/useAuth.ts::useAuth".to_string(),
            kind: NodeKind::Function,
            language: Language::TypeScript,
            file_path: "composables/useAuth.ts".to_string(),
            start_line: 1,
            end_line: 20,
            ..Default::default()
        };
        caches
            .name_cache
            .insert("useAuth".to_string(), vec![composable_node.clone()]);

        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = UnresolvedRef {
            id: "ref:useAuth".to_string(),
            from_node_id: "node:pages/index.vue:1".to_string(),
            reference_name: "useAuth".to_string(),
            reference_kind: "references".to_string(),
            file_path: "pages/index.vue".to_string(),
            ..Default::default()
        };
        let result = VueResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some(), "useAuth composable should resolve");
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 0.85);
        assert_eq!(edge.edge.target_id, composable_node.id);
    }

    // ── resolve: Pinia useXStore ──────────────────────────────────────────────

    #[test]
    fn vue_resolve_pinia_store() {
        let caches = ResolverCaches::default_capacity();
        let store_node = Node {
            id: "function:stores/counter.ts:1:useCounterStore".to_string(),
            name: "useCounterStore".to_string(),
            qualified_name: "stores/counter.ts::useCounterStore".to_string(),
            kind: NodeKind::Function,
            language: Language::TypeScript,
            file_path: "stores/counter.ts".to_string(),
            start_line: 1,
            end_line: 15,
            ..Default::default()
        };
        caches
            .name_cache
            .insert("useCounterStore".to_string(), vec![store_node.clone()]);

        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = UnresolvedRef {
            id: "ref:useCounterStore".to_string(),
            from_node_id: "node:pages/home.vue:5".to_string(),
            reference_name: "useCounterStore".to_string(),
            reference_kind: "references".to_string(),
            file_path: "pages/home.vue".to_string(),
            ..Default::default()
        };
        let result = VueResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some(), "Pinia useCounterStore should resolve");
        let edge = result.unwrap();
        assert!(edge.confidence >= 0.80);
        assert_eq!(edge.edge.target_id, store_node.id);
    }

    // ── resolve: middleware pattern 10 ────────────────────────────────────────

    #[test]
    fn vue_resolve_middleware_pattern_10() {
        let caches = ResolverCaches::default_capacity();
        let mw_node = Node {
            id: "function:middleware/auth.ts:1:auth".to_string(),
            name: "auth".to_string(),
            qualified_name: "middleware/auth.ts::auth".to_string(),
            kind: NodeKind::Function,
            language: Language::TypeScript,
            file_path: "middleware/auth.ts".to_string(),
            start_line: 1,
            end_line: 10,
            ..Default::default()
        };
        caches
            .name_cache
            .insert("auth".to_string(), vec![mw_node.clone()]);

        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = UnresolvedRef {
            id: "route:pages/secret.vue:1:GET:/secret->mw:auth".to_string(),
            from_node_id: "route:pages/secret.vue:1:GET:/secret".to_string(),
            reference_name: "auth".to_string(),
            reference_kind: "references".to_string(),
            file_path: "pages/secret.vue".to_string(),
            line: Some(1),
            ..Default::default()
        };
        let result = VueResolver.resolve(&reference, &ctx).unwrap();
        assert!(result.is_some(), "middleware auth should resolve");
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 0.85);
        assert_eq!(edge.edge.target_id, mw_node.id);
    }

    // ── detect ────────────────────────────────────────────────────────────────

    #[test]
    fn vue_detect_includes_dev_deps() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"devDependencies":{"nuxt":"^3.0.0"}}"#,
        )
        .unwrap();

        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = ResolutionContext {
            project_root: dir.path(),
            project_languages: &[Language::Vue],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &[],
        };
        assert!(
            VueResolver.detect(&ctx),
            "must detect nuxt in devDependencies"
        );
    }

    #[test]
    fn vue_detect_via_vue_file() {
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let known: Vec<String> = vec!["src/App.vue".to_string()];
        let ctx = ResolutionContext {
            project_root: Path::new("/project"),
            project_languages: &[Language::Vue],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &known,
        };
        assert!(VueResolver.detect(&ctx));
    }

    #[test]
    fn vue_detect_via_pinia_dep() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"pinia":"^2.1.0","vue":"^3.3.0"}}"#,
        )
        .unwrap();

        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = ResolutionContext {
            project_root: dir.path(),
            project_languages: &[Language::Vue],
            caches: &caches,
            path_aliases: &aliases,
            known_files: &[],
        };
        assert!(VueResolver.detect(&ctx), "must detect vue+pinia project");
    }

    // ── strip_api_verb_suffix ─────────────────────────────────────────────────

    #[test]
    fn vue_strip_api_verb_suffix() {
        assert_eq!(strip_api_verb_suffix("users/index.get"), "users/index");
        assert_eq!(strip_api_verb_suffix("orders.post"), "orders");
        assert_eq!(strip_api_verb_suffix("users"), "users");
        assert_eq!(strip_api_verb_suffix("index"), "index");
    }

    // ── compose_route_path ────────────────────────────────────────────────────

    #[test]
    fn compose_route_path_relative_child() {
        assert_eq!(compose_route_path("/user", "profile"), "/user/profile");
        assert_eq!(compose_route_path("/user", "settings"), "/user/settings");
    }

    #[test]
    fn compose_route_path_absolute_child_overrides() {
        assert_eq!(compose_route_path("/parent", "/absolute"), "/absolute");
    }

    #[test]
    fn compose_route_path_empty_parent() {
        assert_eq!(compose_route_path("", "profile"), "/profile");
    }

    #[test]
    fn compose_route_path_root_parent() {
        assert_eq!(compose_route_path("/", "about"), "/about");
    }
}

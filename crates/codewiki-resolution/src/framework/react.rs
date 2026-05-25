// T-308 — React framework resolver (full port)
//
// Handles React and Next.js patterns.
// Detects: package.json has "react", "next", or "react-native".
// Extracts: function/arrow/forwardRef/memo components, custom hooks, Next.js routes.
// Resolves: PascalCase component refs, use* hook refs, *Context/*Provider refs.

use super::{make_resolved_edge, FrameworkExtractionResult, FrameworkResolver, ResolutionContext};
use codewiki_core::{CodeWikiError, Language, Node, NodeKind, UnresolvedRef};
use codewiki_storage::traits::ResolvedEdge;
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

static BUILT_IN_TYPES: OnceLock<HashSet<&'static str>> = OnceLock::new();
fn built_in_types() -> &'static HashSet<&'static str> {
    BUILT_IN_TYPES.get_or_init(|| {
        [
            "Array",
            "Boolean",
            "Date",
            "Error",
            "Function",
            "JSON",
            "Math",
            "Number",
            "Object",
            "Promise",
            "RegExp",
            "String",
            "Symbol",
            "Map",
            "Set",
            "WeakMap",
            "WeakSet",
            "React",
            "Component",
            "Fragment",
            "Suspense",
            "StrictMode",
        ]
        .into_iter()
        .collect()
    })
}

fn component_dirs() -> &'static [&'static str] {
    &[
        "/components/",
        "/src/components/",
        "/app/components/",
        "/pages/",
        "/src/pages/",
        "/views/",
    ]
}
fn hook_dirs() -> &'static [&'static str] {
    &["/hooks/", "/src/hooks/", "/lib/hooks/", "/utils/hooks/"]
}
fn context_dirs() -> &'static [&'static str] {
    &["/context/", "/contexts/", "/src/context/", "/providers/"]
}

fn is_pascal_case(s: &str) -> bool {
    s.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
        && s.chars().all(|c| c.is_alphanumeric())
}

pub struct ReactResolver;

impl FrameworkResolver for ReactResolver {
    fn name(&self) -> &'static str {
        "react"
    }
    fn languages(&self) -> Option<&'static [Language]> {
        static LANGS: &[Language] = &[Language::TypeScript, Language::JavaScript];
        Some(LANGS)
    }

    fn detect(&self, context: &ResolutionContext<'_>) -> bool {
        if let Some(raw) = context.read_file("package.json") {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&raw) {
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
                if deps.contains_key("react")
                    || deps.contains_key("next")
                    || deps.contains_key("react-native")
                {
                    return true;
                }
            }
        }
        context
            .known_files
            .iter()
            .any(|f| f.ends_with(".jsx") || f.ends_with(".tsx"))
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &ResolutionContext<'_>,
    ) -> Result<Option<ResolvedEdge>, CodeWikiError> {
        let name = &reference.reference_name;

        // PascalCase component references
        if is_pascal_case(name) && !built_in_types().contains(name.as_str()) {
            let candidates = context.get_nodes_by_name(name);
            let components: Vec<_> = candidates
                .iter()
                .filter(|n| {
                    matches!(
                        n.kind,
                        NodeKind::Component | NodeKind::Function | NodeKind::Class
                    )
                })
                .collect();
            if !components.is_empty() {
                let from_dir = reference
                    .file_path
                    .rsplit('/')
                    .skip(1)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("/");
                let target = components
                    .iter()
                    .find(|n| n.file_path.starts_with(&from_dir))
                    .or_else(|| {
                        components
                            .iter()
                            .find(|n| component_dirs().iter().any(|d| n.file_path.contains(d)))
                    })
                    .or_else(|| components.first());
                if let Some(node) = target {
                    return Ok(Some(make_resolved_edge(
                        reference,
                        node.id.clone(),
                        0.8,
                        self.name(),
                    )));
                }
            }
        }

        // use* hook references
        if name.starts_with("use") && name.len() > 3 {
            let candidates = context.get_nodes_by_name(name);
            let hooks: Vec<_> = candidates
                .iter()
                .filter(|n| n.kind == NodeKind::Function && n.name.starts_with("use"))
                .collect();
            if !hooks.is_empty() {
                let target = hooks
                    .iter()
                    .find(|n| hook_dirs().iter().any(|d| n.file_path.contains(d)))
                    .or_else(|| hooks.first());
                if let Some(node) = target {
                    return Ok(Some(make_resolved_edge(
                        reference,
                        node.id.clone(),
                        0.85,
                        self.name(),
                    )));
                }
            }
        }

        // *Context / *Provider references
        if name.ends_with("Context") || name.ends_with("Provider") {
            let candidates = context.get_nodes_by_name(name);
            if candidates.is_empty() {
                let base = name
                    .trim_end_matches("Context")
                    .trim_end_matches("Provider");
                let fallback = context.get_nodes_by_name(base);
                if let Some(n) = fallback.first() {
                    return Ok(Some(make_resolved_edge(
                        reference,
                        n.id.clone(),
                        0.8,
                        self.name(),
                    )));
                }
            } else {
                let target = candidates
                    .iter()
                    .find(|n| context_dirs().iter().any(|d| n.file_path.contains(d)))
                    .or_else(|| candidates.first());
                if let Some(node) = target {
                    return Ok(Some(make_resolved_edge(
                        reference,
                        node.id.clone(),
                        0.8,
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
        let lang = if file_str.ends_with(".tsx") || file_str.ends_with(".ts") {
            Language::TypeScript
        } else {
            Language::JavaScript
        };

        let mut nodes = Vec::new();

        // Component patterns
        let component_patterns: &[&Regex] = {
            static FN_RE: OnceLock<Regex> = OnceLock::new();
            static ARROW_RE: OnceLock<Regex> = OnceLock::new();
            static FWDREF_RE: OnceLock<Regex> = OnceLock::new();
            static MEMO_RE: OnceLock<Regex> = OnceLock::new();
            static PATS: OnceLock<[&'static Regex; 4]> = OnceLock::new();
            PATS.get_or_init(|| {
                let fn_re = FN_RE.get_or_init(|| Regex::new(r#"(?:export\s+)?function\s+([A-Z][a-zA-Z0-9]*)\s*\("#).unwrap());
                let arrow_re = ARROW_RE.get_or_init(|| Regex::new(r#"(?:export\s+)?(?:const|let)\s+([A-Z][a-zA-Z0-9]*)\s*=\s*(?:\([^)]*\)|[a-zA-Z_][a-zA-Z0-9_]*)\s*=>"#).unwrap());
                let fwdref_re = FWDREF_RE.get_or_init(|| Regex::new(r#"(?:export\s+)?(?:const|let)\s+([A-Z][a-zA-Z0-9]*)\s*=\s*(?:React\.)?forwardRef"#).unwrap());
                let memo_re = MEMO_RE.get_or_init(|| Regex::new(r#"(?:export\s+)?(?:const|let)\s+([A-Z][a-zA-Z0-9]*)\s*=\s*(?:React\.)?memo"#).unwrap());
                [fn_re, arrow_re, fwdref_re, memo_re]
            })
        };

        for re in component_patterns {
            let mut pos = 0;
            while let Some(cap) = re.captures_at(content, pos) {
                let name = cap[1].to_string();
                let m = cap.get(0).unwrap();
                let line = content[..m.start()].lines().count() as u32 + 1;
                // Rough JSX heuristic: look at up to 500 bytes after the match.
                // Walk back from the raw end offset to a valid UTF-8 char boundary
                // so the slice never panics on multi-byte codepoints.
                let after_start = m.end();
                let mut after_end = after_start.saturating_add(500).min(content.len());
                while after_end > after_start && !content.is_char_boundary(after_end) {
                    after_end -= 1;
                }
                let after = &content[after_start..after_end];
                let has_jsx = after.contains('<') && (after.contains("/>") || after.contains("</"));

                if has_jsx {
                    nodes.push(Node {
                        id: format!("component:{}:{}:{}", file_str, name, line),
                        name: name.clone(),
                        qualified_name: format!("{}::{}", file_str, name),
                        kind: NodeKind::Component,
                        language: lang.clone(),
                        file_path: file_str.to_string(),
                        start_line: line,
                        end_line: line,
                        is_exported: m.as_str().contains("export"),
                        ..Default::default()
                    });
                }
                pos = m.end();
            }
        }

        // Custom hooks: use[A-Z]...
        {
            static HOOK_RE: OnceLock<Regex> = OnceLock::new();
            let hook_re = HOOK_RE.get_or_init(|| {
                Regex::new(
                    r#"(?:export\s+)?(?:function|const|let)\s+(use[A-Z][a-zA-Z0-9]*)\s*[=(]"#,
                )
                .unwrap()
            });
            for cap in hook_re.captures_iter(content) {
                let name = cap[1].to_string();
                let m = cap.get(0).unwrap();
                let line = content[..m.start()].lines().count() as u32 + 1;
                nodes.push(Node {
                    id: format!("hook:{}:{}:{}", file_str, name, line),
                    name: name.clone(),
                    qualified_name: format!("{}::{}", file_str, name),
                    kind: NodeKind::Function,
                    language: lang.clone(),
                    file_path: file_str.to_string(),
                    start_line: line,
                    end_line: line,
                    is_exported: m.as_str().contains("export"),
                    ..Default::default()
                });
            }
        }

        // Next.js pages/ and app/ routes
        if (file_str.contains("pages/") || file_str.contains("app/"))
            && content.contains("export default")
        {
            if let Some(route_path) = file_path_to_nextjs_route(&file_str) {
                let byte_offset = content.find("export default").unwrap_or(0);
                let line = content[..byte_offset].lines().count() as u32 + 1;
                nodes.push(Node {
                    id: format!("route:{}:{}:{}", file_str, route_path, line),
                    name: route_path.clone(),
                    qualified_name: format!("{}::route:{}", file_str, route_path),
                    kind: NodeKind::Route,
                    language: lang.clone(),
                    file_path: file_str.to_string(),
                    start_line: line,
                    end_line: line,
                    ..Default::default()
                });
            }
        }

        Ok(FrameworkExtractionResult {
            nodes,
            edges: Vec::new(),
            unresolved_refs: Vec::new(),
        })
    }
}

fn file_path_to_nextjs_route(file_path: &str) -> Option<String> {
    if let Some(pos) = file_path.find("pages/") {
        let rest = &file_path[pos + 6..];
        let mut route = format!(
            "/{}",
            rest.trim_end_matches(".tsx")
                .trim_end_matches(".ts")
                .trim_end_matches(".jsx")
                .trim_end_matches(".js")
        );
        // /index → /
        if route.ends_with("/index") {
            route.truncate(route.len() - 6);
            if route.is_empty() {
                route = "/".to_string();
            }
        }
        // [slug] → :slug
        let route = route.replace('[', ":").replace(']', "");
        return Some(if route.is_empty() {
            "/".to_string()
        } else {
            route
        });
    }
    if let Some(pos) = file_path.find("app/") {
        let rest = &file_path[pos + 4..];
        if !rest.contains("page.") {
            return None;
        }
        let mut route = format!(
            "/{}",
            rest.trim_end_matches(".tsx")
                .trim_end_matches(".ts")
                .trim_end_matches(".jsx")
                .trim_end_matches(".js")
                .trim_end_matches("/page")
        );
        if route.is_empty() {
            route = "/".to_string();
        }
        let route = route.replace('[', ":").replace(']', "");
        return Some(route);
    }
    None
}

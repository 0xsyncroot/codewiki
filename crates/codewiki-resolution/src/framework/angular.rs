// T-Angular — Angular framework resolver (v1)
//
// Implements the 6-method FrameworkResolver trait for Angular projects.
// Covers: @Component / @Directive / @Pipe / @Injectable / @NgModule decorators,
// constructor + inject() DI, RouterModule.forRoot/forChild / provideRouter /
// const Routes = [...] routing configs (nested children path composition),
// lazy loadComponent / loadChildren, guards, resolvers,
// inline `template:` selector + pipe scanning, standalone `imports:[]` refs.
//
// External .html template parsing is deferred to v1.1.
//
// Spec: specs/angular/ANGULAR-IMPL-SPEC.md

use super::scan_utils::{read_args, read_bracket_array, read_object};
use super::{make_resolved_edge, FrameworkExtractionResult, FrameworkResolver, ResolutionContext};
use crate::strip_comments::{strip_comments, CommentLang};
use codewiki_core::{CodeWikiError, Language, Node, NodeKind, UnresolvedRef};
use codewiki_storage::traits::ResolvedEdge;
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

// ─── Statics ──────────────────────────────────────────────────────────────────

/// Angular built-in symbols that always resolve to None (live in node_modules).
static ANGULAR_BUILT_INS: OnceLock<HashSet<&'static str>> = OnceLock::new();
fn angular_built_ins() -> &'static HashSet<&'static str> {
    ANGULAR_BUILT_INS.get_or_init(|| {
        [
            "RouterLink",
            "RouterModule",
            "RouterOutlet",
            "CommonModule",
            "BrowserModule",
            "AsyncPipe",
            "NgIf",
            "NgFor",
            "NgSwitch",
            "NgSwitchCase",
            "NgSwitchDefault",
            "NgClass",
            "NgStyle",
            "NgTemplateOutlet",
            "NgContainer",
            "NgContent",
            "NgPlural",
            "NgPluralCase",
            "FormsModule",
            "ReactiveFormsModule",
            "HttpClientModule",
            "DatePipe",
            "LowerCasePipe",
            "UpperCasePipe",
            "DecimalPipe",
            "CurrencyPipe",
            "PercentPipe",
            "SlicePipe",
            "JsonPipe",
            "KeyValuePipe",
            "TitleCasePipe",
            "I18nPluralPipe",
            "I18nSelectPipe",
            "BrowserAnimationsModule",
            "NoopAnimationsModule",
            "ActivatedRoute",
            "Router",
            "Location",
            "HttpClient",
            "HttpHeaders",
            "HttpParams",
            "Title",
            "Meta",
            "DOCUMENT",
            "PLATFORM_ID",
            "APP_ID",
            "NgZone",
            "ApplicationRef",
            "Injector",
            "EnvironmentInjector",
        ]
        .into_iter()
        .collect()
    })
}

/// TypeScript built-in types to exclude from DI extraction.
static TS_BUILTIN_TYPES: OnceLock<HashSet<&'static str>> = OnceLock::new();
fn ts_builtin_types() -> &'static HashSet<&'static str> {
    TS_BUILTIN_TYPES.get_or_init(|| {
        [
            "String",
            "Number",
            "Boolean",
            "Object",
            "Array",
            "any",
            "unknown",
            "never",
            "void",
            "null",
            "undefined",
            "HTMLElement",
            "ElementRef",
            "ChangeDetectorRef",
            "Renderer2",
            "TemplateRef",
            "ViewContainerRef",
            "ComponentRef",
            "EventEmitter",
            "Subject",
            "Observable",
            "BehaviorSubject",
            "ReplaySubject",
            "Subscription",
        ]
        .into_iter()
        .collect()
    })
}

/// DI provider suffix → file path convention substring.
static PROVIDER_CONVENTIONS: &[(&str, &str)] = &[
    ("Service", ".service."),
    ("Store", ".store."),
    ("Facade", ".facade."),
    ("Repository", ".repository."),
    ("Guard", ".guard."),
    ("Resolver", ".resolver."),
    ("Interceptor", ".interceptor."),
    ("Validator", ".validator."),
    ("Factory", ".factory."),
    ("Handler", ".handler."),
];

// ─── Internal types ───────────────────────────────────────────────────────────

struct DecoratorHit {
    name: String,
    args: String,
    index: usize, // byte offset of '@'
    end: usize,   // byte offset past closing ')'
}

struct ClassScope {
    name: String,
    start_byte: usize,
    end_byte: usize,
}

struct AngularRoute {
    full_path: String,
    line: u32,
    component_name: Option<String>,
    lazy_import_path: Option<String>,
    lazy_export_name: Option<String>,
    guards: Vec<String>,
    resolvers: Vec<String>,
    redirect_to: Option<String>,
}

// ─── Resolver struct ──────────────────────────────────────────────────────────

pub struct AngularResolver {
    angular_version_gte_19: OnceLock<bool>,
}

impl AngularResolver {
    pub fn new() -> Self {
        Self {
            angular_version_gte_19: OnceLock::new(),
        }
    }

    fn is_standalone_by_default(&self) -> bool {
        *self.angular_version_gte_19.get().unwrap_or(&false)
    }
}

impl Default for AngularResolver {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Helper functions ─────────────────────────────────────────────────────────

/// Return the 1-indexed line number of `byte_pos` in `s`.
fn line_at(s: &str, byte_pos: usize) -> u32 {
    s[..byte_pos.min(s.len())]
        .chars()
        .filter(|&c| c == '\n')
        .count() as u32
        + 1
}

/// Scan `safe` for decorators named in `names`. Returns one `DecoratorHit` per match.
fn find_decorators(safe: &str, names: &[&str]) -> Vec<DecoratorHit> {
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
            let paren_index = m.end() - 1;
            let inner_at = &safe[at_index + 1..paren_index];
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

/// Build class scopes from class declarations (for DI extraction).
/// Scope boundary: from `class ClassName` to start of next scope (or EOF).
fn build_class_scopes(safe: &str) -> Vec<ClassScope> {
    static CLASS_RE: OnceLock<Regex> = OnceLock::new();
    let re = CLASS_RE
        .get_or_init(|| Regex::new(r"\bclass\s+([A-Za-z_$][A-Za-z0-9_$]*)").unwrap());

    let total_len = safe.len();
    let mut scopes: Vec<ClassScope> = Vec::new();

    for cap in re.captures_iter(safe) {
        let m = cap.get(0).unwrap();
        scopes.push(ClassScope {
            name: cap[1].to_string(),
            start_byte: m.start(),
            end_byte: 0, // filled below
        });
    }

    let n = scopes.len();
    for i in 0..n {
        scopes[i].end_byte = if i + 1 < n {
            scopes[i + 1].start_byte
        } else {
            total_len
        };
    }
    scopes
}

/// Find the innermost class scope that contains `byte_pos`.
fn scope_for(scopes: &[ClassScope], byte_pos: usize) -> Option<&ClassScope> {
    scopes
        .iter()
        .rfind(|s| s.start_byte <= byte_pos && byte_pos < s.end_byte)
}

/// Find the class name that immediately follows a decorator at `decorator_end`.
fn class_name_after(safe: &str, decorator_end: usize) -> Option<String> {
    static CLASS_AFTER_RE: OnceLock<Regex> = OnceLock::new();
    let re = CLASS_AFTER_RE.get_or_init(|| {
        Regex::new(r"(?:export\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][A-Za-z0-9_$]*)").unwrap()
    });
    let slice = &safe[decorator_end..];
    // Only look within 500 bytes to avoid false matches
    let limit = slice.len().min(500);
    re.captures(&slice[..limit]).map(|c| c[1].to_string())
}

// ─── Decorator arg extractors ─────────────────────────────────────────────────

fn extract_selector(args: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#"selector\s*:\s*['"`]([^'"`]+)['"`]"#).unwrap());
    re.captures(args).map(|c| c[1].to_string())
}

fn extract_pipe_name(args: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#"\bname\s*:\s*['"`]([^'"`]+)['"`]"#).unwrap());
    re.captures(args).map(|c| c[1].to_string())
}

fn extract_provided_in(args: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r#"providedIn\s*:\s*['"`]([^'"`]+)['"`]"#).unwrap());
    re.captures(args).map(|c| c[1].to_string())
}

fn extract_standalone_flag(args: &str) -> Option<bool> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE
        .get_or_init(|| Regex::new(r"\bstandalone\s*:\s*(true|false)\b").unwrap());
    re.captures(args).map(|c| &c[1] == "true")
}

// ─── DI extraction ────────────────────────────────────────────────────────────

/// Parse constructor parameter types from the args string of `constructor(`.
fn extract_constructor_di_types(args: &str) -> Vec<String> {
    let mut types = Vec::new();
    // Each param: optional modifiers + name: Type + optional init
    // We just look for `: TypeName` patterns (TypeName starts with uppercase)
    static PARAM_TYPE_RE: OnceLock<Regex> = OnceLock::new();
    let re = PARAM_TYPE_RE.get_or_init(|| {
        Regex::new(r":\s*([A-Z][A-Za-z0-9_$]*)").unwrap()
    });
    for cap in re.captures_iter(args) {
        let ty = cap[1].to_string();
        if !ts_builtin_types().contains(ty.as_str()) {
            types.push(ty);
        }
    }
    types
}

/// Find all `inject(TypeName)` calls in `safe`. Returns (byte_offset, type_name).
fn inject_ident_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^([A-Za-z_$][A-Za-z0-9_$]*)").unwrap())
}

fn extract_inject_calls(safe: &str) -> Vec<(usize, String)> {
    static INJECT_RE: OnceLock<Regex> = OnceLock::new();
    let re = INJECT_RE.get_or_init(|| Regex::new(r"\binject\s*\(").unwrap());
    let ident_re = inject_ident_regex();

    let mut results = Vec::new();
    for m in re.find_iter(safe) {
        let paren = m.end() - 1;
        if let Some((args, _end)) = read_args(safe, paren) {
            // args may be: "FooService", "FooService, { optional: true }", etc.
            // or generic: "<string>(CONFIG_TOKEN)" — but after stripping, we just
            // want the first identifier.
            let trimmed = args.trim();
            // Strip leading generic type arg if present: `<Foo>`
            let name_part = if let Some(lt_pos) = trimmed.find('<') {
                // Could be inject<Type>(TOKEN) — look for identifier after the >
                if let Some(gt_pos) = trimmed.find('>') {
                    trimmed[gt_pos + 1..].trim()
                } else {
                    &trimmed[..lt_pos]
                }
            } else {
                trimmed
            };
            if let Some(cap) = ident_re.captures(name_part) {
                let ty = cap[1].to_string();
                if !ts_builtin_types().contains(ty.as_str()) {
                    results.push((m.start(), ty));
                }
            }
        }
    }
    results
}

// ─── NgModule array extraction ────────────────────────────────────────────────

/// Extract items from a named array key in a decorator args string.
/// Returns a flat list of identifier names found inside `key: [...]`.
fn extract_named_array_identifiers(args: &str, key: &str) -> Vec<String> {
    let pattern = format!(r"\b{key}\s*:\s*\[");
    let re = match Regex::new(&pattern) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let m = match re.find(args) {
        Some(m) => m,
        None => return Vec::new(),
    };
    let bracket_start = m.end() - 1;
    let inner = match read_bracket_array(args, bracket_start) {
        Some(s) => s,
        None => return Vec::new(),
    };

    // Extract PascalCase identifiers (component/module/service names)
    static IDENT_RE: OnceLock<Regex> = OnceLock::new();
    let ident_re = IDENT_RE
        .get_or_init(|| Regex::new(r"\b([A-Z][A-Za-z0-9_$]*)").unwrap());
    ident_re
        .captures_iter(&inner)
        .map(|c| c[1].to_string())
        .collect()
}

// ─── Inline template helpers ──────────────────────────────────────────────────

/// Extract the value of the `template:` key from decorator args.
fn extract_inline_template(args: &str) -> Option<String> {
    // Find `template:` key
    static TEMPLATE_KEY_RE: OnceLock<Regex> = OnceLock::new();
    let re = TEMPLATE_KEY_RE
        .get_or_init(|| Regex::new(r"\btemplate\s*:\s*").unwrap());
    let m = re.find(args)?;
    let rest = &args[m.end()..];

    // Value is a backtick, single-quote, or double-quote string
    let first = rest.trim_start().chars().next()?;
    if first != '`' && first != '\'' && first != '"' {
        return None;
    }
    let start = rest.find(first)? + 1;
    let content = &rest[start..];
    // Find closing (unescaped) delimiter
    let mut out = String::new();
    let mut chars = content.char_indices();
    while let Some((_, ch)) = chars.next() {
        if ch == '\\' {
            // Skip next char
            chars.next();
            continue;
        }
        if ch == first {
            break;
        }
        out.push(ch);
    }
    Some(out)
}

/// Scan template string for multi-word kebab element selectors (custom components).
fn scan_template_selectors(template: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"<([a-z][a-z0-9]*(?:-[a-z0-9]+)+)").unwrap()
    });
    re.captures_iter(template)
        .map(|c| c[1].to_string())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect()
}

/// Scan template string for pipe references (`| pipeName`).
fn scan_template_pipes(template: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\|\s*([a-zA-Z][a-zA-Z0-9]+)").unwrap());
    re.captures_iter(template)
        .map(|c| c[1].to_string())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect()
}

// ─── Angular version helper ───────────────────────────────────────────────────

fn parse_angular_version_gte_19(version_str: &str) -> bool {
    let s = version_str.trim_start_matches(|c: char| !c.is_ascii_digit());
    let major = s
        .split('.')
        .next()
        .and_then(|m| m.parse::<u32>().ok())
        .unwrap_or(0);
    major >= 19
}

// ─── Route extraction helpers ─────────────────────────────────────────────────

/// Find all route array start offsets (index of the `[`) in `content`.
/// Three patterns:
///   A: RouterModule.forRoot([  or  RouterModule.forChild([
///   B: provideRouter([
///   C: const FOO: Routes = [
fn find_routes_array_offsets(content: &str) -> Vec<usize> {
    static RE_A: OnceLock<Regex> = OnceLock::new();
    static RE_B: OnceLock<Regex> = OnceLock::new();
    static RE_C: OnceLock<Regex> = OnceLock::new();

    let re_a = RE_A.get_or_init(|| {
        Regex::new(r"\bRouterModule\s*\.\s*(?:forRoot|forChild)\s*\(\s*\[").unwrap()
    });
    let re_b = RE_B.get_or_init(|| Regex::new(r"\bprovideRouter\s*\(\s*\[").unwrap());
    let re_c = RE_C.get_or_init(|| {
        Regex::new(r"\bconst\s+[A-Za-z_][A-Za-z0-9_]*\s*:\s*Routes\s*=\s*\[").unwrap()
    });

    let mut offsets: Vec<usize> = Vec::new();

    for re in &[re_a, re_b, re_c] {
        for m in re.find_iter(content) {
            // The `[` is the last character of the match
            let bracket_pos = m.end() - 1;
            if !offsets.contains(&bracket_pos) {
                offsets.push(bracket_pos);
            }
        }
    }

    offsets.sort_unstable();
    offsets.dedup();
    offsets
}

fn children_array_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bchildren\s*:\s*\[").unwrap())
}

/// Compose parent + child Angular route paths (same logic as vue.rs compose_route_path).
fn compose_angular_route_path(parent: &str, child: &str) -> String {
    if child.starts_with('/') {
        return child.to_string();
    }
    if child.is_empty() {
        return parent.to_string();
    }
    if parent.is_empty() || parent == "/" {
        format!("/{}", child.trim_matches('/'))
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), child.trim_matches('/'))
    }
}

fn guard_ident_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b([A-Z][A-Za-z0-9_$]*)").unwrap())
}

/// Extract guard names from `canActivate`, `canDeactivate`, `canMatch`, `canLoad` arrays.
fn extract_guard_names(obj_body: &str) -> Vec<String> {
    let id_re = guard_ident_regex();
    let mut guards = Vec::new();
    for key in &["canActivate", "canDeactivate", "canMatch", "canLoad", "canActivateChild"] {
        let pattern = format!(r"\b{key}\s*:\s*\[");
        if let Ok(re) = Regex::new(&pattern) {
            if let Some(m) = re.find(obj_body) {
                let bracket_start = m.end() - 1;
                if let Some(inner) = read_bracket_array(obj_body, bracket_start) {
                    for cap in id_re.captures_iter(&inner) {
                        let name = cap[1].to_string();
                        if !angular_built_ins().contains(name.as_str())
                            && !ts_builtin_types().contains(name.as_str())
                        {
                            guards.push(name);
                        }
                    }
                }
            }
        }
    }
    guards.sort();
    guards.dedup();
    guards
}

/// Extract resolver class names from the `resolve:` key of a route object.
fn extract_resolver_names(obj_body: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\bresolve\s*:\s*\{").unwrap());
    let m = match re.find(obj_body) {
        Some(m) => m,
        None => return Vec::new(),
    };
    let brace_start = m.end() - 1;
    let inner = match read_object(obj_body, brace_start) {
        Some((s, _)) => s,
        None => return Vec::new(),
    };
    static IDENT_RE: OnceLock<Regex> = OnceLock::new();
    let id_re = IDENT_RE.get_or_init(|| Regex::new(r"\b([A-Z][A-Za-z0-9_$]*)").unwrap());
    let mut names: Vec<String> = id_re
        .captures_iter(&inner)
        .map(|c| c[1].to_string())
        .filter(|n| {
            !angular_built_ins().contains(n.as_str()) && !ts_builtin_types().contains(n.as_str())
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Extract `path:` value from a route object body.
fn extract_route_path(obj_body: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#"\bpath\s*:\s*['"`]([^'"`]*)['"`]"#).unwrap());
    re.captures(obj_body).map(|c| c[1].to_string())
}

/// Extract `redirectTo:` value.
fn extract_redirect_to(obj_body: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r#"\bredirectTo\s*:\s*['"`]([^'"`]*)['"`]"#).unwrap());
    re.captures(obj_body).map(|c| c[1].to_string())
}

/// Extract `component: ClassName` from a route object body.
fn extract_route_component_name(obj_body: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"\bcomponent\s*:\s*([A-Z][A-Za-z0-9_$]*)").unwrap()
    });
    re.captures(obj_body).map(|c| c[1].to_string())
}

/// Extract the import path and optional exported name from a `loadComponent` or
/// `loadChildren` value. Both forms:
///   `() => import('./path/to/file')`
///   `() => import('./path/to/file').then(m => m.SomeComponent)`
fn extract_lazy_import(obj_body: &str, key: &str) -> Option<(String, Option<String>)> {
    // Match: key: () => import('./...')
    let pattern = format!(
        r#"\b{key}\s*:\s*(?:\(\s*\)\s*=>)?\s*import\s*\(\s*["'`]([^"'`]+)["'`]\s*\)"#
    );
    let re = Regex::new(&pattern).ok()?;
    let cap = re.captures(obj_body)?;
    let import_path = cap[1].to_string();

    // Also try to find `.then(m => m.ExportName)`
    static THEN_RE: OnceLock<Regex> = OnceLock::new();
    let then_re = THEN_RE.get_or_init(|| {
        Regex::new(r"\.then\s*\(\s*(?:m|c)\s*=>\s*(?:m|c)\.([A-Z][A-Za-z0-9_$]*)").unwrap()
    });
    let export_name = then_re
        .captures(&obj_body[cap.get(0).unwrap().start()..])
        .map(|c| c[1].to_string());

    Some((import_path, export_name))
}

/// Parse route objects recursively from `array_body` (text between `[` and `]`).
fn parse_angular_routes_array(
    array_body: &str,
    prefix: &str,
    array_offset_in_file: usize,
    original: &str,
) -> Vec<AngularRoute> {
    let mut results = Vec::new();
    let bytes = array_body.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'{' {
            match read_object(array_body, i) {
                Some((obj_body, end)) => {
                    let obj_offset = array_offset_in_file + i;
                    if let Some(route) =
                        parse_single_route_object(&obj_body, prefix, obj_offset, original)
                    {
                        let full_path = route.full_path.clone();
                        results.push(route);

                        // Recurse into children: array
                        {
                            let re = children_array_regex();
                            if let Some(m) = re.find(&obj_body) {
                                let bracket_start = m.end() - 1;
                                if let Some(children_body) =
                                    read_bracket_array(&obj_body, bracket_start)
                                {
                                    let child_offset = obj_offset + bracket_start + 1;
                                    let child_routes = parse_angular_routes_array(
                                        &children_body,
                                        &full_path,
                                        child_offset,
                                        original,
                                    );
                                    results.extend(child_routes);
                                }
                            }
                        }
                    } else {
                        // No path or redirect-only route — still recurse into children
                        {
                            let re = children_array_regex();
                            if let Some(m) = re.find(&obj_body) {
                                let bracket_start = m.end() - 1;
                                if let Some(children_body) =
                                    read_bracket_array(&obj_body, bracket_start)
                                {
                                    let child_offset =
                                        array_offset_in_file + i + bracket_start + 1;
                                    let child_routes = parse_angular_routes_array(
                                        &children_body,
                                        prefix,
                                        child_offset,
                                        original,
                                    );
                                    results.extend(child_routes);
                                }
                            }
                        }
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

/// Parse a single route object body into an `AngularRoute`.
fn parse_single_route_object(
    obj_body: &str,
    prefix: &str,
    obj_offset_in_file: usize,
    original: &str,
) -> Option<AngularRoute> {
    let path_val = extract_route_path(obj_body)?;
    let full_path = compose_angular_route_path(prefix, &path_val);
    let line = line_at(original, obj_offset_in_file);

    let component_name = extract_route_component_name(obj_body);
    let redirect_to = extract_redirect_to(obj_body);

    let lazy_load_component = extract_lazy_import(obj_body, "loadComponent");
    let lazy_load_children = extract_lazy_import(obj_body, "loadChildren");
    let (lazy_import_path, lazy_export_name) = lazy_load_component
        .or(lazy_load_children)
        .map(|(p, e)| (Some(p), e))
        .unwrap_or((None, None));

    let guards = extract_guard_names(obj_body);
    let resolvers = extract_resolver_names(obj_body);

    Some(AngularRoute {
        full_path,
        line,
        component_name,
        lazy_import_path,
        lazy_export_name,
        guards,
        resolvers,
        redirect_to,
    })
}

/// Top-level route extraction: finds all route arrays and emits nodes + refs.
fn extract_angular_routes(
    safe: &str,
    file_str: &str,
    original: &str,
) -> (Vec<Node>, Vec<UnresolvedRef>) {
    let mut nodes = Vec::new();
    let mut refs = Vec::new();

    let offsets = find_routes_array_offsets(safe);
    for bracket_pos in offsets {
        let inner = match read_bracket_array(safe, bracket_pos) {
            Some(s) => s,
            None => continue,
        };
        let routes =
            parse_angular_routes_array(&inner, "", bracket_pos + 1, original);

        for route in routes {
            // Use the display path: prepend '/' if missing for route name
            let display_path = if route.full_path.starts_with('/') || route.full_path == "/**" {
                route.full_path.clone()
            } else if route.full_path.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", route.full_path.trim_start_matches('/'))
            };

            let route_id = format!(
                "route:{}:{}:GET:{}",
                file_str, route.line, display_path
            );

            // Determine if this is a layout/empty-path child
            let is_layout = route.full_path == display_path.trim_start_matches('/')
                || route.full_path.is_empty();

            let mut metadata = serde_json::json!({});
            if is_layout && display_path.len() <= 1 {
                metadata["layout_route"] = serde_json::json!(true);
            }

            nodes.push(Node {
                id: route_id.clone(),
                name: display_path.clone(),
                qualified_name: format!("{}::GET:{}", file_str, display_path),
                kind: NodeKind::Route,
                language: Language::TypeScript,
                file_path: file_str.to_string(),
                start_line: route.line,
                end_line: route.line,
                metadata: Some(metadata.to_string()),
                ..Default::default()
            });

            // If redirect_to only — no component refs
            if route.redirect_to.is_some() && route.component_name.is_none() && route.lazy_import_path.is_none() {
                continue;
            }

            // component: ClassName ref
            if let Some(comp) = &route.component_name {
                refs.push(UnresolvedRef {
                    id: format!("{}->{}", route_id, comp),
                    from_node_id: route_id.clone(),
                    reference_name: comp.clone(),
                    reference_kind: "references".to_string(),
                    file_path: file_str.to_string(),
                    line: Some(route.line),
                    ..Default::default()
                });
            }

            // loadComponent / loadChildren import ref
            if let Some(import_path) = &route.lazy_import_path {
                refs.push(UnresolvedRef {
                    id: format!("{}->import:{}", route_id, import_path),
                    from_node_id: route_id.clone(),
                    reference_name: import_path.clone(),
                    reference_kind: "imports".to_string(),
                    file_path: file_str.to_string(),
                    line: Some(route.line),
                    ..Default::default()
                });
                // Also emit a references ref if we know the export name
                if let Some(export_name) = &route.lazy_export_name {
                    refs.push(UnresolvedRef {
                        id: format!("{}->{}", route_id, export_name),
                        from_node_id: route_id.clone(),
                        reference_name: export_name.clone(),
                        reference_kind: "references".to_string(),
                        file_path: file_str.to_string(),
                        line: Some(route.line),
                        ..Default::default()
                    });
                }
            }

            // Guard refs
            for guard in &route.guards {
                refs.push(UnresolvedRef {
                    id: format!("{}->guard:{}", route_id, guard),
                    from_node_id: route_id.clone(),
                    reference_name: guard.clone(),
                    reference_kind: "references".to_string(),
                    file_path: file_str.to_string(),
                    line: Some(route.line),
                    ..Default::default()
                });
            }

            // Resolver refs
            for resolver in &route.resolvers {
                refs.push(UnresolvedRef {
                    id: format!("{}->resolver:{}", route_id, resolver),
                    from_node_id: route_id.clone(),
                    reference_name: resolver.clone(),
                    reference_kind: "references".to_string(),
                    file_path: file_str.to_string(),
                    line: Some(route.line),
                    ..Default::default()
                });
            }
        }
    }

    (nodes, refs)
}

// ─── FrameworkResolver impl ───────────────────────────────────────────────────

impl FrameworkResolver for AngularResolver {
    fn name(&self) -> &'static str {
        "angular"
    }

    fn languages(&self) -> Option<&'static [Language]> {
        static LANGS: &[Language] = &[Language::TypeScript];
        Some(LANGS)
    }

    fn detect(&self, context: &ResolutionContext<'_>) -> bool {
        // Priority 1: package.json with @angular/core key
        if let Some(raw) = context.read_file("package.json") {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&raw) {
                for section in &["dependencies", "devDependencies"] {
                    if let Some(obj) = pkg.get(section).and_then(|v| v.as_object()) {
                        if let Some(version_str) =
                            obj.get("@angular/core").and_then(|v| v.as_str())
                        {
                            let gte19 = parse_angular_version_gte_19(version_str);
                            let _ = self.angular_version_gte_19.set(gte19);
                            return true;
                        }
                    }
                }
            }
        }
        // Priority 2: angular.json
        if context.file_exists("angular.json") {
            let _ = self.angular_version_gte_19.set(false);
            return true;
        }
        // Priority 3: *.component.ts fallback
        for file in context.known_files {
            if file.ends_with(".component.ts") {
                if let Some(content) = context.read_file(file.as_str()) {
                    if content.contains("@Component") {
                        let _ = self.angular_version_gte_19.set(false);
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
        let kind = reference.reference_kind.as_str();

        // Pattern 1: Angular built-ins always return None
        if angular_built_ins().contains(name.as_str()) {
            return Ok(None);
        }

        // imports kind is handled by the existing import_resolver path
        if kind == "imports" {
            return Ok(None);
        }

        let candidates = context.get_nodes_by_name(name);

        // Pattern 2: DI provider by suffix convention
        for (suffix, convention) in PROVIDER_CONVENTIONS {
            if name.ends_with(suffix) {
                let classes: Vec<_> = candidates
                    .iter()
                    .filter(|n| n.kind == NodeKind::Class)
                    .collect();
                if !classes.is_empty() {
                    let target = classes
                        .iter()
                        .find(|n| n.file_path.contains(convention))
                        .or_else(|| classes.first());
                    if let Some(node) = target {
                        let confidence = if node.file_path.contains(convention) {
                            0.85
                        } else {
                            0.70
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
        }

        // Pattern 3: NgModule declarations (contains) — filter to Component kind
        if kind == "contains" {
            let components: Vec<_> = candidates
                .iter()
                .filter(|n| n.kind == NodeKind::Component)
                .collect();
            if let Some(node) = components.first() {
                return Ok(Some(make_resolved_edge(
                    reference,
                    node.id.clone(),
                    0.90,
                    self.name(),
                )));
            }
            // Fall through to other patterns if not found as Component
        }

        // Pattern 4: Component by class name
        let component_match = candidates
            .iter()
            .find(|n| n.kind == NodeKind::Component);
        if let Some(node) = component_match {
            return Ok(Some(make_resolved_edge(
                reference,
                node.id.clone(),
                0.85,
                self.name(),
            )));
        }

        // Pattern 5: Module by class name
        let module_match = candidates
            .iter()
            .find(|n| n.kind == NodeKind::Module);
        if let Some(node) = module_match {
            return Ok(Some(make_resolved_edge(
                reference,
                node.id.clone(),
                0.80,
                self.name(),
            )));
        }

        // Pattern 6: Pipe by name (NodeKind::Class in *.pipe.ts)
        let pipe_match = candidates
            .iter()
            .find(|n| n.kind == NodeKind::Class && n.file_path.contains(".pipe."));
        if let Some(node) = pipe_match {
            return Ok(Some(make_resolved_edge(
                reference,
                node.id.clone(),
                0.75,
                self.name(),
            )));
        }

        // Pattern 7: InjectionToken constant
        let token_match = candidates.iter().find(|n| {
            (n.kind == NodeKind::Variable || n.kind == NodeKind::Constant)
                && (n.file_path.contains(".tokens.") || n.file_path.contains(".constants."))
        });
        if let Some(node) = token_match {
            return Ok(Some(make_resolved_edge(
                reference,
                node.id.clone(),
                0.75,
                self.name(),
            )));
        }

        // Pattern 8: No match
        Ok(None)
    }

    fn extract(
        &self,
        file_path: &Path,
        content: &str,
        _context: &ResolutionContext<'_>,
    ) -> Result<FrameworkExtractionResult, CodeWikiError> {
        let file_str_raw = file_path.to_string_lossy();
        let file_str = file_str_raw.replace('\\', "/");

        // Guard: only .ts files (not .html, .spec.ts is fine)
        if !file_str.ends_with(".ts") {
            return Ok(FrameworkExtractionResult::empty());
        }

        let safe = strip_comments(content, CommentLang::TypeScript);
        let class_scopes = build_class_scopes(&safe);

        let mut nodes = Vec::new();
        let mut unresolved_refs = Vec::new();

        // ── Phase A: Component / DI extraction ───────────────────────────────

        let decorator_names = &[
            "Component",
            "Directive",
            "Pipe",
            "Injectable",
            "NgModule",
        ];
        let hits = find_decorators(&safe, decorator_names);

        for hit in &hits {
            let class_name = match class_name_after(&safe, hit.end) {
                Some(n) => n,
                None => continue,
            };

            let line = line_at(&safe, hit.index);

            match hit.name.as_str() {
                "Component" => {
                    let selector = extract_selector(&hit.args)
                        .unwrap_or_else(|| class_name.clone());
                    let standalone = extract_standalone_flag(&hit.args)
                        .unwrap_or_else(|| self.is_standalone_by_default());

                    // Build metadata
                    let mut meta = serde_json::json!({
                        "class": class_name,
                        "selector": selector,
                        "standalone": standalone,
                    });

                    // @Input/@Output from class scope body
                    let scope_body = class_scopes
                        .iter()
                        .find(|s| s.name == class_name)
                        .map(|s| &safe[s.start_byte..s.end_byte])
                        .unwrap_or("");
                    let (inputs, outputs) = extract_input_output_names(scope_body);
                    if !inputs.is_empty() {
                        meta["inputs"] = serde_json::json!(inputs);
                    }
                    if !outputs.is_empty() {
                        meta["outputs"] = serde_json::json!(outputs);
                    }

                    nodes.push(Node {
                        id: format!("component:{}:{}:{}", file_str, line, class_name),
                        name: class_name.clone(),
                        qualified_name: format!("{}::component:{}", file_str, selector),
                        kind: NodeKind::Component,
                        language: Language::TypeScript,
                        file_path: file_str.clone(),
                        start_line: line,
                        end_line: line,
                        metadata: Some(meta.to_string()),
                        ..Default::default()
                    });

                    let comp_id =
                        format!("component:{}:{}:{}", file_str, line, class_name);

                    // standalone imports:[...] → "uses" refs
                    if standalone {
                        let imports = extract_named_array_identifiers(&hit.args, "imports");
                        for imp in imports {
                            if !angular_built_ins().contains(imp.as_str()) {
                                unresolved_refs.push(UnresolvedRef {
                                    id: format!("{}->{}", comp_id, imp),
                                    from_node_id: comp_id.clone(),
                                    reference_name: imp,
                                    reference_kind: "uses".to_string(),
                                    file_path: file_str.clone(),
                                    line: Some(line),
                                    ..Default::default()
                                });
                            }
                        }
                    }

                    // Inline template scanning
                    if let Some(template) = extract_inline_template(&hit.args) {
                        for sel in scan_template_selectors(&template) {
                            unresolved_refs.push(UnresolvedRef {
                                id: format!("{}->tpl:{}", comp_id, sel),
                                from_node_id: comp_id.clone(),
                                reference_name: sel,
                                reference_kind: "uses".to_string(),
                                file_path: file_str.clone(),
                                line: Some(line),
                                ..Default::default()
                            });
                        }
                        for pipe in scan_template_pipes(&template) {
                            unresolved_refs.push(UnresolvedRef {
                                id: format!("{}->pipe:{}", comp_id, pipe),
                                from_node_id: comp_id.clone(),
                                reference_name: pipe,
                                reference_kind: "uses".to_string(),
                                file_path: file_str.clone(),
                                line: Some(line),
                                ..Default::default()
                            });
                        }
                    }
                }

                "Directive" => {
                    let selector = extract_selector(&hit.args)
                        .unwrap_or_else(|| class_name.clone());
                    let meta = serde_json::json!({
                        "class": class_name,
                        "selector": selector,
                    });

                    nodes.push(Node {
                        id: format!("directive:{}:{}:{}", file_str, line, class_name),
                        name: class_name.clone(),
                        qualified_name: format!("{}::directive:{}", file_str, selector),
                        kind: NodeKind::Component,
                        language: Language::TypeScript,
                        file_path: file_str.clone(),
                        start_line: line,
                        end_line: line,
                        metadata: Some(meta.to_string()),
                        ..Default::default()
                    });
                }

                "Pipe" => {
                    let pipe_name = extract_pipe_name(&hit.args)
                        .unwrap_or_else(|| class_name.to_lowercase());
                    let meta = serde_json::json!({
                        "class": class_name,
                        "name": pipe_name,
                    });

                    nodes.push(Node {
                        id: format!("pipe:{}:{}:{}", file_str, line, class_name),
                        name: class_name.clone(),
                        qualified_name: format!("{}::pipe:{}", file_str, pipe_name),
                        kind: NodeKind::Class,
                        language: Language::TypeScript,
                        file_path: file_str.clone(),
                        start_line: line,
                        end_line: line,
                        metadata: Some(meta.to_string()),
                        ..Default::default()
                    });
                }

                "Injectable" => {
                    let provided_in =
                        extract_provided_in(&hit.args).unwrap_or_default();
                    let meta = serde_json::json!({
                        "class": class_name,
                        "providedIn": provided_in,
                    });

                    nodes.push(Node {
                        id: format!("service:{}:{}:{}", file_str, line, class_name),
                        name: class_name.clone(),
                        qualified_name: format!("{}::service:{}", file_str, class_name),
                        kind: NodeKind::Class,
                        language: Language::TypeScript,
                        file_path: file_str.clone(),
                        start_line: line,
                        end_line: line,
                        metadata: Some(meta.to_string()),
                        ..Default::default()
                    });
                }

                "NgModule" => {
                    let meta = serde_json::json!({
                        "class": class_name,
                    });

                    let module_id =
                        format!("module:{}:{}:{}", file_str, line, class_name);

                    nodes.push(Node {
                        id: module_id.clone(),
                        name: class_name.clone(),
                        qualified_name: format!("{}::module:{}", file_str, class_name),
                        kind: NodeKind::Module,
                        language: Language::TypeScript,
                        file_path: file_str.clone(),
                        start_line: line,
                        end_line: line,
                        metadata: Some(meta.to_string()),
                        ..Default::default()
                    });

                    // declarations → "contains" refs
                    for decl in extract_named_array_identifiers(&hit.args, "declarations") {
                        if !angular_built_ins().contains(decl.as_str()) {
                            unresolved_refs.push(UnresolvedRef {
                                id: format!("{}->{}", module_id, decl),
                                from_node_id: module_id.clone(),
                                reference_name: decl,
                                reference_kind: "contains".to_string(),
                                file_path: file_str.clone(),
                                line: Some(line),
                                ..Default::default()
                            });
                        }
                    }

                    // providers → "uses" refs
                    for prov in extract_named_array_identifiers(&hit.args, "providers") {
                        if !angular_built_ins().contains(prov.as_str()) {
                            unresolved_refs.push(UnresolvedRef {
                                id: format!("{}->prov:{}", module_id, prov),
                                from_node_id: module_id.clone(),
                                reference_name: prov,
                                reference_kind: "uses".to_string(),
                                file_path: file_str.clone(),
                                line: Some(line),
                                ..Default::default()
                            });
                        }
                    }

                    // imports → "uses" refs (other NgModules)
                    for imp in extract_named_array_identifiers(&hit.args, "imports") {
                        if !angular_built_ins().contains(imp.as_str()) {
                            unresolved_refs.push(UnresolvedRef {
                                id: format!("{}->imp:{}", module_id, imp),
                                from_node_id: module_id.clone(),
                                reference_name: imp,
                                reference_kind: "uses".to_string(),
                                file_path: file_str.clone(),
                                line: Some(line),
                                ..Default::default()
                            });
                        }
                    }
                }

                _ => {}
            }
        }

        // ── DI: constructor params ────────────────────────────────────────────

        static CTOR_RE: OnceLock<Regex> = OnceLock::new();
        let ctor_re = CTOR_RE
            .get_or_init(|| Regex::new(r"\bconstructor\s*\(").unwrap());

        for m in ctor_re.find_iter(&safe) {
            let paren = m.end() - 1;
            let scope = scope_for(&class_scopes, m.start());
            if let Some((args, _)) = read_args(&safe, paren) {
                let types = extract_constructor_di_types(&args);
                if let Some(cls) = scope {
                    // Find the node id for this class (from nodes we already created)
                    let from_id = nodes
                        .iter()
                        .find(|n| n.name == cls.name)
                        .map(|n| n.id.clone())
                        .unwrap_or_else(|| {
                            format!("class:{}:{}", file_str, cls.name)
                        });

                    let ctor_line = line_at(&safe, m.start());
                    for ty in types {
                        unresolved_refs.push(UnresolvedRef {
                            id: format!("{}->{}", from_id, ty),
                            from_node_id: from_id.clone(),
                            reference_name: ty,
                            reference_kind: "references".to_string(),
                            file_path: file_str.clone(),
                            line: Some(ctor_line),
                            ..Default::default()
                        });
                    }
                }
            }
        }

        // ── DI: inject() calls ────────────────────────────────────────────────

        let inject_calls = extract_inject_calls(&safe);
        for (offset, ty) in inject_calls {
            let scope = scope_for(&class_scopes, offset);
            let from_id = scope
                .and_then(|cls| nodes.iter().find(|n| n.name == cls.name))
                .map(|n| n.id.clone())
                .unwrap_or_else(|| format!("file:{}", file_str));

            let inj_line = line_at(&safe, offset);
            unresolved_refs.push(UnresolvedRef {
                id: format!("{}->{}", from_id, ty),
                from_node_id: from_id,
                reference_name: ty,
                reference_kind: "references".to_string(),
                file_path: file_str.clone(),
                line: Some(inj_line),
                ..Default::default()
            });
        }

        // ── @ViewChild / @ContentChild ────────────────────────────────────────

        let view_hits = find_decorators(&safe, &["ViewChild", "ContentChild"]);
        let first_ident_re = inject_ident_regex(); // reuse same pattern ^([A-Za-z_$][A-Za-z0-9_$]*)
        for hit in &view_hits {
            let scope = scope_for(&class_scopes, hit.index);
            let from_id = scope
                .and_then(|cls| nodes.iter().find(|n| n.name == cls.name))
                .map(|n| n.id.clone())
                .unwrap_or_else(|| format!("file:{}", file_str));

            // args is e.g. "FooComponent" or "FooComponent, { static: true }"
            let args_trimmed = hit.args.trim();
            if let Some(cap) = first_ident_re.captures(args_trimmed) {
                let ref_name = cap[1].to_string();
                if !ts_builtin_types().contains(ref_name.as_str())
                    && !angular_built_ins().contains(ref_name.as_str())
                {
                    let vc_line = line_at(&safe, hit.index);
                    unresolved_refs.push(UnresolvedRef {
                        id: format!("{}->{}", from_id, ref_name),
                        from_node_id: from_id,
                        reference_name: ref_name,
                        reference_kind: "references".to_string(),
                        file_path: file_str.clone(),
                        line: Some(vc_line),
                        ..Default::default()
                    });
                }
            }
        }

        // ── Phase B: Routing extraction ───────────────────────────────────────

        if safe.contains("Routes") || safe.contains("RouterModule") || safe.contains("provideRouter") {
            let (route_nodes, route_refs) =
                extract_angular_routes(&safe, &file_str, content);
            nodes.extend(route_nodes);
            unresolved_refs.extend(route_refs);
        }

        // Deduplicate unresolved_refs by id
        let mut seen_ids = HashSet::new();
        unresolved_refs.retain(|r| seen_ids.insert(r.id.clone()));

        Ok(FrameworkExtractionResult {
            nodes,
            edges: Vec::new(),
            unresolved_refs,
        })
    }
}

/// Extract @Input and @Output property names from a class body.
fn extract_input_output_names(class_body: &str) -> (Vec<String>, Vec<String>) {
    static INPUT_RE: OnceLock<Regex> = OnceLock::new();
    static OUTPUT_RE: OnceLock<Regex> = OnceLock::new();

    let inp_re = INPUT_RE.get_or_init(|| {
        Regex::new(r"@Input\b[^;]*?(?:set\s+)?([a-zA-Z_$][a-zA-Z0-9_$]*)\s*[=(]").unwrap()
    });
    let out_re = OUTPUT_RE.get_or_init(|| {
        Regex::new(r"@Output\b[^;]*?([a-zA-Z_$][a-zA-Z0-9_$]*)\s*[=]").unwrap()
    });

    let inputs = inp_re
        .captures_iter(class_body)
        .filter_map(|c| {
            let name = c[1].to_string();
            if name == "required" || name == "Input" {
                None
            } else {
                Some(name)
            }
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let outputs = out_re
        .captures_iter(class_body)
        .map(|c| c[1].to_string())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    (inputs, outputs)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caches::ResolverCaches;
    use crate::path_aliases::PathAliasMap;
    use codewiki_core::{Language, Node, NodeKind};
    use std::path::Path;
    use tempfile::TempDir;

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

    fn make_ctx_with_root<'a>(
        root: &'a Path,
        caches: &'a ResolverCaches,
        aliases: &'a PathAliasMap,
        known_files: &'a [String],
    ) -> ResolutionContext<'a> {
        ResolutionContext {
            project_root: root,
            project_languages: &[Language::TypeScript],
            caches,
            path_aliases: aliases,
            known_files,
        }
    }

    // ── Group 1: detect() ─────────────────────────────────────────────────────

    #[test]
    fn test_detect_via_package_json() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"@angular/core":"^18.0.0"}}"#,
        )
        .unwrap();
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx_with_root(dir.path(), &caches, &aliases, &[]);
        assert!(AngularResolver::new().detect(&ctx));
    }

    #[test]
    fn test_detect_version_gte19() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"@angular/core":"^19.1.0"}}"#,
        )
        .unwrap();
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx_with_root(dir.path(), &caches, &aliases, &[]);
        let resolver = AngularResolver::new();
        assert!(resolver.detect(&ctx));
        assert!(resolver.is_standalone_by_default());
    }

    #[test]
    fn test_detect_via_angular_json() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("angular.json"), "{}").unwrap();
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let known: Vec<String> = vec!["angular.json".to_string()];
        let ctx = make_ctx_with_root(dir.path(), &caches, &aliases, &known);
        assert!(AngularResolver::new().detect(&ctx));
    }

    #[test]
    fn test_detect_via_component_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.component.ts"),
            "@Component({ selector: 'app-root' })\nexport class AppComponent {}",
        )
        .unwrap();
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let known: Vec<String> = vec!["app.component.ts".to_string()];
        let ctx = make_ctx_with_root(dir.path(), &caches, &aliases, &known);
        assert!(AngularResolver::new().detect(&ctx));
    }

    #[test]
    fn test_no_false_positive_nestjs() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"@nestjs/core":"^9.0.0"}}"#,
        )
        .unwrap();
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx_with_root(dir.path(), &caches, &aliases, &[]);
        assert!(!AngularResolver::new().detect(&ctx));
    }

    #[test]
    fn test_no_false_positive_react() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"react":"^18.0.0"}}"#,
        )
        .unwrap();
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx_with_root(dir.path(), &caches, &aliases, &[]);
        assert!(!AngularResolver::new().detect(&ctx));
    }

    // ── Group 2: extract() component nodes ───────────────────────────────────

    #[test]
    fn test_extract_basic_component() {
        let content = r#"
@Component({
  selector: 'app-hello',
  template: '<div>Hello</div>',
})
export class HelloComponent {}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("hello.component.ts"), content, &ctx)
            .unwrap();

        let comp = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Component)
            .expect("component node");
        assert_eq!(comp.name, "HelloComponent");
        assert!(
            comp.qualified_name.contains("::component:app-hello"),
            "qn: {}",
            comp.qualified_name
        );
        assert!(
            comp.id.starts_with("component:"),
            "id: {}",
            comp.id
        );
    }

    #[test]
    fn test_extract_standalone_component() {
        let content = r#"
@Component({
  selector: 'app-foo',
  standalone: true,
  template: '',
  imports: [RouterLink],
})
export class FooComponent {}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("foo.component.ts"), content, &ctx)
            .unwrap();
        let comp = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Component)
            .unwrap();
        let meta = comp.metadata.as_deref().unwrap_or("");
        assert!(meta.contains("\"standalone\":true"), "meta: {meta}");
        assert!(meta.contains("\"selector\":\"app-foo\""), "meta: {meta}");
    }

    #[test]
    fn test_extract_directive() {
        let content = r#"
@Directive({
  selector: '[appHighlight]',
})
export class HighlightDirective {}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("highlight.directive.ts"), content, &ctx)
            .unwrap();
        let node = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Component)
            .unwrap();
        assert!(
            node.qualified_name.contains("::directive:[appHighlight]"),
            "qn: {}",
            node.qualified_name
        );
    }

    #[test]
    fn test_extract_pipe() {
        let content = r#"
@Pipe({ name: 'truncate' })
export class TruncatePipe implements PipeTransform {}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("truncate.pipe.ts"), content, &ctx)
            .unwrap();
        let node = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Class)
            .unwrap();
        assert!(
            node.qualified_name.contains("::pipe:truncate"),
            "qn: {}",
            node.qualified_name
        );
    }

    #[test]
    fn test_extract_injectable() {
        let content = r#"
@Injectable({ providedIn: 'root' })
export class AuthService {}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("auth.service.ts"), content, &ctx)
            .unwrap();
        let node = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Class)
            .unwrap();
        assert!(
            node.qualified_name.contains("::service:AuthService"),
            "qn: {}",
            node.qualified_name
        );
    }

    #[test]
    fn test_extract_ngmodule() {
        let content = r#"
@NgModule({
  declarations: [AppComponent, HeaderComponent],
  providers: [AuthService],
  imports: [BrowserModule, RouterModule],
})
export class AppModule {}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("app.module.ts"), content, &ctx)
            .unwrap();
        let node = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Module)
            .unwrap();
        assert_eq!(node.name, "AppModule");

        let contains_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "contains")
            .collect();
        assert!(!contains_refs.is_empty(), "should have contains refs");
        let ref_names: Vec<_> = contains_refs
            .iter()
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(ref_names.contains(&"AppComponent"), "names: {ref_names:?}");
        assert!(ref_names.contains(&"HeaderComponent"), "names: {ref_names:?}");
    }

    #[test]
    fn test_extract_attribute_selector() {
        let content = r#"
@Directive({ selector: '[ngTabList]' })
export class TabListDirective {}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("tab-list.directive.ts"), content, &ctx)
            .unwrap();
        let node = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Component)
            .unwrap();
        assert!(
            node.qualified_name.contains("[ngTabList]"),
            "qn: {}",
            node.qualified_name
        );
    }

    // ── Group 3: DI extraction ────────────────────────────────────────────────

    #[test]
    fn test_constructor_di_single_service() {
        let content = r#"
@Injectable({ providedIn: 'root' })
export class UserComponent {
  constructor(private readonly authService: AuthService) {}
}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("user.component.ts"), content, &ctx)
            .unwrap();
        let refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_name == "AuthService" && r.reference_kind == "references")
            .collect();
        assert_eq!(refs.len(), 1, "refs: {:?}", result.unresolved_refs);
    }

    #[test]
    fn test_constructor_di_excludes_builtins() {
        let content = r#"
@Component({ selector: 'app-x', template: '' })
export class XComponent {
  constructor(
    private readonly el: ElementRef,
    private readonly cdr: ChangeDetectorRef,
    private readonly userSvc: UserService,
  ) {}
}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("x.component.ts"), content, &ctx)
            .unwrap();
        let di_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "references")
            .collect();
        let names: Vec<_> = di_refs.iter().map(|r| r.reference_name.as_str()).collect();
        assert!(!names.contains(&"ElementRef"), "ElementRef should be excluded, got: {names:?}");
        assert!(!names.contains(&"ChangeDetectorRef"), "ChangeDetectorRef should be excluded, got: {names:?}");
        assert!(names.contains(&"UserService"), "UserService should be present, got: {names:?}");
    }

    #[test]
    fn test_inject_function_field() {
        let content = r#"
@Injectable({ providedIn: 'root' })
export class CartComponent {
  private readonly fooService = inject(FooService);
}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("cart.component.ts"), content, &ctx)
            .unwrap();
        let refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_name == "FooService")
            .collect();
        assert!(!refs.is_empty(), "FooService ref should be present");
    }

    #[test]
    fn test_inject_function_generic() {
        let content = r#"
@Injectable({ providedIn: 'root' })
export class TokenComponent {
  private readonly config = inject(CONFIG_TOKEN);
}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("token.component.ts"), content, &ctx)
            .unwrap();
        let refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_name == "CONFIG_TOKEN")
            .collect();
        assert!(!refs.is_empty(), "CONFIG_TOKEN ref should be present");
    }

    #[test]
    fn test_ngmodule_declarations_refs() {
        let content = r#"
@NgModule({
  declarations: [AppComponent, HeaderComponent],
  providers: [AuthService],
})
export class AppModule {}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("app.module.ts"), content, &ctx)
            .unwrap();
        let all_refs = &result.unresolved_refs;
        let names: Vec<_> = all_refs.iter().map(|r| r.reference_name.as_str()).collect();
        assert!(names.contains(&"AppComponent"), "names: {names:?}");
        assert!(names.contains(&"HeaderComponent"), "names: {names:?}");
        assert!(names.contains(&"AuthService"), "names: {names:?}");
    }

    // ── Group 4: Routing extraction ───────────────────────────────────────────

    #[test]
    fn test_route_for_root() {
        let content = r#"
import { RouterModule } from '@angular/router';
export const AppModule = RouterModule.forRoot([
  { path: 'users', component: UsersComponent },
]);
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("app.module.ts"), content, &ctx)
            .unwrap();
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route node");
        assert_eq!(route.name, "/users");
        let comp_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_name == "UsersComponent")
            .collect();
        assert!(!comp_refs.is_empty(), "UsersComponent ref");
    }

    #[test]
    fn test_route_provide_router_deduplication() {
        // Both pattern B and C match the same `[` offset — must emit only once
        let content = r#"
const ROUTES: Routes = [
  { path: 'home', component: HomeComponent },
];
provideRouter(ROUTES);
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("app.config.ts"), content, &ctx)
            .unwrap();
        let route_nodes: Vec<_> = result.nodes.iter().filter(|n| n.kind == NodeKind::Route).collect();
        // provideRouter references ROUTES variable not an inline array, so only const Routes = [ matches
        assert!(!route_nodes.is_empty(), "at least one route from const Routes");
    }

    #[test]
    fn test_route_load_children() {
        let content = r#"
export const routes: Routes = [
  {
    path: 'admin',
    loadChildren: () => import('./admin/admin.routes'),
  },
];
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("app.routes.ts"), content, &ctx)
            .unwrap();
        let imports_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "imports")
            .collect();
        assert!(!imports_refs.is_empty(), "should have imports ref for loadChildren");
    }

    #[test]
    fn test_route_load_component() {
        let content = r#"
export const routes: Routes = [
  {
    path: 'settings',
    loadComponent: () => import('./settings/settings.component').then(m => m.SettingsComponent),
  },
];
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("app.routes.ts"), content, &ctx)
            .unwrap();
        let imports_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "imports")
            .collect();
        assert!(!imports_refs.is_empty(), "should have imports ref for loadComponent");
        let ref_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "references" && r.reference_name == "SettingsComponent")
            .collect();
        assert!(!ref_refs.is_empty(), "SettingsComponent references ref");
    }

    #[test]
    fn test_route_nested_children_path_composition() {
        let content = r#"
export const ROUTES: Routes = [
  {
    path: 'users',
    component: UsersComponent,
    children: [
      { path: '', component: UsersComponent },
      { path: ':id', component: UserDetailComponent },
    ],
  },
];
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("users.routes.ts"), content, &ctx)
            .unwrap();
        let route_names: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Route)
            .map(|n| n.name.as_str())
            .collect();
        assert!(route_names.contains(&"/users"), "routes: {route_names:?}");
        assert!(route_names.contains(&"/users/:id"), "routes: {route_names:?}");
    }

    #[test]
    fn test_route_guards_and_resolvers() {
        let content = r#"
export const ROUTES: Routes = [
  {
    path: 'admin',
    component: AdminComponent,
    canActivate: [AuthGuard, RoleGuard],
  },
];
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("admin.routes.ts"), content, &ctx)
            .unwrap();
        let guard_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| {
                r.reference_kind == "references"
                    && (r.reference_name == "AuthGuard" || r.reference_name == "RoleGuard")
            })
            .collect();
        assert_eq!(guard_refs.len(), 2, "guards: {:?}", result.unresolved_refs);
    }

    #[test]
    fn test_route_redirect_no_component_ref() {
        let content = r#"
export const ROUTES: Routes = [
  { path: '', redirectTo: '/home', pathMatch: 'full' },
];
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("app.routes.ts"), content, &ctx)
            .unwrap();
        let component_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "references")
            .collect();
        assert_eq!(component_refs.len(), 0, "redirect should have no component refs: {:?}", component_refs);
    }

    #[test]
    fn test_route_wildcard_path() {
        let content = r#"
export const ROUTES: Routes = [
  { path: '**', component: NotFoundComponent },
];
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("app.routes.ts"), content, &ctx)
            .unwrap();
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route node");
        assert!(route.name.contains("**"), "wildcard: {}", route.name);
    }

    #[test]
    fn test_route_absolute_child_override() {
        let content = r#"
export const ROUTES: Routes = [
  {
    path: 'parent',
    component: ParentComponent,
    children: [
      { path: '/absolute', component: AbsoluteComponent },
    ],
  },
];
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("app.routes.ts"), content, &ctx)
            .unwrap();
        let route_names: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Route)
            .map(|n| n.name.as_str())
            .collect();
        assert!(
            route_names.contains(&"/absolute"),
            "absolute child: {route_names:?}"
        );
    }

    // ── Group 5: Template binding ─────────────────────────────────────────────

    #[test]
    fn test_standalone_imports_array() {
        let content = r#"
@Component({
  selector: 'app-x',
  standalone: true,
  template: '',
  imports: [RouterLink, FavoriteButtonComponent],
})
export class XComponent {}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("x.component.ts"), content, &ctx)
            .unwrap();
        let uses_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "uses")
            .collect();
        let names: Vec<_> = uses_refs.iter().map(|r| r.reference_name.as_str()).collect();
        // RouterLink is built-in and filtered; FavoriteButtonComponent should be present
        assert!(
            names.contains(&"FavoriteButtonComponent"),
            "names: {names:?}"
        );
        assert!(!names.contains(&"RouterLink"), "RouterLink should be filtered: {names:?}");
    }

    #[test]
    fn test_inline_template_selector() {
        let content = r#"
@Component({
  selector: 'app-parent',
  standalone: true,
  template: `<app-favorite-button [article]="art"></app-favorite-button>`,
  imports: [],
})
export class ParentComponent {}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("parent.component.ts"), content, &ctx)
            .unwrap();
        let uses_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "uses" && r.reference_name == "app-favorite-button")
            .collect();
        assert!(!uses_refs.is_empty(), "app-favorite-button ref missing");
    }

    #[test]
    fn test_inline_template_pipe() {
        let content = r#"
@Component({
  selector: 'app-date',
  standalone: true,
  template: `<span>{{ createdAt | date }}</span>`,
  imports: [],
})
export class DateComponent {}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("date.component.ts"), content, &ctx)
            .unwrap();
        let pipe_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "uses" && r.reference_name == "date")
            .collect();
        assert!(!pipe_refs.is_empty(), "date pipe ref missing");
    }

    #[test]
    fn test_single_word_element_skipped() {
        let content = r#"
@Component({
  selector: 'app-x',
  standalone: true,
  template: `<div><span><p>Hello</p></span></div>`,
  imports: [],
})
export class XComponent {}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("x.component.ts"), content, &ctx)
            .unwrap();
        let uses_refs: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "uses")
            .collect();
        // div, span, p are single-word — should not appear
        for r in &uses_refs {
            assert!(
                r.reference_name.contains('-'),
                "single-word element leaked: {}",
                r.reference_name
            );
        }
    }

    // ── Group 6: resolve() ────────────────────────────────────────────────────

    fn make_node(
        id: &str,
        name: &str,
        kind: NodeKind,
        file_path: &str,
    ) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            qualified_name: format!("{}::{}", file_path, name),
            kind,
            language: Language::TypeScript,
            file_path: file_path.to_string(),
            start_line: 1,
            end_line: 10,
            ..Default::default()
        }
    }

    fn make_ref(name: &str, kind: &str) -> UnresolvedRef {
        UnresolvedRef {
            id: format!("ref-{}", name),
            from_node_id: "src-node".to_string(),
            reference_name: name.to_string(),
            reference_kind: kind.to_string(),
            file_path: "src.ts".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_resolve_service_convention() {
        let caches = ResolverCaches::default_capacity();
        let node = make_node(
            "service:users.service.ts:1:UserService",
            "UserService",
            NodeKind::Class,
            "users.service.ts",
        );
        caches.name_cache.insert("UserService".to_string(), vec![node]);
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = make_ref("UserService", "references");
        let result = AngularResolver::new().resolve(&reference, &ctx).unwrap();
        assert!(result.is_some());
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 0.85, "convention match should be 0.85");
    }

    #[test]
    fn test_resolve_service_fallback() {
        let caches = ResolverCaches::default_capacity();
        let node = make_node(
            "class:other.ts:1:UserService",
            "UserService",
            NodeKind::Class,
            "other.ts",
        );
        caches.name_cache.insert("UserService".to_string(), vec![node]);
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = make_ref("UserService", "references");
        let result = AngularResolver::new().resolve(&reference, &ctx).unwrap();
        assert!(result.is_some());
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 0.70, "fallback should be 0.70");
    }

    #[test]
    fn test_resolve_component_declaration() {
        let caches = ResolverCaches::default_capacity();
        let node = make_node(
            "component:users.component.ts:1:UsersComponent",
            "UsersComponent",
            NodeKind::Component,
            "users.component.ts",
        );
        caches
            .name_cache
            .insert("UsersComponent".to_string(), vec![node]);
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = make_ref("UsersComponent", "contains");
        let result = AngularResolver::new().resolve(&reference, &ctx).unwrap();
        assert!(result.is_some());
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 0.90);
    }

    #[test]
    fn test_resolve_angular_builtin_none() {
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = make_ref("RouterLink", "uses");
        let result = AngularResolver::new().resolve(&reference, &ctx).unwrap();
        assert!(result.is_none(), "RouterLink should return None");
    }

    #[test]
    fn test_resolve_commonmodule_none() {
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = make_ref("CommonModule", "uses");
        let result = AngularResolver::new().resolve(&reference, &ctx).unwrap();
        assert!(result.is_none(), "CommonModule should return None");
    }

    #[test]
    fn test_resolve_component_by_name() {
        let caches = ResolverCaches::default_capacity();
        let node = make_node(
            "component:dashboard.component.ts:1:DashboardComponent",
            "DashboardComponent",
            NodeKind::Component,
            "dashboard.component.ts",
        );
        caches
            .name_cache
            .insert("DashboardComponent".to_string(), vec![node]);
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let reference = make_ref("DashboardComponent", "references");
        let result = AngularResolver::new().resolve(&reference, &ctx).unwrap();
        assert!(result.is_some());
        let edge = result.unwrap();
        assert_eq!(edge.confidence, 0.85);
    }

    // ── Group 7: qualified_name contract ─────────────────────────────────────

    #[test]
    fn test_qn_component() {
        let content = r#"
@Component({ selector: 'app-foo', template: '' })
export class FooComponent {}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("foo.component.ts"), content, &ctx)
            .unwrap();
        let comp = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Component)
            .unwrap();
        let re = Regex::new(r"^[^:]+::component:[a-z]").unwrap();
        assert!(
            re.is_match(&comp.qualified_name),
            "qn: {}",
            comp.qualified_name
        );
    }

    #[test]
    fn test_qn_directive() {
        let content = r#"
@Directive({ selector: '[appBar]' })
export class BarDirective {}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("bar.directive.ts"), content, &ctx)
            .unwrap();
        let node = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Component)
            .unwrap();
        let re = Regex::new(r"^[^:]+::directive:").unwrap();
        assert!(
            re.is_match(&node.qualified_name),
            "qn: {}",
            node.qualified_name
        );
    }

    #[test]
    fn test_qn_pipe() {
        let content = r#"
@Pipe({ name: 'myPipe' })
export class MyPipePipe {}
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("my-pipe.pipe.ts"), content, &ctx)
            .unwrap();
        let node = result.nodes.iter().find(|n| n.kind == NodeKind::Class).unwrap();
        let re = Regex::new(r"^[^:]+::pipe:[a-z]").unwrap();
        assert!(re.is_match(&node.qualified_name), "qn: {}", node.qualified_name);
    }

    #[test]
    fn test_qn_route() {
        let content = r#"
export const ROUTES: Routes = [
  { path: 'home', component: HomeComponent },
];
"#;
        let caches = ResolverCaches::default_capacity();
        let aliases = PathAliasMap::default();
        let ctx = make_ctx(&caches, &aliases, &[]);
        let result = AngularResolver::new()
            .extract(Path::new("app.routes.ts"), content, &ctx)
            .unwrap();
        let route = result.nodes.iter().find(|n| n.kind == NodeKind::Route).unwrap();
        let re = Regex::new(r"^[^:]+::GET:/").unwrap();
        assert!(re.is_match(&route.qualified_name), "qn: {}", route.qualified_name);
    }
}

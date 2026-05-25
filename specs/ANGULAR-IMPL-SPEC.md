# Angular Resolver — Consolidated Implementation Spec

**Status:** GREEN (one YELLOW note on empty-path child node emission — see §9)  
**Supersedes:** A-components-di.md, B-routing.md, C-corpus-template-bench.md  
**Target file:** `crates/codewiki-resolution/src/framework/angular.rs`  
**Registry change:** one line in `crates/codewiki-resolution/src/framework/mod.rs`  
**No extraction-crate changes for v1.**

---

## 1. Conflict Resolutions

### 1.1 Component node id / qualified_name — RESOLVED

Agent A proposed selector-keyed qualified names (`{file}::component:{selector}`).
Agent B's coordination note mentioned `component:{file}:{line}:{Name}` (class-name-keyed id). These are not truly in conflict — A defined the `qualified_name` format, B was sketching the node `id` format. Both are needed, and both use different fields.

**Decision: node is keyed by CLASS NAME in the `id`, selector lives in `qualified_name` AND `metadata`.**

Rationale: routing resolves by class name (`component: UsersComponent`); templates resolve by selector (`<app-foo>`). The node must be discoverable from both. `context.get_nodes_by_name` looks up by `node.name`, so `name` must be the class name. The `qualified_name` encodes the selector for uniqueness and lookup. The `metadata` JSON includes both for downstream consumers.

| Field | Value |
|---|---|
| `id` | `"component:{file_str}:{start_line}:{ClassName}"` |
| `name` | `"UsersComponent"` (class name — what `get_nodes_by_name` returns) |
| `qualified_name` | `"{file_str}::component:{selector}"` |
| `metadata` | `{"selector":"app-users","class":"UsersComponent","standalone":true,"inputs":[...],"outputs":[...]}` |

For directives, pipes, services, modules:

| Kind | `id` | `qualified_name` |
|---|---|---|
| `@Directive` | `"directive:{file}:{line}:{ClassName}"` | `"{file}::directive:{selector}"` |
| `@Pipe` | `"pipe:{file}:{line}:{ClassName}"` | `"{file}::pipe:{pipe_name}"` |
| `@Injectable` | `"service:{file}:{line}:{ClassName}"` | `"{file}::service:{ClassName}"` |
| `@NgModule` | `"module:{file}:{line}:{ClassName}"` | `"{file}::module:{ClassName}"` |
| `NodeKind::Route` | `"route:{file}:{line}:GET:{path}"` | `"{file}::GET:{path}"` |

`{file_str}` is `file_path.to_string_lossy()` with `\` normalised to `/` (Windows compat, same as all other resolvers).

### 1.2 Single `AngularResolver` struct — CONFIRMED

One struct. One registry line. B's routing helpers and C's template-binding helpers are `fn` items in `angular.rs`, called from within `AngularResolver::extract()`. No separate resolver structs.

### 1.3 `resolve()` returns `None` for Angular built-ins — CONFIRMED

`RouterLink`, `CommonModule`, `AsyncPipe`, `RouterModule`, `NgIf`, `NgFor`, `DatePipe` live in `node_modules`, which is not indexed. `context.get_nodes_by_name` returns an empty vec; `resolve()` returns `Ok(None)`. No panic, no error. This is the correct behaviour for all resolvers and requires no special case — the existing "if candidates is empty, return Ok(None)" flow handles it.

### 1.4 v1 scope — CONFIRMED

v1 = `.ts` files only. Covers: decorators, DI, routing, inline `template:` scanning, standalone `imports:[]` scanning. External `.html` template parsing is deferred to v1.1 (separate ticket). No changes to `codewiki-core/src/types.rs`, `language_detector.rs`, or any extraction-crate file.

### 1.5 No regex backreferences — CONFIRMED

All regexes are Rust `regex`-crate compatible (no backreferences, no lookahead). Nested structure uses `scan_utils::{read_args, read_object, read_bracket_array}`.

---

## 2. Node and Edge Format Table

### 2.1 Nodes emitted by `extract()`

| Decorator | `NodeKind` | `id` format | `qualified_name` |
|---|---|---|---|
| `@Component` | `Component` | `component:{file}:{line}:{ClassName}` | `{file}::component:{selector}` |
| `@Directive` | `Component` | `directive:{file}:{line}:{ClassName}` | `{file}::directive:{selector}` |
| `@Pipe` | `Class` | `pipe:{file}:{line}:{ClassName}` | `{file}::pipe:{name}` |
| `@Injectable` | `Class` | `service:{file}:{line}:{ClassName}` | `{file}::service:{ClassName}` |
| `@NgModule` | `Module` | `module:{file}:{line}:{ClassName}` | `{file}::module:{ClassName}` |
| Route object | `Route` | `route:{file}:{line}:GET:{path}` | `{file}::GET:{path}` |

### 2.2 `UnresolvedRef` shapes emitted by `extract()`

| Source | `reference_kind` | `reference_name` | Resolved by |
|---|---|---|---|
| Constructor param type | `"references"` | `"FooService"` | `resolve()` DI convention lookup |
| `inject(FooService)` | `"references"` | `"FooService"` | `resolve()` DI convention lookup |
| `@NgModule.declarations` | `"contains"` | `"AppComponent"` | `resolve()` Component name lookup |
| `@NgModule.providers` | `"uses"` | `"AuthService"` | `resolve()` DI convention lookup |
| `@NgModule.imports` (NgModule) | `"uses"` | `"MatButtonModule"` | `resolve()` Module name lookup |
| `@ViewChild`/`@ContentChild` | `"references"` | `"FooComponent"` | `resolve()` Component name lookup |
| Route `component:` | `"references"` | `"UsersComponent"` | `resolve()` Component name lookup |
| Route `loadComponent` export | `"references"` | `"SettingsComponent"` | `resolve()` Component name lookup |
| Route `loadChildren` / `loadComponent` import path | `"imports"` | `"./admin/admin.routes"` | existing import resolver |
| Route guard class | `"references"` | `"AuthGuard"` | `resolve()` DI convention lookup |
| Route resolver class | `"references"` | `"PostResolver"` | `resolve()` DI convention lookup |
| Standalone `imports:[]` entry | `"uses"` | `"FavoriteButtonComponent"` | `resolve()` Component/Module lookup |
| Inline template selector | `"uses"` | `"app-favorite-button"` | `resolve()` selector field lookup |
| Inline template pipe | `"uses"` | `"date"` | `resolve()` pipe name lookup |

---

## 3. `AngularResolver` Struct

```rust
pub struct AngularResolver {
    /// Cached result: is @angular/core version >= 19?
    /// Used to determine standalone-by-default semantics.
    /// Populated lazily on first detect() call.
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
```

`OnceLock<bool>` is `Send + Sync`. The struct satisfies the `FrameworkResolver: Send + Sync` bound.

---

## 4. All Six Trait Methods

### 4.1 `name()`

```rust
fn name(&self) -> &'static str { "angular" }
```

### 4.2 `languages()`

```rust
fn languages(&self) -> Option<&'static [Language]> {
    static LANGS: &[Language] = &[Language::TypeScript];
    Some(LANGS)
}
```

### 4.3 `detect()`

Short-circuit priority order:

1. **`package.json` has key `"@angular/core"`** in `dependencies` or `devDependencies`. Also extract version string here to populate `angular_version_gte_19` (parse semver major; `>= 19` sets true). Return `true`.
2. **`angular.json` exists at project root.** Call `context.file_exists("angular.json")`. Return `true`.
3. **Fallback:** Walk `context.known_files` for entries ending with `.component.ts`. Read each; return `true` on first that contains the byte string `@Component`.

The NestJS false-positive check is implicit: step 1 requires the exact key `"@angular/core"` (not `@nestjs/*`), so NestJS projects pass neither step 1 nor step 2.

### 4.4 `extract()`

Called once per `.ts` file. Algorithm:

```
1. Guard: return empty if file_path does not end with ".ts"
2. safe = strip_comments(content, CommentLang::TypeScript)
3. Phase A — Component/DI extraction:
   a. find_decorators(&safe, &["Component","Directive","Pipe","Injectable","NgModule"])
   b. For each hit:
      - read_args(safe, paren_index) → args_str
      - extract selector / name / providedIn / standalone from args_str
      - if @NgModule: read declarations/providers/imports arrays with read_bracket_array
      - emit Node (see §2.1 format)
      - emit UnresolvedRefs for DI, declarations, providers
      - scan imports:[] (standalone components) → "uses" UnresolvedRefs
   c. build_class_scopes(&safe) — derive class boundaries
   d. find constructor(  → read_args → parse typed params → "references" UnresolvedRefs
      (exclude built-in TS types: String, Number, Boolean, Object, Array, any,
       unknown, never, void, HTMLElement, ElementRef, ChangeDetectorRef, Renderer2)
   e. find inject( calls → "references" UnresolvedRefs (same exclusion list)
   f. scan @Input/@Output/@ViewChild/@ContentChild within class body
      - @Input/@Output: accumulate into metadata JSON
      - @ViewChild/@ContentChild: emit "references" UnresolvedRef
   g. For components with inline template:: scan for element selectors and pipe refs
      (see §6 inline template scanning)
4. Phase B — Routing extraction:
   a. Quick gate: skip if !safe.contains("Routes")
   b. find_routes_array_offsets(&safe) → Vec<usize> of bracket positions
   c. for each offset: read_bracket_array → parse_angular_routes_array → Vec<AngularRoute>
   d. For each AngularRoute: emit Route node + UnresolvedRefs (see §2.2)
5. Return combined FrameworkExtractionResult { nodes, edges: vec![], unresolved_refs }
```

### 4.5 `resolve()`

Dispatch table (evaluated in order; return first `Ok(Some(...))` match):

```
reference_kind == "references" || reference_kind == "uses":
  1. Check ANGULAR_BUILT_INS set — return Ok(None) immediately.
  2. Try DI convention lookup (PROVIDER_CONVENTIONS suffix table, same as NestJS).
     - NodeKind::Class candidates; prefer file_path contains convention substring.
     - confidence: 0.85 (convention match) or 0.70 (name-only match).
  3. If reference_kind == "contains": filter to NodeKind::Component, confidence 0.90.
  4. If name matches NodeKind::Component: confidence 0.85.
  5. If name matches NodeKind::Module: confidence 0.80.
  6. Pipe name lookup (NodeKind::Class in *.pipe.ts): confidence 0.75.
  7. InjectionToken lookup (NodeKind::Variable in *.tokens.ts / *.constants.ts): 0.75.
  8. return Ok(None)

reference_kind == "imports":
  return Ok(None)  // handled by existing import_resolver path
```

**Angular built-ins that always return `Ok(None)`:**

```rust
static ANGULAR_BUILT_INS: OnceLock<HashSet<&'static str>> = OnceLock::new();
// RouterLink, RouterModule, RouterOutlet, CommonModule, BrowserModule,
// AsyncPipe, NgIf, NgFor, NgSwitch, NgClass, NgStyle, NgTemplateOutlet,
// FormsModule, ReactiveFormsModule, HttpClientModule, DatePipe, LowerCasePipe,
// UpperCasePipe, DecimalPipe, CurrencyPipe, PercentPipe, SlicePipe, JsonPipe,
// KeyValuePipe, TitleCasePipe, I18nPluralPipe, I18nSelectPipe
```

### 4.6 `min_confidence()`

```rust
fn min_confidence(&self) -> f32 { 0.5 }  // inherits default
```

---

## 5. Helper Functions

All helpers are `fn` items in `angular.rs` (not methods on the struct, except where they need `self`). `pub(crate)` visibility where needed for tests; otherwise private.

### 5.1 Decorator helpers (Phase A)

```rust
// Re-use pattern from nestjs.rs:
fn find_decorators(safe: &str, names: &[&str]) -> Vec<DecoratorHit>
fn build_class_scopes(safe: &str) -> Vec<ClassScope>
fn scope_for<'a>(scopes: &'a [ClassScope], byte_pos: usize) -> Option<&'a ClassScope>
fn line_at(s: &str, byte_pos: usize) -> u32

// Angular-specific key extraction (applied to args string from read_args):
fn extract_selector(args: &str) -> Option<String>      // r#"selector\s*:\s*['"`]([^'"`]+)['"`]"#
fn extract_pipe_name(args: &str) -> Option<String>     // r#"\bname\s*:\s*['"`]([^'"`]+)['"`]"#
fn extract_provided_in(args: &str) -> Option<String>   // r#"providedIn\s*:\s*['"`]([^'"`]+)['"`]"#
fn extract_standalone(args: &str) -> Option<bool>      // r#"\bstandalone\s*:\s*(true|false)\b"#

// DI extraction:
fn extract_constructor_di_types(safe: &str, constructor_paren: usize) -> Vec<String>
fn extract_inject_calls(safe: &str) -> Vec<(usize, String)>  // (byte_offset, type_name)
fn extract_input_output_names(class_body: &str) -> (Vec<String>, Vec<String>)

// TS built-in type exclusion list (OnceLock<HashSet>):
fn ts_builtin_types() -> &'static HashSet<&'static str>
```

### 5.2 Routing helpers (Phase B)

```rust
// Entry point called from extract():
fn extract_angular_routes(
    safe: &str,
    file_str: &str,
    content: &str,  // original (for line_at computations)
) -> (Vec<Node>, Vec<UnresolvedRef>)

// Finding array start offsets (deduplicated):
fn find_routes_array_offsets(content: &str) -> Vec<usize>
// Patterns:
//   A: r"\bRouterModule\s*\.\s*(?:forRoot|forChild)\s*\(\s*\["
//   B: r"\bprovideRouter\s*\(\s*\["
//   C: r"\bconst\s+[A-Z_][A-Z0-9_]*\s*:\s*Routes\s*=\s*\["

// Path composition (identical to vue.rs compose_route_path):
fn compose_angular_route_path(parent: &str, child: &str) -> String

// Recursive parser:
fn parse_angular_routes_array(
    array_body: &str,
    prefix: &str,
    array_offset_in_file: usize,
    original: &str,
) -> Vec<AngularRoute>

fn parse_single_route_object(
    obj_body: &str,
    prefix: &str,
    obj_offset_in_file: usize,
    original: &str,
) -> Option<AngularRoute>

fn extract_guard_names(array_body: &str) -> Vec<String>
fn extract_resolver_names(obj_body: &str) -> Vec<String>

// Internal type (not public):
struct AngularRoute {
    full_path: String,
    line: u32,
    component_name: Option<String>,       // component: UsersComponent
    lazy_import_path: Option<String>,     // loadComponent/loadChildren import path
    lazy_export_name: Option<String>,     // loadComponent/loadChildren export name
    guards: Vec<String>,
    resolvers: Vec<String>,
    redirect_to: Option<String>,
}
```

### 5.3 Inline template helpers (Phase A step 4g)

```rust
// Extract inline template string from decorator args (already returned by read_args).
// Locate "template" key, then find backtick/quote-delimited value.
fn extract_inline_template(args: &str) -> Option<String>

// Scan template for component selector usages (multi-word kebab only):
// r"<([a-z][a-z0-9]*(?:-[a-z0-9]+)+)"  — captures e.g. "app-user-card", "mat-button"
fn scan_template_selectors(template: &str) -> Vec<String>

// Scan template for pipe references:
// r"\|\s*([a-zA-Z][a-zA-Z0-9]+)"
fn scan_template_pipes(template: &str) -> Vec<String>
```

---

## 6. The `resolve()` Pattern List (Complete)

```
PATTERN 1 — Angular built-in, always None
  condition: reference_name in ANGULAR_BUILT_INS
  action: return Ok(None)

PATTERN 2 — DI provider by suffix convention
  condition: reference_name ends with known suffix (Service/Store/Facade/Repository/
             Guard/Resolver/Interceptor/Validator/Factory/Handler)
  action:
    candidates = get_nodes_by_name(name).filter(kind == Class)
    target = prefer file_path contains convention-substring, else first
    confidence = 0.85 if convention match, 0.70 otherwise
    return Ok(Some(make_resolved_edge(...)))

PATTERN 3 — NgModule declarations (contains)
  condition: reference_kind == "contains"
  action:
    candidates = get_nodes_by_name(name).filter(kind == Component)
    return Ok(Some(..., confidence 0.90))

PATTERN 4 — Component by class name (routing + DI)
  condition: candidates include NodeKind::Component
  action: return Ok(Some(..., confidence 0.85))

PATTERN 5 — Module by class name
  condition: candidates include NodeKind::Module
  action: return Ok(Some(..., confidence 0.80))

PATTERN 6 — Pipe by name (uses reference to pipe)
  condition: candidates include NodeKind::Class in *.pipe.ts
  action: return Ok(Some(..., confidence 0.75))

PATTERN 7 — InjectionToken constant
  condition: candidates include NodeKind::Variable or NodeKind::Constant
             preferring file_path contains ".tokens." or ".constants."
  action: return Ok(Some(..., confidence 0.75))

PATTERN 8 — No match
  action: return Ok(None)
```

---

## 7. `detect()` Logic (Complete)

```rust
fn detect(&self, context: &ResolutionContext<'_>) -> bool {
    // Priority 1: package.json
    if let Some(raw) = context.read_file("package.json") {
        if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&raw) {
            for section in &["dependencies", "devDependencies"] {
                if let Some(obj) = pkg.get(section).and_then(|v| v.as_object()) {
                    if let Some(version_str) = obj.get("@angular/core").and_then(|v| v.as_str()) {
                        // Populate OnceLock with version check
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
        let _ = self.angular_version_gte_19.set(false); // unknown, assume < 19
        return true;
    }
    // Priority 3: file naming fallback
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

fn parse_angular_version_gte_19(version_str: &str) -> bool {
    // Strip leading non-digit characters (^, ~, >=, etc.)
    let s = version_str.trim_start_matches(|c: char| !c.is_ascii_digit());
    let major = s.split('.').next().and_then(|m| m.parse::<u32>().ok()).unwrap_or(0);
    major >= 19
}
```

---

## 8. v1 Scope Boundary

### In scope (v1 — this PR)

- `crates/codewiki-resolution/src/framework/angular.rs` (new file)
- `crates/codewiki-resolution/src/framework/mod.rs` (one `pub mod angular;` + one `Arc::new(angular::AngularResolver::new())`)
- All six trait methods + all helper functions defined in §4 and §5
- Inline `template:` scanning and standalone `imports:[]` scanning (Option 1 from C §3.2)
- Unit tests in `#[cfg(test)] mod tests` block at the bottom of `angular.rs`
- `scripts/check-angular-quality.sh` (from C §4.4)

### Deferred to v1.1 (separate ticket: "Angular HTML template extractor")

- `crates/codewiki-core/src/types.rs` — new `Language::AngularHtml` variant
- `crates/codewiki-extraction/src/language_detector.rs` — new `.component.html` match
- `crates/codewiki-extraction/src/languages/angular_html.rs` — new special extractor
- `crates/codewiki-extraction/src/languages/mod.rs` — new dispatch arm

No v1 code touches any of these files.

### Registry line (only change to mod.rs)

```rust
// In default_registry() vec![...]:
Arc::new(angular::AngularResolver::new()),
// Add at top of web frameworks section, before nestjs or after vue
```

And at the top of `mod.rs`:

```rust
pub mod angular;
```

---

## 9. Test and Acceptance Plan

### 9.1 Unit tests (in `angular.rs` #[cfg(test)] block)

All tests follow the pattern of `nestjs.rs`: construct a `ResolverCaches`, build a `ResolutionContext`, call `.extract()` or `.resolve()`, assert on `result.nodes` and `result.unresolved_refs`.

**Group 1 — `detect()`**

| Test name | Input | Assert |
|---|---|---|
| `test_detect_via_package_json` | `package.json` with `"@angular/core":"^18.0.0"` | returns `true` |
| `test_detect_version_gte19` | `package.json` with `"@angular/core":"^19.1.0"` | `is_standalone_by_default()` is `true` |
| `test_detect_via_angular_json` | `angular.json` present, no `package.json` | returns `true` |
| `test_detect_via_component_file` | `app.component.ts` containing `@Component` | returns `true` |
| `test_no_false_positive_nestjs` | `package.json` with only `"@nestjs/core"`, no `angular.json` | returns `false` |
| `test_no_false_positive_react` | `package.json` with `"react"` only | returns `false` |

**Group 2 — `extract()` component nodes**

| Test name | Key assertion |
|---|---|
| `test_extract_basic_component` | `node.name == "HelloComponent"`, `qualified_name` matches `"::component:app-hello"`, `id` starts with `"component:"` |
| `test_extract_standalone_component` | `metadata` contains `"standalone":true` and `"selector":"app-foo"` |
| `test_extract_directive` | `kind == NodeKind::Component`, `qualified_name` contains `"::directive:[appHighlight]"` |
| `test_extract_pipe` | `kind == NodeKind::Class`, `qualified_name` contains `"::pipe:truncate"` |
| `test_extract_injectable` | `kind == NodeKind::Class`, `qualified_name` contains `"::service:AuthService"` |
| `test_extract_ngmodule` | `kind == NodeKind::Module`, has unresolved_refs for each declaration |
| `test_extract_attribute_selector` | selector `[ngTabList]` captured with brackets intact |

**Group 3 — DI extraction**

| Test name | Key assertion |
|---|---|
| `test_constructor_di_single_service` | 1 unresolved_ref, `reference_name == "AuthService"`, `reference_kind == "references"` |
| `test_constructor_di_excludes_builtins` | `ElementRef`, `ChangeDetectorRef` not emitted |
| `test_inject_function_field` | `inject(FooService)` → unresolved_ref `reference_name == "FooService"` |
| `test_inject_function_generic` | `inject<string>(CONFIG_TOKEN)` → `"CONFIG_TOKEN"` |
| `test_ngmodule_declarations_refs` | 3 unresolved_refs (AppComponent contains, HeaderComponent contains, AuthService uses) |

**Group 4 — Routing extraction**

| Test name | Key assertion |
|---|---|
| `test_route_for_root` | 1 route node `name == "/users"`, 1 unresolved_ref for `"UsersComponent"` |
| `test_route_provide_router_deduplication` | same bracket found by pattern B and C only emitted once |
| `test_route_load_children` | 2 unresolved_refs: imports + references |
| `test_route_load_component` | 2 unresolved_refs: imports + references |
| `test_route_nested_children_path_composition` | `/users`, `/users` (empty child), `/users/:id` — all with distinct ids via line number |
| `test_route_guards_and_resolvers` | `AuthGuard`, `RoleGuard` refs emitted from route node |
| `test_route_redirect_no_component_ref` | 0 unresolved_refs |
| `test_route_wildcard_path` | route node `name == "/**"` |
| `test_route_absolute_child_override` | child node `name == "/absolute"` |

**Group 5 — Template binding**

| Test name | Key assertion |
|---|---|
| `test_standalone_imports_array` | `imports: [RouterLink, FavoriteButtonComponent]` → 2 "uses" refs |
| `test_inline_template_selector` | `<app-favorite-button>` → "uses" ref `reference_name == "app-favorite-button"` |
| `test_inline_template_pipe` | `{{ val \| date }}` → "uses" ref `reference_name == "date"` |
| `test_single_word_element_skipped` | `<div>`, `<span>` → no "uses" refs |

**Group 6 — `resolve()`**

| Test name | Key assertion |
|---|---|
| `test_resolve_service_convention` | `UserService` in `users.service.ts` → confidence 0.85 |
| `test_resolve_service_fallback` | `UserService` in `other.ts` → confidence 0.70 |
| `test_resolve_component_declaration` | `reference_kind == "contains"`, `UsersComponent` in cache → confidence 0.90 |
| `test_resolve_angular_builtin_none` | `RouterLink` → `Ok(None)` |
| `test_resolve_commonmodule_none` | `CommonModule` → `Ok(None)` |
| `test_resolve_component_by_name` | routing ref to `DashboardComponent` → `Ok(Some(...))`, confidence 0.85 |

**Group 7 — qualified_name contract**

```
test_qn_component: matches r"^[^:]+::component:[a-z]"
test_qn_directive:  matches r"^[^:]+::directive:"
test_qn_pipe:       matches r"^[^:]+::pipe:[a-z]"
test_qn_route:      matches r"^[^:]+::GET:/"
```

### 9.2 Integration / corpus acceptance (check-angular-quality.sh)

Run after `codewiki init` on each corpus. Thresholds from C §4.2:

| Corpus | Component nodes | Route nodes | DI UnresolvedRefs | Selector coverage |
|---|---|---|---|---|
| angular-realworld-example-app | ≥ 16 | ≥ 8 | ≥ 5 | ≥ 90% |
| ng-matero | ≥ 80 | ≥ 20 | ≥ 25 | ≥ 85% |
| angular/components | ≥ 800 | ≥ 6 | ≥ 20 | ≥ 80% |

Cross-resolver non-regression: 0 Component nodes and 0 Route nodes in `django`, `express`, `flask`, `vuecore` corpora.

Performance budget: `extract()` ≤ 0.5 ms/file average; full `codewiki init` for `angular/components` ≤ 9 s (3× baseline of 2.93 s).

### 9.3 MCP query-level acceptance (post-integration)

- `codewiki_search("app-article-preview")` → returns Component node in `article-preview.component.ts`
- `codewiki_callers("ArticlesService")` → returns Component nodes that inject it
- `codewiki_context("/profile/:username")` → returns Route node + reference to ProfileComponent
- `codewiki_impact("AuthService")` → includes Component nodes with DI edge to AuthService
- `codewiki_callees("AppModule")` → includes Component and Module nodes declared in AppModule

---

## 10. Remaining Conflicts

### YELLOW — Empty-path child route node emission (B §T-B-05)

Agent B's test T-B-05 asserts that an empty-path child (`path: ''`) emits a Route node with `name == prefix` (e.g. `/users`), resulting in two nodes with the same `name` but different `id`s (different line numbers). This is technically correct per the spec (empty-path children are valid layout routes in Angular), but it produces two Route nodes with the same navigation path, which may confuse graph consumers expecting unique paths.

**Decision:** Emit the node for empty-path children (B's call is correct — they are real Angular routes). Document in `metadata` as `"layout_route":true` to distinguish them. The duplicate name is acceptable; `id` uniqueness is guaranteed by the line number. This is a YELLOW note, not a blocking issue — implementers should be aware and may revisit if it causes downstream confusion.

---

## 11. Verdict

**GREEN** — no blocking conflicts remain.

The one YELLOW note (empty-path layout route node duplication) is a known, documented edge case that does not block implementation. All five consolidation points from the brief are resolved:

1. Component node id/qualified_name: class name in `id`, selector in `qualified_name`, both discoverable.
2. Single `AngularResolver` struct with `OnceLock<bool>`: confirmed, one registry line.
3. `resolve()` returns `Ok(None)` for Angular built-ins: confirmed via ANGULAR_BUILT_INS set.
4. v1 scope = `.ts` only, no extraction-crate changes: confirmed.
5. No regex backreferences, nested structures via scan_utils walkers: confirmed.

This document is the implementation contract. Implement `angular.rs` against it directly.

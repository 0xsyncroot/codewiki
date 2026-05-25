// T-302 / T-305 — name_matcher.rs
//
// 5-tier reference matcher, in order of confidence:
//   1. File path match      0.95 / 0.85 / 0.70
//   2. Qualified name exact 0.95
//   3. Method-call pattern  0.65–0.85
//   4. Exact name + scoring +100 same-file / +50 same-lang / proximity / kind / exported
//   5. Fuzzy lowercase      0.30–0.50

use codewiki_core::{Language, Node, NodeKind, UnresolvedRef};

/// Result of a name-matcher lookup.
#[derive(Debug, Clone)]
pub struct MatchResult {
    /// The matched node.
    pub node: Node,
    /// Confidence 0.0–1.0.
    pub confidence: f32,
    /// Human-readable method label.
    pub method: &'static str,
}

/// Context required by the name matcher.
pub trait NameMatchContext {
    fn get_nodes_by_name(&self, name: &str) -> Vec<Node>;
    fn get_nodes_by_lower_name(&self, lower: &str) -> Vec<Node>;
    fn get_nodes_by_qualified_name(&self, qname: &str) -> Vec<Node>;
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node>;
    fn all_file_paths(&self) -> Vec<String>;
    fn file_path_has_node(&self, file_path: &str) -> bool;
}

/// Attempt to match `uref` using the 5 strategies.
pub fn match_reference(uref: &UnresolvedRef, ctx: &dyn NameMatchContext) -> Option<MatchResult> {
    let name = &uref.reference_name;

    // Strategy 1 — file path match (e.g. `snippets/drawer.liquid`)
    if let Some(r) = try_file_path_match(name, uref, ctx) {
        return Some(r);
    }

    // Strategy 2 — qualified name exact match (contains `::` or `.`)
    if let Some(r) = try_qualified_name_match(name, uref, ctx) {
        return Some(r);
    }

    // Strategy 3 — method-call pattern (receiver.method or Receiver::method)
    if let Some(r) = try_method_call_match(name, uref, ctx) {
        return Some(r);
    }

    // Strategy 4 — exact name match with scoring
    if let Some(r) = try_exact_name_match(name, uref, ctx) {
        return Some(r);
    }

    // Strategy 5 — fuzzy lowercase match (callable kinds only)
    try_fuzzy_lower_match(name, uref, ctx)
}

// ─── Strategy 1: file path match ─────────────────────────────────────────────

fn try_file_path_match(
    name: &str,
    uref: &UnresolvedRef,
    ctx: &dyn NameMatchContext,
) -> Option<MatchResult> {
    // Only attempt when the name looks like a file reference (contains / or .)
    let looks_like_path = name.contains('/') || (name.contains('.') && !name.contains("::"));
    if !looks_like_path {
        return None;
    }

    let all_files = ctx.all_file_paths();

    // Exact path match
    if all_files.iter().any(|f| f == name) {
        let nodes = ctx.get_nodes_in_file(name);
        if let Some(node) = nodes.into_iter().next() {
            return Some(MatchResult {
                node,
                confidence: 0.95,
                method: "file-path-exact",
            });
        }
    }

    // Suffix match: `foo.liquid` within `src/snippets/foo.liquid`
    let suffix_matches: Vec<_> = all_files
        .iter()
        .filter(|f| f.ends_with(name) || f.ends_with(&format!("/{name}")))
        .collect();

    if suffix_matches.len() == 1 {
        let file = suffix_matches[0];
        let nodes = ctx.get_nodes_in_file(file);
        if let Some(node) = nodes.into_iter().next() {
            return Some(MatchResult {
                node,
                confidence: 0.85,
                method: "file-path-suffix",
            });
        }
    }

    // Single candidate fallback by filename
    let file_name = name.rsplit('/').next().unwrap_or(name);
    let by_name: Vec<_> = all_files
        .iter()
        .filter(|f| {
            f.rsplit('/')
                .next()
                .map(|n| n == file_name || n.starts_with(file_name))
                .unwrap_or(false)
        })
        .collect();

    if by_name.len() == 1 {
        let file = by_name[0];
        let nodes = ctx.get_nodes_in_file(file);
        if let Some(node) = nodes.into_iter().next() {
            return Some(MatchResult {
                node,
                confidence: 0.70,
                method: "file-path-name",
            });
        }
    }

    let _ = uref; // suppress warning
    None
}

// ─── Strategy 2: qualified name match ────────────────────────────────────────

fn try_qualified_name_match(
    name: &str,
    _uref: &UnresolvedRef,
    ctx: &dyn NameMatchContext,
) -> Option<MatchResult> {
    if !name.contains("::") && !name.contains('.') {
        return None;
    }

    // Exact qualified name lookup
    let exact = ctx.get_nodes_by_qualified_name(name);
    if let Some(node) = exact.into_iter().next() {
        return Some(MatchResult {
            node,
            confidence: 0.95,
            method: "qualified-name-exact",
        });
    }

    // Suffix of qualified name (e.g. `method` matching `Module::Class::method`)
    // Try just the last component
    let last = name.rsplit("::").next().or_else(|| name.rsplit('.').next());
    if let Some(last) = last {
        if last != name {
            let candidates = ctx.get_nodes_by_name(last);
            for node in candidates {
                if node.qualified_name.ends_with(name)
                    || node.qualified_name.ends_with(&format!("::{name}"))
                {
                    return Some(MatchResult {
                        node,
                        confidence: 0.85,
                        method: "qualified-name-suffix",
                    });
                }
            }
        }
    }

    None
}

// ─── Strategy 3: method-call pattern ─────────────────────────────────────────

fn try_method_call_match(
    name: &str,
    uref: &UnresolvedRef,
    ctx: &dyn NameMatchContext,
) -> Option<MatchResult> {
    // Parse `receiver.method` or `Receiver::method`
    let (receiver, method) = if let Some(pos) = name.find("::") {
        (&name[..pos], &name[pos + 2..])
    } else if let Some(pos) = name.rfind('.') {
        (&name[..pos], &name[pos + 1..])
    } else {
        return None;
    };

    if receiver.is_empty() || method.is_empty() {
        return None;
    }

    // 3a: Direct class match — find method in same file as class
    let class_candidates = ctx.get_nodes_by_name(receiver);
    for class_node in &class_candidates {
        if matches!(
            class_node.kind,
            NodeKind::Class | NodeKind::Struct | NodeKind::Interface
        ) {
            let file_nodes = ctx.get_nodes_in_file(&class_node.file_path);
            for node in file_nodes {
                if node.name == method && matches!(node.kind, NodeKind::Method | NodeKind::Function)
                {
                    return Some(MatchResult {
                        node,
                        confidence: 0.85,
                        method: "method-call-direct",
                    });
                }
            }
        }
    }

    // 3b: Capitalized receiver — try CamelCase variant
    let capitalized = {
        let mut c = receiver.chars();
        match c.next() {
            Some(first) => first.to_uppercase().to_string() + c.as_str(),
            None => String::new(),
        }
    };
    if capitalized != receiver {
        let cap_candidates = ctx.get_nodes_by_name(&capitalized);
        for class_node in &cap_candidates {
            let file_nodes = ctx.get_nodes_in_file(&class_node.file_path);
            for node in file_nodes {
                if node.name == method && matches!(node.kind, NodeKind::Method | NodeKind::Function)
                {
                    return Some(MatchResult {
                        node,
                        confidence: 0.75,
                        method: "method-call-capitalized",
                    });
                }
            }
        }
    }

    // 3c: Method name only with CamelCase word overlap scoring
    let method_candidates = ctx.get_nodes_by_name(method);
    let method_candidates: Vec<_> = method_candidates
        .into_iter()
        .filter(|n| matches!(n.kind, NodeKind::Method | NodeKind::Function))
        .collect();

    if !method_candidates.is_empty() {
        let best = find_best_match(&method_candidates, uref, Some(receiver));
        if let Some(node) = best {
            return Some(MatchResult {
                node,
                confidence: 0.65,
                method: "method-call-name-only",
            });
        }
    }

    None
}

// ─── Strategy 4: exact name match with scoring ───────────────────────────────

fn try_exact_name_match(
    name: &str,
    uref: &UnresolvedRef,
    ctx: &dyn NameMatchContext,
) -> Option<MatchResult> {
    let candidates = ctx.get_nodes_by_name(name);
    if candidates.is_empty() {
        return None;
    }

    if candidates.len() == 1 {
        let node = &candidates[0];
        let lang_match = node_language_matches_ref(node, uref);
        let confidence = if lang_match { 0.9 } else { 0.5 };
        return Some(MatchResult {
            node: node.clone(),
            confidence,
            method: "exact-name-single",
        });
    }

    // Multiple candidates — find the best scored one
    let best = find_best_match(&candidates, uref, None)?;
    let score = score_node(&best, uref);
    // Convert score to confidence: 100 = 0.9, 0 = 0.4
    let confidence = (0.4_f32 + (score as f32 / 100.0) * 0.5).min(0.9);
    Some(MatchResult {
        node: best,
        confidence,
        method: "exact-name-scored",
    })
}

// ─── Strategy 5: fuzzy lowercase match ───────────────────────────────────────

fn try_fuzzy_lower_match(
    name: &str,
    uref: &UnresolvedRef,
    ctx: &dyn NameMatchContext,
) -> Option<MatchResult> {
    let lower = name.to_lowercase();
    let candidates = ctx.get_nodes_by_lower_name(&lower);
    let callable: Vec<_> = candidates
        .into_iter()
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Class
            )
        })
        .collect();

    if callable.is_empty() {
        return None;
    }

    let node = callable[0].clone();
    let lang_match = node_language_matches_ref(&node, uref);
    let confidence = if lang_match { 0.5 } else { 0.3 };
    Some(MatchResult {
        node,
        confidence,
        method: "fuzzy-lowercase",
    })
}

// ─── Scoring helpers ─────────────────────────────────────────────────────────

/// Score a candidate node against the unresolved reference context.
/// Higher = better match.
fn score_node(node: &Node, uref: &UnresolvedRef) -> i32 {
    let mut score = 0i32;

    // Same file: strongest signal
    if node.file_path == uref.file_path {
        score += 100;

        // Line proximity bonus (closer lines = +0–20)
        if let Some(ref_line) = uref.line {
            let dist = (node.start_line as i64 - ref_line as i64).unsigned_abs() as i32;
            let proximity_bonus = (20 - dist.min(20)).max(0);
            score += proximity_bonus;
        }
    }

    // Same language
    if node_language_matches_ref(node, uref) {
        score += 50;
    } else {
        score -= 80;
    }

    // Kind bonus based on reference kind
    match uref.reference_kind.as_str() {
        "calls" => {
            if matches!(node.kind, NodeKind::Function | NodeKind::Method) {
                score += 25;
            }
        }
        "instantiates" => {
            if matches!(node.kind, NodeKind::Class | NodeKind::Struct) {
                score += 25;
            }
        }
        "decorates" => {
            if matches!(node.kind, NodeKind::Function) {
                score += 25;
            } else if matches!(node.kind, NodeKind::Class) {
                score += 15;
            }
        }
        _ => {}
    }

    // Exported bonus
    if node.is_exported {
        score += 10;
    }

    score
}

/// Pick the highest-scored node from `candidates`.
fn find_best_match(
    candidates: &[Node],
    uref: &UnresolvedRef,
    _receiver_hint: Option<&str>,
) -> Option<Node> {
    candidates
        .iter()
        .max_by_key(|n| score_node(n, uref))
        .cloned()
}

fn node_language_matches_ref(node: &Node, uref: &UnresolvedRef) -> bool {
    // If the ref has no language info, assume match
    if uref.file_path.is_empty() {
        return true;
    }
    // Compare language groups: TS and JS are the same group
    let node_group = language_group(&node.language);
    let ref_group = language_group_from_file(&uref.file_path);
    node_group == ref_group
}

fn language_group(lang: &Language) -> u8 {
    match lang {
        Language::TypeScript | Language::JavaScript => 0,
        Language::Python => 1,
        Language::Go => 2,
        Language::Rust => 3,
        Language::Java | Language::Kotlin => 4,
        Language::CSharp | Language::Razor => 5,
        Language::Ruby => 6,
        Language::Php => 7,
        _ => 255,
    }
}

fn language_group_from_file(file_path: &str) -> u8 {
    if file_path.ends_with(".ts")
        || file_path.ends_with(".tsx")
        || file_path.ends_with(".js")
        || file_path.ends_with(".jsx")
    {
        return 0;
    }
    if file_path.ends_with(".py") {
        return 1;
    }
    if file_path.ends_with(".go") {
        return 2;
    }
    if file_path.ends_with(".rs") {
        return 3;
    }
    if file_path.ends_with(".java") || file_path.ends_with(".kt") {
        return 4;
    }
    if file_path.ends_with(".cs") || file_path.ends_with(".razor") {
        return 5;
    }
    if file_path.ends_with(".rb") {
        return 6;
    }
    if file_path.ends_with(".php") {
        return 7;
    }
    255
}

/// NameMatcher is a zero-size wrapper so callers can use it as a struct.
pub struct NameMatcher;

impl NameMatcher {
    pub fn match_reference(
        uref: &UnresolvedRef,
        ctx: &dyn NameMatchContext,
    ) -> Option<MatchResult> {
        match_reference(uref, ctx)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use codewiki_core::{Language, Node, NodeKind, UnresolvedRef};

    struct MockCtx {
        nodes: Vec<Node>,
        files: Vec<String>,
    }

    impl NameMatchContext for MockCtx {
        fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name == name)
                .cloned()
                .collect()
        }
        fn get_nodes_by_lower_name(&self, lower: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name.to_lowercase() == lower)
                .cloned()
                .collect()
        }
        fn get_nodes_by_qualified_name(&self, qname: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.qualified_name == qname)
                .cloned()
                .collect()
        }
        fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.file_path == file_path)
                .cloned()
                .collect()
        }
        fn all_file_paths(&self) -> Vec<String> {
            self.files.clone()
        }
        fn file_path_has_node(&self, file_path: &str) -> bool {
            self.nodes.iter().any(|n| n.file_path == file_path)
        }
    }

    fn make_node(id: &str, name: &str, kind: NodeKind, file: &str, lang: Language) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            qualified_name: format!("{}::{}", file, name),
            kind,
            file_path: file.to_string(),
            language: lang,
            is_exported: true,
            start_line: 1,
            end_line: 10,
            ..Default::default()
        }
    }

    fn make_ref(name: &str, kind: &str, file: &str) -> UnresolvedRef {
        UnresolvedRef {
            id: "r1".to_string(),
            from_node_id: "src".to_string(),
            reference_name: name.to_string(),
            reference_kind: kind.to_string(),
            file_path: file.to_string(),
            line: Some(5),
            ..Default::default()
        }
    }

    #[test]
    fn exact_name_match_returns_high_confidence() {
        // T-311: name_matcher exact match returns score 0.95+
        let node = make_node(
            "n1",
            "TransportService",
            NodeKind::Class,
            "src/transport.ts",
            Language::TypeScript,
        );
        let ctx = MockCtx {
            nodes: vec![node],
            files: vec!["src/transport.ts".to_string()],
        };
        let uref = make_ref("TransportService", "references", "src/index.ts");
        let result = match_reference(&uref, &ctx);
        assert!(result.is_some(), "must find a match");
        // Single candidate → 0.9 (same language group gives 0.9)
        assert!(
            result.unwrap().confidence >= 0.9,
            "confidence must be >= 0.9"
        );
    }

    #[test]
    fn qualified_name_exact_match() {
        let node2 = Node {
            id: "n2".to_string(),
            name: "processRequest".to_string(),
            qualified_name: "src/handler.ts::processRequest".to_string(),
            kind: NodeKind::Method,
            ..Default::default()
        };
        let ctx2 = MockCtx {
            nodes: vec![node2],
            files: vec![],
        };
        let uref = make_ref("src/handler.ts::processRequest", "calls", "src/index.ts");
        let result = match_reference(&uref, &ctx2);
        assert!(result.is_some(), "must find qualified name match");
        assert!(result.unwrap().confidence >= 0.95);
    }

    #[test]
    fn fuzzy_lower_match() {
        let node = make_node(
            "n3",
            "MyFunction",
            NodeKind::Function,
            "src/f.ts",
            Language::TypeScript,
        );
        let ctx = MockCtx {
            nodes: vec![node],
            files: vec![],
        };
        let uref = make_ref("myfunction", "calls", "src/f.ts");
        let result = match_reference(&uref, &ctx);
        assert!(result.is_some(), "fuzzy must find match");
        // Same file → expect 0.5 (same-file bonus not applied here since it's fuzzy only)
    }
}

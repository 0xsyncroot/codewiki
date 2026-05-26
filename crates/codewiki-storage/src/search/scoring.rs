/// Search scoring functions.
/// Port of research-source/src/search/query-utils.ts.
use codewiki_core::NodeKind;
use rusqlite::Connection;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Query-time IDF (FIX 2 — context multi-term coverage ranking)
// ---------------------------------------------------------------------------

/// Per-term inverse document frequency computed at query time.
///
/// For each term we count how many nodes match it via FTS (document frequency,
/// `df`) and compute `idf = ln(total / (df + 1)) + 1`. Rare terms (low `df`)
/// get a higher IDF so the coverage boost rewards a candidate that matches a
/// rare *semantic anchor* term more than one matching only a high-frequency
/// term. The `+1` smoothing keeps the IDF positive and finite even when a term
/// matches every node or no node.
#[derive(Debug, Clone, Default)]
pub struct TermIdf {
    /// Map from lowercased term → IDF weight (always >= ~1.0).
    weights: HashMap<String, f32>,
}

impl TermIdf {
    /// IDF weight for a term (case-insensitive). Unknown terms get a neutral
    /// weight of 1.0 so they still contribute one unit of coverage.
    pub fn weight(&self, term: &str) -> f32 {
        self.weights
            .get(&term.to_lowercase())
            .copied()
            .unwrap_or(1.0)
    }

    /// Number of terms with a recorded weight.
    pub fn len(&self) -> usize {
        self.weights.len()
    }

    /// True when no term weights were computed.
    pub fn is_empty(&self) -> bool {
        self.weights.is_empty()
    }
}

/// FTS5 MATCH string for a single term (prefix match), mirroring the per-term
/// shape `fts_query_string` produces. Strips FTS operator characters so a raw
/// query term can't inject syntax. Returns `None` when nothing usable remains.
fn term_fts_match(term: &str) -> Option<String> {
    let cleaned: String = term
        .replace("::", " ")
        .replace(|c: char| "\"*():^".contains(c), "");
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || matches!(cleaned, "AND" | "OR" | "NOT" | "NEAR") {
        return None;
    }
    Some(format!("\"{}\"*", cleaned))
}

/// Compute query-time IDF for each distinct term against the indexed corpus.
///
/// `total` is the node count; each term's `df` is the number of nodes matching
/// it via FTS prefix search. Best-effort: a term whose FTS count errors out is
/// recorded with `df = 0` (max IDF) rather than aborting the whole computation.
///
/// Callers should cache the result for the lifetime of a single query — it runs
/// one cheap `COUNT(*)` per distinct term.
pub fn compute_term_idf(conn: &Connection, terms: &[String]) -> TermIdf {
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
        .unwrap_or(0);
    let total_f = (total.max(0) as f32).max(1.0);

    let mut weights: HashMap<String, f32> = HashMap::new();
    for term in terms {
        let key = term.to_lowercase();
        if weights.contains_key(&key) {
            continue;
        }
        let df = match term_fts_match(&key) {
            Some(m) => fts_match_count(conn, &m),
            None => 0,
        };
        // idf = ln(total / (df + 1)) + 1  — always >= ~1.0 with the +1 floor.
        let idf = (total_f / (df as f32 + 1.0)).ln() + 1.0;
        weights.insert(key, idf.max(1.0));
    }
    TermIdf { weights }
}

/// Count nodes matching an FTS MATCH string. Returns 0 on any error so IDF
/// degrades gracefully to "rare term" rather than failing the query.
fn fts_match_count(conn: &Connection, fts_match: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM nodes_fts WHERE nodes_fts MATCH ?1",
        [fts_match],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// High-value node kinds — ported from TS context/index.ts lines 158-161
// ---------------------------------------------------------------------------

/// Node kinds that provide high information value in context results.
/// Imports/exports are excluded because they have near-zero information density.
pub const HIGH_VALUE_NODE_KINDS: &[&str] = &[
    "function",
    "method",
    "class",
    "interface",
    "type_alias",
    "struct",
    "trait",
    "component",
    "route",
    "variable",
    "constant",
    "enum",
    "module",
    "namespace",
];

// ---------------------------------------------------------------------------
// extract_symbols_from_query — port of TS context/index.ts:43-132
// ---------------------------------------------------------------------------

/// Extract potential symbol names from a natural-language query.
/// Runs 6 regex passes and filters a ~120-word stopword set.
/// Port of `extractSymbolsFromQuery` from TypeScript source.
pub fn extract_symbols_from_query(query: &str) -> Vec<String> {
    use regex::Regex;
    use std::collections::HashSet;
    use std::sync::OnceLock;

    static CAMEL_RE: OnceLock<Regex> = OnceLock::new();
    static SNAKE_RE: OnceLock<Regex> = OnceLock::new();
    static SCREAMING_RE: OnceLock<Regex> = OnceLock::new();
    static ACRONYM_RE: OnceLock<Regex> = OnceLock::new();
    static DOT_RE: OnceLock<Regex> = OnceLock::new();
    static LOWER_RE: OnceLock<Regex> = OnceLock::new();

    let camel_re = CAMEL_RE.get_or_init(|| {
        Regex::new(r"\b([A-Z][a-z]+(?:[A-Z][a-z]*)*|[a-z]+(?:[A-Z][a-z]*)+)\b").unwrap()
    });
    let snake_re =
        SNAKE_RE.get_or_init(|| Regex::new(r"(?i)\b([a-z][a-z0-9]*(?:_[a-z0-9]+)+)\b").unwrap());
    let screaming_re =
        SCREAMING_RE.get_or_init(|| Regex::new(r"\b([A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+)\b").unwrap());
    let acronym_re = ACRONYM_RE.get_or_init(|| Regex::new(r"\b([A-Z]{2,})\b").unwrap());
    let dot_re = DOT_RE.get_or_init(|| {
        Regex::new(r"\b([a-zA-Z][a-zA-Z0-9]*(?:\.[a-zA-Z][a-zA-Z0-9]*)+)\b").unwrap()
    });
    let lower_re = LOWER_RE.get_or_init(|| Regex::new(r"\b([a-z][a-z0-9]{2,})\b").unwrap());

    let mut symbols: HashSet<String> = HashSet::new();

    // CamelCase (len >= 2)
    for cap in camel_re.captures_iter(query) {
        if let Some(m) = cap.get(1) {
            let s = m.as_str();
            if s.len() >= 2 {
                symbols.insert(s.to_string());
            }
        }
    }

    // snake_case (len >= 3)
    for cap in snake_re.captures_iter(query) {
        if let Some(m) = cap.get(1) {
            let s = m.as_str();
            if s.len() >= 3 {
                symbols.insert(s.to_lowercase());
            }
        }
    }

    // SCREAMING_SNAKE
    for cap in screaming_re.captures_iter(query) {
        if let Some(m) = cap.get(1) {
            symbols.insert(m.as_str().to_string());
        }
    }

    // ALL_CAPS acronyms (2+)
    for cap in acronym_re.captures_iter(query) {
        if let Some(m) = cap.get(1) {
            symbols.insert(m.as_str().to_string());
        }
    }

    // dot.notation: add full path AND each part >= 2
    for cap in dot_re.captures_iter(query) {
        if let Some(m) = cap.get(1) {
            let full = m.as_str();
            symbols.insert(full.to_string());
            for part in full.split('.') {
                if part.len() >= 2 {
                    symbols.insert(part.to_string());
                }
            }
        }
    }

    // plain lowercase (3+)
    for cap in lower_re.captures_iter(query) {
        if let Some(m) = cap.get(1) {
            symbols.insert(m.as_str().to_string());
        }
    }

    // Filter out common English words that aren't likely symbol names.
    // Exact port of TS commonWords set (lines 104-129).
    let common_words: HashSet<&str> = [
        "the",
        "and",
        "for",
        "with",
        "from",
        "this",
        "that",
        "have",
        "been",
        "will",
        "would",
        "could",
        "should",
        "does",
        "done",
        "make",
        "made",
        "use",
        "used",
        "using",
        "work",
        "works",
        "find",
        "found",
        "show",
        "call",
        "called",
        "calling",
        "get",
        "set",
        "add",
        "all",
        "any",
        "how",
        "what",
        "when",
        "where",
        "which",
        "who",
        "why",
        "not",
        "but",
        "are",
        "was",
        "were",
        "has",
        "had",
        "its",
        "can",
        "did",
        "may",
        "also",
        "into",
        "than",
        "then",
        "them",
        "each",
        "other",
        "some",
        "such",
        "only",
        "same",
        "about",
        "after",
        "before",
        "between",
        "through",
        "during",
        "without",
        "again",
        "further",
        "once",
        "here",
        "there",
        "both",
        "just",
        "more",
        "most",
        "very",
        "being",
        "having",
        "doing",
        "system",
        "need",
        "needs",
        "want",
        "wants",
        "like",
        "look",
        "change",
        "changes",
        "changed",
        "changing",
        // Common English nouns/verbs that match thousands of unrelated code symbols
        "layer",
        "handle",
        "handles",
        "handling",
        "incoming",
        "outgoing",
        "data",
        "flow",
        "flows",
        "level",
        "levels",
        "request",
        "requests",
        "response",
        "responses",
        "implement",
        "implements",
        "implementation",
        "interface",
        "interfaces",
        "class",
        "classes",
        "method",
        "methods",
        "trigger",
        "triggers",
        "affected",
        "affect",
        "affects",
        "else",
        "code",
        "failing",
        "failed",
        "silently",
        "decide",
        "decides",
        "return",
        "returns",
        "returned",
        "take",
        "takes",
        "taken",
        "check",
        "checks",
        "checked",
        "create",
        "creates",
        "created",
        "read",
        "reads",
        "write",
        "writes",
        "written",
        "start",
        "starts",
        "stop",
        "stops",
        "run",
        "runs",
        "running",
    ]
    .iter()
    .copied()
    .collect();

    symbols
        .into_iter()
        .filter(|s| !common_words.contains(s.to_lowercase().as_str()))
        .collect()
}

// ---------------------------------------------------------------------------
// get_stem_variants — port of TS getStemVariants (context/index.ts:346 area)
// ---------------------------------------------------------------------------

/// Return simple suffix-strip / swap variants of a symbol.
/// Port of `getStemVariants` from TypeScript.
pub fn get_stem_variants(sym: &str) -> Vec<String> {
    let lower = sym.to_lowercase();
    let mut variants = Vec::new();

    // Special case: caching → cache
    if lower.ends_with("caching") {
        variants.push(format!("{}cache", &lower[..lower.len() - 7]));
    }

    // -ing suffix
    if lower.ends_with("ing") && lower.len() > 4 {
        variants.push(lower[..lower.len() - 3].to_string()); // e.g. running → runn (not ideal but matches TS)
                                                             // also try dropping 'ing' and adding 'e': running → run (handled by -ing rule)
    }

    // -s suffix
    if lower.ends_with('s') && lower.len() > 3 {
        variants.push(lower[..lower.len() - 1].to_string());
    }

    // -ed suffix
    if lower.ends_with("ed") && lower.len() > 4 {
        variants.push(lower[..lower.len() - 2].to_string());
        variants.push(format!("{}e", &lower[..lower.len() - 2])); // e.g. cached → cache
    }

    variants
}

/// Kind-based bonus for search ranking.
pub fn kind_bonus(kind: &NodeKind) -> i32 {
    let kind_str = serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    match kind_str.as_str() {
        "function" | "method" => 10,
        "interface" | "trait" | "route" | "protocol" => 9,
        "class" | "component" => 8,
        "type_alias" | "struct" => 6,
        "enum" => 5,
        "module" | "namespace" => 4,
        "constant" | "enum_member" | "property" | "field" => 3,
        "variable" => 2,
        "import" | "export" => 1,
        "parameter" | "file" => 0,
        _ => 0,
    }
}

/// Name-match bonus.
pub fn name_match_bonus(node_name: &str, query: &str) -> i32 {
    let name_lower = node_name.to_lowercase();
    let query_lower_compact = query.replace(char::is_whitespace, "").to_lowercase();

    // Exact match
    if name_lower == query_lower_compact {
        return 80;
    }

    // Exact match on a query token
    let query_tokens: Vec<String> = query
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .filter(|t| t.len() >= 2)
        .collect();
    if query_tokens.len() > 1 && query_tokens.contains(&name_lower) {
        return 60;
    }

    // Name starts with query
    if name_lower.starts_with(&query_lower_compact) {
        let ratio = query_lower_compact.len() as f64 / name_lower.len() as f64;
        return (10.0 + 30.0 * ratio).round() as i32;
    }

    // All camelCase-split terms appear in name
    let raw_terms: Vec<String> = query
        .replace(['_', '.', '-'], " ")
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .filter(|t| t.len() >= 2)
        .collect();
    if raw_terms.len() > 1 && raw_terms.iter().all(|t| name_lower.contains(t.as_str())) {
        return 15;
    }

    // Name contains query as substring
    if name_lower.contains(&query_lower_compact) {
        return 10;
    }

    0
}

/// Score relevance of a file path to a query.
pub fn score_path_relevance(file_path: &str, query: &str) -> i32 {
    let terms = extract_search_terms(query);
    if terms.is_empty() {
        return 0;
    }

    let path_lower = file_path.to_lowercase();
    let file_name = std::path::Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    let dir_name = std::path::Path::new(file_path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .to_lowercase();

    let mut score: i32 = 0;
    for term in &terms {
        if file_name.contains(term.as_str()) {
            score += 10;
        } else if dir_name.contains(term.as_str()) {
            score += 5;
        } else if path_lower.contains(term.as_str()) {
            score += 3;
        }
    }

    // Penalize test files unless query is about tests
    let query_lower = query.to_lowercase();
    let is_test_query = query_lower.contains("test") || query_lower.contains("spec");
    if !is_test_query && is_test_file(file_path) {
        score -= 15;
    }

    score
}

pub fn extract_search_terms(query: &str) -> Vec<String> {
    use std::collections::HashSet;
    let mut tokens: HashSet<String> = HashSet::new();

    // CamelCase split
    let camel_split = query.chars().fold(String::new(), |mut acc, c| {
        if c.is_uppercase() && !acc.is_empty() {
            acc.push(' ');
        }
        acc.push(c);
        acc
    });
    let normalised = camel_split.replace(['_', '.'], " ");

    for word in normalised.split_whitespace() {
        let lower = word.to_lowercase();
        if lower.len() >= 3 {
            tokens.insert(lower);
        }
    }
    tokens.into_iter().collect()
}

pub fn is_test_file(file_path: &str) -> bool {
    let lower = file_path.to_lowercase();
    let file_name = std::path::Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();

    file_name.starts_with("test_")
        || file_name.starts_with("test.")
        || file_name.contains(".test.")
        || file_name.contains(".spec.")
        || file_name.ends_with("_test.go")
        || lower.contains("/__tests__/")
        || lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.contains("/spec/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewiki_core::NodeKind;

    #[test]
    fn kind_bonus_values() {
        assert_eq!(kind_bonus(&NodeKind::Function), 10);
        assert_eq!(kind_bonus(&NodeKind::Interface), 9);
        assert_eq!(kind_bonus(&NodeKind::File), 0);
    }

    #[test]
    fn name_match_exact() {
        assert_eq!(name_match_bonus("TransportService", "TransportService"), 80);
    }

    #[test]
    fn name_match_prefix() {
        let bonus = name_match_bonus("TransportFactory", "Transport");
        assert!(bonus > 10 && bonus <= 40, "got {}", bonus);
    }

    #[test]
    fn path_relevance_test_penalty() {
        let score = score_path_relevance("src/__tests__/foo.test.ts", "user");
        // Test penalty should reduce score
        assert!(score < 0, "expected negative for test file, got {}", score);
    }

    // -------------------------------------------------------------------------
    // extract_symbols_from_query tests
    // -------------------------------------------------------------------------

    #[test]
    fn extract_symbols_filters_stopwords() {
        // "how", "does", "the" are stopwords; "loop" is NOT a stopword — should be kept
        let symbols = extract_symbols_from_query("how does the loop work");
        assert!(
            !symbols.contains(&"how".to_string()),
            "stopword 'how' should be filtered"
        );
        assert!(
            !symbols.contains(&"does".to_string()),
            "stopword 'does' should be filtered"
        );
        assert!(
            !symbols.contains(&"the".to_string()),
            "stopword 'the' should be filtered"
        );
        assert!(
            !symbols.contains(&"work".to_string()),
            "stopword 'work' should be filtered"
        );
        // "loop" is not a stopword (len >= 3, lowercase)
        assert!(
            symbols.contains(&"loop".to_string()),
            "non-stopword 'loop' should survive"
        );
    }

    #[test]
    fn extract_symbols_filters_batch_is_not_a_stopword() {
        // "batch" is not in commonWords — should survive
        let symbols = extract_symbols_from_query("how does the batch loop avoid infinite loops");
        assert!(
            symbols.contains(&"batch".to_string()),
            "'batch' should be extracted"
        );
        assert!(!symbols.contains(&"how".to_string()), "stopword filtered");
    }

    #[test]
    fn extract_symbols_camel_case() {
        let symbols = extract_symbols_from_query("use the BatchProcessor to handle items");
        assert!(
            symbols.contains(&"BatchProcessor".to_string()),
            "CamelCase extracted"
        );
    }

    #[test]
    fn extract_symbols_snake_case() {
        let symbols = extract_symbols_from_query("call run_until_empty to drain");
        assert!(
            symbols.contains(&"run_until_empty".to_string()),
            "snake_case extracted"
        );
    }

    #[test]
    fn extract_symbols_acronym() {
        let symbols = extract_symbols_from_query("parse REST API calls via HTTP");
        assert!(
            symbols.contains(&"REST".to_string()),
            "acronym REST extracted"
        );
        assert!(
            symbols.contains(&"API".to_string()),
            "acronym API extracted"
        );
        assert!(
            symbols.contains(&"HTTP".to_string()),
            "acronym HTTP extracted"
        );
    }

    #[test]
    fn extract_symbols_dot_notation() {
        let symbols = extract_symbols_from_query("access app.isPackaged for config");
        assert!(
            symbols
                .iter()
                .any(|s| s == "app.isPackaged" || s == "isPackaged"),
            "dot notation parts extracted; got: {:?}",
            symbols
        );
    }

    #[test]
    fn extract_symbols_run_is_stopword() {
        // "run" is in commonWords (len 3 lowercase passes lower_re, but is a stopword)
        let symbols = extract_symbols_from_query("how to run the server");
        assert!(!symbols.contains(&"run".to_string()), "'run' is a stopword");
    }

    // -------------------------------------------------------------------------
    // FIX 2: query-time IDF
    // -------------------------------------------------------------------------

    /// A rare term must receive a HIGHER IDF weight than a high-frequency one,
    /// so the coverage boost can reward the semantic anchor over a common term.
    #[test]
    fn idf_weights_rare_term_higher_than_common_term() {
        use crate::connection::open_in_memory;
        use crate::queries::nodes::insert_node;
        use codewiki_core::{Language, Node, NodeKind};

        let conn = open_in_memory().unwrap();
        // "process" appears in 20 node names (high-frequency / common).
        for i in 0..20 {
            insert_node(
                &conn,
                &Node {
                    id: format!("p{}", i),
                    name: format!("process{}", i),
                    qualified_name: format!("process{}", i),
                    kind: NodeKind::Function,
                    language: Language::Rust,
                    file_path: "src/p.rs".to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        // "telemetry" appears in exactly ONE node (rare / anchor).
        insert_node(
            &conn,
            &Node {
                id: "rare1".to_string(),
                name: "telemetryFlush".to_string(),
                qualified_name: "telemetryFlush".to_string(),
                kind: NodeKind::Function,
                language: Language::Rust,
                file_path: "src/t.rs".to_string(),
                ..Default::default()
            },
        )
        .unwrap();

        let terms = vec!["process".to_string(), "telemetry".to_string()];
        let idf = compute_term_idf(&conn, &terms);
        assert!(
            idf.weight("telemetry") > idf.weight("process"),
            "rare term must outweigh common term: telemetry={}, process={}",
            idf.weight("telemetry"),
            idf.weight("process"),
        );
        // Unknown terms get a neutral weight.
        assert_eq!(idf.weight("zzz_unseen"), 1.0);
    }

    /// IDF weights are always finite and >= 1.0 even when a term matches every
    /// node or no node (smoothing + floor).
    #[test]
    fn idf_weights_are_bounded_and_positive() {
        use crate::connection::open_in_memory;
        use crate::queries::nodes::insert_node;
        use codewiki_core::{Language, Node, NodeKind};

        let conn = open_in_memory().unwrap();
        insert_node(
            &conn,
            &Node {
                id: "n1".to_string(),
                name: "common".to_string(),
                qualified_name: "common".to_string(),
                kind: NodeKind::Function,
                language: Language::Rust,
                file_path: "src/x.rs".to_string(),
                ..Default::default()
            },
        )
        .unwrap();

        let terms = vec!["common".to_string(), "missing".to_string()];
        let idf = compute_term_idf(&conn, &terms);
        for t in &terms {
            let w = idf.weight(t);
            assert!(w.is_finite() && w >= 1.0, "weight for {} = {}", t, w);
        }
    }
}

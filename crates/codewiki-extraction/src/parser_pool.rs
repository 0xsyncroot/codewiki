//! T-204 — Per-thread parser pool using thread_local!.

use codewiki_core::Language;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

/// A per-thread pool of `tree_sitter::Parser` instances, one per language.
///
/// `tree_sitter::Parser` is `!Sync` so it cannot be shared across threads.
/// This pool stores one parser per language in thread-local storage so that
/// each rayon worker has its own set of parsers that are never moved.
pub struct ParserPool {
    parsers: HashMap<LanguageKey, tree_sitter::Parser>,
    parse_counts: HashMap<LanguageKey, u32>,
}

/// A hashable / comparable key for a language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum LanguageKey {
    TypeScript,
    /// TypeScript with JSX. Same `Language::TypeScript` at every other layer,
    /// but `.tsx` needs the TSX grammar: the plain one cannot parse JSX, the
    /// tree fills with errors, and most top-level declarations vanish.
    Tsx,
    JavaScript,
    Python,
    Go,
    Rust,
    Java,
    Cpp,
    C,
    CSharp,
    Php,
    Ruby,
    Swift,
    Kotlin,
    Dart,
}

impl LanguageKey {
    fn from_language(lang: &Language) -> Option<Self> {
        match lang {
            Language::TypeScript => Some(Self::TypeScript),
            Language::JavaScript => Some(Self::JavaScript),
            Language::Python => Some(Self::Python),
            Language::Go => Some(Self::Go),
            Language::Rust => Some(Self::Rust),
            Language::Java => Some(Self::Java),
            Language::Cpp => Some(Self::Cpp),
            Language::C => Some(Self::C),
            Language::CSharp => Some(Self::CSharp),
            Language::Php => Some(Self::Php),
            Language::Ruby => Some(Self::Ruby),
            Language::Swift => Some(Self::Swift),
            Language::Kotlin => Some(Self::Kotlin),
            Language::Dart => Some(Self::Dart),
            // WASM-backed languages use a separate path.
            _ => None,
        }
    }

    fn tree_sitter_language(self) -> tree_sitter::Language {
        match self {
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Java => tree_sitter_java::LANGUAGE.into(),
            Self::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Self::C => tree_sitter_c::LANGUAGE.into(),
            Self::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Self::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            Self::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            Self::Swift => tree_sitter_swift::LANGUAGE.into(),
            Self::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
            Self::Dart => tree_sitter_dart::LANGUAGE.into(),
        }
    }
}

/// How many parses before a parser is recycled to reclaim fragmented memory.
const PARSER_RECYCLE_INTERVAL: u32 = 1_000;

impl ParserPool {
    pub fn new() -> Self {
        Self {
            parsers: HashMap::new(),
            parse_counts: HashMap::new(),
        }
    }

    /// Return the parser for `language` as used on `path`, creating it on
    /// first use. `.tsx` files get the TSX grammar; everything else follows
    /// [`Self::parser_for_language`].
    pub fn parser_for_file(
        &mut self,
        language: &Language,
        path: &Path,
    ) -> Option<&mut tree_sitter::Parser> {
        let mut key = LanguageKey::from_language(language)?;
        if key == LanguageKey::TypeScript
            && path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("tsx"))
        {
            key = LanguageKey::Tsx;
        }
        self.parser_for_key(key)
    }

    /// Return a mutable reference to the parser for `language`, creating it on
    /// first use. Returns `None` for WASM-backed languages (Lua, Luau, Pascal,
    /// Scala) and special-case languages (Svelte, Vue, Liquid, DFM).
    pub fn parser_for_language(&mut self, language: &Language) -> Option<&mut tree_sitter::Parser> {
        let key = LanguageKey::from_language(language)?;
        self.parser_for_key(key)
    }

    fn parser_for_key(&mut self, key: LanguageKey) -> Option<&mut tree_sitter::Parser> {
        // Recycle parser every N parses to reclaim fragmented memory.
        let count = self.parse_counts.entry(key).or_insert(0);
        if *count >= PARSER_RECYCLE_INTERVAL {
            self.parsers.remove(&key);
            *count = 0;
        }

        if let std::collections::hash_map::Entry::Vacant(e) = self.parsers.entry(key) {
            let mut parser = tree_sitter::Parser::new();
            let ts_lang = key.tree_sitter_language();
            if let Err(err) = parser.set_language(&ts_lang) {
                tracing::warn!(?key, %err, "failed to set parser language");
                return None;
            }
            e.insert(parser);
        }

        *self.parse_counts.entry(key).or_insert(0) += 1;
        self.parsers.get_mut(&key)
    }

    /// Force-recreate the parser for `language` (clears fragmented heap).
    pub fn reset_parser(&mut self, language: &Language) {
        if let Some(key) = LanguageKey::from_language(language) {
            self.parsers.remove(&key);
            self.parse_counts.insert(key, 0);
        }
    }
}

impl Default for ParserPool {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    /// Per-thread parser pool — each rayon worker owns one instance.
    pub static THREAD_PARSERS: RefCell<ParserPool> = RefCell::new(ParserPool::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_created_for_typescript() {
        let mut pool = ParserPool::new();
        let p = pool.parser_for_language(&Language::TypeScript);
        assert!(p.is_some());
    }

    #[test]
    fn none_for_wasm_language() {
        let mut pool = ParserPool::new();
        assert!(pool.parser_for_language(&Language::Lua).is_none());
        assert!(pool.parser_for_language(&Language::Scala).is_none());
    }

    #[test]
    fn reset_creates_new_parser() {
        let mut pool = ParserPool::new();
        // Get parser to initialize it.
        let _ = pool.parser_for_language(&Language::Python);
        // Reset and verify we can still get a parser after reset.
        pool.reset_parser(&Language::Python);
        let p = pool.parser_for_language(&Language::Python);
        assert!(p.is_some());
    }
}

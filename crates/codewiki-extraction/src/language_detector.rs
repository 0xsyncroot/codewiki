//! T-207 — Language detection from file extension.

use codewiki_core::Language;
use std::path::Path;

/// Map file extensions to `Language` variants.
/// Returns `None` for unknown / unsupported extensions.
pub fn detect_language(path: &Path) -> Option<Language> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    match ext.as_str() {
        // TypeScript
        "ts" | "tsx" | "mts" | "cts" => Some(Language::TypeScript),
        // JavaScript
        "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
        // Python
        "py" | "pyi" | "pyw" => Some(Language::Python),
        // Go
        "go" => Some(Language::Go),
        // Rust
        "rs" => Some(Language::Rust),
        // Java
        "java" => Some(Language::Java),
        // C++
        "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hxx" | "h++" => Some(Language::Cpp),
        // C
        "c" | "h" => Some(Language::C),
        // C#
        "cs" => Some(Language::CSharp),
        // PHP
        "php" | "php3" | "php4" | "php5" | "phtml" => Some(Language::Php),
        // Ruby
        "rb" | "rake" | "gemspec" => Some(Language::Ruby),
        // Swift
        "swift" => Some(Language::Swift),
        // Kotlin
        "kt" | "kts" => Some(Language::Kotlin),
        // Dart
        "dart" => Some(Language::Dart),
        // Scala
        "scala" | "sc" => Some(Language::Scala),
        // Lua
        "lua" => Some(Language::Lua),
        // Luau
        "luau" => Some(Language::Luau),
        // Pascal / Delphi source
        "pas" | "dpr" | "pp" | "lpr" => Some(Language::Pascal),
        // Delphi form files → DFM extractor
        "dfm" | "fmx" => Some(Language::Dfm),
        // Svelte
        "svelte" => Some(Language::Svelte),
        // Vue
        "vue" => Some(Language::Vue),
        // Blazor / Razor Components
        "razor" => Some(Language::Razor),
        // Liquid
        "liquid" => Some(Language::Liquid),
        _ => None,
    }
}

/// Returns `true` if the path has a known source file extension.
pub fn is_source_file(path: &Path) -> bool {
    detect_language(path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typescript_extensions() {
        assert_eq!(
            detect_language(Path::new("foo.ts")),
            Some(Language::TypeScript)
        );
        assert_eq!(
            detect_language(Path::new("foo.tsx")),
            Some(Language::TypeScript)
        );
        assert_eq!(
            detect_language(Path::new("foo.mts")),
            Some(Language::TypeScript)
        );
    }

    #[test]
    fn javascript_extensions() {
        assert_eq!(
            detect_language(Path::new("foo.js")),
            Some(Language::JavaScript)
        );
        assert_eq!(
            detect_language(Path::new("foo.jsx")),
            Some(Language::JavaScript)
        );
        assert_eq!(
            detect_language(Path::new("foo.mjs")),
            Some(Language::JavaScript)
        );
    }

    #[test]
    fn unknown_extension_returns_none() {
        assert_eq!(detect_language(Path::new("foo.wasm")), None);
        assert_eq!(detect_language(Path::new("foo.png")), None);
        assert_eq!(detect_language(Path::new("Makefile")), None);
    }

    #[test]
    fn is_source_file_works() {
        assert!(is_source_file(Path::new("main.rs")));
        assert!(!is_source_file(Path::new("README.md")));
    }

    #[test]
    fn razor_extension_detected() {
        assert_eq!(
            detect_language(Path::new("Counter.razor")),
            Some(Language::Razor)
        );
        assert!(is_source_file(Path::new("Counter.razor")));
        // .razor.cs code-behind files are plain C# (final extension is .cs)
        assert_eq!(
            detect_language(Path::new("Counter.razor.cs")),
            Some(Language::CSharp)
        );
    }

    #[test]
    fn all_languages_covered() {
        let cases = [
            ("a.py", Language::Python),
            ("a.go", Language::Go),
            ("a.rs", Language::Rust),
            ("a.java", Language::Java),
            ("a.cpp", Language::Cpp),
            ("a.c", Language::C),
            ("a.cs", Language::CSharp),
            ("a.php", Language::Php),
            ("a.rb", Language::Ruby),
            ("a.swift", Language::Swift),
            ("a.kt", Language::Kotlin),
            ("a.dart", Language::Dart),
            ("a.scala", Language::Scala),
            ("a.lua", Language::Lua),
            ("a.luau", Language::Luau),
            ("a.pas", Language::Pascal),
            ("a.dfm", Language::Dfm),
            ("a.svelte", Language::Svelte),
            ("a.vue", Language::Vue),
            ("a.razor", Language::Razor),
            ("a.liquid", Language::Liquid),
        ];
        for (path, expected) in cases {
            assert_eq!(
                detect_language(Path::new(path)),
                Some(expected),
                "failed for {path}"
            );
        }
    }
}

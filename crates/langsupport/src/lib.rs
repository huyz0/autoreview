//! Shared language dispatch/parser-setup for the languages `autoreview`
//! analyzes (Go, Java, Kotlin, TypeScript, TSX, JavaScript). Extracted from
//! three near-identical copies that had accumulated in `autoreview-core`
//! (`patch_check.rs`, `analyzers/practices.rs`, `analyzers/complexity.rs`)
//! and `autoreview-symindex` (`extract/mod.rs`) — this crate is the single
//! source of truth going forward; existing callers keep their own
//! extension-matching helpers where they need a project-local enum shape,
//! but new code (starting with `autoreview-dataflow`) should use this.
//!
//! TypeScript/TSX/JavaScript are three distinct tree-sitter grammars (JSX
//! syntax is handled by the JavaScript grammar itself, not a separate one —
//! same split `ast-grep`'s own `language:` values use), not one "JS-family"
//! language — a `.tsx` file parsed with the plain TypeScript grammar fails
//! on JSX syntax, and vice versa a `.ts` file parsed with the Tsx grammar
//! technically works but is the wrong grammar to advertise.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Go,
    Java,
    Kotlin,
    TypeScript,
    Tsx,
    JavaScript,
}

/// Extension-based language dispatch — the same convention already used
/// (independently, three times) across the codebase.
pub fn language_for_path(path: &std::path::Path) -> Option<Language> {
    match path.extension().and_then(|e| e.to_str())? {
        "go" => Some(Language::Go),
        "java" => Some(Language::Java),
        "kt" | "kts" => Some(Language::Kotlin),
        "ts" | "mts" | "cts" => Some(Language::TypeScript),
        "tsx" => Some(Language::Tsx),
        "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
        _ => None,
    }
}

fn ts_language(language: Language) -> tree_sitter::Language {
    match language {
        // Kotlin uses `tree-sitter-kotlin-ng` (the actively-maintained
        // `tree-sitter-grammars` org fork), not the stale `tree-sitter-kotlin`
        // crate this project originally evaluated and rejected for pinning
        // an incompatible `tree-sitter` core version.
        Language::Go => tree_sitter_go::LANGUAGE.into(),
        Language::Java => tree_sitter_java::LANGUAGE.into(),
        Language::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
    }
}

/// A ready-to-use `tree_sitter::Parser` for `language`, or `None` if the
/// grammar fails to load (shouldn't happen in practice — kept fallible to
/// match `Parser::set_language`'s own signature rather than panicking).
pub fn parser_for(language: Language) -> Option<tree_sitter::Parser> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_language(language)).ok()?;
    Some(parser)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatches_by_extension() {
        assert_eq!(language_for_path(std::path::Path::new("main.go")), Some(Language::Go));
        assert_eq!(language_for_path(std::path::Path::new("Foo.java")), Some(Language::Java));
        assert_eq!(language_for_path(std::path::Path::new("foo.kt")), Some(Language::Kotlin));
        assert_eq!(language_for_path(std::path::Path::new("foo.kts")), Some(Language::Kotlin));
        assert_eq!(language_for_path(std::path::Path::new("foo.ts")), Some(Language::TypeScript));
        assert_eq!(language_for_path(std::path::Path::new("foo.mts")), Some(Language::TypeScript));
        assert_eq!(language_for_path(std::path::Path::new("foo.cts")), Some(Language::TypeScript));
        assert_eq!(language_for_path(std::path::Path::new("foo.tsx")), Some(Language::Tsx));
        assert_eq!(language_for_path(std::path::Path::new("foo.js")), Some(Language::JavaScript));
        assert_eq!(language_for_path(std::path::Path::new("foo.jsx")), Some(Language::JavaScript));
        assert_eq!(language_for_path(std::path::Path::new("foo.mjs")), Some(Language::JavaScript));
        assert_eq!(language_for_path(std::path::Path::new("foo.cjs")), Some(Language::JavaScript));
        assert_eq!(language_for_path(std::path::Path::new("foo.py")), None);
    }

    #[test]
    fn parser_for_each_language_parses_a_trivial_snippet() {
        for (lang, src) in [
            (Language::Go, "package main\n"),
            (Language::Java, "class Foo {}\n"),
            (Language::Kotlin, "class Foo\n"),
            (Language::TypeScript, "const x: number = 1;\n"),
            (Language::Tsx, "const x = <div>{1}</div>;\n"),
            (Language::JavaScript, "const x = 1;\n"),
        ] {
            let mut parser = parser_for(lang).expect("parser should load");
            let tree = parser.parse(src, None).expect("should parse");
            assert!(!tree.root_node().has_error(), "{lang:?} produced an error tree for: {src}");
        }
    }
}

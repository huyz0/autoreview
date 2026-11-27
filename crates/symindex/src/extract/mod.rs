pub mod go;
pub mod java;
pub mod kotlin;
pub mod typescript;

use std::path::Path;

use crate::model::TypeDecl;

/// Extension-based language dispatch, same convention as
/// `autoreview-core`'s `patch_check.rs::language_for_extension`. Kotlin
/// uses `tree-sitter-kotlin-ng` (the actively-maintained
/// `tree-sitter-grammars` org fork), not the stale `tree-sitter-kotlin`
/// crate this project originally evaluated and rejected for pinning an
/// incompatible `tree-sitter` core version — `-ng` builds cleanly against
/// the same `tree-sitter` version Go/Java already use. TypeScript and TSX
/// are two distinct grammars (JSX parsing changes what's legal syntax), not
/// one "TS family" language — same split this project's `ast-grep` rule
/// packs and `langsupport` crate already use.
fn language_for_extension(extension: &str) -> Option<tree_sitter::Language> {
    match extension {
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        "java" => Some(tree_sitter_java::LANGUAGE.into()),
        "kt" | "kts" => Some(tree_sitter_kotlin_ng::LANGUAGE.into()),
        "ts" | "mts" | "cts" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        "js" | "jsx" | "mjs" | "cjs" => Some(tree_sitter_javascript::LANGUAGE.into()),
        _ => None,
    }
}

/// Parses `content` (already read into memory) as the language implied by
/// `file`'s extension and extracts its type declarations. Returns an empty
/// list (not an error) for an unsupported language or a file the parser
/// fails to load — callers should treat this the same way every other
/// Stage 1 analyzer treats an unreadable/irrelevant file: skip, don't fail
/// the run.
pub fn extract_file(file: &Path, content: &str) -> Vec<TypeDecl> {
    let Some((extension, tree)) = parse_file(file, content) else { return Vec::new() };
    extract_parsed_file(file, content, extension, &tree)
}

pub(crate) fn parse_file<'a>(file: &'a Path, content: &str) -> Option<(&'a str, tree_sitter::Tree)> {
    let extension = file.extension()?.to_str()?;
    let language = language_for_extension(extension)?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).ok()?;
    Some((extension, parser.parse(content, None)?))
}

pub(crate) fn extract_parsed_file(file: &Path, content: &str, extension: &str, tree: &tree_sitter::Tree) -> Vec<TypeDecl> {
    match extension {
        "go" => go::extract_types(tree, content.as_bytes(), file),
        "java" => java::extract_types(tree, content.as_bytes(), file),
        "kt" | "kts" => kotlin::extract_types(tree, content.as_bytes(), file),
        "ts" | "mts" | "cts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => typescript::extract_types(tree, content.as_bytes(), file),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn extracts_java_types_by_extension() {
        let types = extract_file(&PathBuf::from("Widget.java"), "class Widget {\n    int x;\n}\n");
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name, "Widget");
    }

    #[test]
    fn extracts_go_types_by_extension() {
        let types = extract_file(&PathBuf::from("widget.go"), "package main\n\ntype Widget struct {\n\tX int\n}\n");
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name, "Widget");
    }

    #[test]
    fn extracts_kotlin_types_by_extension() {
        let types = extract_file(&PathBuf::from("Widget.kt"), "class Widget {\n    var x: Int = 0\n}\n");
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name, "Widget");
    }

    #[test]
    fn extracts_typescript_types_by_extension() {
        let types = extract_file(&PathBuf::from("Widget.ts"), "class Widget {\n    x: number = 0;\n}\n");
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name, "Widget");
    }

    #[test]
    fn extracts_javascript_types_by_extension() {
        let types = extract_file(&PathBuf::from("Widget.js"), "class Widget {\n    constructor() {\n        this.x = 0;\n    }\n}\n");
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name, "Widget");
    }

    #[test]
    fn extracts_tsx_types_by_extension() {
        let types = extract_file(&PathBuf::from("Widget.tsx"), "class Widget {\n    x: number = 0;\n}\n\nfunction App() {\n    return <div>hi</div>;\n}\n");
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name, "Widget");
    }

    #[test]
    fn returns_empty_for_an_unsupported_extension() {
        assert!(extract_file(&PathBuf::from("README.md"), "# hi").is_empty());
    }

    #[test]
    fn returns_empty_for_a_file_with_no_extension() {
        assert!(extract_file(&PathBuf::from("Makefile"), "all:\n").is_empty());
    }
}

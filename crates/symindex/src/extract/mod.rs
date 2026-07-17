pub mod go;
pub mod java;

use std::path::Path;

use crate::model::TypeDecl;

/// Extension-based language dispatch, same convention as
/// `autoreview-core`'s `patch_check.rs::language_for_extension`.
fn language_for_extension(extension: &str) -> Option<tree_sitter::Language> {
    match extension {
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        "java" => Some(tree_sitter_java::LANGUAGE.into()),
        _ => None,
    }
}

/// Parses `content` (already read into memory) as the language implied by
/// `file`'s extension and extracts its type declarations. Returns an empty
/// list (not an error) for an unsupported language (Kotlin, Go until Phase
/// 2 lands, anything else) or a file the parser fails to load — callers
/// should treat this the same way every other Stage 1 analyzer treats an
/// unreadable/irrelevant file: skip, don't fail the run.
pub fn extract_file(file: &Path, content: &str) -> Vec<TypeDecl> {
    let Some(extension) = file.extension().and_then(|e| e.to_str()) else { return Vec::new() };
    let Some(language) = language_for_extension(extension) else { return Vec::new() };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(content, None) else { return Vec::new() };

    match extension {
        "go" => go::extract_types(&tree, content.as_bytes(), file),
        "java" => java::extract_types(&tree, content.as_bytes(), file),
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
    fn returns_empty_for_an_unsupported_extension() {
        assert!(extract_file(&PathBuf::from("Widget.kt"), "class Widget").is_empty());
        assert!(extract_file(&PathBuf::from("README.md"), "# hi").is_empty());
    }

    #[test]
    fn returns_empty_for_a_file_with_no_extension() {
        assert!(extract_file(&PathBuf::from("Makefile"), "all:\n").is_empty());
    }
}

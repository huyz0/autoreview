use std::path::Path;

use autoreview_langsupport::{language_for_path, parser_for, Language};
use tree_sitter::Tree;

use super::cache::JvmPackageIndex;

enum Package {
    Named(String),
    Default,
    Unknown,
    Unresolved,
}

pub(super) fn build(root: &Path) -> JvmPackageIndex {
    let raw = autoreview_archgraph::index_java_kotlin_packages_with_coverage(root);
    let mut index = JvmPackageIndex { packages: Default::default(), unavailable: raw.unavailable, malformed: Vec::new(), unresolved: Vec::new() };
    let mut java = parser_for(Language::Java);
    let mut kotlin = parser_for(Language::Kotlin);
    for path in raw.packages.into_values().flatten().chain(raw.unclassified) {
        let parser = match language_for_path(&path) {
            Some(Language::Java) => java.as_mut(),
            Some(Language::Kotlin) => kotlin.as_mut(),
            _ => None,
        };
        let parsed = std::fs::read_to_string(&path).ok().and_then(|source| {
            let tree = parser?.parse(&source, None)?;
            Some(package(&tree, &source))
        });
        match parsed {
            Some(Package::Named(name)) => index.packages.entry(name).or_default().push(path),
            Some(Package::Default) => {}
            Some(Package::Unknown) => index.malformed.push(path),
            Some(Package::Unresolved) => index.unresolved.push(path),
            None => index.unavailable.push(path),
        }
    }
    for paths in index.packages.values_mut() { paths.sort(); }
    index.unavailable.sort();
    index.unavailable.dedup();
    index.malformed.sort();
    index.malformed.dedup();
    index.unresolved.sort();
    index.unresolved.dedup();
    index
}

fn package(tree: &Tree, source: &str) -> Package {
    let root = tree.root_node();
    let mut cursor = root.walk();
    let declarations: Vec<_> = root.named_children(&mut cursor)
        .filter(|node| matches!(node.kind(), "package_declaration" | "package_header"))
        .collect();
    let declaration = match declarations.as_slice() {
        [] => return if root.has_error() { Package::Unknown } else { Package::Default },
        [declaration] if !declaration.has_error() => declaration,
        _ => return Package::Unknown,
    };
    let mut cursor = declaration.walk();
    let name = declaration.named_children(&mut cursor)
        .find(|node| matches!(node.kind(), "identifier" | "scoped_identifier" | "qualified_identifier"))
        .and_then(|node| node.utf8_text(source.as_bytes()).ok());
    match name {
        Some(name) if name.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '$' | '.')) => Package::Named(name.to_owned()),
        _ => Package::Unresolved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_real_declarations_and_keeps_ambiguous_sources_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        let files = [
            ("Comment.java", "/*\n package fake;\n*/\npackage actual;\nclass Comment {}\n"),
            ("Inline.java", "package actual; class Inline {}\n"),
            ("Multiline.java", "package\nactual;\nclass Multiline {}\n"),
            ("Default.java", "/*\n package fake;\n*/\nclass Default {}\n"),
            ("Default.kt", "fun defaultValue() = 1\n"),
            ("K.kt", "/*\n package fake\n*/\npackage real.name\nclass K\n"),
            ("Broken.kt", "fun broken( {\n"),
            ("Escaped.kt", "package `when`\nclass Escaped\n"),
        ];
        for (name, source) in files { std::fs::write(dir.path().join(name), source).unwrap(); }
        let index = build(dir.path());
        assert_eq!(index.packages.get("actual").unwrap().len(), 3);
        assert_eq!(index.packages.get("real.name").unwrap().len(), 1);
        assert!(!index.packages.contains_key("fake"));
        assert!(index.malformed.contains(&dir.path().join("Broken.kt")));
        assert!(index.unresolved.contains(&dir.path().join("Escaped.kt")));
        assert!(!index.malformed.contains(&dir.path().join("Default.java")));
        assert!(!index.malformed.contains(&dir.path().join("Default.kt")));
    }
}

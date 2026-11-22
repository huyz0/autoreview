use std::path::{Path, PathBuf};

use autoreview_archgraph::{declared_package, index_java_kotlin_packages};
use autoreview_dataflow::call_order::KeyedImport;
use autoreview_langsupport::Language;

use super::{children, text, Resolver};
use tree_sitter::Node;

pub(super) fn imports(
    resolver: &Resolver<'_>,
    path: &Path,
    language: Language,
    root: Node<'_>,
    source: &str,
) -> Vec<KeyedImport> {
    let mut bindings = siblings(
        resolver,
        path,
        language,
        declared_package(source).as_deref(),
    );
    let declarations = children(root)
        .into_iter()
        .filter(|node| matches!(node.kind(), "import_declaration" | "import"))
        .filter_map(|node| text(node, source)?.strip_prefix("import "));
    for line in declarations {
        let target = line.trim_end_matches(';').trim();
        match language {
            Language::Java => bindings.extend(java(resolver, target)),
            Language::Kotlin => bindings.extend(kotlin(resolver, target)),
            _ => {}
        }
    }
    bindings
}

fn siblings(
    resolver: &Resolver<'_>,
    path: &Path,
    language: Language,
    package: Option<&str>,
) -> Vec<KeyedImport> {
    let paths = match package {
        Some(package) => package_files(resolver, package).to_vec(),
        None => resolver.files(
            path.parent().unwrap_or(&resolver.root),
            if language == Language::Java {
                "java"
            } else {
                "kt"
            },
        ),
    };
    paths
        .into_iter()
        .filter(|file| file != path)
        .filter_map(|file| {
            if language == Language::Kotlin && file.extension().is_some_and(|ext| ext == "kt") {
                return resolver.binding(&file, "", None, None, true);
            }
            if file.extension().is_some_and(|ext| ext == "java") {
                let owner = file.file_stem()?.to_str()?;
                return resolver.binding(&file, &format!("{owner}."), None, Some(owner), true);
            }
            None
        })
        .collect()
}

fn java(resolver: &Resolver<'_>, target: &str) -> Vec<KeyedImport> {
    if let Some(target) = target.strip_prefix("static ") {
        if let Some((class, member)) = target.rsplit_once('.') {
            return java_class(resolver, class, Some(member));
        }
    } else {
        return java_class(resolver, target, None);
    }
    Vec::new()
}

fn java_class(resolver: &Resolver<'_>, class: &str, member: Option<&str>) -> Vec<KeyedImport> {
    let Some((package, owner)) = class.rsplit_once('.') else {
        return Vec::new();
    };
    let prefix = format!("{owner}.");
    let (local, name) = match member {
        Some("*") => ("", None),
        Some(member) => (member, Some(member)),
        None => (&*prefix, None),
    };
    package_files(resolver, package)
        .iter()
        .filter_map(|file| {
            if file.extension().is_some_and(|ext| ext == "java") {
                resolver.binding(file, local, name, Some(owner), true)
            } else {
                None
            }
        })
        .collect()
}

fn kotlin(resolver: &Resolver<'_>, target: &str) -> Vec<KeyedImport> {
    let (target, alias) = target
        .split_once(" as ")
        .map_or((target, None), |(path, alias)| {
            (path.trim(), Some(alias.trim()))
        });
    let Some((package, member)) = target.rsplit_once('.') else {
        return Vec::new();
    };
    let name = (member != "*").then_some(member);
    let local = alias.or(name).unwrap_or_default();
    let mut bindings: Vec<_> = package_files(resolver, package)
        .iter()
        .filter_map(|file| {
            (file.extension().is_some_and(|ext| ext == "kt"))
                .then(|| resolver.binding(file, local, name, None, true))
                .flatten()
        })
        .collect();
    if name.is_some() && alias.is_none() {
        bindings.extend(java(resolver, &format!("static {target}")));
    }
    bindings
}

fn package_files<'a>(resolver: &'a Resolver<'_>, package: &str) -> &'a [PathBuf] {
    resolver
        .packages
        .java_kotlin_packages
        .get_or_init(|| index_java_kotlin_packages(&resolver.root))
        .get(package)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

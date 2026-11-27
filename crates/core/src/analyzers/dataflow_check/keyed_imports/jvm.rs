use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use autoreview_archgraph::declared_package;
use autoreview_dataflow::call_order::KeyedImport;
use autoreview_langsupport::{language_for_path, Language};
use autoreview_schema::AnalysisCoverageIssue;

use super::{children, issue, text, CallOrderRuleDef, Resolver, Source};
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
        .jvm_index(&resolver.root)
        .packages.get(package)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

pub(super) fn record_index_coverage(
    resolver: &Resolver<'_>, sources: &BTreeMap<String, Source>, entries: &BTreeSet<String>,
    rules: &[(&CallOrderRuleDef, Language)], coverage: &mut Vec<AnalysisCoverageIssue>,
) {
    let uses_index = sources.values().any(|file| matches!(file.language, Language::Java | Language::Kotlin)
        && (autoreview_archgraph::declared_package(&file.parsed.0).is_some()
            || !autoreview_archgraph::extract_java_kotlin_imports(&file.parsed.0).is_empty()));
    let Some(index) = resolver.packages.java_kotlin_packages.get().filter(|_| uses_index) else { return };
    let unavailable = index.unavailable.iter().map(|path| (path, "package_index_unavailable", "A source or directory could not be inspected during Java/Kotlin package discovery; cross-file call-sequence analysis may be incomplete."));
    let malformed = index.malformed.iter().map(|path| (path, "package_index_syntax_errors", "A source has syntax errors and no recognizable package; cross-file call-sequence analysis may be incomplete."));
    let unresolved = index.unresolved.iter().map(|path| (path, "package_index_unresolved", "A package declaration could not be resolved safely; cross-file call-sequence analysis may be incomplete."));
    for (path, reason, detail) in unavailable.chain(malformed).chain(unresolved) {
        let relative = resolver.relative(path);
        if relative.as_ref().is_some_and(|path| entries.contains(path)) { continue; }
        for (rule, language) in rules.iter().filter(|(_, language)| matches!(language, Language::Java | Language::Kotlin)) {
            if !entries.iter().any(|path| language_for_path(Path::new(path)).is_some_and(|actual| actual.matches_rule_language(*language))) { continue; }
            let mut limitation = issue(relative.as_deref(), reason, "dependency", detail);
            limitation.rule = Some(rule.id.clone());
            coverage.push(limitation);
        }
    }
}

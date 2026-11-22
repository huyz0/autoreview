use std::path::{Path, PathBuf};

use autoreview_dataflow::call_order::KeyedImport;
use tree_sitter::Node;

use super::{children, text, unquote, ImportSource, Resolver};
use autoreview_langsupport::{language_for_path, Language};

impl ImportSource<'_> {
    pub(super) fn web(&self) -> Vec<KeyedImport> {
        let (resolver, path) = (self.resolver, self.path);
        let source = self.source;
        children(self.node)
            .into_iter()
            .filter(|node| node.kind() == "import_statement")
            .flat_map(|node| {
                let target = node
                    .child_by_field_name("source")
                    .and_then(|value| text(value, source))
                    .and_then(unquote);
                let Some(file) =
                    target.and_then(|target| web_file(resolver, path.parent()?, target))
                else {
                    return Vec::new();
                };
                web_bindings(resolver, node, source, &file)
            })
            .collect()
    }
}

fn web_bindings(
    resolver: &Resolver<'_>,
    node: Node<'_>,
    source: &str,
    file: &Path,
) -> Vec<KeyedImport> {
    let Some(clause) = children(node)
        .into_iter()
        .find(|node| node.kind() == "import_clause")
    else {
        return Vec::new();
    };
    children(clause)
        .into_iter()
        .flat_map(|part| match part.kind() {
            "named_imports" => children(part)
                .into_iter()
                .filter_map(|item| {
                    let name = text(item.child_by_field_name("name")?, source)?;
                    let alias = item
                        .child_by_field_name("alias")
                        .and_then(|alias| text(alias, source))
                        .unwrap_or(name);
                    resolver.binding(file, alias, Some(name), None, true)
                })
                .collect(),
            "identifier" => text(part, source)
                .and_then(|local| resolver.binding(file, local, Some("default"), None, true))
                .into_iter()
                .collect(),
            "namespace_import" => part
                .named_child(0)
                .and_then(|name| text(name, source))
                .and_then(|name| resolver.binding(file, &format!("{name}."), None, None, true))
                .into_iter()
                .collect(),
            _ => Vec::new(),
        })
        .collect()
}

fn web_file(resolver: &Resolver<'_>, parent: &Path, target: &str) -> Option<PathBuf> {
    if !target.starts_with("./") && !target.starts_with("../") {
        return None;
    }
    let base = parent.join(target);
    let extensions = ["ts", "tsx", "js", "mjs", "cjs", "mts", "cts"];
    let mut candidates = vec![base.clone()];
    if base.extension().is_none() {
        candidates.extend(extensions.iter().map(|ext| base.with_extension(ext)));
        candidates.extend(
            extensions
                .iter()
                .map(|ext| base.join(format!("index.{ext}"))),
        );
    } else if base.extension().is_some_and(|ext| ext == "js") {
        candidates.push(base.with_extension("ts"));
        candidates.push(base.with_extension("tsx"));
    }
    candidates.into_iter().find(|path| {
        resolver.relative(path).is_some()
            && path.is_file()
            && language_for_path(path).is_some_and(|lang| {
                matches!(
                    lang,
                    Language::JavaScript | Language::TypeScript | Language::Tsx
                )
            })
    })
}

use super::{children, issue, text, unquote, AnalysisCoverageIssue, ImportSource, KeyedImport, Node, Path, PathBuf, Resolver};
use autoreview_langsupport::{language_for_path, Language};

impl ImportSource<'_> {
    pub(super) fn web(&self, coverage: &mut Vec<AnalysisCoverageIssue>) -> Vec<KeyedImport> {
        let (resolver, path) = (self.resolver, self.path);
        let source = self.source;
        children(self.node)
            .into_iter()
            .filter(|node| node.kind() == "import_statement")
            .flat_map(|node| {
                if !has_runtime_bindings(node) {
                    return Vec::new();
                }
                let target = node
                    .child_by_field_name("source")
                    .and_then(|value| text(value, source))
                    .and_then(unquote);
                let Some(target) = target else { return Vec::new() };
                let Some(parent) = path.parent() else { return Vec::new() };
                let Some(file) = web_file(resolver, parent, target) else {
                    if may_be_local_code(target) {
                        let relative = resolver.relative(path);
                        let detail = format!("The local import {target:?} could not be resolved to supported source; helper analysis may be incomplete.");
                        let mut record = issue(relative.as_deref(), "unresolved_local_import", "dependency", &detail);
                        record.line = Some(node.start_position().row as u32 + 1);
                        coverage.push(record);
                    }
                    return Vec::new();
                };
                web_bindings(resolver, node, source, &file)
            })
            .collect()
    }
}

fn may_be_local_code(target: &str) -> bool {
    if !target.starts_with("./") && !target.starts_with("../") {
        return false;
    }
    Path::new(target).extension().is_none_or(|ext| matches!(ext.to_str(), Some("ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts")))
}

fn has_runtime_bindings(import: Node<'_>) -> bool {
    if import.children(&mut import.walk()).any(|child| child.kind() == "type") {
        return false;
    }
    let Some(clause) = children(import).into_iter().find(|node| node.kind() == "import_clause") else {
        return false;
    };
    children(clause).into_iter().any(|part| match part.kind() {
        "identifier" | "namespace_import" => true,
        "named_imports" => children(part).into_iter().any(|item| {
            item.kind() == "import_specifier" && !item.children(&mut item.walk()).any(|child| child.kind() == "type")
        }),
        _ => false,
    })
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
                    if item.children(&mut item.walk()).any(|child| child.kind() == "type") {
                        return None;
                    }
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
    let extensions = ["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"];
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

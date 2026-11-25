use super::{children, issue, text, unquote, AnalysisCoverageIssue, ImportSource, KeyedExport, KeyedImport, Node, Path, PathBuf, Resolver};
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

    pub(super) fn web_exports(&self, coverage: &mut Vec<AnalysisCoverageIssue>) -> Vec<KeyedExport> {
        children(self.node).into_iter().filter(|node| node.kind() == "export_statement")
            .flat_map(|node| self.web_export(node, coverage)).collect()
    }

    fn web_export(&self, node: Node<'_>, coverage: &mut Vec<AnalysisCoverageIssue>) -> Vec<KeyedExport> {
        if node.children(&mut node.walk()).any(|child| child.kind() == "type") {
            return Vec::new();
        }
        let clause = children(node).into_iter().find(|child| child.kind() == "export_clause");
        let star = node.children(&mut node.walk()).any(|child| child.kind() == "*");
        let namespace = children(node).into_iter().any(|child| child.kind() == "namespace_export");
        if clause.is_none() && !star && !namespace {
            let exports = declaration_exports(node, self.source);
            if exports.contains(&KeyedExport::Unknown) {
                self.reexport_issue(node, "unsupported_export_binding", "A runtime export binding could not be enumerated; helper analysis may be incomplete.", coverage);
            }
            return exports;
        }
        let names = exported_names(clause, self.source);
        if names.is_empty() && !star && !namespace {
            return Vec::new();
        }
        let target = node.child_by_field_name("source").and_then(|value| text(value, self.source)).and_then(unquote);
        let path = target.map(|target| self.reexport_path(node, target, coverage));
        if namespace {
            self.namespace_export(node, coverage)
        } else if star {
            path.into_iter().map(|path| KeyedExport::Star { path }).collect()
        } else {
            names.into_iter().map(|(local, exported)| KeyedExport::Named { exported, local, path: path.clone() }).collect()
        }
    }

    fn namespace_export(&self, node: Node<'_>, coverage: &mut Vec<AnalysisCoverageIssue>) -> Vec<KeyedExport> {
        self.reexport_issue(node, "unsupported_reexport", "Namespace re-exports are not modeled by helper analysis.", coverage);
        children(node).into_iter().find(|child| child.kind() == "namespace_export")
            .and_then(|namespace| namespace.named_child(0))
            .and_then(|name| text(name, self.source))
            .map(|name| vec![KeyedExport::Named { exported: name.to_owned(), local: String::new(), path: None }])
            .unwrap_or_else(|| vec![KeyedExport::Unknown])
    }

    fn reexport_path(&self, node: Node<'_>, target: &str, coverage: &mut Vec<AnalysisCoverageIssue>) -> String {
        if let Some(path) = self.path.parent().and_then(|parent| web_file(self.resolver, parent, target)).and_then(|file| self.resolver.relative(&file)) {
            return path;
        }
        let detail = format!("The re-export {target:?} could not be resolved to supported source; helper analysis may be incomplete.");
        self.reexport_issue(node, "unresolved_reexport", &detail, coverage);
        String::new()
    }

    fn reexport_issue(&self, node: Node<'_>, reason: &str, detail: &str, coverage: &mut Vec<AnalysisCoverageIssue>) {
        let path = self.resolver.relative(self.path);
        let mut record = issue(path.as_deref(), reason, "dependency", detail);
        record.line = Some(node.start_position().row as u32 + 1);
        coverage.push(record);
    }
}

fn exported_names(clause: Option<Node<'_>>, source: &str) -> Vec<(String, String)> {
    clause.into_iter().flat_map(children).filter_map(|item| {
        if item.kind() != "export_specifier" || item.children(&mut item.walk()).any(|child| child.kind() == "type") {
            return None;
        }
        let local = text(item.child_by_field_name("name")?, source)?;
        let exported = item.child_by_field_name("alias").and_then(|alias| text(alias, source)).unwrap_or(local);
        Some((local.to_owned(), exported.to_owned()))
    }).collect()
}

fn declaration_exports(node: Node<'_>, source: &str) -> Vec<KeyedExport> {
    let Some(declaration) = node.child_by_field_name("declaration") else { return Vec::new() };
    if matches!(declaration.kind(), "interface_declaration" | "type_alias_declaration") {
        return Vec::new();
    }
    let default = node.children(&mut node.walk()).any(|child| child.kind() == "default");
    let names: Vec<_> = if matches!(declaration.kind(), "lexical_declaration" | "variable_declaration") {
        children(declaration).into_iter().filter_map(|item| item.child_by_field_name("name")).collect()
    } else {
        declaration.child_by_field_name("name").into_iter().collect()
    };
    names.into_iter().map(|name| {
        if !matches!(name.kind(), "identifier" | "type_identifier") {
            return KeyedExport::Unknown;
        }
        let Some(local) = text(name, source).map(str::to_owned) else { return KeyedExport::Unknown };
        KeyedExport::Named { exported: if default { "default".into() } else { local.clone() }, local, path: None }
    }).collect()
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

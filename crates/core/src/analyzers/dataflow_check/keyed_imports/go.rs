use super::{children, issue, text, unquote, AnalysisCoverageIssue, ImportSource, KeyedImport, Node, Path, PathBuf, Resolver};
use autoreview_langsupport::Language;

impl ImportSource<'_> {
    pub(super) fn go(&self, coverage: &mut Vec<AnalysisCoverageIssue>) -> Vec<KeyedImport> {
        let resolver = self.resolver;
        let path = self.path;
        let node = self.node;
        let source = self.source;
        let parent = path.parent().unwrap_or(&resolver.root);
        let mut imports = Vec::new();
        let package = go_package(node, source);
        for file in go_files(resolver, parent) {
            if file == path {
                continue;
            }
            let other_package = resolver.go_package_of(&file, coverage);
            if other_package.is_some() && other_package.as_deref() == package
                && resolver.go_may_supply_helper(&file, source) {
                imports.extend(resolver.binding(&file, "", None, None, false));
            }
        }
        let Some(module) = autoreview_archgraph::discover_go_module_path(&resolver.root) else {
            return imports;
        };
        let mut nodes = children(node);
        while let Some(node) = nodes.pop() {
            match node.kind() {
                "import_declaration" | "import_spec_list" => nodes.extend(children(node)),
                "import_spec" => imports.extend(go_import(resolver, node, source, &module, coverage)),
                _ => {}
            }
        }
        imports
    }
}

impl Resolver<'_> {
    fn go_package_of(&self, file: &Path, coverage: &mut Vec<AnalysisCoverageIssue>) -> Option<String> {
        let parsed = self.cache.get(file, Language::Go);
        let package = parsed.as_ref().and_then(|parsed| go_package(parsed.1.root_node(), &parsed.0))
            .filter(|name| !name.is_empty());
        if let Some(package) = package {
            return Some(package.to_owned());
        }
        let (reason, detail) = match parsed {
            None => ("dependency_unavailable", "The Go source could not be read or parsed, so its package and potential helpers could not be determined."),
            Some(parsed) if parsed.1.root_node().has_error() => ("dependency_syntax_errors", "The Go source has syntax errors and its package could not be determined; helper discovery may be incomplete."),
            Some(_) => ("dependency_package_unavailable", "The Go source has no identifiable package; helper discovery may be incomplete."),
        };
        if let Some(path) = self.relative(file) {
            if !coverage.iter().any(|item| item.path.as_deref() == Some(path.as_str()) && item.reason == reason) {
                coverage.push(issue(Some(&path), reason, "dependency", detail));
            }
        }
        None
    }

    // The keyed program resolves Go helpers by their declared function name.
    // Searching the entire importing source (including comments and strings)
    // deliberately over-approximates references, while avoiding unrelated
    // package files. Keep files with syntax errors so coverage can report them.
    fn go_may_supply_helper(&self, file: &Path, referring_source: &str) -> bool {
        let Some(parsed) = self.cache.get(file, Language::Go) else {
            return true;
        };
        let root = parsed.1.root_node();
        if root.has_error() {
            return true;
        }
        children(root).into_iter().filter(|node| node.kind() == "function_declaration").any(|node| {
            node.child_by_field_name("name")
                .and_then(|name| text(name, &parsed.0))
                .is_none_or(|name| referring_source.contains(name))
        })
    }
}

fn go_import(
    resolver: &Resolver<'_>,
    node: Node<'_>,
    source: &str,
    module: &str,
    coverage: &mut Vec<AnalysisCoverageIssue>,
) -> Vec<KeyedImport> {
    let Some(target) = node
        .child_by_field_name("path")
        .and_then(|path| text(path, source))
        .and_then(unquote)
    else {
        return Vec::new();
    };
    let Some(suffix) = target
        .strip_prefix(module)
        .and_then(|target| target.strip_prefix('/'))
    else {
        return Vec::new();
    };
    let alias = node
        .child_by_field_name("name")
        .and_then(|name| text(name, source));
    if matches!(alias, Some("_" | ".")) {
        return Vec::new();
    }
    go_files(resolver, &resolver.root.join(suffix))
        .into_iter()
        .filter_map(|file| {
            let package = resolver.go_package_of(&file, coverage)?;
            if !resolver.go_may_supply_helper(&file, source) {
                return None;
            }
            let prefix = format!("{}.", alias.unwrap_or(&package));
            resolver.binding(&file, &prefix, None, None, true)
        })
        .collect()
}

fn go_files(resolver: &Resolver<'_>, dir: &Path) -> Vec<PathBuf> {
    resolver
        .files(dir, "go")
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.ends_with("_test.go"))
        })
        .collect()
}

fn go_package<'src>(node: Node<'_>, source: &'src str) -> Option<&'src str> {
    let clause = children(node)
        .into_iter()
        .find(|node| node.kind() == "package_clause")?;
    text(clause.named_child(0)?, source)
}

use super::{children, text, unquote, ImportSource, KeyedImport, Node, Path, PathBuf, Resolver};
use autoreview_langsupport::Language;

impl ImportSource<'_> {
    pub(super) fn go(&self) -> Vec<KeyedImport> {
        let resolver = self.resolver;
        let path = self.path;
        let node = self.node;
        let source = self.source;
        let parent = path.parent().unwrap_or(&resolver.root);
        let mut imports = Vec::new();
        let package = go_package(node, source);
        for file in go_files(resolver, parent) {
            if file != path && resolver.go_package_of(&file).as_deref() == package {
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
                "import_spec" => imports.extend(go_import(resolver, node, source, &module)),
                _ => {}
            }
        }
        imports
    }
}

impl Resolver<'_> {
    fn go_package_of(&self, file: &Path) -> Option<String> {
        let parsed = self.cache.get(file, Language::Go)?;
        go_package(parsed.1.root_node(), &parsed.0).map(str::to_owned)
    }
}

fn go_import(
    resolver: &Resolver<'_>,
    node: Node<'_>,
    source: &str,
    module: &str,
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
            let package = resolver.go_package_of(&file)?;
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

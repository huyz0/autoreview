use super::{children, text, ImportSource, KeyedImport, Node, Path, PathBuf, Resolver};

impl ImportSource<'_> {
    pub(super) fn rust(&self) -> Vec<KeyedImport> {
        let path = self.path;
        let resolver = self.resolver;
        let Some(dir) = module_dir(path) else {
            return Vec::new();
        };
        children(self.node)
            .into_iter()
            .flat_map(|node| match node.kind() {
                "mod_item" if node.child_by_field_name("body").is_none() => {
                    let name = node
                        .child_by_field_name("name")
                        .and_then(|name| text(name, self.source));
                    name.and_then(|name| {
                        rust_module(resolver, &dir.join(name)).and_then(|file| {
                            resolver.binding(&file, &format!("{name}::"), None, None, true)
                        })
                    })
                    .into_iter()
                    .collect()
                }
                "use_declaration" => rust_uses(resolver, path, &dir, node, self.source),
                _ => Vec::new(),
            })
            .collect()
    }
}

fn rust_uses(
    resolver: &Resolver<'_>,
    path: &Path,
    dir: &Path,
    node: Node<'_>,
    source: &str,
) -> Vec<KeyedImport> {
    let Some(usage) = node
        .child_by_field_name("argument")
        .and_then(|node| text(node, source))
    else {
        return Vec::new();
    };
    let paths = rust_paths(usage.trim());
    paths
        .into_iter()
        .filter_map(|usage| {
            let (qualified, alias) = usage
                .split_once(" as ")
                .map_or((usage.as_str(), None), |(path, alias)| {
                    (path.trim(), Some(alias.trim()))
                });
            let (module, member) = qualified.rsplit_once("::")?;
            let file = rust_use_file(resolver, path, dir, module)?;
            resolver.binding(&file, alias.unwrap_or(member), Some(member), None, true)
        })
        .collect()
}

fn rust_paths(usage: &str) -> Vec<String> {
    if let Some((prefix, members)) = usage.split_once("::{") {
        return members
            .strip_suffix('}')
            .into_iter()
            .flat_map(|members| members.split(','))
            .map(str::trim)
            .filter(|member| !member.is_empty() && !member.contains(['{', '}']))
            .map(|member| format!("{prefix}::{member}"))
            .collect();
    }
    vec![usage.to_owned()]
}

fn rust_use_file(
    resolver: &Resolver<'_>,
    path: &Path,
    dir: &Path,
    module: &str,
) -> Option<PathBuf> {
    let crate_dir = path
        .ancestors()
        .skip(1)
        .find(|parent| {
            parent.starts_with(&resolver.root)
                && (parent.join("lib.rs").is_file() || parent.join("main.rs").is_file())
        })
        .unwrap_or(dir);
    let (base, module) = match module.split_once("::") {
        Some(("crate", rest)) => (crate_dir, rest),
        Some(("self", rest)) => (dir, rest),
        Some(("super", rest)) => (dir.parent()?, rest),
        _ => (crate_dir, module),
    };
    if !module
        .split("::")
        .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_alphanumeric() || ch == '_'))
    {
        return None;
    }
    rust_module(resolver, &base.join(module.replace("::", "/")))
}

fn module_dir(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let name = path.file_stem()?.to_str()?;
    if matches!(name, "mod" | "lib" | "main") {
        Some(parent.to_owned())
    } else {
        Some(parent.join(name))
    }
}

fn rust_module(resolver: &Resolver<'_>, base: &Path) -> Option<PathBuf> {
    let candidates = [base.with_extension("rs"), base.join("mod.rs")];
    let mut files = candidates
        .into_iter()
        .filter(|path| resolver.relative(path).is_some() && path.is_file());
    let file = files.next()?;
    files.next().is_none().then_some(file)
}

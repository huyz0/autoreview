use super::{children, issue, text, AnalysisCoverageIssue, ImportSource, KeyedImport, Language, Node, Path, PathBuf, Resolver};

impl ImportSource<'_> {
    pub(super) fn rust(&self, coverage: &mut Vec<AnalysisCoverageIssue>) -> Vec<KeyedImport> {
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
                "use_declaration" => rust_uses(self, &dir, node, coverage),
                _ => Vec::new(),
            })
            .collect()
    }
}

fn rust_uses(
    input: &ImportSource<'_>, dir: &Path, node: Node<'_>, coverage: &mut Vec<AnalysisCoverageIssue>,
) -> Vec<KeyedImport> {
    let ImportSource { resolver, path, source, .. } = *input;
    let conditional = has_attributes(node);
    let Some(usage) = node
        .child_by_field_name("argument")
        .and_then(|node| text(node, source))
    else {
        return Vec::new();
    };
    let paths = rust_paths(usage.trim());
    paths.into_iter().flat_map(|usage| {
        let (qualified, alias) = usage.split_once(" as ")
            .map_or((usage.as_str(), None), |(path, alias)| (path.trim(), Some(alias.trim())));
        let Some((module, member)) = qualified.rsplit_once("::") else { return Vec::new() };
        let mut bindings = Vec::new();
        if let Some(file) = (!conditional).then(|| rust_use_file(resolver, path, dir, module)).flatten() {
            if let Some(binding) = resolver.binding(&file, alias.unwrap_or(member), Some(member), None, true) {
                bindings.push(binding);
            }
        }
        let (module_path, default) = if member == "self" {
            (module, module.rsplit("::").next().unwrap_or(module))
        } else { (qualified, member) };
        let local = format!("{}::", alias.unwrap_or(default));
        let namespace = (!conditional).then(|| verified_namespace(resolver, path, module_path)).flatten();
        if let Some(file) = namespace {
            if let Some(binding) = resolver.binding(&file, &local, None, None, true) { bindings.push(binding); }
        } else if module_path.starts_with("crate::") && namespace_called(input.node, &local, source) {
            let mut record = issue(resolver.relative(path).as_deref(), "unresolved_rust_module_import", "dependency",
                "A used Rust module import could not be resolved safely; cross-file call-sequence analysis may be incomplete.");
            record.line = Some(node.start_position().row as u32 + 1);
            coverage.push(record);
        }
        bindings
    }).collect()

}

fn namespace_called(root: Node<'_>, prefix: &str, source: &str) -> bool {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node.kind() == "call_expression" && node.child_by_field_name("function")
            .and_then(|function| text(function, source)).is_some_and(|name| name.starts_with(prefix)) { return true; }
        pending.extend(children(node));
    }
    false
}

// Resolve only declared external modules with a unique conventional path.
// Attributes can change the selected file or build configuration.
fn verified_namespace(resolver: &Resolver<'_>, path: &Path, qualified: &str) -> Option<PathBuf> {
    let suffix = qualified.strip_prefix("crate::")?;
    let parts: Vec<_> = suffix.split("::").collect();
    if parts.is_empty() || parts.iter().any(|part| part.is_empty() || !part.chars().all(|ch| ch.is_alphanumeric() || ch == '_')) { return None; }
    let dir = path.ancestors().skip(1).find(|parent| parent.starts_with(&resolver.root)
        && (parent.join("lib.rs").is_file() || parent.join("main.rs").is_file()))?;
    let roots: Vec<_> = [dir.join("lib.rs"), dir.join("main.rs")].into_iter().filter(|file| file.is_file()).collect();
    let [root] = roots.as_slice() else { return None };
    let mut current = root.clone();
    for name in parts {
        let parsed = resolver.cache.get(&current, Language::Rust)?;
        if parsed.1.root_node().has_error() { return None; }
        let modules: Vec<_> = children(parsed.1.root_node()).into_iter().filter(|node| node.kind() == "mod_item"
            && node.child_by_field_name("body").is_none()
            && node.child_by_field_name("name").and_then(|part| text(part, &parsed.0)) == Some(name)).collect();
        let [module] = modules.as_slice() else { return None };
        if has_attributes(*module) { return None; }
        let next = rust_module(resolver, &module_dir(&current)?.join(name))?;
        current = next;
    }
    Some(current)
}

fn has_attributes(node: Node<'_>) -> bool {
    std::iter::successors(node.prev_named_sibling(), |node| node.prev_named_sibling())
        .take_while(|node| matches!(node.kind(), "attribute_item" | "line_comment" | "block_comment"))
        .any(|node| node.kind() == "attribute_item")
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

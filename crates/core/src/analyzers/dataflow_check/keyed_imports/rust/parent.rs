use super::{children, has_attributes, parent_module, reexport_module, rust_paths, text, valid_name, Language, Node, Path, PathBuf, Resolver};

// Children can use a private name imported by their immediate parent. Follow
// only one unambiguous, unconditional module import to a verified module file;
// unmodeled imports and declarations must not turn into guessed targets.
pub(super) fn private_parent_namespace(resolver: &Resolver<'_>, path: &Path, qualified: &str) -> Option<PathBuf> {
    let name = qualified.strip_prefix("super::")?;
    if !valid_name(name) { return None; }
    let parent = parent_module(resolver, path)?;
    let parsed = resolver.cache.get(&parent, Language::Rust)?;
    let root = parsed.1.root_node();
    if root.has_error() { return None; }
    let matches = children(root).into_iter().try_fold(Vec::new(), |mut found, node| {
        found.extend(parent_item_targets(resolver, &parent, node, &parsed.0, name)?);
        Some(found)
    })?;
    let [target] = matches.as_slice() else { return None };
    Some(target.clone())
}

fn parent_item_targets(resolver: &Resolver<'_>, parent: &Path, node: Node<'_>, source: &str, name: &str) -> Option<Vec<PathBuf>> {
    if node.kind() != "use_declaration" {
        let macro_item = node.kind() == "macro_invocation" || (node.kind() == "expression_statement"
            && children(node).iter().any(|part| part.kind() == "macro_invocation"));
        let collision = node.child_by_field_name("name").and_then(|part| text(part, source)) == Some(name);
        return (!macro_item && !collision).then(Vec::new);
    }
    let usage = node.child_by_field_name("argument").and_then(|part| text(part, source))?;
    if usage.matches('{').count() > 1 || usage.matches('}').count() > 1 { return None; }
    let paths = rust_paths(usage.trim());
    if paths.is_empty() { return None; }
    paths.into_iter().try_fold(Vec::new(), |mut found, usage| {
        if let Some(target) = parent_import_target(resolver, parent, node, &usage, name)? { found.push(target); }
        Some(found)
    })
}

fn parent_import_target(resolver: &Resolver<'_>, parent: &Path, node: Node<'_>, usage: &str, name: &str) -> Option<Option<PathBuf>> {
    let (target, alias) = usage.split_once(" as ").map_or((usage, None), |(target, alias)| (target.trim(), Some(alias.trim())));
    let (module, member) = target.rsplit_once("::")?;
    if member == "*" { return None; }
    let default = if member == "self" { module.rsplit("::").next()? } else { member };
    if alias.unwrap_or(default) != name { return Some(None); }
    if has_attributes(node) { return None; }
    let target = if member == "self" { module } else { target };
    reexport_module(resolver, parent, target).map(Some)
}


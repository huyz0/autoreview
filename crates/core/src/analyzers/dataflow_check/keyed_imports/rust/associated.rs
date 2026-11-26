use std::collections::HashSet;
use super::{children, has_attributes, reexport_module, rust_paths, text, Language, Node, Path, PathBuf, Resolver};

type Binding = (PathBuf, String);

// Follow unique explicit re-exports through verified module edges.
pub(super) fn associated_type_origin(resolver: &Resolver<'_>, path: &Path, name: &str) -> Option<Binding> {
    let mut current = (path.to_path_buf(), name.to_owned());
    let mut seen = HashSet::new();
    for _ in 0..32 {
        if !seen.insert((resolver.relative(&current.0)?, current.1.clone())) { return None; }
        match associated_step(resolver, &current.0, &current.1)? {
            Step::Found => return Some(current),
            Step::Next(next) => current = next,
        }
    }
    None
}

enum Step { Found, Next(Binding) }

fn associated_step(resolver: &Resolver<'_>, path: &Path, name: &str) -> Option<Step> {
    let parsed = resolver.cache.get(path, Language::Rust)?;
    let root = parsed.1.root_node();
    if root.has_error() || has_module_macro(root) { return None; }
    let next = explicit_export(resolver, path, root, &parsed.0, name)?;
    if local_associated_type(root, &parsed.0, name) {
        return next.is_none().then_some(Step::Found);
    }
    if children(root).into_iter().any(|node| node.child_by_field_name("name")
        .and_then(|part| text(part, &parsed.0)) == Some(name)) { return None; }
    next.map(Step::Next)
}

fn has_module_macro(root: Node<'_>) -> bool {
    children(root).into_iter().any(|node| node.kind() == "macro_invocation"
        || (node.kind() == "expression_statement" && children(node).into_iter().any(|part| part.kind() == "macro_invocation")))
}

// Outer None is an unsupported or ambiguous binding; inner None means absent.
fn explicit_export(resolver: &Resolver<'_>, path: &Path, root: Node<'_>, source: &str, name: &str) -> Option<Option<Binding>> {
    let mut found = None;
    for node in children(root).into_iter().filter(|node| node.kind() == "use_declaration") {
        if let Some(binding) = export_from_use(resolver, path, node, source, name)? {
            if found.replace(binding).is_some() { return None; }
        }
    }
    Some(found)
}

fn export_from_use(resolver: &Resolver<'_>, path: &Path, node: Node<'_>, source: &str, name: &str) -> Option<Option<Binding>> {
    let usage = node.child_by_field_name("argument").and_then(|part| text(part, source))?;
    if usage.matches('{').count() > 1 { return None; }
    let paths = rust_paths(usage.trim());
    if paths.is_empty() { return None; }
    let mut found = None;
    for usage in paths {
        if let Some(binding) = export_path(resolver, path, node, source, name, &usage)? {
            if found.replace(binding).is_some() { return None; }
        }
    }
    Some(found)
}

fn export_path(resolver: &Resolver<'_>, path: &Path, node: Node<'_>, source: &str, name: &str, usage: &str) -> Option<Option<Binding>> {
    let (qualified, alias) = usage.split_once(" as ").map_or((usage, None), |(path, alias)| (path.trim(), Some(alias.trim())));
    let (module, imported) = qualified.rsplit_once("::")?;
    if imported == "*" || imported.contains(['{', '}']) { return None; }
    if alias.unwrap_or(imported) != name { return Some(None); }
    let visibility = children(node).into_iter().find(|part| part.kind() == "visibility_modifier")
        .and_then(|part| text(part, source));
    if has_attributes(node) || !matches!(visibility, Some("pub" | "pub(crate)")) { return None; }
    Some(Some((reexport_module(resolver, path, module)?, imported.to_owned())))
}

fn local_associated_type(root: Node<'_>, source: &str, name: &str) -> bool {
    let matching: Vec<_> = children(root).into_iter().filter(|node|
        node.child_by_field_name("name").and_then(|part| text(part, source)) == Some(name)).collect();
    matches!(matching.as_slice(), [node] if matches!(node.kind(), "struct_item" | "enum_item")
        && !has_attributes(*node) && node.child_by_field_name("type_parameters").is_none()
        && children(*node).into_iter().find(|child| child.kind() == "visibility_modifier")
            .and_then(|visibility| text(visibility, source)).is_some_and(|visibility| matches!(visibility, "pub" | "pub(crate)")))
}

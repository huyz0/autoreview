use super::{
    children, has_attributes, reexport_module, rust_paths, text, Language, Node, Path, PathBuf,
    Resolver,
};
use std::collections::HashSet;

type Binding = (PathBuf, String);

// Only defer import coverage to call resolution when every matching call is
// in a plainly lowered top-level function body. Other syntax keeps the
// conservative import warning, even if some calls resolve successfully.
pub(super) fn simple_calls(root: Node<'_>, prefix: &str, source: &str) -> bool {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node.kind() == "call_expression" {
            if let Some(member) = node.child_by_field_name("function").and_then(|part| text(part, source))
                .and_then(|name| name.strip_prefix(prefix)) {
                if member.is_empty() || !member.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
                    || !simple_call(node, source) { return false; }
            }
        }
        pending.extend(children(node));
    }
    true
}

fn simple_call(node: Node<'_>, source: &str) -> bool {
    let mut parent = node.parent();
    while let Some(node) = parent {
        match node.kind() {
            "function_item" => {
                let Some(name) = node.child_by_field_name("name") else { return false };
                let signature = source.get(node.start_byte()..name.start_byte()).unwrap_or_default();
                return node.parent().is_some_and(|root| root.kind() == "source_file")
                    && !signature.split(|ch: char| !ch.is_ascii_alphanumeric()).any(|word| word == "async");
            }
            "block" | "expression_statement" | "return_expression" => parent = node.parent(),
            _ => return false,
        }
    }
    false
}

// Follow unique explicit re-exports through verified module edges.
pub(super) fn associated_type_origin(
    resolver: &Resolver<'_>,
    path: &Path,
    name: &str,
) -> Option<Binding> {
    let mut current = (path.to_path_buf(), name.to_owned());
    let mut seen = HashSet::new();
    for _ in 0..32 {
        if !seen.insert((resolver.relative(&current.0)?, current.1.clone())) {
            return None;
        }
        match associated_step(resolver, &current.0, &current.1)? {
            Step::Found => return Some(current),
            Step::Next(next) => current = next,
        }
    }
    None
}

enum Step {
    Found,
    Next(Binding),
}

fn associated_step(resolver: &Resolver<'_>, path: &Path, name: &str) -> Option<Step> {
    let parsed = resolver.cache.get(path, Language::Rust)?;
    let root = parsed.1.root_node();
    if root.has_error() || has_module_macro(root) {
        return None;
    }
    let next = ExportLookup {
        resolver,
        path,
        source: &parsed.0,
        name,
    }
    .explicit_export(root)?;
    if local_associated_type(root, &parsed.0, name) {
        return next.is_none().then_some(Step::Found);
    }
    if children(root).into_iter().any(|node| {
        node.child_by_field_name("name")
            .and_then(|part| text(part, &parsed.0))
            == Some(name)
    }) {
        return None;
    }
    next.map(Step::Next)
}

fn has_module_macro(root: Node<'_>) -> bool {
    children(root).into_iter().any(|node| {
        node.kind() == "macro_invocation"
            || (node.kind() == "expression_statement"
                && children(node)
                    .into_iter()
                    .any(|part| part.kind() == "macro_invocation"))
    })
}

struct ExportLookup<'a, 'b> {
    resolver: &'a Resolver<'b>,
    path: &'a Path,
    source: &'a str,
    name: &'a str,
}

// Outer None is unsupported or ambiguous; inner None means absent.
fn unique_bindings(
    bindings: impl Iterator<Item = Option<Option<Binding>>>,
) -> Option<Option<Binding>> {
    let mut found = None;
    for binding in bindings {
        if let Some(binding) = binding? {
            if found.replace(binding).is_some() {
                return None;
            }
        }
    }
    Some(found)
}

impl ExportLookup<'_, '_> {
    fn explicit_export(&self, root: Node<'_>) -> Option<Option<Binding>> {
        unique_bindings(
            children(root)
                .into_iter()
                .filter(|node| node.kind() == "use_declaration")
                .map(|node| self.export_from_use(node)),
        )
    }

    fn export_from_use(&self, node: Node<'_>) -> Option<Option<Binding>> {
        let usage = node
            .child_by_field_name("argument")
            .and_then(|part| text(part, self.source))?;
        if usage.matches('{').count() > 1 {
            return None;
        }
        let paths = rust_paths(usage.trim());
        if paths.is_empty() {
            return None;
        }
        unique_bindings(
            paths
                .into_iter()
                .map(|usage| self.export_path(node, &usage)),
        )
    }

    fn export_path(&self, node: Node<'_>, usage: &str) -> Option<Option<Binding>> {
        let (qualified, alias) = usage
            .split_once(" as ")
            .map_or((usage, None), |(path, alias)| {
                (path.trim(), Some(alias.trim()))
            });
        let (module, imported) = qualified.rsplit_once("::")?;
        if imported == "*" || imported.contains(['{', '}']) {
            return None;
        }
        if alias.unwrap_or(imported) != self.name {
            return Some(None);
        }
        let visibility = children(node)
            .into_iter()
            .find(|part| part.kind() == "visibility_modifier")
            .and_then(|part| text(part, self.source));
        if has_attributes(node) || !matches!(visibility, Some("pub" | "pub(crate)")) {
            return None;
        }
        Some(Some((
            reexport_module(self.resolver, self.path, module)?,
            imported.to_owned(),
        )))
    }
}

fn local_associated_type(root: Node<'_>, source: &str, name: &str) -> bool {
    let matching: Vec<_> = children(root)
        .into_iter()
        .filter(|node| {
            node.child_by_field_name("name")
                .and_then(|part| text(part, source))
                == Some(name)
        })
        .collect();
    matches!(matching.as_slice(), [node] if matches!(node.kind(), "struct_item" | "enum_item")
        && !has_attributes(*node) && node.child_by_field_name("type_parameters").is_none()
        && children(*node).into_iter().find(|child| child.kind() == "visibility_modifier")
            .and_then(|visibility| text(visibility, source)).is_some_and(|visibility| matches!(visibility, "pub" | "pub(crate)")))
}

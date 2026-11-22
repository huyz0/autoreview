use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

mod bindings;
mod indexing;
mod string_types;
use bindings::visible_local;

pub(in crate::analyzers) fn children(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .collect::<Vec<_>>()
        .into_iter()
}

pub(in crate::analyzers) fn descendants(root: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut pending = vec![root];
    std::iter::from_fn(move || {
        let next = pending.pop()?;
        let mut cursor = next.walk();
        let start = pending.len();
        pending.extend(next.named_children(&mut cursor));
        pending[start..].reverse();
        Some(next)
    })
}

pub(super) fn ancestors(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    std::iter::successors(node.parent(), |parent| parent.parent())
}

pub(in crate::analyzers) fn is_comment(node: Node<'_>) -> bool {
    matches!(node.kind(), "line_comment" | "block_comment")
}

pub(super) fn is_execution_boundary(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "function_item"
            | "async_block"
            | "closure_expression"
            | "const_block"
            | "const_item"
            | "static_item"
            | "gen_block"
    )
}

pub(in crate::analyzers) fn attributes(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    std::iter::successors(node.prev_named_sibling(), |prev| prev.prev_named_sibling())
        .take_while(|prev| prev.kind() == "attribute_item" || is_comment(*prev))
        .filter(|prev| prev.kind() == "attribute_item")
        .filter_map(|item| children(item).find(|child| child.kind() == "attribute"))
}

pub(in crate::analyzers) fn is_test_item(node: Node<'_>, text: &str) -> bool {
    attributes(node).any(|attribute| {
        let name: String = text[attribute.byte_range()].split_whitespace().collect();
        matches!(name.as_str(), "cfg(test)" | "test" | "tokio::test" | "async_std::test")
            || name.starts_with("tokio::test(")
            || name.starts_with("async_std::test(")
    })
}

pub(super) fn arguments(call: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    call.child_by_field_name("arguments")
        .into_iter()
        .flat_map(children)
        .filter(|node| !is_comment(*node))
}

pub(super) fn method_call(call: Node<'_>) -> Option<(Node<'_>, Node<'_>)> {
    if call.kind() != "call_expression" {
        return None;
    }
    let function = call.child_by_field_name("function")?;
    let function = if function.kind() == "generic_function" {
        function.child_by_field_name("function")?
    } else {
        function
    };
    if function.kind() != "field_expression" {
        return None;
    }
    Some((
        function.child_by_field_name("value")?,
        function.child_by_field_name("field")?,
    ))
}

fn is_module_boundary(node: Node<'_>) -> bool {
    node.kind() == "declaration_list"
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "mod_item")
}

#[derive(Clone)]
struct Path {
    absolute: bool,
    parts: Vec<String>,
}

impl Path {
    fn empty() -> Self {
        Self {
            absolute: false,
            parts: Vec::new(),
        }
    }

    fn join(mut self, suffix: Path) -> Self {
        if suffix.absolute {
            return suffix;
        }
        self.parts.extend(suffix.parts);
        self
    }
}

#[derive(Default)]
struct Scope<'tree> {
    imports: HashMap<String, Option<Path>>,
    items: HashSet<String>,
    locals: HashMap<String, Vec<Node<'tree>>>,
    glob: bool,
    opaque_macros: bool,
}

pub(in crate::analyzers) struct Source<'tree, 'text> {
    input: &'text str,
    scopes: HashMap<usize, Scope<'tree>>,
    defined_macros: HashSet<&'text str>,
}

impl<'tree, 'text> Source<'tree, 'text> {
    pub(in crate::analyzers) fn new(root: Node<'tree>, input: &'text str) -> Self {
        let mut source = Self {
            input,
            scopes: HashMap::new(),
            defined_macros: HashSet::new(),
        };
        for node in descendants(root) {
            match node.kind() {
                "source_file" | "block" | "declaration_list" => {
                    source.scopes.insert(node.id(), source.index_scope(node));
                }
                "macro_definition" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        source.defined_macros.insert(identifier(source.text(name)));
                    }
                }
                _ => {}
            }
        }
        source
    }

    pub(in crate::analyzers) fn text(&self, node: Node<'_>) -> &'text str {
        &self.input[node.byte_range()]
    }

    pub(super) fn is_test_item(&self, node: Node<'_>) -> bool {
        is_test_item(node, self.input)
    }

    pub(super) fn external_path(&self, node: Node<'tree>) -> Option<String> {
        let mut reference = self.path(node)?;
        let head = reference.parts.first()?;
        if reference.absolute {
            return known_crate(head).then(|| reference.parts.join("::"));
        }
        let mut locals_visible = true;
        for scope_node in ancestors(node) {
            if self.binds_type_parameter(scope_node, head)
                || locals_visible && self.binds_in_context(scope_node, node, head)
            {
                return None;
            }
            if scope_node.kind() == "function_item" {
                locals_visible = false;
            }
            if let Some(scope) = self.scopes.get(&scope_node.id()) {
                if scope.items.contains(head)
                    || locals_visible && visible_local(scope, head, node).is_some()
                {
                    return None;
                }
                if let Some(imported) = scope.imports.get(head) {
                    let imported = imported.as_ref()?;
                    let root = imported.parts.first()?;
                    if !known_crate(root)
                        || !imported.absolute && self.crate_root_is_ambiguous(scope_node, root)
                    {
                        return None;
                    }
                    reference.parts.splice(..1, imported.parts.iter().cloned());
                    return Some(reference.parts.join("::"));
                }
                if scope.glob {
                    return None;
                }
            }
            if is_module_boundary(scope_node) {
                break;
            }
        }
        (reference.parts.len() > 1 && known_crate(head)).then(|| reference.parts.join("::"))
    }

    pub(in crate::analyzers) fn standard_macro(&self, node: Node<'tree>, names: &[&str]) -> bool {
        // Textual macros can shadow imports, and an exported definition
        // can enter scope from a nested module.
        if node.kind() == "identifier"
            && (self.defined_macros.contains(identifier(self.text(node)))
                || ancestors(node).any(|parent| {
                    self.scopes
                        .get(&parent.id())
                        .is_some_and(|scope| scope.opaque_macros)
                }))
        {
            return false;
        }
        if let Some(path) = self.external_path(node) {
            return path
                .strip_prefix("std::")
                .or_else(|| path.strip_prefix("core::"))
                .is_some_and(|name| names.contains(&name));
        }
        if node.kind() != "identifier" || !names.contains(&self.text(node)) {
            return false;
        }
        // Textual macro scope can reach nested modules.
        self.unshadowed_prelude(node, self.text(node))
    }

    pub(super) fn unshadowed_prelude(&self, node: Node<'tree>, name: &str) -> bool {
        matches!(node.kind(), "identifier" | "type_identifier")
            && self.text(node) == name
            && !ancestors(node).any(|parent| {
                self.binds_type_parameter(parent, name)
                    || self.scopes.get(&parent.id()).is_some_and(|scope| {
                        scope.items.contains(name)
                            || scope.imports.contains_key(name)
                            || scope.glob
                            || scope.opaque_macros
                    })
            })
    }

    fn binds_type_parameter(&self, scope: Node<'tree>, name: &str) -> bool {
        scope
            .child_by_field_name("type_parameters")
            .into_iter()
            .flat_map(children)
            .filter_map(|parameter| parameter.child_by_field_name("name"))
            .any(|parameter| identifier(self.text(parameter)) == name)
    }

    fn crate_root_is_ambiguous(&self, scope_node: Node<'tree>, name: &str) -> bool {
        for node in std::iter::once(scope_node).chain(ancestors(scope_node)) {
            if self.scopes.get(&node.id()).is_some_and(|scope| {
                scope.items.contains(name)
                    || scope.glob
                    || scope
                        .imports
                        .get(name)
                        .is_some_and(|root| root.as_ref().is_none_or(|root| root.parts != [name]))
            }) {
                return true;
            }
            if is_module_boundary(node) {
                break;
            }
        }
        false
    }

}

fn identifier(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

fn known_crate(name: &str) -> bool {
    matches!(
        name,
        "std"
            | "core"
            | "alloc"
            | "tokio"
            | "sqlx"
            | "diesel"
            | "rusqlite"
            | "mockall"
            | "reqwest"
            | "tracing"
            | "tempfile"
            | "rustls"
            | "secrecy"
            | "axum"
            | "actix_web"
    )
}

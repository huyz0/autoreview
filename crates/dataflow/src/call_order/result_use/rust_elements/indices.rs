use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use crate::lower::{rust::element_access, site::children};

use super::{descendants, in_body, Syntax};

pub(super) struct Indices<'a, 'src> {
    source: &'src [u8],
    syntax: &'a Syntax<'src>,
    values: HashMap<usize, usize>,
    raw_names: HashSet<&'src str>,
}

impl<'a, 'src> Indices<'a, 'src> {
    pub(super) fn scan(body: Node<'_>, source: &'src [u8], syntax: &'a Syntax<'src>) -> Self {
        let raw_names = descendants(body)
            .filter(|node| node.kind() == "identifier")
            .filter_map(|node| node.utf8_text(source).ok()?.strip_prefix("r#"))
            .collect();
        let mut indices = Self { source, syntax, values: HashMap::new(), raw_names };
        // The lexical resolver hides the new binding while its initializer is evaluated.
        for declaration in descendants(body).filter(|node| node.kind() == "let_declaration" && in_body(*node, body)) {
            if let Some((binding, value)) = indices.initializer(declaration) {
                indices.values.insert(binding, value);
            }
        }
        indices
    }

    pub(super) fn value(&self, node: Node<'_>) -> Option<usize> {
        let value = element_access::ungroup(node);
        if value.kind() != "identifier" { return None; }
        let name = value.utf8_text(self.source).ok()?;
        if name.starts_with("r#") || self.raw_names.contains(name) { return None; }
        let at = value.start_byte();
        let binding = self.syntax.local_result_binding(name, at, false)?;
        let index = *self.values.get(&binding)?;
        if self.syntax.local_import_shadows(name, at) || may_shadow(value, name, binding, self.source) {
            return None;
        }
        Some(index)
    }

    fn initializer(&self, declaration: Node<'_>) -> Option<(usize, usize)> {
        let binding = element_access::immutable_let(declaration)?;
        if has_attributes(declaration) { return None; }
        let initializer = declaration.child_by_field_name("value")?;
        let index = element_access::literal_usize(initializer, self.source).or_else(|| self.value(initializer))?;
        Some((binding.start_byte(), index))
    }
}

fn has_attributes(declaration: Node<'_>) -> bool {
    std::iter::successors(declaration.prev_named_sibling(), Node::prev_named_sibling)
        .find(|node| !matches!(node.kind(), "line_comment" | "block_comment"))
        .is_some_and(|node| node.kind() == "attribute_item")
}

fn may_shadow(node: Node<'_>, name: &str, binding: usize, source: &[u8]) -> bool {
    for scope in std::iter::successors(node.parent(), Node::parent).filter(|node| node.kind() == "block") {
        let within_binding_scope = scope.byte_range().contains(&binding);
        // A macro in an inner block can introduce an item whose scope covers the entire block.
        let after_binding = within_binding_scope.then_some(binding..node.start_byte());
        if children(scope).any(|child| shadows_index(child, name, after_binding.as_ref(), source)) { return true; }
        if within_binding_scope { break; }
    }
    false
}

fn shadows_index(statement: Node<'_>, name: &str, after_binding: Option<&std::ops::Range<usize>>, source: &[u8]) -> bool {
    match statement.kind() {
        "const_item" | "static_item" => statement.child_by_field_name("name")
            .is_some_and(|node| node.utf8_text(source) == Ok(name)),
        "macro_invocation" | "expression_statement" if after_binding.is_none_or(|range| range.contains(&statement.start_byte())) => {
            statement.kind() == "macro_invocation"
                || children(statement).next().is_some_and(|node| node.kind() == "macro_invocation")
        }
        _ => false,
    }
}

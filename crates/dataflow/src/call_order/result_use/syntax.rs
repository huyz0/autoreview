use tree_sitter::Node;

use crate::lower::{keyed_syntax::assignment_parts, site};

pub(super) fn descendants(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut stack = vec![node];
    std::iter::from_fn(move || {
        let node = stack.pop()?;
        let children: Vec<_> = site::children(node).collect();
        stack.extend(children.into_iter().rev());
        Some(node)
    })
}

pub(super) fn transparent(mut node: Node<'_>) -> Node<'_> {
    while matches!(
        node.kind(),
        "parenthesized_expression" | "as_expression" | "satisfies_expression" | "non_null_expression"
    ) {
        let Some(inner) = site::children(node).next() else { break };
        node = inner;
    }
    node
}

pub(super) fn items(node: Node<'_>) -> Vec<Node<'_>> {
    if node.kind() == "expression_list" {
        return site::children(node).collect();
    }
    vec![node]
}

pub(super) fn initializers<'tree>(node: Node<'tree>, source: &[u8]) -> Vec<(Node<'tree>, Node<'tree>)> {
    let Some((target, Some(value), false)) = assignment_parts(node, source) else { return Vec::new() };
    let targets = targets(node, target);
    let values = items(value);
    if targets.len() != values.len() {
        return Vec::new();
    }
    targets.into_iter().zip(values.into_iter().map(transparent)).collect()
}

pub(super) fn targets<'tree>(node: Node<'tree>, target: Node<'tree>) -> Vec<Node<'tree>> {
    if node.kind() == "var_spec" {
        return node.children_by_field_name("name", &mut node.walk()).collect();
    }
    items(target)
}

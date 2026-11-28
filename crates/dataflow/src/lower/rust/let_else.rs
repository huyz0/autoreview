use super::{line, match_flow, Bindings, EdgeKind, Lower, Node, NodeId};
use crate::lower::site::children;

pub(crate) fn bindings(node: Node<'_>) -> Option<Vec<Node<'_>>> {
    supports(node).then(|| pattern_bindings(node)).flatten()
}

fn pattern_bindings(node: Node<'_>) -> Option<Vec<Node<'_>>> {
    if node.kind() != "let_declaration" || node.child_by_field_name("alternative").is_none() {
        return None;
    }
    match_flow::projection_bindings(node)
}

pub(super) fn supports(node: Node<'_>) -> bool {
    pattern_bindings(node).is_some()
        && node
            .child_by_field_name("value")
            .is_some_and(match_flow::simple_value)
        && node
            .child_by_field_name("alternative")
            .and_then(|block| children(block).last())
            .and_then(|statement| {
                if statement.kind() == "expression_statement" {
                    children(statement).next()
                } else {
                    Some(statement)
                }
            })
            .is_some_and(|tail| {
                matches!(
                    tail.kind(),
                    "return_expression" | "break_expression" | "continue_expression"
                )
            })
}

impl Lower<'_> {
    pub(super) fn lower_let_else(
        &mut self,
        node: Node<'_>,
        at: NodeId,
        names: &mut Bindings,
    ) -> Option<NodeId> {
        let otherwise = node.child_by_field_name("alternative")?;
        let tested = self.evaluate(node.child_by_field_name("value"), at, names);
        let unmatched = self.cfg.push_node(line(otherwise));
        if node.child_by_field_name("pattern").is_none_or(|pattern| pattern.kind() != "_") {
            self.edge(tested, unmatched, EdgeKind::False);
        }
        self.block(otherwise, unmatched, names);
        let matched = self.cfg.push_node(line(node));
        self.edge(tested, matched, EdgeKind::True);
        for binding in pattern_bindings(node)? {
            self.project_pattern_binding(binding, matched, names);
        }
        Some(matched)
    }
}

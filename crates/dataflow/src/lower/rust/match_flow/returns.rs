use crate::lower::{
    line,
    rust::{Bindings, Lower, Node, NodeId},
    site,
};

use super::simple_value;

pub(super) fn statement_match(parent: Node<'_>) -> bool {
    parent.kind() == "expression_statement"
        && parent.parent().is_some_and(|block| block.kind() == "block")
        && (has_semicolon(parent) || !last_statement(parent))
}

pub(in crate::lower::rust) fn returns_match(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() == "return_expression" {
        return true;
    }
    let expression = if parent.kind() == "expression_statement" && !has_semicolon(parent) {
        parent
    } else {
        node
    };
    last_statement(expression)
        && expression.parent().is_some_and(|block| {
            block.kind() == "block"
                && block.parent().is_some_and(|function| {
                    function.kind() == "function_item"
                        && function.child_by_field_name("body") == Some(block)
                })
        })
}

fn has_semicolon(node: Node<'_>) -> bool {
    node.children(&mut node.walk())
        .any(|part| part.kind() == ";")
}

fn last_statement(node: Node<'_>) -> bool {
    std::iter::successors(node.next_named_sibling(), Node::next_named_sibling)
        .all(|part| matches!(part.kind(), "line_comment" | "block_comment"))
}

impl Lower<'_> {
    pub(super) fn return_match_arm(&mut self, arm: Node<'_>, mut at: NodeId, names: &mut Bindings) {
        if arm.kind() != "block" {
            self.return_match_value(arm, at, names);
            return;
        }
        let mut statements = site::children(arm).peekable();
        while let Some(statement) = statements.next() {
            if statements.peek().is_none() && simple_value(statement) {
                self.return_match_value(statement, at, names);
                return;
            }
            let Some(end) = self.statement(statement, at, names) else {
                return;
            };
            at = end;
        }
        self.finish(at, line(arm));
    }

    fn return_match_value(&mut self, value: Node<'_>, at: NodeId, names: &mut Bindings) {
        if value.kind() == "return_expression" {
            self.statement(value, at, names);
            return;
        }
        let end = self.evaluate(Some(value), at, names);
        if let Some(evaluation) = self.cfg.nodes[end].evaluations.last_mut() {
            evaluation.returned = true;
        }
        self.return_identity(value, end);
        self.finish(end, line(value));
    }
}

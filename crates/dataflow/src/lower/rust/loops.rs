use super::{argument_flow, branch_value::Mode, line, Bindings, EdgeKind, Lower, Node, NodeId};
use crate::cfg::{RhsShape, Stmt};
use crate::lower::site::children;

pub(super) struct JumpTarget {
    label: Option<String>,
    head: Option<NodeId>,
    after: NodeId,
}

fn label(owner: Node<'_>) -> Option<Node<'_>> {
    children(owner).find(|child| child.kind() == "label")
}

fn is_loop(node: Node<'_>) -> bool {
    matches!(node.kind(), "for_expression" | "while_expression" | "loop_expression")
}

pub(crate) fn is_labeled_block(node: Node<'_>) -> bool {
    node.kind() == "block" && label(node).is_some()
}

pub(crate) fn block_binding(block: Node<'_>) -> String {
    format!("{}-block@{}", super::OPERAND_TEMPORARY, block.start_byte())
}

fn traversable(parent: Node<'_>, jump: Node<'_>, source: &[u8]) -> bool {
    match parent.kind() {
        "function_item" | "closure_expression" | "async_block" | "const_block" | "try_block" | "gen_block" => false,
        "while_expression" | "if_expression" if contains_condition(parent, jump) => {
            super::conditional::control_test(parent, source).is_some()
        }
        "for_expression" => parent.child_by_field_name("body")
            .is_some_and(|body| body.byte_range().contains(&jump.start_byte())),
        _ => true,
    }
}

fn contains_condition(owner: Node<'_>, jump: Node<'_>) -> bool {
    owner.child_by_field_name("condition")
        .is_some_and(|condition| condition.byte_range().contains(&jump.start_byte()))
}

pub(crate) fn supports_label(node: Node<'_>, source: &[u8]) -> bool {
    let Some(owner) = node.parent() else { return false };
    match owner.kind() {
        "block" => argument_flow::can_evaluate(owner) && super::branch_value::supports_block_tail(owner, source),
        "break_expression" | "continue_expression" => {
            let Ok(name) = node.utf8_text(source) else { return false };
            std::iter::successors(owner.parent(), |parent| parent.parent())
                .take_while(|parent| traversable(*parent, owner, source))
                .find(|parent| label(*parent).is_some_and(|candidate| candidate.utf8_text(source) == Ok(name)))
                .is_some_and(|target| is_loop(target) || (owner.kind() == "break_expression" && is_labeled_block(target)))
        }
        _ => is_loop(owner),
    }
}

impl Lower<'_> {
    pub(super) fn has_incoming(&self, target: NodeId) -> bool {
        self.cfg.edges.iter().any(|(_, to, _)| *to == target)
    }

    pub(super) fn push_loop(&mut self, owner: Node<'_>, head: NodeId, after: NodeId) {
        let label = label(owner).map(|node| self.text(node).to_owned());
        self.loops.push(JumpTarget { label, head: Some(head), after });
    }

    pub(super) fn labeled_block(
        &mut self, block: Node<'_>, at: NodeId, names: &Bindings, target: Option<String>,
    ) -> Option<NodeId> {
        let after = self.cfg.push_node(block.end_position().row as u32 + 1);
        let label = label(block).map(|node| self.text(node).to_owned());
        self.loops.push(JumpTarget { label, head: None, after });
        let temporary = block_binding(block);
        let mode = Mode::Operand { syntax: block, temporary: &temporary };
        if let Some(end) = self.lower_value_block(block, at, &mut names.clone(), &mode) {
            self.edge(end, after, EdgeKind::Fallthrough);
        }
        self.loops.pop();
        if let Some(lhs) = target {
            self.cfg.emit(after, Stmt::Assign { lhs, rhs: RhsShape::Var(temporary) }, line(block));
        }
        self.has_incoming(after).then_some(after)
    }

    pub(super) fn jump(&mut self, stmt: Node<'_>, at: NodeId, names: &Bindings) {
        let named = label(stmt).map(|node| self.text(node));
        let target = self.loops.iter().rev().find(|target| match named {
            Some(name) => target.label.as_deref() == Some(name),
            None => target.head.is_some(),
        });
        let Some(target) = target else { return };
        let (head, after) = (target.head, target.after);
        let value = children(stmt).find(|node| node.kind() != "label");
        if head.is_none() {
            self.break_from_block(stmt, value, at, after, names);
        } else if let Some(at) = self.value(value, at, names, None) {
            crate::lower::record_evaluation(&mut self.cfg, at, stmt);
            if stmt.kind() == "break_expression" {
                self.edge(at, after, EdgeKind::Fallthrough);
            } else if let Some(head) = head {
                self.edge(at, head, EdgeKind::Loop);
            }
        }
    }

    fn break_from_block(&mut self, stmt: Node<'_>, value: Option<Node<'_>>, at: NodeId, after: NodeId, names: &Bindings) {
        let named = label(stmt).map(|node| self.text(node));
        let block = std::iter::successors(stmt.parent(), |parent| parent.parent())
            .find(|node| is_labeled_block(*node) && label(*node).map(|label| self.text(label)) == named);
        if let Some(block) = block {
            if let Some(end) = self.complete_operand(block, value, &block_binding(block), at, names) {
                self.edge(end, after, EdgeKind::Fallthrough);
            }
        }
    }
}

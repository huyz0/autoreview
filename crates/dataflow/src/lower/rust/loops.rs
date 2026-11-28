use super::{Bindings, EdgeKind, Lower, Node, NodeId};
use crate::lower::site::children;

pub(super) struct LoopTarget {
    label: Option<String>,
    head: NodeId,
    after: NodeId,
}

fn label(owner: Node<'_>) -> Option<Node<'_>> {
    children(owner).find(|child| child.kind() == "label")
}

fn loop_expression(node: Node<'_>) -> bool {
    matches!(node.kind(), "for_expression" | "while_expression" | "loop_expression")
}

pub(crate) fn supports_label(node: Node<'_>, source: &[u8]) -> bool {
    let Some(owner) = node.parent() else { return false };
    if loop_expression(owner) {
        return true;
    }
    if !matches!(owner.kind(), "break_expression" | "continue_expression") {
        return false;
    }
    let Ok(name) = node.utf8_text(source) else { return false };
    std::iter::successors(owner.parent(), |parent| parent.parent())
        .take_while(|parent| !matches!(parent.kind(), "function_item" | "closure_expression" | "async_block"))
        .find(|parent| label(*parent).is_some_and(|candidate| candidate.utf8_text(source) == Ok(name)))
        .is_some_and(loop_expression)
}

impl Lower<'_> {
    pub(super) fn push_loop(&mut self, owner: Node<'_>, head: NodeId, after: NodeId) {
        let label = label(owner).map(|node| self.text(node).to_owned());
        self.loops.push(LoopTarget { label, head, after });
    }

    pub(super) fn jump(&mut self, stmt: Node<'_>, at: NodeId, names: &Bindings) {
        let value = children(stmt).find(|node| node.kind() != "label");
        let Some(at) = self.value(value, at, names, None) else { return };
        crate::lower::record_evaluation(&mut self.cfg, at, stmt);
        let label = label(stmt).map(|node| self.text(node));
        if let Some(target) = self.loops.iter().rev()
            .find(|target| label.is_none_or(|name| target.label.as_deref() == Some(name)))
        {
            if stmt.kind() == "break_expression" {
                self.edge(at, target.after, EdgeKind::Fallthrough);
            } else {
                self.edge(at, target.head, EdgeKind::Loop);
            }
        }
    }
}

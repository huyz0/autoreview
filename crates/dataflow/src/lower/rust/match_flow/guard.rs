use super::{simple_arm, simple_value, Bindings, EdgeKind, Lower, Node, NodeId};
use crate::lower::rust::branch_value::Mode;

enum Predicate<'a> {
    And(Node<'a>, Node<'a>),
    Or(Node<'a>, Node<'a>),
    Comparison(Node<'a>, Node<'a>),
    Not(Node<'a>),
    Block(Node<'a>),
    Value(Node<'a>),
}

fn negated(node: Node<'_>) -> Option<Node<'_>> {
    let negation = node.kind() == "unary_expression"
        && node.children(&mut node.walk()).any(|child| child.kind() == "!");
    negation.then(|| crate::lower::site::children(node).next()).flatten()
}

fn predicate(node: Node<'_>) -> Predicate<'_> {
    let node = crate::lower::rust::branch_value::ungroup_value(node);
    if let Some(value) = negated(node) {
        return Predicate::Not(value);
    }
    if let Some((left, right)) = node
        .child_by_field_name("left")
        .zip(node.child_by_field_name("right"))
    {
        match node
            .child_by_field_name("operator")
            .map(|operator| operator.kind())
        {
            Some("&&") => return Predicate::And(left, right),
            Some("||") => return Predicate::Or(left, right),
            Some("==" | "!=" | "<" | ">" | "<=" | ">=") => {
                return Predicate::Comparison(left, right)
            }
            _ => {}
        }
    }
    if node.kind() == "block" && !crate::lower::rust::is_labeled_block(node) {
        Predicate::Block(node)
    } else {
        Predicate::Value(node)
    }
}

pub(in crate::lower::rust) fn supported(node: Node<'_>, source: &[u8]) -> bool {
    match predicate(node) {
        Predicate::And(left, right) | Predicate::Or(left, right) => {
            supported(left, source) && supported(right, source)
        }
        Predicate::Not(value) => supported(value, source),
        Predicate::Comparison(left, right) => simple_value(left, source) && simple_value(right, source),
        Predicate::Block(block) => simple_arm(block, source),
        Predicate::Value(value) => simple_value(value, source),
    }
}

pub(in crate::lower::rust) fn has_control_flow(node: Node<'_>) -> bool {
    match predicate(node) {
        Predicate::And(_, _) | Predicate::Or(_, _) | Predicate::Block(_) => true,
        Predicate::Not(value) => has_control_flow(value),
        _ => false,
    }
}

impl Lower<'_> {
    pub(in crate::lower::rust) fn lower_guard(
        &mut self,
        node: Node<'_>,
        at: NodeId,
        targets: (NodeId, NodeId),
        names: &Bindings,
    ) {
        let (pass, fail) = targets;
        match predicate(node) {
            Predicate::And(left, right) => {
                let next = self.cfg.push_node(crate::lower::line(right));
                self.lower_guard(left, at, (next, fail), names);
                self.lower_guard(right, next, targets, names);
            }
            Predicate::Or(left, right) => {
                let next = self.cfg.push_node(crate::lower::line(right));
                self.lower_guard(left, at, (pass, next), names);
                self.lower_guard(right, next, targets, names);
            }
            Predicate::Not(value) => self.lower_guard(value, at, (fail, pass), names),
            Predicate::Comparison(left, right) => {
                let Some(end) = self.evaluate(Some(left), at, names)
                    .and_then(|left| self.evaluate(Some(right), left, names)) else { return };
                self.edge(end, pass, EdgeKind::True);
                self.edge(end, fail, EdgeKind::False);
            }
            Predicate::Block(block) => {
                self.lower_value_block(block, at, &mut names.clone(), &Mode::Predicate(targets));
            }
            Predicate::Value(value) => {
                let Some(end) = self.evaluate(Some(value), at, names) else { return };
                match self.text(value) {
                    "true" => self.edge(end, pass, EdgeKind::True),
                    "false" => self.edge(end, fail, EdgeKind::False),
                    _ => {
                        self.edge(end, pass, EdgeKind::True);
                        self.edge(end, fail, EdgeKind::False);
                    }
                }
            }
        }
    }
}

use super::{simple_value, Bindings, EdgeKind, Lower, Node, NodeId};

enum Predicate<'a> {
    And(Node<'a>, Node<'a>),
    Or(Node<'a>, Node<'a>),
    Comparison(Node<'a>, Node<'a>),
    Not(Node<'a>),
    Value(Node<'a>),
}

fn predicate(mut node: Node<'_>) -> Predicate<'_> {
    while node.kind() == "parenthesized_expression" {
        let Some(inner) = crate::lower::site::children(node).next() else {
            break;
        };
        node = inner;
    }
    if node.kind() == "unary_expression"
        && node
            .children(&mut node.walk())
            .any(|child| child.kind() == "!")
    {
        if let Some(value) = crate::lower::site::children(node).next() {
            return Predicate::Not(value);
        }
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
    Predicate::Value(node)
}

pub(in crate::lower::rust) fn supported(node: Node<'_>) -> bool {
    match predicate(node) {
        Predicate::And(left, right) | Predicate::Or(left, right) => {
            supported(left) && supported(right)
        }
        Predicate::Not(value) => supported(value),
        Predicate::Comparison(left, right) => simple_value(left) && simple_value(right),
        Predicate::Value(value) => simple_value(value),
    }
}

pub(in crate::lower::rust) fn has_short_circuit(node: Node<'_>) -> bool {
    match predicate(node) {
        Predicate::And(_, _) | Predicate::Or(_, _) => true,
        Predicate::Not(value) => has_short_circuit(value),
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
                let end = self.evaluate(Some(left), at, names);
                let end = self.evaluate(Some(right), end, names);
                self.edge(end, pass, EdgeKind::True);
                self.edge(end, fail, EdgeKind::False);
            }
            Predicate::Value(value) => {
                let end = self.evaluate(Some(value), at, names);
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

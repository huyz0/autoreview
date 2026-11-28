use super::{line, match_flow, Bindings, EdgeKind, Lower, Node, NodeId};
use crate::lower::site::children;

pub(super) fn test(owner: Node<'_>) -> Option<Node<'_>> {
    owner
        .child_by_field_name("condition")
        .filter(|node| matches!(node.kind(), "let_condition" | "let_chain"))
}

pub(super) fn control_test<'tree>(owner: Node<'tree>, source: &[u8]) -> Option<Node<'tree>> {
    test(owner).or_else(|| {
        owner.child_by_field_name("condition").filter(|condition| {
            match_flow::guard::has_short_circuit(*condition) && match_flow::guard::supported(*condition, source)
        })
    })
}

pub(crate) fn bindings<'tree>(node: Node<'tree>, source: &[u8]) -> Option<Vec<Node<'tree>>> {
    (node.kind() == "let_condition")
        .then(|| match_flow::projection_bindings(node, source))
        .flatten()
}

pub(super) fn supported(node: Node<'_>, source: &[u8]) -> bool {
    match node.kind() {
        "let_chain" => children(node).all(|part| supported(part, source)),
        "let_condition" => {
            bindings(node, source).is_some()
                && node
                    .child_by_field_name("value")
                    .is_some_and(|value| match_flow::simple_value(value, source))
        }
        _ => match_flow::guard::supported(node, source),
    }
}

pub(super) fn root_branch(node: Node<'_>) -> Node<'_> {
    std::iter::successors(Some(node), |node| {
        node.parent()
            .filter(|parent| parent.kind() == "else_clause")?
            .parent()
    })
    .last()
    .unwrap_or(node)
}

pub(super) fn supports_owner(node: Node<'_>) -> bool {
    if node.kind() != "if_expression"
        || test(node).is_none()
        || node.child_by_field_name("alternative").is_none()
    {
        return true;
    }
    super::branch_value::is_discarded_statement(root_branch(node)) || unit_branch(node)
}

pub(super) fn unit_branch(node: Node<'_>) -> bool {
    match node.kind() {
        "block" => children(node).last().is_none_or(|tail| {
            matches!(
                tail.kind(),
                "let_declaration"
                    | "return_expression"
                    | "break_expression"
                    | "continue_expression"
                    | "empty_statement"
            ) || (tail.kind() == "expression_statement"
                && tail
                    .children(&mut tail.walk())
                    .any(|child| child.kind() == ";"))
        }),
        "if_expression" => node
            .child_by_field_name("alternative")
            .is_none_or(|otherwise| {
                node.child_by_field_name("consequence")
                    .is_some_and(unit_branch)
                    && unit_branch(otherwise)
            }),
        "else_clause" => children(node).next().is_some_and(unit_branch),
        _ => false,
    }
}

impl Lower<'_> {
    pub(super) fn tested_branch(
        &mut self,
        test: Node<'_>,
        at: NodeId,
        names: &Bindings,
    ) -> Option<NodeId> {
        let expr = test.parent()?;
        let consequence = expr.child_by_field_name("consequence")?;
        let (passed, failed, success_names) =
            self.split_branch(test, at, (line(consequence), line(expr)), names);
        let then_end = self.block(consequence, passed, &success_names);
        let otherwise = expr
            .child_by_field_name("alternative")
            .and_then(|node| children(node).next());
        let else_end = otherwise.map_or(Some(failed), |node| {
            self.flow_expression(node, failed, names)
        });
        self.join_paths(expr, [then_end, else_end].into_iter().flatten())
    }

    pub(super) fn split_branch(
        &mut self,
        test: Node<'_>,
        at: NodeId,
        lines: (u32, u32),
        names: &Bindings,
    ) -> (NodeId, NodeId, Bindings) {
        let passed = self.cfg.push_node(lines.0);
        let failed = self.cfg.push_node(lines.1);
        let mut success = names.clone();
        self.lower_pattern_test(test, at, (passed, failed), &mut success);
        (passed, failed, success)
    }

    pub(super) fn join_paths(
        &mut self,
        expr: Node<'_>,
        ends: impl IntoIterator<Item = NodeId>,
    ) -> Option<NodeId> {
        let mut ends = ends.into_iter().peekable();
        ends.peek()?;
        let join = self.cfg.push_node(line(expr));
        for end in ends {
            self.edge(end, join, EdgeKind::Fallthrough);
        }
        Some(join)
    }

    pub(super) fn tested_loop(&mut self, test: Node<'_>, at: NodeId, names: &Bindings) -> Option<NodeId> {
        let expr = test.parent()?;
        let body = expr.child_by_field_name("body")?;
        let head = self.cfg.push_node(line(expr));
        let start = self.cfg.push_node(line(body));
        let after = self.cfg.push_node(expr.end_position().row as u32 + 1);
        self.edge(at, head, EdgeKind::Fallthrough);
        let mut success_names = names.clone();
        self.lower_pattern_test(test, head, (start, after), &mut success_names);
        self.loops.push((head, after));
        if let Some(end) = self.block(body, start, &success_names) {
            self.edge(end, head, EdgeKind::Loop);
        }
        self.loops.pop();
        Some(after)
    }

    pub(super) fn lower_pattern_test(
        &mut self,
        node: Node<'_>,
        mut at: NodeId,
        targets: (NodeId, NodeId),
        names: &mut Bindings,
    ) {
        let (passed, failed) = targets;
        if node.kind() == "let_chain" {
            let mut parts = children(node).peekable();
            while let Some(part) = parts.next() {
                let next = parts
                    .peek()
                    .map_or(passed, |next| self.cfg.push_node(line(*next)));
                self.lower_pattern_test(part, at, (next, failed), names);
                at = next;
            }
        } else if let Some(bindings) = bindings(node, self.source) {
            let Some(evaluated) = self.evaluate(node.child_by_field_name("value"), at, names) else { return };
            let matched = self.cfg.push_node(line(node));
            self.edge(evaluated, matched, EdgeKind::True);
            if node.child_by_field_name("pattern").is_none_or(|pattern| pattern.kind() != "_") {
                self.edge(evaluated, failed, EdgeKind::False);
            }
            for binding in bindings {
                self.project_pattern_binding(binding, matched, names);
            }
            self.edge(matched, passed, EdgeKind::Fallthrough);
        } else {
            self.lower_guard(node, at, targets, names);
        }
    }
}

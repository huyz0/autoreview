use super::{branch_value::{self, Mode}, line, match_flow, Bindings, EdgeKind, Lower, Node, NodeId};
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
            .is_some_and(supported_initializer)
        && node
            .child_by_field_name("alternative")
            .is_some_and(diverges)
}

fn supported_initializer(value: Node<'_>) -> bool {
    match branch_value::from_value(value) {
        Some(branch) if value != branch && branch.kind() == "if_expression" => {
            branch_value::supports_if(branch)
        }
        Some(branch) if value != branch => match_flow::supports(branch),
        Some(_) => false,
        None => match_flow::simple_value(value),
    }
}

pub(super) fn is_fallback(block: Node<'_>) -> bool {
    block.parent().is_some_and(|owner| {
        owner.kind() == "let_declaration" && owner.child_by_field_name("alternative") == Some(block)
    })
}

fn diverges(node: Node<'_>) -> bool {
    match node.kind() {
        "return_expression" | "break_expression" | "continue_expression" => true,
        "block" => children(node).last().is_some_and(diverges),
        "expression_statement" | "else_clause" => children(node).next().is_some_and(diverges),
        "if_expression" => {
            node.child_by_field_name("consequence").is_some_and(diverges)
                && node.child_by_field_name("alternative").is_some_and(diverges)
        }
        "match_expression" => diverging_match(node),
        _ => false,
    }
}

fn diverging_match(node: Node<'_>) -> bool {
    match_flow::supports(node) && node.child_by_field_name("body").is_some_and(|body| {
        children(body).all(|arm| arm.child_by_field_name("value").is_some_and(diverges))
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
        let value = node.child_by_field_name("value")?;
        let tested = if let Some(branch) = branch_value::from_value(value) {
            self.lower_branch_value(branch, at, names, &Mode::Project)?
        } else {
            self.evaluate(Some(value), at, names)
        };
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

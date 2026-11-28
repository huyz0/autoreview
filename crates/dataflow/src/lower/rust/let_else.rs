use super::{branch_value::{self, Mode}, line, match_flow, Bindings, EdgeKind, Lower, Node, NodeId};
use crate::lower::site::children;

pub(crate) fn bindings<'tree>(node: Node<'tree>, source: &[u8]) -> Option<Vec<Node<'tree>>> {
    supports(node, source).then(|| pattern_bindings(node, source)).flatten()
}

fn pattern_bindings<'tree>(node: Node<'tree>, source: &[u8]) -> Option<Vec<Node<'tree>>> {
    if node.kind() != "let_declaration" || node.child_by_field_name("alternative").is_none() {
        return None;
    }
    match_flow::projection_bindings(node, source)
}

pub(super) fn supports(node: Node<'_>, source: &[u8]) -> bool {
    pattern_bindings(node, source).is_some()
        && node
            .child_by_field_name("value")
            .is_some_and(|value| supported_initializer(value, source))
        && node
            .child_by_field_name("alternative")
            .is_some_and(|fallback| diverges(fallback, source))
}

fn supported_initializer(value: Node<'_>, source: &[u8]) -> bool {
    match branch_value::from_value(value) {
        Some(branch) if value != branch && branch.kind() == "if_expression" => {
            branch_value::supports_if(branch, source)
        }
        Some(branch) if value != branch => match_flow::supports(branch, source),
        Some(_) => false,
        None => match_flow::simple_value(value),
    }
}

pub(super) fn is_fallback(block: Node<'_>) -> bool {
    block.parent().is_some_and(|owner| {
        owner.kind() == "let_declaration" && owner.child_by_field_name("alternative") == Some(block)
    })
}

fn diverges(node: Node<'_>, source: &[u8]) -> bool {
    let diverges = |child| diverges(child, source);
    match node.kind() {
        "return_expression" | "break_expression" | "continue_expression" => true,
        "block" => children(node).last().is_some_and(diverges),
        "expression_statement" | "else_clause" => children(node).next().is_some_and(diverges),
        "if_expression" => {
            node.child_by_field_name("consequence").is_some_and(diverges)
                && node.child_by_field_name("alternative").is_some_and(diverges)
        }
        "match_expression" => diverging_match(node, source),
        _ => false,
    }
}

fn diverging_match(node: Node<'_>, source: &[u8]) -> bool {
    match_flow::supports(node, source) && node.child_by_field_name("body").is_some_and(|body| {
        children(body).all(|arm| arm.child_by_field_name("value").is_some_and(|value| diverges(value, source)))
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
        for binding in pattern_bindings(node, self.source)? {
            self.project_pattern_binding(binding, matched, names);
        }
        Some(matched)
    }
}

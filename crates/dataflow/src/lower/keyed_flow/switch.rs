use tree_sitter::Node;
use crate::cfg::{Cfg, EdgeKind, NodeId, Stmt};
use super::{line, site, Flow};

/// Only handle switches with pure labels and no jumps. Other forms must remain
/// unsupported until their evaluation order and targets are modeled.
pub(crate) fn supports_go_switch(node: Node<'_>) -> bool {
    if node.kind() != "expression_switch_statement" || node.child_by_field_name("initializer").is_some() { return false; }
    if node.child_by_field_name("value").is_some_and(|value| !pure(value)) { return false; }
    let mut pending = vec![node];
    while let Some(current) = pending.pop() {
        if matches!(current.kind(), "break_statement" | "fallthrough_statement" | "goto_statement" | "labeled_statement") { return false; }
        if current.kind() == "expression_case" && current.child_by_field_name("value").is_some_and(|value| !pure(value)) { return false; }
        pending.extend(site::children(current));
    }
    true
}

fn pure(node: Node<'_>) -> bool {
    matches!(node.kind(), "expression_list" | "identifier" | "selector_expression" | "field_identifier" | "int_literal" | "float_literal" | "imaginary_literal" | "rune_literal" | "raw_string_literal" | "interpreted_string_literal" | "true" | "false" | "nil")
        && site::children(node).all(pure)
}

impl Flow<'_> {
    pub(super) fn go_switch(&self, cfg: &mut Cfg<Stmt>, node: Node<'_>, start: NodeId) -> NodeId {
        if let Some(value) = node.child_by_field_name("value") {
            super::super::record_evaluation(cfg, start, value);
        }
        let after = cfg.push_node(node.end_position().row as u32 + 1);
        let mut has_default = false;
        for case in site::children(node).filter(|child| matches!(child.kind(), "expression_case" | "default_case")) {
            has_default |= case.kind() == "default_case";
            let entry = cfg.push_node(line(case));
            cfg.edges.push((start, entry, EdgeKind::True));
            if let Some(value) = case.child_by_field_name("value") {
                super::super::record_evaluation(cfg, entry, value);
            }
            let end = site::children(case).find(|child| child.kind() == "statement_list")
                .map_or(entry, |statements| self.block(cfg, statements, entry));
            cfg.edges.push((end, after, EdgeKind::Fallthrough));
        }
        if !has_default { cfg.edges.push((start, after, EdgeKind::False)); }
        after
    }
}

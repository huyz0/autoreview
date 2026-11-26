use std::collections::BTreeSet;
use tree_sitter::Node;

use super::super::super::keyed::{KeyedFunction, KeyedLimitation, KeyedUnit, program::Program};
use crate::lower::site;

// A whole-statement evaluation is not enough to model a conditional source:
// the selected branch must have its own flow node.
pub(super) fn limitations(program: &Program<'_, '_, '_>, units: &[KeyedUnit<'_, '_, '_>], reachable: &BTreeSet<usize>) -> Vec<KeyedLimitation> {
    let mut result = Vec::new();
    let mut index = 0;
    for unit in units {
        for function in unit.functions {
            if reachable.contains(&index) { result.extend(scan(program, unit, function, index)); }
            index += 1;
        }
    }
    result
}

fn scan(program: &Program<'_, '_, '_>, unit: &KeyedUnit<'_, '_, '_>, function: &KeyedFunction<'_, '_>, index: usize) -> Vec<KeyedLimitation> {
    let Some(plan) = program.frames[index].plan.as_ref().filter(|plan| plan.has_registration()) else { return Vec::new(); };
    let mut result = Vec::new();
    let mut pending = vec![function.node];
    while let Some(node) = pending.pop() {
        if let Some(arm) = conditional_arm(node) {
            let source = site::name(node, unit.source).is_some_and(|name| plan.source_mode_at(&name, node.start_byte()).is_some());
            let modeled = function.graph.nodes.iter().flat_map(|block| &block.evaluations).any(|evaluation|
                arm.start_byte() <= evaluation.span.start && evaluation.span.end <= arm.end_byte()
                    && evaluation.span.start <= node.start_byte() && node.end_byte() <= evaluation.span.end);
            if source && !modeled {
                result.push(KeyedLimitation { reason: "unmodeled_conditional_result", path: Some(unit.path.to_owned()), line: Some(node.start_position().row as u32 + 1) });
            }
        }
        pending.extend(site::children(node));
    }
    result
}

fn conditional_arm(node: Node<'_>) -> Option<Node<'_>> {
    if !matches!(node.kind(), "call_expression" | "method_invocation" | "new_expression") { return None; }
    let conditional = std::iter::successors(node.parent(), |parent| parent.parent())
        .take_while(|parent| !matches!(parent.kind(), "function_declaration" | "function_expression" | "arrow_function" | "method_definition"))
        .find(|parent| parent.kind() == "ternary_expression" && site::discarded_conditional_expression(*parent))?;
    ["consequence", "alternative"].into_iter().filter_map(|field| conditional.child_by_field_name(field))
        .find(|arm| arm.start_byte() <= node.start_byte() && node.end_byte() <= arm.end_byte())
}

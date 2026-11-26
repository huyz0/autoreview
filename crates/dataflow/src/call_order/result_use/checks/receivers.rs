use std::collections::BTreeSet;
use tree_sitter::Node;

use crate::lower::site;
use super::super::super::keyed::KeyedLimitation;
use super::super::{Flow, KeyedUnit, Plan, Program};
use super::super::super::KeyedFunction;

// A direct result passed to an opaque callee cannot be assumed to be used.
// Keep the existing finding behavior and expose this uncertainty separately.
pub(super) fn limitations(program: &Program<'_, '_, '_>, units: &[KeyedUnit<'_, '_, '_>], reachable: &BTreeSet<usize>) -> Vec<KeyedLimitation> {
    let mut limitations = Vec::new();
    let mut index = 0;
    for unit in units {
        for function in unit.functions {
            let current = index;
            index += 1;
            if reachable.contains(&current) { limitations.extend(scan_frame(program, unit, function, current)); }
        }
    }
    limitations
}

fn scan_frame(program: &Program<'_, '_, '_>, unit: &KeyedUnit<'_, '_, '_>, function: &KeyedFunction<'_, '_>, index: usize) -> Vec<KeyedLimitation> {
    let frame = &program.frames[index];
    let Some(plan) = &frame.plan else { return Vec::new(); };
    if !plan.has_registration() { return Vec::new(); }
    let calls = program.resolved_calls(index, frame.graph, plan);
    let flow = Flow::new(frame.graph, plan);
    let mut limitations = Vec::new();
    let mut pending = vec![function.node];
    while let Some(node) = pending.pop() {
        if node != function.node && nested_function(node) { continue; }
        if let Some(outer) = receiver(node, unit.source, plan) {
            let reached = frame.graph.nodes.iter().zip(&flow.reachable).any(|(block, reached)| *reached && block.evaluations.iter().any(|evaluation| evaluation.span.start <= outer.start_byte() && outer.end_byte() <= evaluation.span.end));
            let modeled = calls.iter().any(|(start, target)| *start == outer.start_byte() && program.frames[*target].plan.is_some());
            if reached && !modeled {
                limitations.push(KeyedLimitation { reason: "unresolved_result_receiver", path: Some(unit.path.to_owned()), line: Some(node.start_position().row as u32 + 1) });
            }
        }
        pending.extend(node.named_children(&mut node.walk()));
    }
    limitations
}

fn nested_function(node: Node<'_>) -> bool {
    matches!(node.kind(), "function_declaration" | "function_expression" | "arrow_function" | "method_definition" | "function_item" | "method_declaration")
}

fn receiver<'tree>(node: Node<'tree>, source: &[u8], plan: &Plan<'_, '_>) -> Option<Node<'tree>> {
    if !matches!(node.kind(), "call_expression" | "method_invocation" | "new_expression") || plan.result_components.contains_key(&node.start_byte()) { return None; }
    let name = site::name(node, source)?;
    plan.source_mode_at(&name, node.start_byte())?;
    let mut argument = node;
    while let Some(parent) = argument.parent().filter(|parent| matches!(parent.kind(), "parenthesized_expression" | "value_argument")) { argument = parent; }
    let list = argument.parent()?;
    let outer = list.parent()?;
    (site::arguments(outer) == Some(list)).then_some(outer)
}

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
        limitations.extend(receiver_issue(program, index, unit, plan, &flow, &calls, node));
        pending.extend(node.named_children(&mut node.walk()));
    }
    limitations
}

fn receiver_issue(program: &Program<'_, '_, '_>, index: usize, unit: &KeyedUnit<'_, '_, '_>, plan: &Plan<'_, '_>, flow: &Flow, calls: &BTreeSet<(usize, usize)>, node: Node<'_>) -> Option<KeyedLimitation> {
    let (outer, wrapped) = receiver(node, unit.source, plan)?;
    let reached = program.frames[index].graph.nodes.iter().zip(&flow.reachable).any(|(block, reached)| *reached && block.evaluations.iter().any(|evaluation| evaluation.span.start <= outer.start_byte() && outer.end_byte() <= evaluation.span.end));
    let modeled = !wrapped && modeled_receiver(program, calls, outer.start_byte());
    (reached && !modeled).then(|| KeyedLimitation { reason: if wrapped { "wrapped_result_receiver" } else { "unresolved_result_receiver" }, path: Some(unit.path.to_owned()), line: Some(node.start_position().row as u32 + 1) })
}

fn modeled_receiver(program: &Program<'_, '_, '_>, calls: &BTreeSet<(usize, usize)>, start: usize) -> bool {
    calls.iter().any(|(position, target)| *position == start && program.frames[*target].plan.is_some())
}

fn nested_function(node: Node<'_>) -> bool {
    matches!(node.kind(), "function_declaration" | "function_expression" | "arrow_function" | "method_definition" | "function_item" | "method_declaration")
}

fn receiver<'tree>(node: Node<'tree>, source: &[u8], plan: &Plan<'_, '_>) -> Option<(Node<'tree>, bool)> {
    if !matches!(node.kind(), "call_expression" | "method_invocation" | "new_expression") || plan.result_components.contains_key(&node.start_byte()) { return None; }
    let name = site::name(node, source)?;
    plan.source_mode_at(&name, node.start_byte())?;
    let mut argument = node;
    let mut wrapped = false;
    while let Some(parent) = argument.parent().filter(|parent| matches!(parent.kind(),
        "parenthesized_expression" | "value_argument" | "array" | "spread_element" | "spread_expression")) {
        wrapped |= matches!(parent.kind(), "array" | "spread_element" | "spread_expression");
        argument = parent;
    }
    let list = argument.parent()?;
    let outer = list.parent()?;
    (site::arguments(outer) == Some(list)).then_some((outer, wrapped))
}

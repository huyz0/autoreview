use std::collections::{BTreeMap, BTreeSet};

use crate::call_order::{keyed::program::Program, KeyedUnit};

type Position = (usize, usize);

// A verified helper call is itself a promise source, but a synchronous helper
// that only forwards another verified call should retain that inner origin.
// Keep the outer source if there is any ambiguity or a recursive cycle.
pub(super) fn forwarded_calls(
    program: &Program<'_, '_, '_>, units: &[KeyedUnit<'_, '_, '_>],
    sources: &BTreeMap<usize, BTreeSet<usize>>,
) -> BTreeMap<usize, BTreeSet<usize>> {
    let functions: Vec<_> = units.iter().flat_map(|unit| unit.functions.iter()).collect();
    let mut edges = BTreeMap::<Position, Vec<Position>>::new();
    for (index, frame) in program.frames.iter().enumerate() {
        let Some(plan) = &frame.plan else { continue };
        for ((start, _), target) in program.resolved_calls(index, frame.graph, plan) {
            if !contains(sources, frame.unit, start) { continue; }
            if let Some(inner) = forwarded_target(program, units, &functions, sources, target) {
                edges.insert((frame.unit, start), inner);
            }
        }
    }
    let mut forwarded = BTreeMap::<usize, BTreeSet<usize>>::new();
    let mut cache = BTreeMap::new();
    for &start in edges.keys() {
        if reaches_terminal(start, &edges, &mut cache) { forwarded.entry(start.0).or_default().insert(start.1); }
    }
    forwarded
}

fn forwarded_target(
    program: &Program<'_, '_, '_>, units: &[KeyedUnit<'_, '_, '_>],
    functions: &[&crate::call_order::KeyedFunction<'_, '_>], sources: &BTreeMap<usize, BTreeSet<usize>>, target: usize,
) -> Option<Vec<Position>> {
    let callee = &program.frames[target];
    callee.plan.as_ref()?;
    let function = functions.get(target)?;
    let calls = forwarded_returns(function.node, units[callee.unit].source)?;
    calls.into_iter().map(|inner| {
        let lowered = callee.graph.nodes.iter().flat_map(|node| &node.evaluations)
            .any(|evaluation| evaluation.span.start <= inner && inner < evaluation.span.end);
        (lowered && contains(sources, callee.unit, inner)).then_some((callee.unit, inner))
    }).collect()
}

fn contains(sources: &BTreeMap<usize, BTreeSet<usize>>, unit: usize, start: usize) -> bool {
    sources.get(&unit).is_some_and(|sites| sites.contains(&start))
}

fn reaches_terminal(start: Position, edges: &BTreeMap<Position, Vec<Position>>, cache: &mut BTreeMap<Position, bool>) -> bool {
    let mut active = BTreeSet::new();
    let mut pending = vec![(start, false)];
    while let Some((position, leaving)) = pending.pop() {
        if leaving {
            active.remove(&position);
            cache.insert(position, true);
            continue;
        }
        if cache.get(&position) == Some(&true) { continue; }
        if cache.get(&position) == Some(&false) || (edges.contains_key(&position) && !active.insert(position)) {
            for item in active { cache.insert(item, false); }
            return false;
        }
        if let Some(next) = edges.get(&position) {
            pending.push((position, true));
            pending.extend(next.iter().rev().map(|&item| (item, false)));
        }
    }
    true
}

fn forwarded_returns(function: tree_sitter::Node<'_>, source: &[u8]) -> Option<Vec<usize>> {
    let body = function.child_by_field_name("body")?;
    let signature = source.get(function.start_byte()..body.start_byte())?;
    if body.kind() != "statement_block" || signature.split(|byte| !byte.is_ascii_alphanumeric()).any(|word| word == b"async") { return None; }
    let mut pending = vec![body];
    let mut calls = Vec::new();
    while let Some(node) = pending.pop() {
        if calls.len() + pending.len() > 128 { return None; }
        match node.kind() {
            "statement_block" | "else_clause" => pending.push(only_child(node)?),
            "if_statement" => {
                pending.push(node.child_by_field_name("consequence")?);
                pending.push(node.child_by_field_name("alternative")?);
            }
            "return_statement" => {
                let value = only_child(node)?;
                if value.kind() != "call_expression" || crate::lower::site::name(value, source).is_none() { return None; }
                calls.push(value.start_byte());
            }
            _ => return None,
        }
    }
    (!calls.is_empty()).then_some(calls)
}

fn only_child(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    let children: Vec<_> = node.named_children(&mut node.walk()).filter(|child| child.kind() != "comment").collect();
    let [child] = children.as_slice() else { return None };
    Some(*child)
}

#[test]
fn forwarding_rejects_cycles_and_preserves_terminal_chains() {
    let edges = BTreeMap::from([((0, 1), vec![(1, 2), (2, 3)]), ((1, 2), vec![(2, 3)]), ((3, 4), vec![(4, 5)]), ((4, 5), vec![(3, 4)]), ((5, 6), vec![(0, 1), (3, 4)])]);
    let mut cache = BTreeMap::new();
    assert!(reaches_terminal((0, 1), &edges, &mut cache));
    assert!(!reaches_terminal((3, 4), &edges, &mut cache));
    assert!(!reaches_terminal((5, 6), &edges, &mut cache));
}

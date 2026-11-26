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
    let mut edges = BTreeMap::<Position, Position>::new();
    for (index, frame) in program.frames.iter().enumerate() {
        let Some(plan) = &frame.plan else { continue };
        for (start, target) in program.resolved_calls(index, frame.graph, plan) {
            if !contains(sources, frame.unit, start) { continue; }
            let callee = &program.frames[target];
            if callee.plan.is_none() { continue; }
            let Some(function) = functions.get(target) else { continue };
            let Some(inner) = forwarded_return(function.node, units[callee.unit].source) else { continue };
            let lowered = callee.graph.nodes.iter().flat_map(|node| &node.evaluations)
                .any(|evaluation| evaluation.span.start <= inner && inner < evaluation.span.end);
            if lowered && contains(sources, callee.unit, inner) { edges.insert((frame.unit, start), (callee.unit, inner)); }
        }
    }
    let mut forwarded = BTreeMap::<usize, BTreeSet<usize>>::new();
    let mut cache = BTreeMap::new();
    for &start in edges.keys() {
        if reaches_terminal(start, &edges, &mut cache) { forwarded.entry(start.0).or_default().insert(start.1); }
    }
    forwarded
}

fn contains(sources: &BTreeMap<usize, BTreeSet<usize>>, unit: usize, start: usize) -> bool {
    sources.get(&unit).is_some_and(|sites| sites.contains(&start))
}

fn reaches_terminal(start: Position, edges: &BTreeMap<Position, Position>, cache: &mut BTreeMap<Position, bool>) -> bool {
    let mut seen = BTreeSet::new();
    let mut current = start;
    let terminal = loop {
        if let Some(&result) = cache.get(&current) { break result; }
        let Some(&next) = edges.get(&current) else { break true };
        if !seen.insert(current) { break false; }
        current = next;
    };
    for position in seen { cache.insert(position, terminal); }
    terminal
}

fn forwarded_return(function: tree_sitter::Node<'_>, source: &[u8]) -> Option<usize> {
    let body = function.child_by_field_name("body")?;
    if body.kind() != "statement_block" { return None; }
    let signature = source.get(function.start_byte()..body.start_byte())?;
    if signature.split(|byte| !byte.is_ascii_alphanumeric()).any(|word| word == b"async") { return None; }
    let statements: Vec<_> = body.named_children(&mut body.walk()).filter(|node| node.kind() != "comment").collect();
    let [statement] = statements.as_slice() else { return None };
    if statement.kind() != "return_statement" { return None; }
    let values: Vec<_> = statement.named_children(&mut statement.walk()).filter(|node| node.kind() != "comment").collect();
    let [value] = values.as_slice() else { return None };
    (value.kind() == "call_expression" && crate::lower::site::name(*value, source).is_some()).then_some(value.start_byte())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarding_rejects_cycles_and_preserves_terminal_chains() {
        let edges = BTreeMap::from([((0, 1), (1, 2)), ((1, 2), (2, 3)), ((3, 4), (4, 5)), ((4, 5), (3, 4))]);
        let mut cache = BTreeMap::new();
        assert!(reaches_terminal((0, 1), &edges, &mut cache));
        assert!(!reaches_terminal((3, 4), &edges, &mut cache));
    }
}

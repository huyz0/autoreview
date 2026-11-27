use std::collections::{BTreeMap, BTreeSet};
mod forwarding;
mod receivers;
mod conditionals;
use super::{effects, summaries, CallOrderHit, CallOrderSpec, HelperSummary, KeyedUnit, Origin, Program};

pub(in crate::call_order) fn check_units(
    spec: &CallOrderSpec,
    units: &[KeyedUnit<'_, '_, '_>],
) -> (Vec<CallOrderHit>, bool, Vec<super::super::keyed::KeyedLimitation>) {
    check_units_with_sources(spec, units, &BTreeMap::new())
}

pub(in crate::call_order) fn check_units_with_sources(
    spec: &CallOrderSpec,
    units: &[KeyedUnit<'_, '_, '_>],
    sources: &BTreeMap<usize, BTreeSet<usize>>,
) -> (Vec<CallOrderHit>, bool, Vec<super::super::keyed::KeyedLimitation>) {
    check_units_with_components(spec, units, sources, &BTreeMap::new())
}

pub(in crate::call_order) fn check_units_with_components(
    spec: &CallOrderSpec, units: &[KeyedUnit<'_, '_, '_>], sources: &BTreeMap<usize, BTreeSet<usize>>,
    components: &BTreeMap<usize, BTreeMap<usize, super::super::ResultComponents>>,
) -> (Vec<CallOrderHit>, bool, Vec<super::super::keyed::KeyedLimitation>) {
    check_units_with_spans(spec, units, sources, components, &BTreeMap::new())
}

pub(in crate::call_order) fn check_units_with_spans(
    spec: &CallOrderSpec, units: &[KeyedUnit<'_, '_, '_>], sources: &BTreeMap<usize, BTreeSet<usize>>,
    components: &BTreeMap<usize, BTreeMap<usize, super::super::ResultComponents>>,
    spans: &BTreeMap<usize, BTreeSet<(usize, usize)>>,
) -> (Vec<CallOrderHit>, bool, Vec<super::super::keyed::KeyedLimitation>) {
    let call_index: Vec<_> = units.iter().enumerate().map(|(index, unit)| {
        if spans.contains_key(&index) { index_calls(unit) } else { BTreeMap::new() }
    }).collect();
    let forwarding_sources = forwarding_candidates(sources, spans, &call_index);
    let forwarded = if spec.contract.as_ref().is_some_and(|contract| contract.typed_promise) {
        forwarding::forwarded_calls(&Program::new(spec, units), units, &forwarding_sources)
    } else { BTreeMap::new() };
    let mut program = Program::new(spec, units);
    install_sources(&mut program, sources, spans, &forwarded, components);
    let mut limitations = unmodeled_sources(&program, units, sources, components);
    limitations.extend(unmodeled_spans(&program, units, spans, &call_index));
    if !program.has_registration() {
        limitations.extend(program.resolution_limitations());
        return (Vec::new(), false, limitations);
    }
    let (summaries, summary_limit) = summaries::build(&program, units);
    let hits = collect_hits(&program, units, &summaries, &mut limitations);
    let mut combined = BTreeMap::new();
    for hit in hits {
        let key = (hit.path.clone(), hit.source_line, hit.trigger_call.clone());
        combined
            .entry(key)
            .and_modify(|previous: &mut CallOrderHit| {
                if previous.source_start != hit.source_start { previous.source_start = None; }
                previous.related_calls.extend(hit.related_calls.iter().cloned());
                previous.related_calls.sort();
                previous.related_calls.dedup();
            })
            .or_insert(hit);
    }
    limitations.extend(program.resolution_limitations());
    (combined.into_values().collect(), summary_limit, limitations)
}

fn forwarding_candidates(
    sources: &BTreeMap<usize, BTreeSet<usize>>, spans: &BTreeMap<usize, BTreeSet<(usize, usize)>>,
    call_index: &[BTreeMap<usize, BTreeSet<usize>>],
) -> BTreeMap<usize, BTreeSet<usize>> {
    let mut result = sources.clone();
    for (&unit, sites) in spans {
        let Some(index) = call_index.get(unit) else { continue };
        for &(start, end) in sites {
            if unique_call_at(index, start, end) { result.entry(unit).or_default().insert(start); }
        }
    }
    result
}

fn install_sources(
    program: &mut Program<'_, '_, '_>, sources: &BTreeMap<usize, BTreeSet<usize>>,
    spans: &BTreeMap<usize, BTreeSet<(usize, usize)>>, forwarded: &BTreeMap<usize, BTreeSet<usize>>,
    components: &BTreeMap<usize, BTreeMap<usize, super::super::ResultComponents>>,
) {
    for frame in &mut program.frames {
        let Some(plan) = &mut frame.plan else { continue };
        if let Some(offsets) = sources.get(&frame.unit) { plan.extra_sources.clone_from(offsets); }
        if let Some(sites) = spans.get(&frame.unit) { plan.extra_spans.clone_from(sites); }
        if let Some(offsets) = forwarded.get(&frame.unit) {
            plan.extra_sources.retain(|offset| !offsets.contains(offset));
            plan.extra_spans.retain(|(start, _)| !offsets.contains(start));
        }
        if let Some(shapes) = components.get(&frame.unit) { plan.result_components.clone_from(shapes); }
    }
}

fn index_calls(unit: &KeyedUnit<'_, '_, '_>) -> BTreeMap<usize, BTreeSet<usize>> {
    let mut calls = BTreeMap::<usize, BTreeSet<usize>>::new();
    for function in unit.functions {
        let mut pending = vec![function.node];
        while let Some(node) = pending.pop() {
            if matches!(node.kind(), "call_expression" | "method_invocation" | "new_expression") {
                calls.entry(node.start_byte()).or_default().insert(node.end_byte());
            }
            pending.extend(node.named_children(&mut node.walk()));
        }
    }
    calls
}

fn unique_call_at(index: &BTreeMap<usize, BTreeSet<usize>>, start: usize, end: usize) -> bool {
    index.get(&start).is_some_and(|ends| ends.len() == 1 && ends.contains(&end))
}

fn unmodeled_sources(
    program: &Program<'_, '_, '_>, units: &[KeyedUnit<'_, '_, '_>], sources: &BTreeMap<usize, BTreeSet<usize>>,
    components: &BTreeMap<usize, BTreeMap<usize, super::super::ResultComponents>>,
) -> Vec<super::super::keyed::KeyedLimitation> {
    let mut limitations = Vec::new();
    let functions: Vec<_> = units.iter().flat_map(|unit| unit.functions.iter()).collect();
    let reachable = super::sources::reachable_frames(program, units, &functions);
    limitations.extend(receivers::limitations(program, units, &reachable));
    limitations.extend(conditionals::limitations(program, units, &reachable));
    if sources.keys().any(|index| *index >= units.len()) {
        limitations.push(super::super::keyed::KeyedLimitation { reason: "verified_source_unmodeled", path: None, line: None });
    }
    let mut base = 0;
    let mut resolved_by_frame = BTreeMap::new();
    for (unit_index, unit) in units.iter().enumerate() {
        for &start in sources.get(&unit_index).into_iter().flatten() {
            if source_modeled(program, (unit, base), start, &reachable) != Some(false) { continue; }
            limitations.push(source_limitation(unit, start, "verified_source_unmodeled"));
        }
        for &start in components.get(&unit_index).into_iter().flat_map(|items| items.keys()) {
            if component_helper_unmodeled(program, (unit, base), start, &reachable, &mut resolved_by_frame) {
                limitations.push(source_limitation(unit, start, "go_multi_result_unmodeled"));
            }
        }
        base += unit.functions.len();
    }
    limitations
}

fn unmodeled_spans(
    program: &Program<'_, '_, '_>, units: &[KeyedUnit<'_, '_, '_>], spans: &BTreeMap<usize, BTreeSet<(usize, usize)>>,
    call_index: &[BTreeMap<usize, BTreeSet<usize>>],
) -> Vec<super::super::keyed::KeyedLimitation> {
    let functions: Vec<_> = units.iter().flat_map(|unit| unit.functions.iter()).collect();
    let reachable = super::sources::reachable_frames(program, units, &functions);
    let mut limitations = Vec::new();
    let mut base = 0;
    for (unit_index, unit) in units.iter().enumerate() {
        for &(start, end) in spans.get(&unit_index).into_iter().flatten() {
            if start >= end || end > unit.source.len() {
                limitations.push(source_limitation(unit, start, "verified_source_unmodeled"));
                continue;
            }
            let owner = unit.functions.iter().enumerate().filter(|(_, function)| function.node.start_byte() <= start && end <= function.node.end_byte())
                .min_by_key(|(_, function)| function.node.end_byte() - function.node.start_byte());
            if owner.is_some_and(|(index, _)| !reachable.contains(&(base + index))) || (owner.is_none() && !unit.entry) { continue; }
            let modeled = owner.is_some_and(|(index, function)| {
                let frame = &program.frames[base + index];
                if !unique_call_at(&call_index[unit_index], start, end) && frame.plan.as_ref().is_some_and(|plan| program.resolved_calls(base + index, frame.graph, plan).iter().any(|(position, _)| *position == start)) {
                    limitations.push(source_limitation(unit, start, "nested_helper_resolution_unmodeled"));
                }
                let call = function.node.named_descendant_for_byte_range(start, end).and_then(|node| {
                    std::iter::successors(Some(node), |node| node.parent()).find(|node| node.start_byte() == start && node.end_byte() == end
                        && matches!(node.kind(), "call_expression" | "method_invocation" | "new_expression"))
                });
                frame.plan.is_some() && call.is_some_and(|node| crate::lower::site::name(node, unit.source).is_some())
                    && frame.graph.nodes.iter().flat_map(|node| &node.evaluations).any(|item| item.span.start <= start && end <= item.span.end)
            });
            if !modeled { limitations.push(source_limitation(unit, start, "verified_source_unmodeled")); }
        }
        base += unit.functions.len();
    }
    if spans.keys().any(|index| *index >= units.len()) {
        limitations.push(super::super::keyed::KeyedLimitation { reason: "verified_source_unmodeled", path: None, line: None });
    }
    limitations
}

fn source_modeled(
    program: &Program<'_, '_, '_>, context: (&KeyedUnit<'_, '_, '_>, usize), start: usize,
    reachable: &BTreeSet<usize>,
) -> Option<bool> {
    let (unit, base) = context;
    let owner = unit.functions.iter().enumerate()
        .filter(|(_, function)| function.node.start_byte() <= start && start < function.node.end_byte())
        .min_by_key(|(_, function)| function.node.end_byte() - function.node.start_byte());
    if owner.is_some_and(|(index, _)| !reachable.contains(&(base + index))) || (owner.is_none() && !unit.entry) { return None; }
    Some(owner.is_some_and(|(index, function)| {
        let frame = &program.frames[base + index];
        let call = function.node.descendant_for_byte_range(start, start.saturating_add(1))
            .and_then(|node| std::iter::successors(Some(node), |node| node.parent())
                .find(|node| node.start_byte() == start && matches!(node.kind(), "call_expression" | "method_invocation" | "new_expression")));
        frame.plan.is_some() && call.is_some_and(|node| crate::lower::site::name(node, unit.source).is_some())
            && frame.graph.nodes.iter().flat_map(|node| &node.evaluations).any(|item| item.span.start <= start && start < item.span.end)
    }))
}

// Multi-result arguments need a uniquely resolved, modeled receiving helper.
fn component_helper_unmodeled(
    program: &Program<'_, '_, '_>, context: (&KeyedUnit<'_, '_, '_>, usize), start: usize,
    reachable: &BTreeSet<usize>, calls: &mut BTreeMap<usize, BTreeSet<(usize, usize)>>,
) -> bool {
    let (unit, base) = context;
    let owner = unit.functions.iter().enumerate()
        .filter(|(_, function)| function.node.start_byte() <= start && start < function.node.end_byte())
        .min_by_key(|(_, function)| function.node.end_byte() - function.node.start_byte());
    let Some((index, function)) = owner else { return false; };
    if !reachable.contains(&(base + index)) { return false; }
    let Some(mut call) = function.node.descendant_for_byte_range(start, start.saturating_add(1))
        .and_then(|node| std::iter::successors(Some(node), |node| node.parent())
            .find(|node| node.start_byte() == start && node.kind() == "call_expression")) else { return false; };
    while let Some(parent) = call.parent().filter(|node| node.kind() == "parenthesized_expression") { call = parent; }
    let Some(outer) = call.parent().filter(|node| node.kind() == "argument_list").and_then(|node| node.parent()) else { return false; };
    let frame = &program.frames[base + index];
    let calls = calls.entry(base + index).or_insert_with(|| frame.plan.as_ref()
        .map(|plan| program.resolved_calls(base + index, frame.graph, plan)).unwrap_or_default());
    !calls.iter().any(|(position, target)| *position == outer.start_byte() && program.frames[*target].plan.is_some())
}

fn source_limitation(unit: &KeyedUnit<'_, '_, '_>, start: usize, reason: &'static str) -> super::super::keyed::KeyedLimitation {
    let line = unit.source.get(..start).map(|source| source.iter().filter(|byte| **byte == b'\n').count() as u32 + 1);
    super::super::keyed::KeyedLimitation { reason, path: Some(unit.path.to_owned()), line }
}

fn collect_hits(
    program: &Program<'_, '_, '_>, units: &[KeyedUnit<'_, '_, '_>], summaries: &BTreeMap<usize, HelperSummary>,
    limitations: &mut Vec<super::super::keyed::KeyedLimitation>,
) -> Vec<CallOrderHit> {
    let mut hits = Vec::new();
    let mut frame_index = 0;
    for unit in units {
        for function in unit.functions {
            let frame = &program.frames[frame_index];
            let current = frame_index;
            frame_index += 1;
            if !unit.entry {
                continue;
            }
            let Some(plan) = frame.plan.as_ref() else {
                continue;
            };
            let calls =
                summaries::resolved_helpers(program, current, function.graph, plan, summaries);
            if !plan.has_registration() && calls.is_empty() {
                continue;
            }
            let analysis = effects::lower(plan, function, unit.source, false, &calls);
            limitations.extend(analysis.opaque_origins().into_iter().map(|(_, origin)| super::super::keyed::KeyedLimitation {
                reason: "unresolved_result_receiver", path: origin.path.or_else(|| Some(unit.path.to_owned())), line: Some(origin.line),
            }));
            let unused: Vec<_> = analysis.unused().collect();
            let syntax = (function.node, unit.source);
            let (partial, status) = super::partial::unused(&analysis.owner, function.graph, plan, syntax, !analysis.closures.is_empty());
            let helper = super::partial::helper_paths(&analysis.owner, function.graph, plan, syntax, (&unused, !analysis.closures.is_empty()));
            let resolved: Vec<_> = unused.iter().chain(&helper.hits).chain(&helper.observed).cloned().collect();
            let reason = partial_reason(status, (&analysis.owner, function.graph, plan, syntax), &resolved)
                .or_else(|| helper.incomplete.then_some("result_use_path_unmodeled"));
            if let Some(reason) = reason {
                limitations.push(super::super::keyed::KeyedLimitation { reason, path: Some(unit.path.to_owned()), line: Some(function.node.start_position().row as u32 + 1) });
            }
            hits.extend(unused.into_iter().chain(partial).chain(helper.hits).map(|(node, origin)| make_hit(node, origin, unit, units)));
        }
    }
    hits
}

fn partial_reason(status: super::partial::Status, context: (&super::Body, &crate::cfg::Cfg<crate::cfg::Stmt>, &super::Plan<'_, '_>, (tree_sitter::Node<'_>, &[u8])), unused: &[(crate::cfg::NodeId, Origin)]) -> Option<&'static str> {
    let (body, graph, plan, syntax) = context;
    match status {
        super::partial::Status::Limited => Some("result_use_path_limit"),
        super::partial::Status::Unsupported if super::partial::has_observed_origin(body, graph, plan, syntax, unused) => Some("result_use_path_unmodeled"),
        _ => None,
    }
}

fn make_hit(node: crate::cfg::NodeId, origin: Origin, unit: &KeyedUnit<'_, '_, '_>, units: &[KeyedUnit<'_, '_, '_>]) -> CallOrderHit {
    let related_calls = origin.calls.iter().filter_map(|&(index, offset)| {
        let unit = units.get(index)?;
        let before = unit.source.get(..offset)?;
        let line = before.iter().filter(|byte| **byte == b'\n').count() as u32 + 1;
        (!unit.path.is_empty()).then(|| (unit.path.to_owned(), line))
    }).collect();
    CallOrderHit {
        trigger_call: origin.call, node, source_line: origin.line, source_start: Some(origin.start),
        path: origin.path.or_else(|| (!unit.path.is_empty()).then(|| unit.path.to_owned())),
        entry: None, related_calls,
    }
}


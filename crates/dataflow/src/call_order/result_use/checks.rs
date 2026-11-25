use std::collections::{BTreeMap, BTreeSet};
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
    let mut program = Program::new(spec, units);
    for frame in &mut program.frames {
        if let (Some(plan), Some(offsets)) = (&mut frame.plan, sources.get(&frame.unit)) {
            plan.extra_sources.clone_from(offsets);
        }
    }
    if !program.has_registration() {
        return (Vec::new(), false, Vec::new());
    }
    let (summaries, summary_limit) = summaries::build(&program, units);
    let hits = collect_hits(&program, units, &summaries);
    let mut combined = BTreeMap::new();
    for hit in hits {
        let key = (hit.path.clone(), hit.source_line, hit.trigger_call.clone());
        combined
            .entry(key)
            .and_modify(|previous: &mut CallOrderHit| {
                previous.related_calls.extend(hit.related_calls.iter().cloned());
                previous.related_calls.sort();
                previous.related_calls.dedup();
            })
            .or_insert(hit);
    }
    (combined.into_values().collect(), summary_limit, program.resolution_limitations())
}

fn collect_hits(
    program: &Program<'_, '_, '_>, units: &[KeyedUnit<'_, '_, '_>], summaries: &BTreeMap<usize, HelperSummary>,
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
            hits.extend(analysis.unused().map(|(node, origin)| make_hit(node, origin, unit, units)));
        }
    }
    hits
}

fn make_hit(node: crate::cfg::NodeId, origin: Origin, unit: &KeyedUnit<'_, '_, '_>, units: &[KeyedUnit<'_, '_, '_>]) -> CallOrderHit {
    let related_calls = origin.calls.iter().filter_map(|&(index, offset)| {
        let unit = units.get(index)?;
        let before = unit.source.get(..offset)?;
        let line = before.iter().filter(|byte| **byte == b'\n').count() as u32 + 1;
        (!unit.path.is_empty()).then(|| (unit.path.to_owned(), line))
    }).collect();
    CallOrderHit {
        trigger_call: origin.call, node, source_line: origin.line,
        path: origin.path.or_else(|| (!unit.path.is_empty()).then(|| unit.path.to_owned())),
        entry: None, related_calls,
    }
}


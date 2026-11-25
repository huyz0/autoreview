use std::collections::BTreeMap;

use super::super::KeyedFunction;
use super::{effects, HelperOutcome, HelperSummary, KeyedUnit, Place, Program};

pub(super) fn build(
    program: &Program<'_, '_, '_>,
    units: &[KeyedUnit<'_, '_, '_>],
) -> BTreeMap<usize, HelperSummary> {
    let mut summaries = BTreeMap::new();
    for (index, frame) in program.frames.iter().enumerate() {
        if frame.plan.is_some() {
            let empty = HelperOutcome {
                parameters: vec![false; frame.params.len()],
                origins: Vec::new(),
            };
            summaries.insert(
                index,
                HelperSummary {
                    when_discarded: empty.clone(),
                    when_observed: empty,
                },
            );
        }
    }
    let budget = program.frames.len().saturating_mul(16).max(16);
    let mut converged = false;
    for _ in 0..budget {
        let previous = summaries.clone();
        summaries = round(program, units, &previous);
        if summaries == previous {
            converged = true;
            break;
        }
    }
    if !converged {
        summaries.clear();
    }
    summaries
}

fn round(
    program: &Program<'_, '_, '_>,
    units: &[KeyedUnit<'_, '_, '_>],
    previous: &BTreeMap<usize, HelperSummary>,
) -> BTreeMap<usize, HelperSummary> {
    let mut summaries = BTreeMap::new();
    let mut index = 0;
    for unit in units {
        for function in unit.functions {
            let frame = &program.frames[index];
            if let Some(plan) = &frame.plan {
                let calls = program
                    .resolved_calls(index, function.graph, plan)
                    .into_iter()
                    .filter_map(|(site, target)| {
                        previous
                            .get(&target)
                            .cloned()
                            .map(|summary| (site, summary))
                    })
                    .collect();
                let mut summary = summarize(plan, function, unit, &frame.params, &calls);
                set_origin_paths(&mut summary, unit.path);
                summaries.insert(index, summary);
            }
            index += 1;
        }
    }
    summaries
}

fn summarize(
    plan: &super::Plan<'_, '_>,
    function: &KeyedFunction<'_, '_>,
    unit: &KeyedUnit<'_, '_, '_>,
    params: &[String],
    calls: &BTreeMap<usize, HelperSummary>,
) -> HelperSummary {
    let body_start = function
        .node
        .child_by_field_name("body")
        .map_or(function.node.end_byte().saturating_sub(1), |body| {
            body.start_byte()
        });
    let parameters: Vec<_> = params
        .iter()
        .map(|name| {
            plan.syntax
                .local_result_binding(name, body_start, false)
                .map(Place::Local)
        })
        .collect();
    effects::lower(plan, function, unit.source, true, calls).helper_summary(&parameters)
}

fn set_origin_paths(summary: &mut HelperSummary, path: &str) {
    for outcome in [&mut summary.when_discarded, &mut summary.when_observed] {
        for (origin, _) in &mut outcome.origins {
            if origin.path.is_none() {
                origin.path = (!path.is_empty()).then(|| path.to_owned());
            }
        }
    }
}

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::cfg::{Cfg, Stmt};

use super::super::KeyedFunction;
use super::{effects, HelperOutcome, HelperSummary, KeyedUnit, Place, Plan, Program};

pub(super) fn build(
    program: &Program<'_, '_, '_>,
    units: &[KeyedUnit<'_, '_, '_>],
) -> (BTreeMap<usize, HelperSummary>, bool) {
    let count = program.frames.len();
    // The former algorithm allowed 16 * N complete rounds of N functions.
    // Keep the same worst-case evaluation budget for the worklist.
    let budget = count
        .saturating_mul(count.max(1))
        .saturating_mul(16)
        .max(16);
    build_with_budget(program, units, budget)
}

fn build_with_budget(
    program: &Program<'_, '_, '_>,
    units: &[KeyedUnit<'_, '_, '_>],
    budget: usize,
) -> (BTreeMap<usize, HelperSummary>, bool) {
    let functions: Vec<_> = units
        .iter()
        .flat_map(|unit| unit.functions.iter().map(move |function| (unit, function)))
        .collect();
    let SummaryGraph {
        mut summaries,
        calls,
        callers,
    } = SummaryGraph::new(program);
    let mut queued: Vec<_> = program
        .frames
        .iter()
        .map(|frame| frame.plan.is_some())
        .collect();
    let mut pending: VecDeque<_> = (0..functions.len())
        .rev()
        .filter(|&index| queued[index])
        .collect();
    let mut remaining = budget;
    while let Some(index) = pending.pop_front() {
        let Some(next) = remaining.checked_sub(1) else {
            return (BTreeMap::new(), true);
        };
        remaining = next;
        queued[index] = false;
        let frame = &program.frames[index];
        let plan = frame
            .plan
            .as_ref()
            .expect("only planned functions are queued");
        let (unit, function) = functions[index];
        let available = select_summaries(&calls[index], &summaries);
        let mut summary = summarize(plan, function, unit, &frame.params, &available);
        set_origin_paths(&mut summary, unit.path);
        if summaries.get(&index) != Some(&summary) {
            summaries.insert(index, summary);
            for &caller in &callers[index] {
                if !queued[caller] {
                    queued[caller] = true;
                    pending.push_back(caller);
                }
            }
        }
    }
    (summaries, false)
}

struct SummaryGraph {
    summaries: BTreeMap<usize, HelperSummary>,
    calls: Vec<BTreeSet<((usize, usize), usize)>>,
    callers: Vec<Vec<usize>>,
}

impl SummaryGraph {
    fn new(program: &Program<'_, '_, '_>) -> Self {
        let mut summaries = BTreeMap::new();
        let mut calls = vec![BTreeSet::new(); program.frames.len()];
        let mut callers = vec![Vec::new(); program.frames.len()];
        for (index, frame) in program.frames.iter().enumerate() {
            let Some(plan) = &frame.plan else { continue };
            summaries.insert(index, empty_summary(frame.params.len()));
            calls[index] = program.resolved_calls(index, frame.graph, plan);
            for &(_, target) in &calls[index] {
                callers[target].push(index);
            }
        }
        Self {
            summaries,
            calls,
            callers,
        }
    }
}

fn empty_summary(parameters: usize) -> HelperSummary {
    let empty = HelperOutcome {
        parameters: vec![false; parameters],
        unobserved_parameters: vec![None; parameters],
        conditional_parameters: None,
        origins: Vec::new(),
        opaque_parameters: vec![false; parameters],
        opaque_origins: Vec::new(),
    };
    HelperSummary {
        when_discarded: empty.clone(),
        when_observed: empty,
    }
}

fn summarize(
    plan: &super::Plan<'_, '_>,
    function: &KeyedFunction<'_, '_>,
    unit: &KeyedUnit<'_, '_, '_>,
    params: &[String],
    calls: &BTreeMap<(usize, usize), HelperSummary>,
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
    let analysis = effects::lower(plan, function, unit.source, true, calls);
    let mut summary = analysis.helper_summary(&parameters);
    let syntax = (function.node, unit.source);
    let guard = (plan.spec.contract.as_ref().is_some_and(|contract| contract.language != crate::call_order::ContractLanguage::Go))
        .then(|| super::partial::unique_parameter_guard(function.graph, plan, params)).flatten();
    for (used, outcome) in [(false, &mut summary.when_discarded), (true, &mut summary.when_observed)] {
        outcome.unobserved_parameters = super::partial::unobserved_parameters(
            &analysis.owner, function.graph, plan, syntax, super::partial::ParameterQuery {
                parameters: &parameters, return_used: used, has_closures: !analysis.closures.is_empty(), assumed_guard: None,
            },
        );
        if let Some((guard_index, identity)) = &guard {
            let specialized = |truth| super::partial::unobserved_parameters(
                &analysis.owner, function.graph, plan, syntax,
                super::partial::ParameterQuery { parameters: &parameters, return_used: used,
                    has_closures: !analysis.closures.is_empty(), assumed_guard: Some((identity.clone(), truth)) },
            );
            outcome.conditional_parameters = Some(super::ConditionalParameters {
                guard_index: *guard_index, when_false: specialized(false), when_true: specialized(true),
            });
        }
    }
    summary
}

fn set_origin_paths(summary: &mut HelperSummary, path: &str) {
    for outcome in [&mut summary.when_discarded, &mut summary.when_observed] {
        for (origin, _) in &mut outcome.origins {
            if origin.path.is_none() {
                origin.path = (!path.is_empty()).then(|| path.to_owned());
            }
        }
        for origin in &mut outcome.opaque_origins {
            if origin.path.is_none() { origin.path = (!path.is_empty()).then(|| path.to_owned()); }
        }
    }
}

pub(super) fn resolved_helpers(
    program: &Program<'_, '_, '_>,
    index: usize,
    graph: &Cfg<Stmt>,
    plan: &Plan<'_, '_>,
    summaries: &BTreeMap<usize, HelperSummary>,
) -> BTreeMap<(usize, usize), HelperSummary> {
    select_summaries(&program.resolved_calls(index, graph, plan), summaries)
}

fn select_summaries(
    calls: &BTreeSet<((usize, usize), usize)>,
    summaries: &BTreeMap<usize, HelperSummary>,
) -> BTreeMap<(usize, usize), HelperSummary> {
    calls
        .iter()
        .filter_map(|(site, target)| {
            summaries
                .get(target)
                .cloned()
                .map(|summary| (*site, summary))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call_order::ContractLanguage;
    use autoreview_langsupport::Language;

    #[test]
    fn helper_summaries_with_shared_start_keep_independent_effects() {
        let mut inner = empty_summary(1);
        inner.when_observed.parameters[0] = true;
        let outer = empty_summary(1);
        let calls = BTreeSet::from([((10, 15), 0), ((10, 17), 1)]);
        let summaries = BTreeMap::from([(0, inner), (1, outer)]);
        let selected = select_summaries(&calls, &summaries);
        assert!(selected[&(10, 15)].when_observed.parameters[0]);
        assert!(!selected[&(10, 17)].when_observed.parameters[0]);
    }

    #[test]
    fn helper_parameter_paths_distinguish_partial_and_complete_returns() {
        for (source, expected) in [
            ("function helper(value, flag) { if (flag) return value; }", true),
            ("function helper(value, flag) { if (flag) return value; else return value; }", false),
            ("function helper(value, flag) { if (flag) { if (flag) return value; } else return value; }", false),
        ] {
            super::super::test_support::with_program((Language::JavaScript, ContractLanguage::Web), source, ("unused", "missing"), |program, units| {
                let (summaries, exhausted) = build(program, units);
                assert!(!exhausted);
                assert_eq!(summaries[&0].when_observed.unobserved_parameters[0], Some(expected));
                assert_eq!(summaries[&0].when_discarded.unobserved_parameters[0], Some(true));
                let conditional = summaries[&0].when_observed.conditional_parameters.as_ref().unwrap();
                assert_eq!(conditional.guard_index, 1);
                assert_eq!(conditional.when_true[0], Some(false));
                assert_eq!(conditional.when_false[0], Some(expected));
            });
        }
    }

    #[test]
    fn helper_parameter_paths_reject_unstable_or_cyclic_control_flow() {
        for source in [
            "function helper(value, flag) { if (flag) { flag = false; } if (flag) return value; }",
            "function helper(value, flag) { while (flag) { value = value; } return value; }",
        ] {
            super::super::test_support::with_program((Language::JavaScript, ContractLanguage::Web), source, ("unused", "missing"), |program, units| {
                let (summaries, exhausted) = build(program, units);
                assert!(!exhausted);
                assert_eq!(summaries[&0].when_observed.unobserved_parameters[0], None, "{source}");
            });
        }
    }

    #[test]
    fn reports_summary_budget_exhaustion() {
        let source = "package main\nimport \"os\"\nfunc move(a, b string) error { return os.Rename(a, b) }\n";
        super::super::test_support::with_program(
            (Language::Go, ContractLanguage::Go),
            source,
            ("os", "Rename"),
            |program, units| {
                let (summaries, exhausted) = build_with_budget(program, units, 0);
                assert!(exhausted && summaries.is_empty());
                assert!(!build(program, units).1);
            },
        );
    }

    #[test]
    fn worklist_matches_rounds_and_avoids_rescanning_a_helper_chain() {
        let mut source = String::from("package main\nimport \"os\"\n");
        let count = 40;
        for index in 0..count - 1 {
            source.push_str(&format!(
                "func f{index}(p string) error {{ return f{}(p) }}\n",
                index + 1
            ));
        }
        source.push_str(&format!(
            "func f{}(p string) error {{ return os.Remove(p) }}\n",
            count - 1
        ));
        with_remove_program(&source, |program, units| {
            let (actual, exhausted) = build_with_budget(program, units, count);
            assert!(
                !exhausted,
                "the chain should converge in one dependency-ordered pass"
            );
            assert!(build_with_budget(program, units, count - 1).1);
            assert!(actual == reference_rounds(program, units));
        });
    }

    #[test]
    fn worklist_matches_rounds_for_recursive_and_branching_helpers() {
        let examples = [
            r#"package main
import "os"
func first(p string, n int) error {
    if n > 0 { return second(p, n - 1) }
    return os.Remove(p)
}
func second(p string, n int) error {
    if n > 0 { return first(p, n - 1) }
    return os.Remove(p)
}
func ignore(p string) { first(p, 2) }
"#,
            r#"package main
import "os"
func forward(err error) error { return err }
func discard(err error) {}
func run(p string, cond bool) error {
    result := os.Remove(p)
    if cond { discard(result); return nil }
    return forward(result)
}
"#,
            r#"package main
import "os"
func recur(p string, n int) error {
    if n > 0 { recur(p, n - 1) }
    return os.Remove(p)
}
func use(p string) error { return recur(p, 2) }
func drop(p string) { recur(p, 3) }
"#,
        ];
        for source in examples {
            with_remove_program(source, |program, units| {
                let (actual, exhausted) = build(program, units);
                assert!(!exhausted);
                assert!(actual == reference_rounds(program, units), "{source}");
            });
        }
    }

    fn with_remove_program(
        source: &str,
        check: impl FnOnce(&Program<'_, '_, '_>, &[KeyedUnit<'_, '_, '_>]),
    ) {
        super::super::test_support::with_program(
            (Language::Go, ContractLanguage::Go),
            source,
            ("os", "Remove"),
            check,
        );
    }

    fn reference_rounds(
        program: &Program<'_, '_, '_>,
        units: &[KeyedUnit<'_, '_, '_>],
    ) -> BTreeMap<usize, HelperSummary> {
        let mut previous = BTreeMap::new();
        for (index, frame) in program.frames.iter().enumerate() {
            if frame.plan.is_some() {
                previous.insert(index, empty_summary(frame.params.len()));
            }
        }
        for _ in 0..program.frames.len() * 16 {
            let mut next = BTreeMap::new();
            let mut index = 0;
            for unit in units {
                for function in unit.functions {
                    let frame = &program.frames[index];
                    if let Some(plan) = &frame.plan {
                        let calls =
                            resolved_helpers(program, index, function.graph, plan, &previous);
                        let mut summary = summarize(plan, function, unit, &frame.params, &calls);
                        set_origin_paths(&mut summary, unit.path);
                        next.insert(index, summary);
                    }
                    index += 1;
                }
            }
            if next == previous {
                return next;
            }
            previous = next;
        }
        panic!("reference summaries did not converge");
    }
}

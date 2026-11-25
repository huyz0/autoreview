use std::collections::BTreeMap;

use crate::cfg::{Cfg, Stmt};

use super::super::KeyedFunction;
use super::{effects, HelperOutcome, HelperSummary, KeyedUnit, Place, Plan, Program};

pub(super) fn build(
    program: &Program<'_, '_, '_>,
    units: &[KeyedUnit<'_, '_, '_>],
) -> (BTreeMap<usize, HelperSummary>, bool) {
    let budget = program.frames.len().saturating_mul(16).max(16);
    build_with_budget(program, units, budget)
}

fn build_with_budget(
    program: &Program<'_, '_, '_>,
    units: &[KeyedUnit<'_, '_, '_>],
    budget: usize,
) -> (BTreeMap<usize, HelperSummary>, bool) {
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
    (summaries, !converged)
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
                let calls = resolved_helpers(program, index, function.graph, plan, previous);
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

pub(super) fn resolved_helpers(
    program: &Program<'_, '_, '_>,
    index: usize,
    graph: &Cfg<Stmt>,
    plan: &Plan<'_, '_>,
    summaries: &BTreeMap<usize, HelperSummary>,
) -> BTreeMap<usize, HelperSummary> {
    program
        .resolved_calls(index, graph, plan)
        .into_iter()
        .filter_map(|(site, target)| {
            summaries
                .get(&target)
                .cloned()
                .map(|summary| (site, summary))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call_order::{CallOrderSpec, ContractLanguage, ContractMode, ResultContract};
    use autoreview_langsupport::Language;

    #[test]
    fn reports_summary_budget_exhaustion() {
        let source = "package main\nimport \"os\"\nfunc move(a, b string) error { return os.Rename(a, b) }\n";
        let mut parser = autoreview_langsupport::parser_for(Language::Go).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let function = root.named_children(&mut root.walk()).find(|node| node.kind() == "function_declaration").unwrap();
        let graph = crate::lower::keyed_variant(Language::Go, source.as_bytes(), function);
        let functions = [KeyedFunction { node: function, graph: &graph }];
        let units = [KeyedUnit { path: "main.go", source: source.as_bytes(), functions: &functions, imports: &[], entry: true }];
        let spec = CallOrderSpec {
            rule_id: "test".into(), after: vec![], unless: vec![], before: vec![],
            check_before_return: false, identity: None,
            contract: Some(ResultContract { mode: ContractMode::MustUse, language: ContractLanguage::Go,
                import: "os".into(), methods: vec!["Rename".into()], borrows: vec![],
                acquire_awaited: false, complete_observed: false }),
        };
        let program = Program::new(&spec, &units);
        let (summaries, exhausted) = build_with_budget(&program, &units, 0);
        assert!(exhausted && summaries.is_empty());
        assert!(!build(&program, &units).1);
    }
}

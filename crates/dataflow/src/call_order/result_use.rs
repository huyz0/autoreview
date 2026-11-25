//! Local liveness for API results. A read must observe the value that
//! reached it, so later assignments to the same local cannot satisfy it.

mod closures;
mod effects;
mod places;
mod references;
mod syntax;

use std::collections::{BTreeMap, HashSet, VecDeque};

use crate::cfg::{Cfg, EdgeKind, NodeId, Stmt};

use super::{keyed::program::Program, plan::Plan, CallOrderHit, CallOrderSpec, ContractLanguage, KeyedUnit};
use effects::{Demand, Effect, Origin, Value, Write};
use places::Place;

pub(super) fn check_units(spec: &CallOrderSpec, units: &[KeyedUnit<'_, '_, '_>]) -> Vec<CallOrderHit> {
    let mut hits = Vec::new();
    let program = Program::new(spec, units);
    let mut summaries = BTreeMap::new();
    for (index, frame) in program.frames.iter().enumerate() {
        if frame.plan.is_some() {
            let empty = HelperOutcome { parameters: vec![false; frame.params.len()], origins: Vec::new() };
            summaries.insert(index, HelperSummary { when_discarded: empty.clone(), when_observed: empty });
        }
    }
    let budget = program.frames.len().saturating_mul(16).max(16);
    let mut converged = false;
    for _ in 0..budget {
        let previous = summaries.clone();
        let mut index = 0;
        for unit in units {
            for function in unit.functions {
                let frame = &program.frames[index];
                if let Some(plan) = &frame.plan {
                    let resolved = program.resolved_calls(index, function.graph, plan);
                    let calls: BTreeMap<_, _> = resolved.into_iter()
                        .filter_map(|(site, target)| previous.get(&target).cloned().map(|summary| (site, summary)))
                        .collect();
                    let body_start = function.node.child_by_field_name("body")
                        .map_or(function.node.end_byte().saturating_sub(1), |body| body.start_byte());
                    let parameters: Vec<_> = frame.params.iter().map(|name| {
                        plan.syntax.local_result_binding(name, body_start, false).map(Place::Local)
                    }).collect();
                    let analysis = effects::lower(plan, function, unit.source, true, &calls);
                    let mut summary = analysis.helper_summary(&parameters);
                    for outcome in [&mut summary.when_discarded, &mut summary.when_observed] {
                        for (origin, _) in &mut outcome.origins {
                            if origin.path.is_none() {
                                origin.path = (!unit.path.is_empty()).then(|| unit.path.to_owned());
                            }
                        }
                    }
                    summaries.insert(index, summary);
                }
                index += 1;
            }
        }
        if summaries == previous {
            converged = true;
            break;
        }
    }
    if !converged {
        summaries.clear();
    }
    let mut frame_index = 0;
    for unit in units {
        for function in unit.functions {
            let frame = &program.frames[frame_index];
            let current = frame_index;
            frame_index += 1;
            if !unit.entry {
                continue;
            }
            let Some(plan) = frame.plan.as_ref() else { continue };
            let calls: BTreeMap<_, _> = program.resolved_calls(current, function.graph, plan)
                .into_iter().filter_map(|(site, target)| summaries.get(&target).cloned().map(|summary| (site, summary)))
                .collect();
            if !plan.has_registration() && calls.is_empty() {
                continue;
            }
            let analysis = effects::lower(plan, function, unit.source, false, &calls);
            hits.extend(analysis.unused().map(|(node, origin)| CallOrderHit {
                trigger_call: origin.call,
                node,
                source_line: origin.line,
                path: origin.path.or_else(|| (!unit.path.is_empty()).then(|| unit.path.to_owned())),
                entry: None,
            }));
        }
    }
    let mut seen = HashSet::new();
    hits.retain(|hit| seen.insert((hit.path.clone(), hit.source_line, hit.trigger_call.clone())));
    hits
}

type Live = HashSet<Place>;

struct Analysis {
    owner: Body,
    closures: Vec<Body>,
}

struct Body {
    flow: Flow,
    effects: Vec<Vec<Effect>>,
}

#[derive(Clone, PartialEq, Eq)]
struct HelperSummary {
    when_discarded: HelperOutcome,
    when_observed: HelperOutcome,
}

#[derive(Clone, PartialEq, Eq)]
struct HelperOutcome {
    parameters: Vec<bool>,
    origins: Vec<(Origin, bool)>,
}

impl Analysis {
    fn unused(&self) -> impl Iterator<Item = (NodeId, Origin)> {
        let seed = Live::new();
        let mut origins = BTreeMap::new();
        self.record(&seed, &mut |node, origin, used| {
            // A finally block or a closure called twice has multiple CFG paths to its site.
            origins
                .entry((origin.path.clone(), origin.start, origin.calls.clone()))
                .and_modify(|(_, _, live)| *live |= used)
                .or_insert_with(|| (node, origin.clone(), used));
        });
        origins.into_values().filter_map(|(node, origin, used)| (!used).then_some((node, origin)))
    }

    fn record(&self, seed: &Live, report: &mut dyn FnMut(NodeId, &Origin, bool)) -> Live {
        let facts = self.owner.solve(seed, &self.closures);
        self.owner.record(&facts, seed, &self.closures, report);
        facts[self.owner.flow.entry].clone()
    }

    fn helper_summary(&self, parameters: &[Option<Place>]) -> HelperSummary {
        HelperSummary {
            when_discarded: self.helper_outcome(parameters, false),
            when_observed: self.helper_outcome(parameters, true),
        }
    }

    fn helper_outcome(&self, parameters: &[Option<Place>], return_used: bool) -> HelperOutcome {
        let mut seed = Live::new();
        if return_used {
            seed.insert(Place::Return);
        }
        let mut origins = BTreeMap::new();
        let live = self.record(&seed, &mut |_, origin, used| {
            origins.entry((origin.path.clone(), origin.start, origin.calls.clone())).and_modify(|(_, previous)| *previous |= used)
                .or_insert_with(|| (origin.clone(), used));
        });
        HelperOutcome {
            parameters: parameters.iter().map(|parameter| parameter.is_none_or(|place| live.contains(&place))).collect(),
            origins: origins.into_values().collect(),
        }
    }
}

struct Flow {
    entry: NodeId,
    exit: NodeId,
    successors: Vec<Vec<NodeId>>,
    predecessors: Vec<Vec<NodeId>>,
    reachable: Vec<bool>,
}

impl Flow {
    fn new(cfg: &Cfg<Stmt>, plan: &Plan<'_, '_>) -> Self {
        let possible = |from: NodeId, edge| match cfg.nodes[from].condition.as_ref() {
            Some((value, polarity))
                if matches!(value.as_str(), "true" | "false") && !plan.syntax.shadows_api(value) =>
            {
                match edge {
                    EdgeKind::True => (value == "true") == *polarity,
                    EdgeKind::False => (value == "true") != *polarity,
                    _ => true,
                }
            }
            _ => true,
        };
        let reachable = cfg.reachable_where(possible);
        let mut successors = vec![Vec::new(); cfg.nodes.len()];
        let mut predecessors = successors.clone();
        for &(from, to, edge) in &cfg.edges {
            // An uncaught exception cannot deliver the caller's normal
            // continuation into a closure body.
            let throws = to == cfg.exit && cfg.nodes[from].stmts.iter().any(|stmt| matches!(stmt, Stmt::Throw { .. }));
            if reachable[from] && possible(from, edge) && !throws {
                successors[from].push(to);
                predecessors[to].push(from);
            }
        }
        Self { entry: cfg.entry, exit: cfg.exit, successors, predecessors, reachable }
    }

    fn live_after(&self, node: NodeId, facts: &[Live], seed: &Live) -> Live {
        if node == self.exit {
            return seed.clone();
        }
        self.successors[node].iter().flat_map(|&next| facts[next].iter().copied()).collect()
    }
}

type Observer<'a> = dyn FnMut(&Origin, bool) + 'a;

impl Body {
    fn solve(&self, seed: &Live, closures: &[Body]) -> Vec<Live> {
        let mut facts = vec![Live::new(); self.effects.len()];
        let mut queued = self.flow.reachable.clone();
        let mut work: VecDeque<_> = (0..self.effects.len()).rev().filter(|&node| queued[node]).collect();
        while let Some(node) = work.pop_front() {
            queued[node] = false;
            let mut live = self.flow.live_after(node, &facts, seed);
            transfer(&self.effects[node], &mut live, closures, None);
            if facts[node] != live {
                facts[node] = live;
                for &before in &self.flow.predecessors[node] {
                    if !std::mem::replace(&mut queued[before], true) {
                        work.push_back(before);
                    }
                }
            }
        }
        facts
    }

    fn record(&self, facts: &[Live], seed: &Live, closures: &[Body], report: &mut dyn FnMut(NodeId, &Origin, bool)) {
        for (node, effects) in self.effects.iter().enumerate().filter(|(node, _)| self.flow.reachable[*node]) {
            let mut live = self.flow.live_after(node, facts, seed);
            transfer(effects, &mut live, closures, Some(&mut |origin, used| report(node, origin, used)));
        }
    }

    fn invoke(&self, live: &mut Live, demand: Demand, report: Option<&mut Observer<'_>>) {
        let mut seed = std::mem::take(live);
        if demand.observed(&mut seed) {
            seed.insert(Place::Return);
        }
        let facts = self.solve(&seed, &[]);
        live.clone_from(&facts[self.flow.entry]);
        live.remove(&Place::Return);
        if let Some(report) = report {
            self.record(&facts, &seed, &[], &mut |_, origin, used| report(origin, used));
        }
    }
}

fn transfer(effects: &[Effect], live: &mut Live, closures: &[Body], mut observe: Option<&mut Observer<'_>>) {
    for effect in effects.iter().rev() {
        match effect {
            Effect::Read(binding) => {
                live.insert(*binding);
            }
            Effect::Write(writes) => {
                transfer_writes(writes, live, observe.as_deref_mut());
            }
            Effect::Invoke(index, demand) => {
                closures[*index].invoke(live, *demand, observe.as_deref_mut());
            }
            Effect::ApplyHelper { arguments, demand, when_discarded, when_observed, origins_discarded, origins_observed } => {
                let observed = demand.observed(live);
                let required = if observed { when_observed } else { when_discarded };
                for (argument, used) in arguments.iter().zip(required) {
                    if *used {
                        live.extend(*argument);
                    }
                }
                if let Some(report) = &mut observe {
                    let origins = if observed { origins_observed } else { origins_discarded };
                    for (origin, used) in origins {
                        report(origin, *used);
                    }
                }
            }
        }
    }
}

fn transfer_writes(writes: &[Write], live: &mut Live, mut observe: Option<&mut Observer<'_>>) {
    // Go multi-assignment evaluates every RHS before replacing any target.
    let mut copies = Vec::new();
    for write in writes.iter().rev() {
        let used = write.target.is_some_and(|binding| live.remove(&binding));
        match &write.value {
            Value::Source(origin) => {
                if let Some(report) = &mut observe {
                    report(origin, used);
                }
            }
            Value::Copy(binding) if used => copies.push(*binding),
            _ => {}
        }
    }
    live.extend(copies);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call_order::{ContractMode, ResultContract};
    use autoreview_langsupport::Language;

    #[test]
    fn helper_effect_propagates_only_the_arguments_required_by_the_caller() {
        let output = Place::CallResult(50);
        let effects = [Effect::ApplyHelper {
            arguments: vec![Some(Place::Local(1)), Some(Place::Local(2))],
            demand: Demand::Slot(output),
            when_discarded: vec![true, false],
            when_observed: vec![true, true],
            origins_discarded: vec![],
            origins_observed: vec![],
        }];
        for observed in [false, true] {
            let mut live = Live::new();
            if observed { live.insert(output); }
            transfer(&effects, &mut live, &[], None);
            assert!(live.contains(&Place::Local(1)));
            assert_eq!(live.contains(&Place::Local(2)), observed);
            assert!(!live.contains(&output));
        }
    }

    #[test]
    fn helper_summaries_follow_rust_and_kotlin_expression_returns() {
        let examples = [
            (Language::Rust, ContractLanguage::Rust, "use std::fs::remove_file; fn run(p: &str) -> Result<(), Error> { remove_file(p) }", "std::fs", "remove_file"),
            (Language::Kotlin, ContractLanguage::Kotlin, "import kotlin.runCatching\nfun run(s: String) = runCatching { s.toInt() }", "kotlin", "runCatching"),
        ];
        for (language, contract_language, source, import, method) in examples {
            let mut parser = autoreview_langsupport::parser_for(language).unwrap();
            let tree = parser.parse(source, None).unwrap();
            let root = tree.root_node();
            let function = root.named_children(&mut root.walk())
                .find(|node| matches!(node.kind(), "function_item" | "function_declaration")).unwrap();
            let graph = crate::lower::keyed_variant(language, source.as_bytes(), function);
            let spec = CallOrderSpec {
                rule_id: "test".into(), after: vec![], unless: vec![], before: vec![],
                check_before_return: false, identity: None,
                contract: Some(ResultContract {
                    mode: ContractMode::MustUse, language: contract_language,
                    import: import.into(), methods: vec![method.into()], borrows: vec![],
                    acquire_awaited: false, complete_observed: false,
                }),
            };
            let plan = Plan::new(&spec, &graph, function, source.as_bytes(), 0).unwrap();
            let analysis = effects::lower(&plan, &super::super::KeyedFunction { node: function, graph: &graph }, source.as_bytes(), true, &BTreeMap::new());
            for (observed, expected) in [(false, false), (true, true)] {
                let mut seed = Live::new();
                if observed { seed.insert(Place::Return); }
                let mut found = Vec::new();
                analysis.record(&seed, &mut |_, _, used| found.push(used));
                assert_eq!(found, [expected], "{language:?}");
            }
        }
    }

    #[test]
    fn helper_summary_respects_the_callers_return_demand() {
        let source = b"package p\nimport \"os\"\nfunc moveFile(a, b string) error { return os.Rename(a, b) }\n";
        let mut parser = autoreview_langsupport::parser_for(Language::Go).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let function = root.named_children(&mut root.walk())
            .find(|node| node.kind() == "function_declaration").unwrap();
        let graph = crate::lower::keyed_variant(Language::Go, source, function);
        let spec = CallOrderSpec {
            rule_id: "test".into(), after: vec![], unless: vec![], before: vec![],
            check_before_return: false, identity: None,
            contract: Some(ResultContract {
                mode: ContractMode::MustUse, language: ContractLanguage::Go,
                import: "os".into(), methods: vec!["Rename".into()], borrows: vec![],
                acquire_awaited: false, complete_observed: false,
            }),
        };
        let plan = Plan::new(&spec, &graph, function, source, 0).unwrap();
        let analysis = effects::lower(&plan, &super::super::KeyedFunction { node: function, graph: &graph }, source, true, &BTreeMap::new());
        for (observed, expected) in [(false, false), (true, true)] {
            let mut seed = Live::new();
            if observed { seed.insert(Place::Return); }
            let mut found = Vec::new();
            analysis.record(&seed, &mut |_, origin, used| found.push((origin.call.clone(), used)));
            assert_eq!(found, [("os.Rename".into(), expected)]);
        }
    }

    #[test]
    fn helper_parameter_liveness_distinguishes_use_from_forwarding() {
        let source = b"package p\nimport \"os\"\nfunc forward(err error) error { return err }\n";
        let mut parser = autoreview_langsupport::parser_for(Language::Go).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let function = root.named_children(&mut root.walk())
            .find(|node| node.kind() == "function_declaration").unwrap();
        let graph = crate::lower::keyed_variant(Language::Go, source, function);
        let spec = CallOrderSpec {
            rule_id: "test".into(), after: vec![], unless: vec![], before: vec![],
            check_before_return: false, identity: None,
            contract: Some(ResultContract {
                mode: ContractMode::MustUse, language: ContractLanguage::Go,
                import: "os".into(), methods: vec!["Rename".into()], borrows: vec![],
                acquire_awaited: false, complete_observed: false,
            }),
        };
        let plan = Plan::new(&spec, &graph, function, source, 0).unwrap();
        let body = function.child_by_field_name("body").unwrap();
        let parameter = plan.syntax.local_result_binding("err", body.start_byte(), false).unwrap();
        let analysis = effects::lower(&plan, &super::super::KeyedFunction { node: function, graph: &graph }, source, true, &BTreeMap::new());
        for (observed, expected) in [(false, false), (true, true)] {
            let mut seed = Live::new();
            if observed { seed.insert(Place::Return); }
            let entry = analysis.record(&seed, &mut |_, _, _| {});
            assert_eq!(entry.contains(&Place::Local(parameter)), expected);
        }
        let summary = analysis.helper_summary(&[Some(Place::Local(parameter))]);
        assert_eq!(summary.when_discarded.parameters, [false]);
        assert_eq!(summary.when_observed.parameters, [true]);
        assert!(summary.when_discarded.origins.is_empty());
    }
}

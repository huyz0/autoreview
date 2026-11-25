//! Local liveness for API results. A read must observe the value that
//! reached it, so later assignments to the same local cannot satisfy it.

mod closures;
mod effects;
mod places;
mod references;
mod summaries;
mod syntax;
#[cfg(test)]
mod test_support;

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use crate::cfg::{Cfg, EdgeKind, NodeId, Stmt};

use super::{
    keyed::program::Program, plan::Plan, CallOrderHit, CallOrderSpec, ContractLanguage, KeyedUnit,
};
use effects::{Demand, Effect, HelperCall, Origin, Value, Write};
use places::Place;

pub(super) fn check_units(
    spec: &CallOrderSpec,
    units: &[KeyedUnit<'_, '_, '_>],
) -> (Vec<CallOrderHit>, bool, Vec<super::keyed::KeyedLimitation>) {
    check_units_with_sources(spec, units, &BTreeMap::new())
}

fn check_units_with_sources(
    spec: &CallOrderSpec,
    units: &[KeyedUnit<'_, '_, '_>],
    sources: &BTreeMap<usize, BTreeSet<usize>>,
) -> (Vec<CallOrderHit>, bool, Vec<super::keyed::KeyedLimitation>) {
    let mut hits = Vec::new();
    let mut program = Program::new(spec, units);
    for frame in &mut program.frames {
        if let (Some(plan), Some(offsets)) = (&mut frame.plan, sources.get(&frame.unit)) {
            plan.extra_sources.clone_from(offsets);
        }
    }
    if !program
        .frames
        .iter()
        .filter_map(|frame| frame.plan.as_ref())
        .any(Plan::has_registration)
    {
        return (hits, false, Vec::new());
    }
    let (summaries, summary_limit) = summaries::build(&program, units);
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
                summaries::resolved_helpers(&program, current, function.graph, plan, &summaries);
            if !plan.has_registration() && calls.is_empty() {
                continue;
            }
            let analysis = effects::lower(plan, function, unit.source, false, &calls);
            hits.extend(analysis.unused().map(|(node, origin)| {
                CallOrderHit {
                    trigger_call: origin.call,
                    node,
                    source_line: origin.line,
                    path: origin
                        .path
                        .or_else(|| (!unit.path.is_empty()).then(|| unit.path.to_owned())),
                    entry: None,
                    related_calls: origin
                        .calls
                        .iter()
                        .filter_map(|&(unit_index, offset)| {
                            let unit = units.get(unit_index)?;
                            let before = unit.source.get(..offset)?;
                            let line =
                                before.iter().filter(|byte| **byte == b'\n').count() as u32 + 1;
                            (!unit.path.is_empty()).then(|| (unit.path.to_owned(), line))
                        })
                        .collect(),
                }
            }));
        }
    }
    let mut combined = BTreeMap::new();
    for hit in hits {
        let key = (hit.path.clone(), hit.source_line, hit.trigger_call.clone());
        combined
            .entry(key)
            .and_modify(|previous: &mut CallOrderHit| {
                previous
                    .related_calls
                    .extend(hit.related_calls.iter().cloned());
                previous.related_calls.sort();
                previous.related_calls.dedup();
            })
            .or_insert(hit);
    }
    (combined.into_values().collect(), summary_limit, program.resolution_limitations())
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
        origins
            .into_values()
            .filter_map(|(node, origin, used)| (!used).then_some((node, origin)))
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
            origins
                .entry((origin.path.clone(), origin.start, origin.calls.clone()))
                .and_modify(|(_, previous)| *previous |= used)
                .or_insert_with(|| (origin.clone(), used));
        });
        HelperOutcome {
            parameters: parameters
                .iter()
                .map(|parameter| parameter.is_none_or(|place| live.contains(&place)))
                .collect(),
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
                if matches!(value.as_str(), "true" | "false")
                    && !plan.syntax.shadows_api(value) =>
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
            let throws = to == cfg.exit
                && cfg.nodes[from]
                    .stmts
                    .iter()
                    .any(|stmt| matches!(stmt, Stmt::Throw { .. }));
            if reachable[from] && possible(from, edge) && !throws {
                successors[from].push(to);
                predecessors[to].push(from);
            }
        }
        Self {
            entry: cfg.entry,
            exit: cfg.exit,
            successors,
            predecessors,
            reachable,
        }
    }

    fn live_after(&self, node: NodeId, facts: &[Live], seed: &Live) -> Live {
        if node == self.exit {
            return seed.clone();
        }
        self.successors[node]
            .iter()
            .flat_map(|&next| facts[next].iter().copied())
            .collect()
    }
}

type Observer<'a> = dyn FnMut(&Origin, bool) + 'a;

impl Body {
    fn solve(&self, seed: &Live, closures: &[Body]) -> Vec<Live> {
        let mut facts = vec![Live::new(); self.effects.len()];
        let mut queued = self.flow.reachable.clone();
        let mut work: VecDeque<_> = (0..self.effects.len())
            .rev()
            .filter(|&node| queued[node])
            .collect();
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

    fn record(
        &self,
        facts: &[Live],
        seed: &Live,
        closures: &[Body],
        report: &mut dyn FnMut(NodeId, &Origin, bool),
    ) {
        for (node, effects) in self
            .effects
            .iter()
            .enumerate()
            .filter(|(node, _)| self.flow.reachable[*node])
        {
            let mut live = self.flow.live_after(node, facts, seed);
            transfer(
                effects,
                &mut live,
                closures,
                Some(&mut |origin, used| report(node, origin, used)),
            );
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
            self.record(&facts, &seed, &[], &mut |_, origin, used| {
                report(origin, used)
            });
        }
    }
}

fn transfer(
    effects: &[Effect],
    live: &mut Live,
    closures: &[Body],
    mut observe: Option<&mut Observer<'_>>,
) {
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
            Effect::ApplyHelper(call) => apply_helper(call, live, observe.as_deref_mut()),
        }
    }
}

fn apply_helper(call: &HelperCall, live: &mut Live, observe: Option<&mut Observer<'_>>) {
    let observed = call.demand.observed(live);
    let required = if observed {
        &call.when_observed
    } else {
        &call.when_discarded
    };
    for (argument, used) in call.arguments.iter().zip(required) {
        if *used {
            live.extend(*argument);
        }
    }
    if let Some(report) = observe {
        let origins = if observed {
            &call.origins_observed
        } else {
            &call.origins_discarded
        };
        for (origin, used) in origins {
            report(origin, *used);
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
    use autoreview_langsupport::Language;

    #[test]
    fn call_site_sources_flow_through_helpers_without_matching_other_calls() {
        let source = "function helper() { return acquire(); } function run() { helper(); unrelated(); }";
        test_support::with_program((Language::TypeScript, ContractLanguage::Web), source, ("unused", "missing"), |program, units| {
            let spec = program.frames[0].plan.as_ref().unwrap().spec;
            let offset = source.find("acquire()").unwrap();
            let sources = BTreeMap::from([(0, BTreeSet::from([offset]))]);
            let (hits, exhausted, _) = check_units_with_sources(spec, units, &sources);
            assert!(!exhausted);
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].trigger_call, "acquire");
            assert!(check_units_with_sources(spec, units, &BTreeMap::new()).0.is_empty());
        });
    }

    #[test]
    fn call_site_source_is_clean_when_its_value_is_observed() {
        let source = "function run() { const value = acquire(); consume(value); }";
        test_support::with_program((Language::TypeScript, ContractLanguage::Web), source, ("unused", "missing"), |program, units| {
            let spec = program.frames[0].plan.as_ref().unwrap().spec;
            let sources = BTreeMap::from([(0, BTreeSet::from([source.find("acquire()").unwrap()]))]);
            assert!(check_units_with_sources(spec, units, &sources).0.is_empty());
        });
    }

    #[test]
    fn helper_effect_propagates_only_the_arguments_required_by_the_caller() {
        let output = Place::CallResult(50);
        let effects = [Effect::ApplyHelper(HelperCall {
            arguments: vec![Some(Place::Local(1)), Some(Place::Local(2))],
            demand: Demand::Slot(output),
            when_discarded: vec![true, false],
            when_observed: vec![true, true],
            origins_discarded: vec![],
            origins_observed: vec![],
        })];
        for observed in [false, true] {
            let mut live = Live::new();
            if observed {
                live.insert(output);
            }
            transfer(&effects, &mut live, &[], None);
            assert!(live.contains(&Place::Local(1)));
            assert_eq!(live.contains(&Place::Local(2)), observed);
            assert!(!live.contains(&output));
        }
    }

    fn with_helper_analysis(
        languages: (Language, ContractLanguage),
        source: &str,
        api: (&str, &str),
        check: impl FnOnce(&Analysis, &Plan<'_, '_>, tree_sitter::Node<'_>),
    ) {
        test_support::with_program(languages, source, api, |program, units| {
            let function = &units[0].functions[0];
            let plan = program.frames[0].plan.as_ref().unwrap();
            let analysis =
                effects::lower(plan, function, source.as_bytes(), true, &BTreeMap::new());
            check(&analysis, plan, function.node);
        });
    }

    #[test]
    fn helper_summaries_respect_return_demand_in_each_language() {
        let examples = [
            (Language::Rust, ContractLanguage::Rust,
             "use std::fs::remove_file; fn run(p: &str) -> Result<(), Error> { remove_file(p) }",
             "std::fs", "remove_file"),
            (Language::Kotlin, ContractLanguage::Kotlin,
             "import kotlin.runCatching\nfun run(s: String) = runCatching { s.toInt() }",
             "kotlin", "runCatching"),
            (Language::Go, ContractLanguage::Go,
             "package p\nimport \"os\"\nfunc moveFile(a, b string) error { return os.Rename(a, b) }\n",
             "os", "Rename"),
        ];
        for (language, contract_language, source, import, method) in examples {
            with_helper_analysis(
                (language, contract_language),
                source,
                (import, method),
                |analysis, _, _| {
                    for observed in [false, true] {
                        let seed = return_seed(observed);
                        let mut found = Vec::new();
                        analysis.record(&seed, &mut |_, _, used| found.push(used));
                        assert_eq!(found, [observed], "{language:?}");
                    }
                },
            );
        }
    }

    #[test]
    fn helper_parameter_liveness_distinguishes_use_from_forwarding() {
        let source = "package p\nimport \"os\"\nfunc forward(err error) error { return err }\n";
        with_helper_analysis(
            (Language::Go, ContractLanguage::Go),
            source,
            ("os", "Rename"),
            |analysis, plan, function| {
                let body = function.child_by_field_name("body").unwrap();
                let parameter = plan
                    .syntax
                    .local_result_binding("err", body.start_byte(), false)
                    .unwrap();
                for observed in [false, true] {
                    let seed = return_seed(observed);
                    let entry = analysis.record(&seed, &mut |_, _, _| {});
                    assert_eq!(entry.contains(&Place::Local(parameter)), observed);
                }
                let summary = analysis.helper_summary(&[Some(Place::Local(parameter))]);
                assert_eq!(summary.when_discarded.parameters, [false]);
                assert_eq!(summary.when_observed.parameters, [true]);
                assert!(summary.when_discarded.origins.is_empty());
            },
        );
    }
    fn return_seed(observed: bool) -> Live {
        if observed {
            Live::from([Place::Return])
        } else {
            Live::new()
        }
    }
}

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

use std::collections::{BTreeMap, HashSet, VecDeque};
#[cfg(test)]
use std::collections::BTreeSet;

use crate::cfg::{Cfg, EdgeKind, NodeId, Stmt};

use super::{
    keyed::program::Program, plan::Plan, CallOrderHit, CallOrderSpec, ContractLanguage, KeyedUnit,
};
use effects::{Demand, Effect, HelperCall, Origin, Value, Write};
use places::Place;

mod sources;
pub use sources::{discover_factory_sources, FactorySourceDiscovery};

mod checks;
pub(super) use checks::{check_units, check_units_with_sources, check_units_with_components};

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
                .entry((origin.path.clone(), origin.start, origin.component, origin.calls.clone()))
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
                .entry((origin.path.clone(), origin.start, origin.component, origin.calls.clone()))
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
    fn verified_go_tuple_components_track_the_error_independently() {
        let source = "package p\nfunc dropped() { value, _ := acquire(); _ = value }\nfunc used() { _, err := acquire(); if err != nil { return } }\nfunc overwritten() { _, err := acquire(); err = nil; if err != nil { return } }\nfunc both() { _, first, second := two(); if first != nil { return }; _ = second }\n";
        test_support::with_program((Language::Go, ContractLanguage::Go), source, ("unused", "missing"), |program, units| {
            let spec = program.frames[0].plan.as_ref().unwrap().spec;
            let mut offsets = BTreeSet::new();
            let mut shapes = BTreeMap::new();
            for (start, _) in source.match_indices("acquire()") {
                offsets.insert(start);
                shapes.insert(start, super::super::ResultComponents { count: 2, indices: vec![1] });
            }
            let start = source.find("two()").unwrap();
            offsets.insert(start);
            shapes.insert(start, super::super::ResultComponents { count: 3, indices: vec![1, 2] });
            let (hits, exhausted, limits) = check_units_with_components(spec, units, &BTreeMap::from([(0, offsets)]), &BTreeMap::from([(0, shapes)]));
            assert!(!exhausted && limits.is_empty());
            assert_eq!(hits.iter().map(|hit| hit.source_line).collect::<Vec<_>>(), [2, 4, 5]);
            let invalid = FactorySourceDiscovery { candidates: BTreeMap::from([(0, BTreeSet::from([start]))]), limitations: Vec::new() };
            let bad = BTreeMap::from([(0, BTreeMap::from([(start, super::super::ResultComponents { count: 2, indices: vec![2] })]))]);
            let result = super::super::check_keyed_units_with_verified_components(spec, units, invalid, &bad);
            assert!(result.hits.is_empty());
            assert!(result.limitations.iter().any(|item| item.reason == "invalid_result_components"));
        });
    }

    #[test]
    fn verified_sources_in_unreachable_dependency_frames_do_not_block_entries() {
        use crate::call_order::KeyedFunction;
        let texts = ["package p\nfunc root() { unrelated() }\n", "package p\nfunc unrelated() { switch dynamic() { case 1: acquire() } }\n"];
        let trees: Vec<_> = texts.iter().map(|source| autoreview_langsupport::parser_for(Language::Go).unwrap().parse(source, None).unwrap()).collect();
        let nodes: Vec<_> = trees.iter().map(|tree| tree.root_node().named_children(&mut tree.root_node().walk()).find(|node| node.kind() == "function_declaration").unwrap()).collect();
        let graphs: Vec<_> = nodes.iter().zip(texts).map(|(node, source)| crate::lower::keyed_variant(Language::Go, source.as_bytes(), *node)).collect();
        let functions: Vec<_> = nodes.iter().zip(&graphs).map(|(node, graph)| [KeyedFunction { node: *node, graph }]).collect();
        let units = [KeyedUnit { path: "root.go", source: texts[0].as_bytes(), functions: &functions[0], imports: &[], exports: &[], entry: true }, KeyedUnit { path: "dependency.go", source: texts[1].as_bytes(), functions: &functions[1], imports: &[], exports: &[], entry: false }];
        test_support::with_program((Language::Go, ContractLanguage::Go), "package p\nfunc dummy() {}", ("unused", "missing"), |program, _| {
            let spec = program.frames[0].plan.as_ref().unwrap().spec;
            let offsets = BTreeMap::from([(1, BTreeSet::from([texts[1].find("acquire()").unwrap()]))]);
            assert!(check_units_with_sources(spec, &units, &offsets).2.is_empty());
            let imports = [crate::call_order::KeyedImport { local: String::new(), path: "dependency.go".into(), name: None, owner: None, exported: false }];
            let connected = [KeyedUnit { path: "root.go", source: texts[0].as_bytes(), functions: &functions[0], imports: &imports, exports: &[], entry: true }, KeyedUnit { path: "dependency.go", source: texts[1].as_bytes(), functions: &functions[1], imports: &[], exports: &[], entry: false }];
            assert!(check_units_with_sources(spec, &connected, &offsets).2.iter().any(|issue| issue.reason == "verified_source_unmodeled"));
            let entries = [KeyedUnit { entry: true, ..units[1] }];
            let root_offsets = BTreeMap::from([(0, BTreeSet::from([texts[1].find("acquire()").unwrap()]))]);
            assert!(check_units_with_sources(spec, &entries, &root_offsets).2.iter().any(|issue| issue.reason == "verified_source_unmodeled"));
        });
    }

    #[test]
    fn factory_discovery_reports_incomplete_graph_and_inventory() {
        use crate::promise_types::TypeModule;
        let source = "const { value } = source;";
        let tree = autoreview_langsupport::parser_for(Language::TypeScript).unwrap().parse(source, None).unwrap();
        let modules = [TypeModule { path: "main.ts", source: source.as_bytes(), root: tree.root_node() }];
        let units = [KeyedUnit { path: "main.ts", source: source.as_bytes(), functions: &[], imports: &[], exports: &[], entry: true }];
        test_support::with_program((Language::TypeScript, ContractLanguage::Web), "function dummy() {}", ("unused", "missing"), |program, _| {
            let spec = program.frames[0].plan.as_ref().unwrap().spec;
            let incomplete = discover_factory_sources(spec, &modules, &units);
            assert_eq!(incomplete.limitations[0].reason, "factory_binding_inventory_incomplete");
            let mismatch = discover_factory_sources(spec, &modules, &[]);
            assert_eq!(mismatch.limitations[0].reason, "factory_module_graph_incomplete");
            assert!(mismatch.candidates.is_empty());
        });
    }

    #[test]
    fn imported_factory_evidence_can_feed_result_use_without_shadowed_calls() {
        use crate::call_order::{KeyedExport, KeyedFunction, KeyedImport};
        use crate::promise_types::TypeModule;
        let source = "import { upload, unrelated } from './barrel'; function run() { unrelated?.('x'); switch (1) { case 1: break; } upload('outside'); [1].map(() => { upload('x'); upload(); upload(...args); upload?.('x'); }); } function shadow(upload: (x: string) => string) { [1].map(() => { upload('x'); upload?.('x'); upload(...args); }); }";
        let texts = [source, "export * from './binding';", "import create from './factory'; export const upload = create({});", "export default function create(x: object): (ctx: string) => Promise<void> { throw 1; }"];
        let paths = ["caller", "barrel", "binding", "factory"];
        let trees: Vec<_> = texts.iter().map(|source| autoreview_langsupport::parser_for(Language::TypeScript).unwrap().parse(source, None).unwrap()).collect();
        let modules: Vec<_> = texts.iter().zip(&trees).zip(paths).map(|((source, tree), path)| TypeModule { path, source: source.as_bytes(), root: tree.root_node() }).collect();
        let mut nodes: Vec<_> = trees[0].root_node().named_children(&mut trees[0].root_node().walk()).filter(|node| node.kind() == "function_declaration").collect();
        let mut pending = vec![trees[0].root_node()];
        while let Some(node) = pending.pop() {
            if node.kind() == "arrow_function" { nodes.push(node); }
            pending.extend(node.named_children(&mut node.walk()));
        }
        let graphs: Vec<_> = nodes.iter().map(|node| crate::lower::keyed_variant(Language::TypeScript, source.as_bytes(), *node)).collect();
        let functions: Vec<_> = nodes.iter().zip(&graphs).map(|(node, graph)| KeyedFunction { node: *node, graph }).collect();
        let imports = [vec![KeyedImport { local: "upload".into(), path: "barrel".into(), name: Some("upload".into()), owner: None, exported: true }, KeyedImport { local: "unrelated".into(), path: "barrel".into(), name: Some("unrelated".into()), owner: None, exported: true }], vec![], vec![KeyedImport { local: "create".into(), path: "factory".into(), name: Some("default".into()), owner: None, exported: true }], vec![]];
        let exports = [vec![], vec![KeyedExport::Star { path: "binding".into() }], vec![KeyedExport::Named { exported: "upload".into(), local: "upload".into(), path: None }], vec![KeyedExport::Named { exported: "default".into(), local: "create".into(), path: None }]];
        let units: Vec<_> = modules.iter().enumerate().map(|(i, module)| KeyedUnit { path: module.path, source: module.source, functions: if i == 0 { &functions } else { &[] }, imports: &imports[i], exports: &exports[i], entry: i == 0 }).collect();
        test_support::with_program((Language::TypeScript, ContractLanguage::Web), "function dummy() {}", ("unused", "missing"), |dummy, _| {
            let spec = dummy.frames[0].plan.as_ref().unwrap().spec;
            let discovery = discover_factory_sources(spec, &modules, &units);
            assert!(discovery.limitations.iter().any(|issue| issue.reason == "promise_type_unresolved"));
            assert!(discovery.limitations.iter().any(|issue| issue.reason == "factory_flow_unmodeled"));
            assert_eq!(discovery.limitations.iter().filter(|issue| issue.reason == "factory_call_unmodeled").count(), 2);
            let offsets = discovery.candidates.get(&0).unwrap().clone();
            assert_eq!(offsets.len(), 1);
            let (hits, exhausted, _) = check_units_with_sources(spec, &units, &BTreeMap::from([(0, offsets)]));
            assert!(!exhausted);
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].trigger_call, "upload");
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

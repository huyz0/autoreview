//! Local liveness for API results. A read must observe the value that
//! reached it, so later assignments to the same local cannot satisfy it.

mod closures;
mod effects;
mod places;
mod references;
mod syntax;

use std::collections::{BTreeMap, HashSet, VecDeque};

use crate::cfg::{Cfg, EdgeKind, NodeId, Stmt};

use super::{plan::Plan, CallOrderHit, CallOrderSpec, ContractLanguage, KeyedUnit};
use effects::{Demand, Effect, Origin, Value};
use places::Place;

pub(super) fn check_units(spec: &CallOrderSpec, units: &[KeyedUnit<'_, '_, '_>]) -> Vec<CallOrderHit> {
    let mut hits = Vec::new();
    for unit in units.iter().filter(|unit| unit.entry) {
        for function in unit.functions {
            let Some(plan) = Plan::new(spec, function.graph, function.node, unit.source, 0) else { continue };
            if !plan.has_registration() {
                continue;
            }
            let analysis = effects::lower(&plan, function, unit.source);
            hits.extend(analysis.unused().map(|(node, origin)| CallOrderHit {
                trigger_call: origin.call,
                node,
                source_line: origin.line,
                path: (!unit.path.is_empty()).then(|| unit.path.to_owned()),
                entry: None,
            }));
        }
    }
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

impl Analysis {
    fn unused(&self) -> impl Iterator<Item = (NodeId, Origin)> {
        let seed = Live::new();
        let facts = self.owner.solve(&seed, &self.closures);
        let mut origins = BTreeMap::new();
        self.owner.record(&facts, &seed, &self.closures, &mut |node, origin, used| {
            // A finally block or a closure called twice has multiple CFG paths to its site.
            origins
                .entry(origin.start)
                .and_modify(|(_, _, live)| *live |= used)
                .or_insert_with(|| (node, origin.clone(), used));
        });
        origins.into_values().filter_map(|(node, origin, used)| (!used).then_some((node, origin)))
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
                // Go multi-assignment evaluates every RHS before replacing
                // any target, so kill all destinations before adding copies.
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
            Effect::Invoke(index, demand) => {
                closures[*index].invoke(live, *demand, observe.as_mut().map(|report| &mut **report));
            }
        }
    }
}

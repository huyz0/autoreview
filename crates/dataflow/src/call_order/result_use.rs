//! Local liveness for API results. A read must observe the value that
//! reached it, so later assignments to the same local cannot satisfy it.

mod effects;

use std::collections::{BTreeMap, HashSet, VecDeque};

use crate::cfg::{Cfg, EdgeKind, NodeId, Stmt};

use super::{plan::Plan, CallOrderHit, CallOrderSpec, KeyedUnit};
use effects::{Effect, Origin, Value};

pub(super) fn check_units(spec: &CallOrderSpec, units: &[KeyedUnit<'_, '_, '_>]) -> Vec<CallOrderHit> {
    let mut hits = Vec::new();
    for unit in units.iter().filter(|unit| unit.entry) {
        for function in unit.functions {
            let Some(plan) = Plan::new(spec, function.graph, function.node, unit.source, 0) else { continue };
            if !plan.has_registration() {
                continue;
            }
            let flow = Flow::new(function.graph, &plan);
            let effects = effects::lower(&plan, function, unit.source, &flow.reachable);
            hits.extend(flow.unused(&effects).map(|(node, origin)| CallOrderHit {
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

type Live = HashSet<usize>;

struct Flow {
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
            if reachable[from] && possible(from, edge) {
                successors[from].push(to);
                predecessors[to].push(from);
            }
        }
        Self { successors, predecessors, reachable }
    }

    fn live_after(&self, node: NodeId, facts: &[Live]) -> Live {
        self.successors[node].iter().flat_map(|&next| facts[next].iter().copied()).collect()
    }

    fn solve(&self, effects: &[Vec<Effect>]) -> Vec<Live> {
        let mut facts = vec![Live::new(); effects.len()];
        let mut queued = self.reachable.clone();
        let mut work: VecDeque<_> = (0..effects.len()).rev().filter(|&node| queued[node]).collect();
        while let Some(node) = work.pop_front() {
            queued[node] = false;
            let mut live = self.live_after(node, &facts);
            transfer(&effects[node], &mut live, |_, _| {});
            if facts[node] != live {
                facts[node] = live;
                for &before in &self.predecessors[node] {
                    if !std::mem::replace(&mut queued[before], true) {
                        work.push_back(before);
                    }
                }
            }
        }
        facts
    }

    fn unused(&self, effects: &[Vec<Effect>]) -> impl Iterator<Item = (NodeId, Origin)> {
        let facts = self.solve(effects);
        let mut origins = BTreeMap::new();
        for (node, effects) in effects.iter().enumerate().filter(|(node, _)| self.reachable[*node]) {
            transfer(effects, &mut self.live_after(node, &facts), |origin, used| {
                // A finally block can have CFG copies on several exit paths.
                origins
                    .entry(origin.start)
                    .and_modify(|(_, _, live)| *live |= used)
                    .or_insert_with(|| (node, origin.clone(), used));
            });
        }
        origins.into_values().filter_map(|(node, origin, used)| (!used).then_some((node, origin)))
    }
}

fn transfer(effects: &[Effect], live: &mut Live, mut observe: impl FnMut(&Origin, bool)) {
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
                        Value::Source(origin) => observe(origin, used),
                        Value::Copy(binding) if used => copies.push(*binding),
                        _ => {}
                    }
                }
                live.extend(copies);
            }
        }
    }
}

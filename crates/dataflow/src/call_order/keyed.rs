use std::collections::{BTreeMap, HashSet};

use tree_sitter::Node;

use crate::cfg::{Cfg, NodeId, Stmt};
use crate::lattice::Lattice;
use crate::solver;

use super::identity::Entry;
use super::plan::{prepare, Effect, Step};
use super::{CallOrderHit, CallOrderSpec, EntryHit};

#[derive(Clone, PartialEq, Eq)]
struct Pending(BTreeMap<Entry, Status>);

#[derive(Clone, Copy, PartialEq, Eq)]
struct Status {
    // Each field records whether a path with the corresponding state
    // reaches this point. Keep pending and pending-without-defer separate.
    unarmed: bool,
    live: Option<u32>,
    uncovered: Option<u32>,
}

impl Status {
    const ENTRY: Self = Self {
        unarmed: true,
        live: None,
        uncovered: None,
    };

    fn join(self, other: Self) -> Self {
        Self {
            unarmed: self.unarmed || other.unarmed,
            live: self.live.into_iter().chain(other.live).min(),
            uncovered: self.uncovered.into_iter().chain(other.uncovered).min(),
        }
    }
}

impl Lattice for Pending {
    fn bottom() -> Self {
        Self(BTreeMap::new())
    }

    fn join(&self, other: &Self) -> Self {
        let mut joined = self.clone();
        for (entry, &status) in &other.0 {
            joined
                .0
                .entry(entry.clone())
                .and_modify(|current| *current = current.join(status))
                .or_insert(status);
        }
        joined
    }
}

impl Step {
    fn trigger_hit(&self, pending: &Pending, id: NodeId, line: u32) -> Option<CallOrderHit> {
        let (entry, call) = self.trigger.as_ref()?;
        let registration = pending.0.get(entry)?.live?;
        Some(hit(call, id, line, entry, registration))
    }

    fn apply(&self, pending: &mut Pending, line: u32) {
        if self.exit.is_some() {
            pending.0.clear();
        }
        match &self.effect {
            Some(Effect::Remove(entry)) => {
                if let Some(status) = pending.0.get_mut(entry) {
                    status.live = None;
                    status.uncovered = None;
                }
            }
            Some(Effect::Register(entry)) => {
                if let Some(status) = pending.0.get_mut(entry) {
                    status.live = Some(line);
                    status.uncovered = status.unarmed.then_some(line);
                }
            }
            Some(Effect::Defer(entry)) => {
                if let Some(status) = pending.0.get_mut(entry) {
                    status.unarmed = false;
                    status.uncovered = None;
                }
            }
            None => {}
        }
    }
}

/// Runs a keyed rule on the source function and its lowered CFG.
pub fn check_keyed(
    spec: &CallOrderSpec,
    cfg: &Cfg<Stmt>,
    function: Node<'_>,
    source: &[u8],
) -> Vec<CallOrderHit> {
    let Some(steps) = prepare(spec, cfg, function, source) else {
        return Vec::new();
    };
    let initial = Pending(
        steps
            .iter()
            .flatten()
            .filter_map(|step| match &step.effect {
                Some(Effect::Register(entry)) => Some((entry.clone(), Status::ENTRY)),
                _ => None,
            })
            .collect(),
    );
    if initial.0.is_empty() {
        return Vec::new();
    }
    let reachable = Reachable::from_entry(cfg);
    let out = solver::solve(cfg, |id, node, incoming: &Pending| {
        let mut pending = if id == cfg.entry {
            initial.clone()
        } else {
            incoming.clone()
        };
        if reachable.contains(id) {
            for (index, step) in steps[id].iter().enumerate() {
                step.apply(&mut pending, node.statement_line(index));
            }
        }
        pending
    });
    Analysis {
        cfg,
        out: &out,
        initial: &initial,
        check_exits: spec.check_before_return,
    }
    .collect(&steps, &reachable)
}

struct Reachable(HashSet<NodeId>);

impl Reachable {
    fn from_entry(cfg: &Cfg<Stmt>) -> Self {
        let mut seen = HashSet::new();
        let mut work = vec![cfg.entry];
        while let Some(node) = work.pop() {
            if seen.insert(node) {
                work.extend(cfg.successors(node));
            }
        }
        Self(seen)
    }

    fn contains(&self, node: NodeId) -> bool {
        self.0.contains(&node)
    }
}

struct Analysis<'a> {
    cfg: &'a Cfg<Stmt>,
    out: &'a [Pending],
    initial: &'a Pending,
    check_exits: bool,
}

impl Analysis<'_> {
    fn collect(&self, steps: &[Vec<Step>], reachable: &Reachable) -> Vec<CallOrderHit> {
        let mut hits = Vec::new();
        for (id, steps) in steps
            .iter()
            .enumerate()
            .filter(|(id, _)| reachable.contains(*id))
        {
            self.collect_node(id, steps, &mut hits);
        }
        if self.check_exits {
            let exit = self.cfg.exit;
            let line = self.cfg.nodes[exit].source_line;
            hits.extend(exit_hits("fallthrough", exit, line, &self.out[exit]));
        }
        let mut seen = HashSet::new();
        hits.retain(|hit| {
            seen.insert((hit.source_line, hit.trigger_call.clone(), hit.entry.clone()))
        });
        hits
    }

    fn collect_node(&self, id: NodeId, steps: &[Step], hits: &mut Vec<CallOrderHit>) {
        let mut pending = self.incoming(id);
        for (index, step) in steps.iter().enumerate() {
            let line = self.cfg.nodes[id].statement_line(index);
            hits.extend(step.trigger_hit(&pending, id, line));
            if let Some(exit) = step.exit.filter(|_| self.check_exits) {
                hits.extend(exit_hits(exit, id, line, &pending));
            }
            step.apply(&mut pending, line);
        }
    }

    fn incoming(&self, id: NodeId) -> Pending {
        if id == self.cfg.entry {
            return self.initial.clone();
        }
        self.cfg
            .predecessors(id)
            .into_iter()
            .fold(Pending::bottom(), |facts, pred| facts.join(&self.out[pred]))
    }
}

fn exit_hits<'a>(
    call: &'a str,
    id: NodeId,
    line: u32,
    pending: &'a Pending,
) -> impl Iterator<Item = CallOrderHit> + 'a {
    pending
        .0
        .iter()
        .filter_map(move |(entry, status)| Some(hit(call, id, line, entry, status.uncovered?)))
}

fn hit(
    call: &str,
    node: NodeId,
    source_line: u32,
    entry: &Entry,
    registration_line: u32,
) -> CallOrderHit {
    CallOrderHit {
        trigger_call: call.to_owned(),
        node,
        source_line,
        entry: Some(EntryHit {
            collection: entry.collection.clone(),
            key: entry.display_key(),
            registration_line,
        }),
    }
}

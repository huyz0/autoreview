use std::collections::{BTreeMap, HashSet};

use tree_sitter::Node;

use crate::cfg::{CallTarget, Cfg, NodeId, Stmt};
use crate::lattice::Lattice;
use crate::solver;

use super::identity::{Entry, Syntax};
use super::{CallOrderHit, CallOrderSpec, EntryHit, EntryIdentity};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pending<K>(BTreeMap<K, u32>);

impl<K: Clone + Ord> Lattice for Pending<K> {
    fn bottom() -> Self {
        Self(BTreeMap::new())
    }

    fn join(&self, other: &Self) -> Self {
        let mut joined = self.clone();
        for (entry, &line) in &other.0 {
            joined
                .0
                .entry(entry.clone())
                .and_modify(|first| *first = (*first).min(line))
                .or_insert(line);
        }
        joined
    }
}

#[derive(Default)]
struct Step {
    register: Option<Entry>,
    remove: Option<Entry>,
    trigger: Option<(Entry, String)>,
    returns: bool,
}

impl Step {
    fn trigger_hit(&self, pending: &Pending<Entry>, id: NodeId, line: u32) -> Option<CallOrderHit> {
        let (entry, call) = self.trigger.as_ref()?;
        let &registration = pending.0.get(entry)?;
        Some(hit(call, id, line, entry, registration))
    }

    fn apply(&self, pending: &mut Pending<Entry>, line: u32) {
        if self.returns {
            pending.0.clear();
        }
        if let Some(entry) = &self.remove {
            pending.0.remove(entry);
        }
        if let Some(entry) = &self.register {
            pending.0.insert(entry.clone(), line);
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
    let reachable = Reachable::from_entry(cfg);
    let out = solver::solve(cfg, |id, node, pending: &Pending<Entry>| {
        let mut pending = pending.clone();
        if reachable.contains(id) {
            for (index, step) in steps[id].iter().enumerate() {
                step.apply(&mut pending, node.statement_line(index));
            }
        }
        pending
    });
    collect_hits(spec.check_before_return, cfg, &steps, &reachable, &out)
}

fn prepare(
    spec: &CallOrderSpec,
    cfg: &Cfg<Stmt>,
    function: Node<'_>,
    source: &[u8],
) -> Option<Vec<Vec<Step>>> {
    let selector = spec.identity?;
    if !cfg.nodes.iter().flat_map(|node| &node.stmts).any(|stmt| {
        matches!(
            stmt,
            Stmt::Call { target: CallTarget::Named(name), site: Some(_), .. }
                if spec.after.iter().any(|pattern| pattern.matches(name))
        )
    }) {
        return None;
    }
    let syntax = Syntax::scan(function, source)?;
    let mut modeled = HashSet::new();
    let steps = cfg
        .nodes
        .iter()
        .map(|node| {
            node.stmts
                .iter()
                .map(|stmt| step(spec, selector, &syntax, stmt, &mut modeled))
                .collect::<Option<Vec<_>>>()
        })
        .collect::<Option<Vec<_>>>()?;
    syntax
        .models_cleanup(&spec.unless, &modeled)
        .then_some(steps)
}

fn step(
    spec: &CallOrderSpec,
    selector: EntryIdentity,
    syntax: &Syntax<'_>,
    stmt: &Stmt,
    modeled: &mut HashSet<usize>,
) -> Option<Step> {
    let mut step = Step {
        returns: matches!(stmt, Stmt::Return { .. }),
        ..Step::default()
    };
    let Stmt::Call {
        target: CallTarget::Named(name),
        site,
        ..
    } = stmt
    else {
        return Some(step);
    };
    let entry = || syntax.entry(selector, site.as_ref()?);
    if spec.unless.iter().any(|pattern| pattern.matches(name)) {
        step.remove = Some(entry()?);
        modeled.insert(site.as_ref()?.start);
    } else if spec.after.iter().any(|pattern| pattern.matches(name)) {
        step.register = entry();
    }
    if spec.before.iter().any(|pattern| pattern.matches(name)) {
        step.trigger = entry().map(|entry| (entry, name.clone()));
    }
    Some(step)
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

fn collect_hits(
    check_returns: bool,
    cfg: &Cfg<Stmt>,
    steps: &[Vec<Step>],
    reachable: &Reachable,
    out: &[Pending<Entry>],
) -> Vec<CallOrderHit> {
    let mut hits = Vec::new();
    for (id, node) in cfg
        .nodes
        .iter()
        .enumerate()
        .filter(|(id, _)| reachable.contains(*id))
    {
        let mut pending = cfg
            .predecessors(id)
            .iter()
            .fold(Pending::bottom(), |facts, &pred| facts.join(&out[pred]));
        for (index, step) in steps[id].iter().enumerate() {
            let line = node.statement_line(index);
            hits.extend(step.trigger_hit(&pending, id, line));
            if step.returns && check_returns {
                hits.extend(exit_hits("return", id, line, &pending));
            }
            step.apply(&mut pending, line);
        }
    }
    if check_returns {
        let line = cfg.nodes[cfg.exit].source_line;
        hits.extend(exit_hits("fallthrough", cfg.exit, line, &out[cfg.exit]));
    }
    hits
}

fn exit_hits<'a>(
    call: &'a str,
    id: NodeId,
    line: u32,
    pending: &'a Pending<Entry>,
) -> impl Iterator<Item = CallOrderHit> + 'a {
    pending
        .0
        .iter()
        .map(move |(entry, &registration)| hit(call, id, line, entry, registration))
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

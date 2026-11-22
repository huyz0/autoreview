use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use tree_sitter::Node;

use crate::cfg::{CallTarget, Cfg, EdgeKind, IdentityOperand, NodeId, Stmt};

use super::identity::{Entry, Values};
use super::plan::Plan;
use super::{CallOrderHit, CallOrderSpec, EntryHit};

mod state;
use state::{Deferred, State};

const PATH_BUDGET: usize = 16_384;

/// Check each distinct abstract state so a branch cannot borrow a
/// collection, key, or deferred cleanup from another branch.
pub fn check_keyed(
    spec: &CallOrderSpec,
    cfg: &Cfg<Stmt>,
    function: Node<'_>,
    source: &[u8],
) -> Vec<CallOrderHit> {
    let Some(plan) = Plan::new(spec, cfg, function, source) else {
        return Vec::new();
    };
    let mut engine = Engine {
        plan,
        remaining: PATH_BUDGET,
        hits: Vec::new(),
    };
    engine.check(cfg).unwrap_or_default()
}

struct Engine<'a, 'src> {
    plan: Plan<'a, 'src>,
    remaining: usize,
    hits: Vec<CallOrderHit>,
}

struct Exit<'graph> {
    state: State<'graph>,
    trigger: &'static str,
    node: NodeId,
    line: u32,
}

impl Engine<'_, '_> {
    fn check(&mut self, cfg: &Cfg<Stmt>) -> Option<Vec<CallOrderHit>> {
        for exit in self.paths(cfg, State::default())? {
            for state in self.unwind(exit.state)? {
                if self.plan.spec.check_before_return {
                    self.hits
                        .extend(state.pending.iter().map(|(entry, &registration)| {
                            hit(exit.trigger, exit.node, exit.line, entry, registration)
                        }));
                }
            }
        }
        self.hits.sort_by(|left, right| {
            (left.node, left.source_line, &left.trigger_call, &left.entry).cmp(&(
                right.node,
                right.source_line,
                &right.trigger_call,
                &right.entry,
            ))
        });
        let mut seen = HashSet::new();
        self.hits.retain(|hit| {
            seen.insert((hit.source_line, hit.trigger_call.clone(), hit.entry.clone()))
        });
        Some(std::mem::take(&mut self.hits))
    }

    fn paths<'graph>(
        &mut self,
        cfg: &'graph Cfg<Stmt>,
        state: State<'graph>,
    ) -> Option<Vec<Exit<'graph>>> {
        let outgoing = outgoing_edges(cfg);
        let mut work = VecDeque::from([(cfg.entry, state)]);
        let mut seen = BTreeSet::new();
        let mut exits = Vec::new();
        while let Some((id, mut state)) = work.pop_front() {
            if !seen.insert((id, state.clone())) {
                continue;
            }
            self.remaining = self.remaining.checked_sub(1)?;
            if let Some((trigger, line)) = self.visit(cfg, id, &mut state)? {
                exits.push(Exit {
                    state,
                    trigger,
                    node: id,
                    line,
                });
                continue;
            }
            for &(to, edge) in &outgoing[id] {
                if let Some(successor) = state.branch(&self.plan, &cfg.nodes[id], edge) {
                    work.push_back((to, successor));
                }
            }
        }
        Some(exits)
    }

    fn visit<'graph>(
        &mut self,
        cfg: &'graph Cfg<Stmt>,
        id: NodeId,
        state: &mut State<'graph>,
    ) -> Option<Option<(&'static str, u32)>> {
        let node = &cfg.nodes[id];
        for (index, stmt) in node.stmts.iter().enumerate() {
            let line = node.statement_line(index);
            if let Some(trigger) = exit_kind(stmt) {
                return Some(Some((trigger, line)));
            }
            self.apply(stmt, state, id, line)?;
        }
        Some((id == cfg.exit).then_some(("fallthrough", node.source_line)))
    }

    fn apply<'graph>(
        &mut self,
        stmt: &'graph Stmt,
        state: &mut State<'graph>,
        id: NodeId,
        line: u32,
    ) -> Option<()> {
        match stmt {
            Stmt::Rebind(write) => state.rebind(&self.plan, write),
            Stmt::DeferredBlock(body) => state.deferred.push(Deferred::Block(body)),
            Stmt::DeferredCall { target, site } => {
                let call = self.plan.call(target, site.as_deref(), &state.values)?;
                if call.registered || call.trigger {
                    return None;
                }
                if let Some(entry) = call.entry.filter(|_| call.removed) {
                    state.deferred.push(Deferred::Remove(entry));
                }
            }
            Stmt::Call {
                target: CallTarget::Named(name),
                site,
                ..
            } => {
                let call = self.plan.call(name, site.as_deref(), &state.values)?;
                if let Some(entry) = call.entry {
                    if let Some(&registration) = state.pending.get(&entry).filter(|_| call.trigger)
                    {
                        self.hits.push(hit(name, id, line, &entry, registration));
                    }
                    state.update(entry, call.registered, call.removed, line);
                }
            }
            _ => {}
        }
        Some(())
    }

    fn unwind<'graph>(&mut self, mut state: State<'graph>) -> Option<Vec<State<'graph>>> {
        let deferred = std::mem::take(&mut state.deferred);
        let mut outcomes = vec![state];
        for action in deferred.into_iter().rev() {
            let mut next = BTreeSet::new();
            for state in outcomes {
                next.extend(self.deferred(action.clone(), state)?);
            }
            outcomes = next.into_iter().collect();
        }
        Some(outcomes)
    }

    fn deferred<'graph>(
        &mut self,
        action: Deferred<'graph>,
        mut state: State<'graph>,
    ) -> Option<Vec<State<'graph>>> {
        match action {
            Deferred::Remove(entry) => {
                state.pending.remove(&entry);
                Some(vec![state])
            }
            Deferred::Block(cfg) => {
                let mut outcomes = Vec::new();
                for exit in self.paths(cfg, state)? {
                    outcomes.extend(self.unwind(exit.state)?);
                }
                Some(outcomes)
            }
        }
    }
}

fn outgoing_edges(cfg: &Cfg<Stmt>) -> Vec<Vec<(NodeId, EdgeKind)>> {
    let mut outgoing = vec![Vec::new(); cfg.nodes.len()];
    for &(from, to, kind) in &cfg.edges {
        outgoing[from].push((to, kind));
    }
    outgoing
}

fn exit_kind(stmt: &Stmt) -> Option<&'static str> {
    match stmt {
        Stmt::Return { .. } => Some("return"),
        Stmt::Throw { implicit: true, .. } => Some("implicit throw"),
        Stmt::Throw { .. } => Some("throw"),
        _ => None,
    }
}

fn hit(call: &str, node: NodeId, line: u32, entry: &Entry, registration_line: u32) -> CallOrderHit {
    CallOrderHit {
        trigger_call: call.to_owned(),
        node,
        source_line: line,
        entry: Some(EntryHit {
            collection: entry.display_collection(),
            key: entry.display_key(),
            registration_line,
        }),
    }
}

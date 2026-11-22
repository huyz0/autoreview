use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use tree_sitter::Node;

use crate::cfg::{CallSite, CallTarget, Cfg, EdgeKind, IdentityOperand, NodeId, Stmt};

use super::identity::{Entry, Values};
use super::plan::Plan;
use super::{CallOrderHit, CallOrderSpec, EntryHit};

mod helpers;
mod program;
mod state;
pub use program::KeyedFunction;
use program::Program;
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
    check_keyed_file(
        spec,
        &[KeyedFunction {
            node: function,
            graph: cfg,
        }],
        source,
    )
}

/// Analyze helper bodies in the context of each same-file caller.
pub fn check_keyed_file(
    spec: &CallOrderSpec,
    functions: &[KeyedFunction<'_, '_>],
    source: &[u8],
) -> Vec<CallOrderHit> {
    let program = Program::new(spec, functions, source);
    if !program
        .frames
        .iter()
        .filter_map(|frame| frame.plan.as_ref())
        .any(|plan| plan.syntax.has_registration(&spec.after))
    {
        return Vec::new();
    }
    program
        .roots()
        .into_iter()
        .filter_map(|root| {
            let mut engine = Engine {
                program: &program,
                active: vec![root],
                remaining: PATH_BUDGET,
                hits: Vec::new(),
            };
            engine.check(root)
        })
        .flatten()
        .collect()
}

struct Engine<'program, 'spec, 'src, 'graph> {
    program: &'program Program<'spec, 'src, 'graph>,
    active: Vec<usize>,
    remaining: usize,
    hits: Vec<CallOrderHit>,
}

struct StatementLocation {
    node: NodeId,
    line: u32,
}

struct Exit<'graph> {
    state: State<'graph>,
    trigger: &'static str,
    node: NodeId,
    line: u32,
}

impl<'graph> Engine<'_, '_, '_, 'graph> {
    fn check(&mut self, frame: usize) -> Option<Vec<CallOrderHit>> {
        let root = &self.program.frames[frame];
        let check_exits = root.plan.as_ref()?.spec.check_before_return;
        for exit in self.paths(frame, root.graph, State::default())? {
            for state in self.unwind(frame, exit.state)? {
                if check_exits {
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

    fn paths(
        &mut self,
        frame: usize,
        cfg: &'graph Cfg<Stmt>,
        state: State<'graph>,
    ) -> Option<Vec<Exit<'graph>>> {
        let plan = self.program.frames[frame].plan.as_ref()?;
        let outgoing = outgoing_edges(cfg);
        let mut work = VecDeque::from([(cfg.entry, state)]);
        let mut seen = BTreeSet::new();
        let mut exits = Vec::new();
        while let Some((id, state)) = work.pop_front() {
            if !seen.insert((id, state.clone())) {
                continue;
            }
            self.remaining = self.remaining.checked_sub(1)?;
            let states = self.visit(frame, cfg, id, state)?;
            if let Some((trigger, line)) = exit_at(cfg, id) {
                exits.extend(states.into_iter().map(|state| Exit {
                    state,
                    trigger,
                    node: id,
                    line,
                }));
                continue;
            }
            for state in states {
                match self.fork_helper(frame, &cfg.nodes[id], &state)? {
                    Some(outcomes) => {
                        for (threw, state) in outcomes {
                            work.extend(
                                outgoing[id]
                                    .iter()
                                    .filter(|(_, edge)| (*edge == EdgeKind::Exceptional) == threw)
                                    .map(|(to, _)| (*to, state.clone())),
                            );
                        }
                    }
                    None => work.extend(outgoing[id].iter().filter_map(|(to, edge)| {
                        state
                            .branch(plan, &cfg.nodes[id], *edge)
                            .map(|successor| (*to, successor))
                    })),
                }
            }
        }
        Some(exits)
    }

    fn visit(
        &mut self,
        frame: usize,
        cfg: &'graph Cfg<Stmt>,
        id: NodeId,
        state: State<'graph>,
    ) -> Option<Vec<State<'graph>>> {
        let node = &cfg.nodes[id];
        let mut states = vec![state];
        for (index, stmt) in node.stmts.iter().enumerate() {
            let mut next = Vec::new();
            for state in states {
                next.extend(self.apply(frame, stmt, state, id, node.statement_line(index))?);
            }
            states = next;
        }
        Some(states)
    }

    fn apply(
        &mut self,
        frame: usize,
        stmt: &'graph Stmt,
        mut state: State<'graph>,
        id: NodeId,
        line: u32,
    ) -> Option<Vec<State<'graph>>> {
        let plan = self.program.frames[frame].plan.as_ref()?;
        match stmt {
            Stmt::Rebind(write) => state.rebind(plan, write),
            Stmt::DeferredBlock(body) => state.schedule(Deferred::Block(body))?,
            Stmt::DeferredCall { target, site } => {
                self.schedule_helper(frame, target, site.as_deref(), &mut state)?
            }
            Stmt::Call {
                target: CallTarget::Named(name),
                site,
                ..
            } => {
                if let Some(outcomes) = self.apply_call(
                    frame,
                    name,
                    site.as_deref(),
                    &mut state,
                    StatementLocation { node: id, line },
                )? {
                    return Some(outcomes);
                }
            }
            _ => {}
        }
        Some(vec![state])
    }

    fn unwind(&mut self, frame: usize, mut state: State<'graph>) -> Option<Vec<State<'graph>>> {
        let deferred = std::mem::take(&mut state.deferred);
        let mut outcomes = vec![state];
        for action in deferred.into_iter().rev() {
            let mut next = BTreeSet::new();
            for state in outcomes {
                next.extend(self.deferred(frame, action.clone(), state)?);
            }
            outcomes = next.into_iter().collect();
        }
        Some(outcomes)
    }

    fn deferred(
        &mut self,
        frame: usize,
        action: Deferred<'graph>,
        mut state: State<'graph>,
    ) -> Option<Vec<State<'graph>>> {
        match action {
            Deferred::Remove(entry) => {
                state.remove(&entry);
                Some(vec![state])
            }
            Deferred::Helper(target, args) => {
                let outcomes = self.invoke(target, args, state)?;
                outcomes
                    .into_iter()
                    .map(|(threw, state)| (!threw).then_some(state))
                    .collect()
            }
            Deferred::Block(cfg) => {
                let mut outcomes = Vec::new();
                for exit in self.paths(frame, cfg, state)? {
                    outcomes.extend(self.unwind(frame, exit.state)?);
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

fn exit_at(cfg: &Cfg<Stmt>, id: NodeId) -> Option<(&'static str, u32)> {
    let node = &cfg.nodes[id];
    node.stmts
        .iter()
        .enumerate()
        .find_map(|(index, stmt)| exit_kind(stmt).map(|kind| (kind, node.statement_line(index))))
        .or_else(|| (id == cfg.exit).then_some(("fallthrough", node.source_line)))
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

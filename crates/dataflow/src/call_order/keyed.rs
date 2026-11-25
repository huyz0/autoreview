use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use tree_sitter::Node;

use crate::cfg::{CallSite, CallTarget, Cfg, EdgeKind, IdentityOperand, NodeId, Stmt};

use super::identity::{Entry, Values};
use super::plan::Plan;
use super::{CallOrderHit, CallOrderSpec, EntryHit};

mod contracts;
mod coverage;
pub use coverage::{check_keyed_units, check_keyed_units_with_coverage, keyed_flow_exhausted, KeyedCheckResult, KeyedLimitation};
mod helpers;
pub(super) mod program;
mod state;
use program::Program;
pub use program::{resolve_declaration_import, DeclarationResolution, KeyedDeclaration, KeyedExport, KeyedFunction, KeyedImport, KeyedUnit};
use state::{Deferred, Registration, State};

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
    check_keyed_units(
        spec,
        &[KeyedUnit {
            path: "",
            source,
            functions,
            imports: &[],
            exports: &[],
            entry: true,
        }],
    )
}

fn check_with_budget(spec: &CallOrderSpec, units: &[KeyedUnit<'_, '_, '_>], budget: usize) -> KeyedCheckResult {
    if spec.tracks_result_use() {
        let (hits, exhausted, mut limitations) = super::result_use::check_units(spec, units);
        limitations.extend(exhausted.then_some(KeyedLimitation {
            reason: "helper_summary_limit", path: None, line: None,
        }));
        return KeyedCheckResult { hits, limitations };
    }
    let program = Program::new(spec, units);
    if !program
        .frames
        .iter()
        .filter_map(|frame| frame.plan.as_ref())
        .any(Plan::has_registration)
    {
        return KeyedCheckResult { hits: Vec::new(), limitations: Vec::new() };
    }
    let mut hits = Vec::new();
    let mut limitations = Vec::new();
    for root in program.roots() {
        if program.frames[root].plan.is_none() {
            continue;
        }
        let mut engine = Engine { program: &program, active: vec![root], remaining: budget, hits: Vec::new() };
        match engine.check(root) {
            Some(found) => hits.extend(found),
            None => {
                let frame = &program.frames[root];
                limitations.push(KeyedLimitation {
                    reason: if engine.remaining == 0 { "path_budget" } else { "unmodeled_path" },
                    path: program.path(frame.unit),
                    line: frame.graph.nodes.get(frame.graph.entry).map(|node| node.source_line),
                });
            }
        }
    }
    limitations.extend(program.resolution_limitations());
    KeyedCheckResult { hits, limitations }
}

struct Engine<'program, 'spec, 'src, 'graph> {
    program: &'program Program<'spec, 'src, 'graph>,
    active: Vec<usize>,
    remaining: usize,
    hits: Vec<CallOrderHit>,
}

#[derive(Clone, Copy)]
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
        let spec = root.plan.as_ref()?.spec;
        let mut reported_handles = HashSet::new();
        for exit in self.paths(frame, root.graph, State::default())? {
            let location = StatementLocation { node: exit.node, line: exit.line };
            for mut state in self.unwind(frame, exit.state)? {
                if spec.contract.is_some() {
                    state.transfer_returned_handle();
                }
                if spec.check_before_return {
                    self.record_pending(frame, exit.trigger, location, &state, &mut reported_handles);
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
            seen.insert((
                hit.path.clone(),
                hit.source_line,
                hit.trigger_call.clone(),
                hit.entry.clone(),
            ))
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
            Stmt::ReturnIdentity(value) => {
                state.returned = value
                    .as_ref()
                    .and_then(|value| state.materialize(plan, value, usize::MAX));
            }
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
                    .map(|(threw, _, state)| (!threw).then_some(state))
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

impl Engine<'_, '_, '_, '_> {
    fn record_pending(
        &mut self, frame: usize, trigger: &str, location: StatementLocation,
        state: &State<'_>, handles: &mut HashSet<(usize, usize)>,
    ) {
        for (entry, &registration) in &state.pending {
            if entry.key.is_none() {
                if let IdentityOperand::Fresh { unit, start, .. } = &entry.collection {
                    if !handles.insert((*unit, *start)) {
                        continue;
                    }
                }
            }
            self.record_hit(trigger, frame, location, entry, registration);
        }
    }

    fn registration(&self, frame: usize, line: u32) -> Registration {
        Registration {
            unit: self.program.frames[frame].unit,
            line,
        }
    }

    fn record_hit(
        &mut self,
        call: &str,
        frame: usize,
        location: StatementLocation,
        entry: &Entry,
        registration: Registration,
    ) {
        self.hits.push(CallOrderHit {
            trigger_call: call.to_owned(),
            node: location.node,
            source_line: location.line,
            path: self.program.path(self.program.frames[frame].unit),
            related_calls: Vec::new(),
            entry: Some(EntryHit {
                collection: entry.display_collection(),
                key: entry.display_key(),
                registration_line: registration.line,
                registration_path: self.program.path(registration.unit),
            }),
        });
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::call_order::{CollectionSelector, EntryIdentity};
    use crate::taint::NamePattern;
    use autoreview_langsupport::Language;

    #[test]
    fn reports_path_budget_exhaustion_for_the_affected_root() {
        let source = "function run(map, key) { map.set(key, 1); }";
        let mut parser = autoreview_langsupport::parser_for(Language::JavaScript).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let function = tree.root_node().named_child(0).unwrap();
        let graph = crate::lower::keyed_variant(Language::JavaScript, source.as_bytes(), function);
        let functions = [KeyedFunction { node: function, graph: &graph }];
        let units = [KeyedUnit { path: "main.js", source: source.as_bytes(), functions: &functions, imports: &[], exports: &[], entry: true }];
        let spec = CallOrderSpec {
            rule_id: "test".into(), after: vec![NamePattern::suffix("set")],
            unless: vec![NamePattern::suffix("delete")], before: vec![], check_before_return: true,
            identity: Some(EntryIdentity { collection: CollectionSelector::Receiver, key_argument: 0 }), contract: None,
        };
        let result = check_with_budget(&spec, &units, 0);
        assert!(result.hits.is_empty());
        assert_eq!(result.limitations.len(), 1);
        assert_eq!(result.limitations[0].reason, "path_budget");
        assert_eq!(result.limitations[0].path.as_deref(), Some("main.js"));
        assert!(check_with_budget(&spec, &units, PATH_BUDGET).limitations.is_empty());
    }
}

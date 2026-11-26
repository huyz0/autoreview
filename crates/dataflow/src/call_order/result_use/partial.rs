//! Find locally tracked results that are observed on some, but not all, paths.
//! Unsupported effects and cyclic or ambiguous control flow retain the existing
//! analysis rather than guessing about helper or guard behavior.
use std::collections::{BTreeMap, BTreeSet, VecDeque};

mod guards;
mod helper_coverage;
mod parameters;

pub(super) use helper_coverage::has_partial_helper;
pub(super) use parameters::unobserved_parameters;

use super::{Body, Demand, Effect, Origin, Place, Plan, Value, Write};
use crate::cfg::{Cfg, EdgeKind, IdentityOperand, NodeId, Stmt};
use guards::{guard, hidden_at, supported_edges, throws, Visits};
use tree_sitter::Node;

const PATH_LIMIT: usize = 8192;

#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
struct State {
    active: bool,
    aliases: BTreeSet<Place>,
    guards: BTreeMap<IdentityOperand, bool>,
}

#[derive(PartialEq, Eq)]
pub(super) enum Status {
    Complete,
    Unsupported,
    Limited,
}

pub(super) fn unused(
    body: &Body,
    cfg: &Cfg<Stmt>,
    plan: &Plan<'_, '_>,
    syntax: (Node<'_>, &[u8]),
    has_closures: bool,
) -> (Vec<(NodeId, Origin)>, Status) {
    let edges = outgoing(body, cfg);
    let hidden = cfg
        .nodes
        .iter()
        .enumerate()
        .any(|(index, _)| body.flow.reachable[index] && hidden_at(cfg, index, syntax));
    if !hidden
        && !edges
            .iter()
            .flatten()
            .any(|(_, edge)| matches!(edge, EdgeKind::True | EdgeKind::False))
    {
        return (Vec::new(), Status::Complete);
    }
    let origins = local_origins(body);
    if origins.is_empty() {
        return (Vec::new(), Status::Complete);
    }
    if origins.len() > 64 {
        return (Vec::new(), Status::Limited);
    }
    if hidden || has_closures || !supported(body, cfg, plan, &edges) {
        return (Vec::new(), Status::Unsupported);
    }
    match find_unobserved(body, cfg, plan, &edges, origins) {
        Some(hits) => (hits, Status::Complete),
        None => (Vec::new(), Status::Limited),
    }
}

fn local_origins(body: &Body) -> Vec<(NodeId, Origin)> {
    source_origins(body, |place| matches!(place, Place::Local(_)))
}

fn source_origins(body: &Body, include: fn(Place) -> bool) -> Vec<(NodeId, Origin)> {
    body.effects
        .iter()
        .enumerate()
        .flat_map(|(node, effects)| {
            effects
                .iter()
                .filter_map(move |effect| match effect {
                    Effect::Write(writes) => Some(writes.iter().filter_map(move |write| {
                        match (write.target, &write.value) {
                            (Some(place), Value::Source(origin)) if include(place) => {
                                Some((node, origin.clone()))
                            }
                            _ => None,
                        }
                    })),
                    _ => None,
                })
                .flatten()
        })
        .collect()
}

fn find_unobserved(
    body: &Body,
    cfg: &Cfg<Stmt>,
    plan: &Plan<'_, '_>,
    edges: &[Vec<(NodeId, EdgeKind)>],
    origins: Vec<(NodeId, Origin)>,
) -> Option<Vec<(NodeId, Origin)>> {
    let mut hits = Vec::new();
    for (node, origin) in origins {
        if unobserved_path(body, cfg, plan, edges, &origin)? {
            hits.push((node, origin));
        }
    }
    Some(hits)
}

pub(super) fn has_observed_origin(
    body: &Body,
    cfg: &Cfg<Stmt>,
    syntax: (Node<'_>, &[u8]),
    unused: &[(NodeId, Origin)],
) -> bool {
    body.effects.iter().enumerate().any(|(node, effects)| {
        effects.iter().any(|effect| match effect {
            Effect::Write(writes) => {
                writes
                    .iter()
                    .any(|write| match (&write.value, write.target) {
                        (Value::Source(origin), Some(Place::Local(_))) => {
                            !unused.iter().any(|(_, other)| other == origin)
                                && !observed_before_branch(body, cfg, syntax, node, origin)
                        }
                        _ => false,
                    })
            }
            _ => false,
        })
    })
}

fn observed_before_branch(
    body: &Body,
    cfg: &Cfg<Stmt>,
    syntax: (Node<'_>, &[u8]),
    mut node: NodeId,
    origin: &Origin,
) -> bool {
    let mut state = State::default();
    let mut seen = BTreeSet::new();
    while seen.insert(node) {
        if cfg.nodes[node].evaluations.iter().any(|range| {
            super::effects::evaluation(syntax.0, range)
                .is_some_and(|value| has_conditional(value, syntax.1))
        }) {
            return false;
        }
        if let Some(observed) = observe_straight_line(&body.effects[node], origin, &mut state) {
            return observed;
        }
        let [next] = body.flow.successors[node].as_slice() else {
            return false;
        };
        node = *next;
    }
    false
}

fn observe_straight_line(effects: &[Effect], origin: &Origin, state: &mut State) -> Option<bool> {
    for effect in effects {
        if state.active && !supported_effect(effect) {
            return Some(false);
        }
        if observed(std::slice::from_ref(effect), origin, state) {
            return Some(true);
        }
        if state.active && state.aliases.is_empty() {
            return Some(false);
        }
    }
    None
}

fn has_conditional(node: Node<'_>, source: &[u8]) -> bool {
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if matches!(
            node.kind(),
            "ternary_expression"
                | "conditional_expression"
                | "if_expression"
                | "match_expression"
                | "when_expression"
                | "optional_chain"
                | "logical_expression"
                | "conjunction_expression"
                | "disjunction_expression"
                | "elvis_expression"
                | "null_coalescing_expression"
        ) {
            return true;
        }
        if node.kind() == "binary_expression"
            && node
                .child_by_field_name("operator")
                .and_then(|operator| operator.utf8_text(source).ok())
                .is_some_and(|operator| matches!(operator, "&&" | "||" | "??"))
        {
            return true;
        }
        if node.kind() == "binary_expression"
            && node
                .children(&mut node.walk())
                .any(|child| matches!(child.kind(), "&&" | "||" | "??" | "?:"))
        {
            return true;
        }
        pending.extend(node.named_children(&mut node.walk()));
    }
    false
}

fn outgoing(body: &Body, cfg: &Cfg<Stmt>) -> Vec<Vec<(NodeId, EdgeKind)>> {
    let mut edges = vec![Vec::new(); cfg.nodes.len()];
    for &(from, to, edge) in &cfg.edges {
        if body.flow.successors[from].contains(&to) {
            edges[from].push((to, edge));
        }
    }
    edges
}

fn supported(
    body: &Body,
    cfg: &Cfg<Stmt>,
    plan: &Plan<'_, '_>,
    edges: &[Vec<(NodeId, EdgeKind)>],
) -> bool {
    if cyclic(body) {
        return false;
    }
    let branches = edges
        .iter()
        .filter(|outgoing| {
            outgoing
                .iter()
                .any(|(_, edge)| matches!(edge, EdgeKind::True | EdgeKind::False))
        })
        .count();
    edges
        .iter()
        .enumerate()
        .all(|(node, outgoing)| supported_edges(cfg, plan, node, outgoing, branches))
        && body.effects.iter().flatten().all(supported_effect)
}

fn supported_effect(effect: &Effect) -> bool {
    supported_effect_with_return(effect, false)
}

fn supported_effect_with_return(effect: &Effect, allow_return: bool) -> bool {
    match effect {
        Effect::Read(Place::Local(_)) => true,
        Effect::Write(writes) => writes.iter().all(|write| {
            write.target.is_none_or(|place| {
                matches!(place, Place::Local(_)) || (allow_return && place == Place::Return)
            }) && matches!(
                write.value,
                Value::Source(_) | Value::Copy(Place::Local(_)) | Value::Other
            )
        }),
        _ => false,
    }
}

fn cyclic(body: &Body) -> bool {
    let mut indegree: Vec<_> = body.flow.predecessors.iter().map(Vec::len).collect();
    let mut queue: VecDeque<_> = body
        .flow
        .reachable
        .iter()
        .enumerate()
        .filter_map(|(node, reached)| (*reached && indegree[node] == 0).then_some(node))
        .collect();
    let mut seen = 0;
    while let Some(node) = queue.pop_front() {
        seen += 1;
        for &next in &body.flow.successors[node] {
            indegree[next] -= 1;
            if indegree[next] == 0 {
                queue.push_back(next);
            }
        }
    }
    seen != body
        .flow
        .reachable
        .iter()
        .filter(|reached| **reached)
        .count()
}

fn unobserved_path(
    body: &Body,
    cfg: &Cfg<Stmt>,
    plan: &Plan<'_, '_>,
    edges: &[Vec<(NodeId, EdgeKind)>],
    origin: &Origin,
) -> Option<bool> {
    let mut seen = Visits::default();
    let mut pending = vec![(body.flow.entry, State::default())];
    while let Some((node, mut state)) = pending.pop() {
        if !seen.enter(cfg, node, &state)? {
            continue;
        }
        if observed(&body.effects[node], origin, &mut state) {
            continue;
        }
        if state.active && (state.aliases.is_empty() || node == body.flow.exit) {
            return Some(true);
        }
        pending.extend(edges[node].iter().filter_map(|&(to, edge)| {
            branch(state.clone(), cfg, plan, node, edge).map(|next| (to, next))
        }));
    }
    Some(false)
}

fn observed(effects: &[Effect], origin: &Origin, state: &mut State) -> bool {
    for effect in effects {
        match effect {
            Effect::Read(place) if state.active && state.aliases.contains(place) => return true,
            Effect::Write(writes) => update_aliases(writes, origin, state),
            _ => {}
        }
    }
    false
}

fn update_aliases(writes: &[Write], origin: &Origin, state: &mut State) {
    let old = state.aliases.clone();
    for target in writes.iter().filter_map(|write| write.target) {
        state.aliases.remove(&target);
    }
    for write in writes {
        match (&write.value, write.target) {
            (Value::Source(value), target) if value == origin => {
                state.active = true;
                if let Some(target) = target {
                    state.aliases.insert(target);
                }
            }
            (Value::Copy(value), Some(target)) if old.contains(value) => {
                state.aliases.insert(target);
            }
            _ => {}
        }
    }
}

fn branch(
    mut state: State,
    cfg: &Cfg<Stmt>,
    plan: &Plan<'_, '_>,
    node: NodeId,
    edge: EdgeKind,
) -> Option<State> {
    if !matches!(edge, EdgeKind::True | EdgeKind::False) {
        return Some(state);
    }
    let (name, polarity) = cfg.nodes[node].condition.as_ref()?;
    let truth = (edge == EdgeKind::True) == *polarity;
    let Some(identity) = guard(plan, name) else {
        return Some(state);
    };
    if let IdentityOperand::Boolean(value) = identity {
        return (value == truth).then_some(state);
    }
    if state
        .guards
        .get(&identity)
        .is_some_and(|previous| *previous != truth)
    {
        return None;
    }
    state.guards.insert(identity, truth);
    Some(state)
}

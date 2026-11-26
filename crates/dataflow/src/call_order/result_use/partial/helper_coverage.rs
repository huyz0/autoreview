use std::collections::BTreeSet;

use super::{
    branch, cyclic, hidden_at, observed, outgoing, source_origins, supported_edges, throws, Body,
    Cfg, Demand, EdgeKind, Effect, Node, Origin, Place, Plan, State, Stmt, Value, PATH_LIMIT,
};
use crate::cfg::NodeId;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Evidence {
    None,
    Unknown,
    Definite,
}

pub(in crate::call_order::result_use) struct HelperPaths {
    pub hits: Vec<(NodeId, Origin)>,
    pub observed: Vec<(NodeId, Origin)>,
    pub incomplete: bool,
}

pub(in crate::call_order::result_use) fn helper_paths(
    body: &Body,
    cfg: &Cfg<Stmt>,
    plan: &Plan<'_, '_>,
    syntax: (Node<'_>, &[u8]),
    options: (&[(NodeId, Origin)], bool),
) -> HelperPaths {
    let (unused, has_closures) = options;
    let edges = outgoing(body, cfg);
    let precise = !has_closures && precisely_modeled((body, cfg, plan, syntax), &edges);
    let mut hits = Vec::new();
    let mut observed = Vec::new();
    let mut incomplete = false;
    for (node, origin) in source_origins(body, |place| {
        matches!(
            place,
            Place::Local(_) | Place::CallResult(_) | Place::CallComponent(_, _)
        )
    }) {
        if unused.iter().any(|(_, other)| *other == origin) {
            continue;
        }
        match uncertain_path((body, cfg, plan, syntax), &edges, &origin) {
            (Evidence::Definite, _) if precise => hits.push((node, origin)),
            (Evidence::Definite | Evidence::Unknown, _) => incomplete = true,
            (Evidence::None, true) if precise => observed.push((node, origin)),
            (Evidence::None, _) => {}
        }
    }
    HelperPaths {
        hits,
        observed,
        incomplete,
    }
}

fn precisely_modeled(
    context: (&Body, &Cfg<Stmt>, &Plan<'_, '_>, (Node<'_>, &[u8])),
    edges: &[Vec<(NodeId, EdgeKind)>],
) -> bool {
    let (body, cfg, plan, syntax) = context;
    !cyclic(body)
        && edges
            .iter()
            .enumerate()
            .all(|(node, out)| supported_edges(cfg, plan, node, out, 2))
        && body.effects.iter().flatten().all(precise_effect)
        && !cfg
            .nodes
            .iter()
            .enumerate()
            .any(|(node, _)| body.flow.reachable[node] && hidden_at(cfg, node, syntax))
}

fn precise_effect(effect: &Effect) -> bool {
    match effect {
        Effect::Read(place) => matches!(
            place,
            Place::Local(_) | Place::CallResult(_) | Place::CallComponent(_, _)
        ),
        Effect::Write(writes) => writes.iter().all(|write| {
            write.target.is_none_or(|place| {
                matches!(
                    place,
                    Place::Local(_)
                        | Place::CallResult(_)
                        | Place::CallComponent(_, _)
                        | Place::Return
                )
            }) && matches!(
                write.value,
                Value::Source(_)
                    | Value::Copy(
                        Place::Local(_) | Place::CallResult(_) | Place::CallComponent(_, _)
                    )
                    | Value::Other
            )
        }),
        Effect::ApplyHelper(_) => true,
        _ => false,
    }
}

fn uncertain_path(
    context: (&Body, &Cfg<Stmt>, &Plan<'_, '_>, (Node<'_>, &[u8])),
    edges: &[Vec<(NodeId, EdgeKind)>],
    origin: &Origin,
) -> (Evidence, bool) {
    let (body, cfg, plan, syntax) = context;
    let mut pending = vec![(body.flow.entry, State::default(), Evidence::None)];
    let mut visited = BTreeSet::new();
    let mut unsettled = false;
    while let Some((node, mut state, mut evidence)) = pending.pop() {
        match visit(&mut visited, cfg, node, &state, evidence) {
            None => return (Evidence::Unknown, false),
            Some(false) => continue,
            Some(true) => {}
        }
        let conditional = hidden_at(cfg, node, syntax);
        if process(
            &body.effects[node],
            origin,
            &mut state,
            &mut evidence,
            conditional,
        ) {
            continue;
        }
        if state.active && (state.aliases.is_empty() || node == body.flow.exit) {
            if evidence != Evidence::None {
                return (evidence, false);
            }
            unsettled = true;
            continue;
        }
        extend_paths(
            &mut pending,
            (cfg, plan),
            node,
            &edges[node],
            (state, evidence),
        );
    }
    (Evidence::None, !unsettled)
}

fn visit(
    visited: &mut BTreeSet<(NodeId, State, Evidence)>,
    cfg: &Cfg<Stmt>,
    node: NodeId,
    state: &State,
    evidence: Evidence,
) -> Option<bool> {
    if !visited.insert((node, state.clone(), evidence)) {
        return Some(false);
    }
    if visited.len() > PATH_LIMIT {
        return None;
    }
    Some(!throws(cfg, node))
}

fn extend_paths(
    pending: &mut Vec<(NodeId, State, Evidence)>,
    context: (&Cfg<Stmt>, &Plan<'_, '_>),
    node: NodeId,
    edges: &[(NodeId, EdgeKind)],
    current: (State, Evidence),
) {
    let (cfg, plan) = context;
    let (state, evidence) = current;
    let stable = supported_edges(cfg, plan, node, edges, 2);
    pending.extend(edges.iter().filter_map(|&(to, edge)| {
        if stable {
            branch(state.clone(), cfg, plan, node, edge)
        } else {
            Some(state.clone())
        }
        .map(|next| (to, next, evidence))
    }));
}

fn process(
    effects: &[Effect],
    origin: &Origin,
    state: &mut State,
    evidence: &mut Evidence,
    conditional: bool,
) -> bool {
    for effect in effects {
        if !(conditional && matches!(effect, Effect::Read(_)))
            && observed(std::slice::from_ref(effect), origin, state)
        {
            return true;
        }
        if let Effect::ApplyHelper(call) = effect {
            if helper_observes(call, state, evidence, conditional) {
                return true;
            }
        }
    }
    false
}

fn helper_observes(
    call: &super::super::HelperCall,
    state: &mut State,
    evidence: &mut Evidence,
    conditional: bool,
) -> bool {
    for (index, argument) in call.arguments.iter().enumerate() {
        if !argument.is_some_and(|place| state.aliases.contains(&place)) {
            continue;
        }
        if let Demand::Slot(slot) = call.demand {
            if slot_observes(call, state, evidence, conditional, (index, slot)) {
                return true;
            }
            continue;
        }
        if direct_helper_observes(call, state, evidence, conditional, index) {
            return true;
        }
    }
    false
}

fn direct_helper_observes(
    call: &super::super::HelperCall,
    state: &State,
    evidence: &mut Evidence,
    conditional: bool,
    index: usize,
) -> bool {
    let base = match call.demand {
        Demand::Observed => call.unobserved_observed.get(index),
        Demand::Discarded => call.unobserved_discarded.get(index),
        Demand::Slot(_) => None,
    };
    let guarded = guarded_value(call, state, index, matches!(call.demand, Demand::Observed));
    let value = guarded.or_else(|| base.copied().flatten());
    if value == Some(false) && !conditional {
        return true;
    }
    *evidence = if !conditional && guarded == Some(true) && *evidence != Evidence::Unknown {
        Evidence::Definite
    } else {
        Evidence::Unknown
    };
    false
}

fn slot_observes(
    call: &super::super::HelperCall,
    state: &mut State,
    evidence: &mut Evidence,
    conditional: bool,
    target: (usize, Place),
) -> bool {
    let (index, slot) = target;
    let guarded_discarded = guarded_value(call, state, index, false);
    let guarded_observed = guarded_value(call, state, index, true);
    let discarded =
        guarded_discarded.or_else(|| call.unobserved_discarded.get(index).copied().flatten());
    let observed =
        guarded_observed.or_else(|| call.unobserved_observed.get(index).copied().flatten());
    if !conditional && discarded == Some(false) {
        return true;
    }
    if !conditional && observed == Some(false) {
        state.aliases.insert(slot);
    }
    let proven_skip = if observed == Some(false) {
        guarded_discarded == Some(true)
    } else {
        guarded_observed == Some(true)
    };
    *evidence = if !conditional && proven_skip && *evidence != Evidence::Unknown {
        Evidence::Definite
    } else {
        Evidence::Unknown
    };
    false
}

fn guarded_value(
    call: &super::super::HelperCall,
    state: &State,
    index: usize,
    observed: bool,
) -> Option<bool> {
    let truth = state.guards.get(call.guard.as_ref()?)?;
    let summary = if observed {
        call.conditional_observed.as_ref()
    } else {
        call.conditional_discarded.as_ref()
    }?;
    if *truth {
        summary.when_true.get(index)
    } else {
        summary.when_false.get(index)
    }
    .copied()
    .flatten()
}

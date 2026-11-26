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

pub(in crate::call_order::result_use) fn helper_paths(
    body: &Body,
    cfg: &Cfg<Stmt>,
    plan: &Plan<'_, '_>,
    syntax: (Node<'_>, &[u8]),
    unused: &[(NodeId, Origin)],
    has_closures: bool,
) -> (Vec<(NodeId, Origin)>, bool) {
    let edges = outgoing(body, cfg);
    let precise = !has_closures
        && !cyclic(body)
        && edges
            .iter()
            .enumerate()
            .all(|(node, out)| supported_edges(cfg, plan, node, out, 2))
        && body.effects.iter().flatten().all(precise_effect)
        && !cfg
            .nodes
            .iter()
            .enumerate()
            .any(|(node, _)| body.flow.reachable[node] && hidden_at(cfg, node, syntax));
    let mut hits = Vec::new();
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
            Evidence::Definite if precise => hits.push((node, origin)),
            Evidence::Definite | Evidence::Unknown => incomplete = true,
            Evidence::None => {}
        }
    }
    (hits, incomplete)
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
) -> Evidence {
    let (body, cfg, plan, syntax) = context;
    let mut pending = vec![(body.flow.entry, State::default(), Evidence::None)];
    let mut visited = BTreeSet::new();
    let mut found = Evidence::None;
    while let Some((node, mut state, mut evidence)) = pending.pop() {
        if !visited.insert((node, state.clone(), evidence)) {
            continue;
        }
        if visited.len() > PATH_LIMIT {
            return if found == Evidence::None {
                found
            } else {
                Evidence::Unknown
            };
        }
        if throws(cfg, node) {
            continue;
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
        if evidence != Evidence::None {
            found = Evidence::Unknown;
        }
        if state.active
            && (state.aliases.is_empty() || node == body.flow.exit)
            && evidence != Evidence::None
        {
            return evidence;
        }
        let stable = supported_edges(cfg, plan, node, &edges[node], 2);
        pending.extend(edges[node].iter().filter_map(|&(to, edge)| {
            if stable {
                branch(state.clone(), cfg, plan, node, edge)
            } else {
                Some(state.clone())
            }
            .map(|next| (to, next, evidence))
        }));
    }
    Evidence::None
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
    state: &State,
    evidence: &mut Evidence,
    conditional: bool,
) -> bool {
    for (index, argument) in call.arguments.iter().enumerate() {
        if !argument.is_some_and(|place| state.aliases.contains(&place)) {
            continue;
        }
        let base = match call.demand {
            Demand::Observed => call.unobserved_observed.get(index),
            Demand::Discarded => call.unobserved_discarded.get(index),
            Demand::Slot(_) => None,
        };
        let specialized = match call.demand {
            Demand::Observed => call.conditional_observed.as_ref(),
            Demand::Discarded => call.conditional_discarded.as_ref(),
            Demand::Slot(_) => None,
        };
        let guarded = call
            .guard
            .as_ref()
            .and_then(|guard| state.guards.get(guard))
            .and_then(|truth| {
                specialized.and_then(|summary| {
                    if *truth {
                        summary.when_true.get(index)
                    } else {
                        summary.when_false.get(index)
                    }
                })
            })
            .filter(|value| value.is_some());
        let value = guarded.or(base);
        if value == Some(&Some(false)) && !conditional {
            return true;
        }
        *evidence =
            if !conditional && guarded == Some(&Some(true)) && *evidence != Evidence::Unknown {
                Evidence::Definite
            } else {
                Evidence::Unknown
            };
    }
    false
}

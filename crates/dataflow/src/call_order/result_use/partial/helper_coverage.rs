use std::collections::BTreeSet;

use super::{
    branch, has_conditional, local_origins, observed, outgoing, supported_edges, Body, Cfg, Demand,
    EdgeKind, Effect, Node, Origin, Plan, State, Stmt, PATH_LIMIT,
};
use crate::cfg::NodeId;

/// A conditional helper use is incomplete unless every affected caller path observes the value.
pub(in crate::call_order::result_use) fn has_partial_helper(
    body: &Body,
    cfg: &Cfg<Stmt>,
    plan: &Plan<'_, '_>,
    syntax: (Node<'_>, &[u8]),
    unused: &[(NodeId, Origin)],
) -> bool {
    let edges = outgoing(body, cfg);
    local_origins(body).into_iter().any(|(node, origin)| {
        !unused.iter().any(|(_, other)| *other == origin)
            && uncertain_path(body, cfg, plan, syntax, &edges, node, &origin)
    })
}

fn uncertain_path(
    body: &Body,
    cfg: &Cfg<Stmt>,
    plan: &Plan<'_, '_>,
    syntax: (Node<'_>, &[u8]),
    edges: &[Vec<(NodeId, EdgeKind)>],
    start: NodeId,
    origin: &Origin,
) -> bool {
    let mut pending = vec![(start, State::default(), false)];
    let mut visited = BTreeSet::new();
    let mut found = false;
    while let Some((node, mut state, mut uncertain)) = pending.pop() {
        if !visited.insert((node, state.clone(), uncertain)) {
            continue;
        }
        if visited.len() > PATH_LIMIT {
            return found;
        }
        if cfg.nodes[node]
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::Throw { .. }))
        {
            continue;
        }
        let conditional = cfg.nodes[node].evaluations.iter().any(|range| {
            super::super::effects::evaluation(syntax.0, range)
                .is_some_and(|value| has_conditional(value, syntax.1))
        });
        if process(
            &body.effects[node],
            origin,
            &mut state,
            &mut uncertain,
            conditional,
        ) {
            continue;
        }
        found |= uncertain;
        if state.active && (state.aliases.is_empty() || node == body.flow.exit) && uncertain {
            return true;
        }
        let stable = supported_edges(cfg, plan, node, &edges[node], 2);
        pending.extend(edges[node].iter().filter_map(|&(to, edge)| {
            if stable {
                branch(state.clone(), cfg, plan, node, edge)
            } else {
                Some(state.clone())
            }
            .map(|next| (to, next, uncertain))
        }));
    }
    false
}

fn process(
    effects: &[Effect],
    origin: &Origin,
    state: &mut State,
    uncertain: &mut bool,
    conditional: bool,
) -> bool {
    for effect in effects {
        if !(conditional && matches!(effect, Effect::Read(_)))
            && observed(std::slice::from_ref(effect), origin, state)
        {
            return true;
        }
        if let Effect::ApplyHelper(call) = effect {
            for (index, argument) in call.arguments.iter().enumerate() {
                if !argument.is_some_and(|place| state.aliases.contains(&place)) {
                    continue;
                }
                let value = match call.demand {
                    Demand::Observed => call.unobserved_observed.get(index),
                    Demand::Discarded => call.unobserved_discarded.get(index),
                    Demand::Slot(_) => None,
                };
                match value {
                    Some(Some(false)) if !conditional => return true,
                    Some(Some(true)) | Some(None) | None => *uncertain = true,
                    Some(Some(false)) => *uncertain = true,
                }
            }
        }
    }
    false
}

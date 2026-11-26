use std::collections::BTreeSet;

use super::{
    branch, cyclic, has_conditional, outgoing, supported_edges, Body, Cfg, EdgeKind, Effect, Node,
    Place, Plan, State, Stmt, Value, PATH_LIMIT,
};

/// `None` means that this helper is outside the bounded model.
pub(in crate::call_order::result_use) fn unobserved_parameters(
    body: &Body,
    cfg: &Cfg<Stmt>,
    plan: &Plan<'_, '_>,
    syntax: (Node<'_>, &[u8]),
    parameters: &[Option<Place>],
    return_used: bool,
    has_closures: bool,
) -> Vec<Option<bool>> {
    if parameters.len() > 64 {
        return vec![None; parameters.len()];
    }
    let edges = outgoing(body, cfg);
    let branches = edges
        .iter()
        .filter(|out| {
            out.iter()
                .any(|(_, edge)| matches!(edge, EdgeKind::True | EdgeKind::False))
        })
        .count();
    let supported = !has_closures
        && !cyclic(body)
        && edges
            .iter()
            .enumerate()
            .all(|(node, out)| supported_edges(cfg, plan, node, out, branches))
        && body.effects.iter().flatten().all(supported_effect)
        && !cfg.nodes.iter().enumerate().any(|(index, node)| {
            body.flow.reachable[index]
                && node.evaluations.iter().any(|range| {
                    super::super::effects::evaluation(syntax.0, range)
                        .is_some_and(|node| has_conditional(node, syntax.1))
                })
        });
    parameters
        .iter()
        .map(|parameter| {
            if !supported {
                return None;
            }
            let parameter = parameter.filter(|place| matches!(place, Place::Local(_)))?;
            unobserved(body, cfg, plan, &edges, parameter, return_used)
        })
        .collect()
}

fn supported_effect(effect: &Effect) -> bool {
    match effect {
        Effect::Read(Place::Local(_)) => true,
        Effect::Write(writes) => writes.iter().all(|write| {
            write
                .target
                .is_none_or(|place| matches!(place, Place::Local(_) | Place::Return))
                && matches!(
                    write.value,
                    Value::Source(_) | Value::Copy(Place::Local(_)) | Value::Other
                )
        }),
        _ => false,
    }
}

fn unobserved(
    body: &Body,
    cfg: &Cfg<Stmt>,
    plan: &Plan<'_, '_>,
    edges: &[Vec<(usize, EdgeKind)>],
    parameter: Place,
    return_used: bool,
) -> Option<bool> {
    let mut initial = State {
        active: true,
        ..State::default()
    };
    initial.aliases.insert(parameter);
    let mut pending = vec![(body.flow.entry, initial)];
    let mut seen = BTreeSet::new();
    while let Some((node, mut state)) = pending.pop() {
        if !seen.insert((node, state.clone())) {
            continue;
        }
        if seen.len() > PATH_LIMIT {
            return None;
        }
        if cfg.nodes[node]
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::Throw { .. }))
        {
            continue;
        }
        if observes(&body.effects[node], &mut state, return_used) {
            continue;
        }
        if state.aliases.is_empty() || node == body.flow.exit {
            return Some(true);
        }
        pending.extend(edges[node].iter().filter_map(|&(to, edge)| {
            branch(state.clone(), cfg, plan, node, edge).map(|state| (to, state))
        }));
    }
    Some(false)
}

fn observes(effects: &[Effect], state: &mut State, return_used: bool) -> bool {
    for effect in effects {
        match effect {
            Effect::Read(place) if state.aliases.contains(place) => return true,
            Effect::Write(writes) => {
                let previous = state.aliases.clone();
                if return_used && writes.iter().any(|write| {
                    write.target == Some(Place::Return)
                        && matches!(&write.value, Value::Copy(place) if previous.contains(place))
                }) {
                    return true;
                }
                for target in writes.iter().filter_map(|write| write.target) {
                    state.aliases.remove(&target);
                }
                for write in writes {
                    if let (Some(target), Value::Copy(source)) = (write.target, &write.value) {
                        if previous.contains(source) && target != Place::Return {
                            state.aliases.insert(target);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    false
}

use std::collections::BTreeSet;

use super::{
    branch, cyclic, hidden_at, outgoing, straight_line, supported_edges,
    supported_effect_with_return, Body, Cfg, EdgeKind, Effect, Node, Place, Plan, State, Stmt,
    Value, Visits,
};

/// `None` means that this helper is outside the bounded model.
pub(in crate::call_order::result_use) fn unobserved_parameters(
    body: &Body,
    cfg: &Cfg<Stmt>,
    plan: &Plan<'_, '_>,
    syntax: (Node<'_>, &[u8]),
    options: (&[Option<Place>], bool, bool),
) -> Vec<Option<bool>> {
    let (parameters, return_used, has_closures) = options;
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
        && body
            .effects
            .iter()
            .flatten()
            .all(|effect| supported_effect_with_return(effect, true))
        && !cfg
            .nodes
            .iter()
            .enumerate()
            .any(|(index, _)| body.flow.reachable[index] && hidden_at(cfg, index, syntax));
    parameters
        .iter()
        .map(|parameter| {
            let parameter = parameter.filter(|place| matches!(place, Place::Local(_)))?;
            if !supported {
                return parameter_read_before_branch(body, cfg, syntax, parameter, return_used)
                    .then_some(false);
            }
            unobserved(body, cfg, plan, &edges, (parameter, return_used))
        })
        .collect()
}

fn parameter_read_before_branch(
    body: &Body,
    cfg: &Cfg<Stmt>,
    syntax: (Node<'_>, &[u8]),
    parameter: Place,
    return_used: bool,
) -> bool {
    let mut state = State {
        active: true,
        ..State::default()
    };
    state.aliases.insert(parameter);
    straight_line(body, cfg, syntax, body.flow.entry, |effects| {
        if !effects
            .iter()
            .all(|effect| supported_effect_with_return(effect, true))
        {
            return Some(false);
        }
        if observes(effects, &mut state, return_used) {
            return Some(true);
        }
        state.aliases.is_empty().then_some(false)
    })
}

fn unobserved(
    body: &Body,
    cfg: &Cfg<Stmt>,
    plan: &Plan<'_, '_>,
    edges: &[Vec<(usize, EdgeKind)>],
    target: (Place, bool),
) -> Option<bool> {
    let (parameter, return_used) = target;
    let mut initial = State {
        active: true,
        ..State::default()
    };
    initial.aliases.insert(parameter);
    let mut pending = vec![(body.flow.entry, initial)];
    let mut seen = Visits::default();
    while let Some((node, mut state)) = pending.pop() {
        if !seen.enter(cfg, node, &state)? {
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
                if return_used && returns_alias(writes, &previous) {
                    return true;
                }
                update_parameter_aliases(writes, &previous, state);
            }
            _ => {}
        }
    }
    false
}

fn returns_alias(writes: &[super::Write], previous: &BTreeSet<Place>) -> bool {
    writes.iter().any(|write| {
        write.target == Some(Place::Return)
            && matches!(&write.value, Value::Copy(place) if previous.contains(place))
    })
}

fn update_parameter_aliases(
    writes: &[super::Write],
    previous: &BTreeSet<Place>,
    state: &mut State,
) {
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

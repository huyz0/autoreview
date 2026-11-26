use std::collections::{BTreeMap, BTreeSet};

use super::{
    has_conditional, Body, Cfg, EdgeKind, Effect, IdentityOperand, Node, NodeId, Plan, State, Stmt,
    PATH_LIMIT,
};

#[derive(Default)]
pub(super) struct Visits(BTreeSet<(NodeId, State)>);

impl Visits {
    pub(super) fn enter(&mut self, cfg: &Cfg<Stmt>, node: NodeId, state: &State) -> Option<bool> {
        if !self.0.insert((node, state.clone())) {
            return Some(false);
        }
        if self.0.len() > PATH_LIMIT {
            return None;
        }
        Some(!throws(cfg, node))
    }
}

pub(super) fn straight_line(
    body: &Body,
    cfg: &Cfg<Stmt>,
    syntax: (Node<'_>, &[u8]),
    mut node: NodeId,
    mut check: impl FnMut(&[Effect]) -> Option<bool>,
) -> bool {
    let mut seen = BTreeSet::new();
    while seen.insert(node) {
        if hidden_at(cfg, node, syntax) {
            return false;
        }
        if let Some(result) = check(&body.effects[node]) {
            return result;
        }
        let [next] = body.flow.successors[node].as_slice() else {
            return false;
        };
        node = *next;
    }
    false
}

pub(super) fn supported_edges(
    cfg: &Cfg<Stmt>,
    plan: &Plan<'_, '_>,
    node: NodeId,
    outgoing: &[(NodeId, EdgeKind)],
    branches: usize,
) -> bool {
    let has_branch = outgoing
        .iter()
        .any(|(_, edge)| matches!(edge, EdgeKind::True | EdgeKind::False));
    if has_branch && cfg.nodes[node].condition.is_none() {
        return false;
    }
    if outgoing.iter().any(|(target, edge)| {
        *edge == EdgeKind::Exceptional
            && !cfg.nodes[*target]
                .stmts
                .iter()
                .any(|stmt| matches!(stmt, Stmt::Throw { .. }))
    }) {
        return false;
    }
    if let Some((name, _)) = &cfg.nodes[node].condition {
        let identity = guard(plan, name);
        if (branches > 1 || plan.syntax.repeated_guard(name)) && identity.is_none() {
            return false;
        }
        if identity
            .as_ref()
            .is_some_and(|identity| guard_is_rebound(cfg, name, identity))
        {
            return false;
        }
    }
    true
}

fn guard_is_rebound(cfg: &Cfg<Stmt>, name: &str, identity: &IdentityOperand) -> bool {
    let root = match identity {
        IdentityOperand::Binding(value) | IdentityOperand::Scoped { name: value, .. } => {
            Some(value.as_str())
        }
        _ => None,
    };
    cfg.nodes
        .iter()
        .flat_map(|node| &node.stmts)
        .any(|stmt| match stmt {
            Stmt::Rebind(write) => write.target().0 == name || root == Some(write.target().0),
            _ => false,
        })
}

pub(super) fn guard(plan: &Plan<'_, '_>, name: &str) -> Option<IdentityOperand> {
    match name {
        "true" => Some(IdentityOperand::Boolean(true)),
        "false" => Some(IdentityOperand::Boolean(false)),
        _ => plan.syntax.resolve(&BTreeMap::new(), name, usize::MAX),
    }
}

pub(super) fn hidden_at(cfg: &Cfg<Stmt>, node: NodeId, syntax: (Node<'_>, &[u8])) -> bool {
    cfg.nodes[node].evaluations.iter().any(|range| {
        super::super::effects::evaluation(syntax.0, range)
            .is_some_and(|value| has_conditional(value, syntax.1))
    })
}

pub(super) fn throws(cfg: &Cfg<Stmt>, node: NodeId) -> bool {
    cfg.nodes[node]
        .stmts
        .iter()
        .any(|stmt| matches!(stmt, Stmt::Throw { .. }))
}

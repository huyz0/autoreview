use std::collections::BTreeMap;

use super::{Cfg, EdgeKind, IdentityOperand, NodeId, Plan, Stmt};

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

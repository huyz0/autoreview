//! Checks call obligations using control-flow graphs.
//! Keyed rules track collection and key values through branches, loops,
//! and eligible same-file helper calls.

mod bindings;
mod identity;
mod plan;
mod keyed;

pub use keyed::{check_keyed, check_keyed_file, KeyedFunction};

use crate::cfg::{Cfg, CfgNode, NodeId, Stmt};
use crate::lattice::Lattice;
use crate::solver;
use crate::taint::NamePattern;

#[derive(Debug, Clone)]
pub struct CallOrderSpec {
    pub rule_id: String,
    /// A call matching one of these sets `pending`.
    pub after: Vec<NamePattern>,
    /// A call matching one of these clears `pending` — checked before
    /// `after` at the same call so a rule can't accidentally seed and
    /// immediately clear itself off one ambiguous name (in practice
    /// `after`/`unless` should never overlap on the same call name, but
    /// `unless` taking priority is the safer default if they ever do).
    pub unless: Vec<NamePattern>,
    /// A call matching one of these, reached while `pending`, is a hit.
    /// Empty is valid when `check_before_return` alone is the check mode.
    pub before: Vec<NamePattern>,
    /// Flag every `Stmt::Return` reached while still `pending` — the
    /// "obligation must be resolved before this function exits" check
    /// mode (unreleased-lock's shape), independent of (and combinable
    /// with) `before`.
    pub check_before_return: bool,
    pub identity: Option<EntryIdentity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionSelector {
    Receiver,
    Argument(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryIdentity {
    pub collection: CollectionSelector,
    pub key_argument: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntryHit {
    pub collection: String,
    pub key: String,
    pub registration_line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending(bool);

impl Lattice for Pending {
    fn bottom() -> Self {
        Pending(false)
    }
    fn join(&self, other: &Self) -> Self {
        Pending(self.0 || other.0)
    }
}

fn apply(spec: &CallOrderSpec, pending: bool, stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Call { target: crate::cfg::CallTarget::Named(name), .. } => {
            if spec.unless.iter().any(|p| p.matches(name)) {
                false
            } else if spec.after.iter().any(|p| p.matches(name)) {
                true
            } else {
                pending
            }
        }
        _ => pending,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallOrderHit {
    pub trigger_call: String,
    pub node: NodeId,
    pub source_line: u32,
    pub entry: Option<EntryHit>,
}

/// Runs the original function-wide obligation check.
pub fn check(spec: &CallOrderSpec, cfg: &Cfg<Stmt>) -> Vec<CallOrderHit> {
    if spec.identity.is_some() {
        return Vec::new();
    }
    let out_facts = solver::solve(cfg, |_id, node: &CfgNode<Stmt>, in_fact: &Pending| {
        Pending(node.stmts.iter().fold(in_fact.0, |pending, stmt| apply(spec, pending, stmt)))
    });

    let mut hits = Vec::new();
    for (node_id, node) in cfg.nodes.iter().enumerate() {
        let preds = cfg.predecessors(node_id);
        let mut pending = preds.iter().any(|&p| out_facts[p].0);
        for (index, stmt) in node.stmts.iter().enumerate() {
            match stmt {
                Stmt::Call { target: crate::cfg::CallTarget::Named(name), .. } if pending && spec.before.iter().any(|p| p.matches(name)) => {
                    hits.push(CallOrderHit { trigger_call: name.clone(), node: node_id, source_line: node.statement_line(index), entry: None });
                }
                Stmt::Return { .. } if pending && spec.check_before_return => {
                    hits.push(CallOrderHit { trigger_call: "return".to_string(), node: node_id, source_line: node.statement_line(index), entry: None });
                }
                _ => {}
            }
            pending = apply(spec, pending, stmt);
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfg::{CallTarget, EdgeKind, RhsShape};

    fn xxe_spec() -> CallOrderSpec {
        CallOrderSpec {
            rule_id: "test-xxe".to_string(),
            after: vec![NamePattern::suffix("newInstance")],
            unless: vec![NamePattern::suffix("setFeature")],
            before: vec![NamePattern::suffix("parse")],
            check_before_return: false,
            identity: None,
        }
    }

    fn lock_spec() -> CallOrderSpec {
        CallOrderSpec {
            rule_id: "test-unreleased-lock".to_string(),
            after: vec![NamePattern::suffix("Lock")],
            unless: vec![NamePattern::suffix("Unlock")],
            before: vec![],
            check_before_return: true,
            identity: None,
        }
    }

    fn straight_line(stmts: Vec<Stmt>) -> Cfg<Stmt> {
        let mut cfg: Cfg<Stmt> = Cfg::new_empty();
        let entry = cfg.push_node(1);
        cfg.nodes[entry].stmts = stmts;
        cfg.entry = entry;
        cfg.exit = entry;
        cfg
    }

    fn call(name: &str, assigned_to: Option<&str>) -> Stmt {
        Stmt::Call { target: CallTarget::Named(name.to_string()), args: vec![], assigned_to: assigned_to.map(str::to_string), site: None }
    }

    #[test]
    fn flags_parse_reached_without_an_intervening_setfeature_call() {
        let cfg = straight_line(vec![call("DocumentBuilderFactory.newInstance", Some("dbf")), call("dbf.parse", None)]);
        let hits = check(&xxe_spec(), &cfg);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
        assert_eq!(hits[0].trigger_call, "dbf.parse");
    }

    #[test]
    fn does_not_flag_parse_after_setfeature_was_called() {
        let cfg = straight_line(vec![call("DocumentBuilderFactory.newInstance", Some("dbf")), call("dbf.setFeature", None), call("dbf.parse", None)]);
        assert!(check(&xxe_spec(), &cfg).is_empty());
    }

    #[test]
    fn does_not_flag_parse_with_no_newinstance_call_at_all() {
        let cfg = straight_line(vec![call("dbf.parse", None)]);
        assert!(check(&xxe_spec(), &cfg).is_empty());
    }

    #[test]
    fn flags_a_return_reached_while_a_lock_is_still_held() {
        let mut cfg: Cfg<Stmt> = Cfg::new_empty();
        let entry = cfg.push_node(1);
        cfg.nodes[entry].stmts = vec![call("mu.Lock", None), Stmt::Return { value: None }];
        cfg.entry = entry;
        cfg.exit = entry;
        let hits = check(&lock_spec(), &cfg);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
    }

    #[test]
    fn does_not_flag_a_return_after_the_lock_was_released() {
        let mut cfg: Cfg<Stmt> = Cfg::new_empty();
        let entry = cfg.push_node(1);
        cfg.nodes[entry].stmts = vec![call("mu.Lock", None), call("mu.Unlock", None), Stmt::Return { value: None }];
        cfg.entry = entry;
        cfg.exit = entry;
        assert!(check(&lock_spec(), &cfg).is_empty());
    }

    #[test]
    fn does_not_flag_a_return_with_no_lock_ever_taken() {
        let mut cfg: Cfg<Stmt> = Cfg::new_empty();
        let entry = cfg.push_node(1);
        cfg.nodes[entry].stmts = vec![Stmt::Return { value: None }];
        cfg.entry = entry;
        cfg.exit = entry;
        assert!(check(&lock_spec(), &cfg).is_empty());
    }

    #[test]
    fn flags_only_the_branch_that_never_released_the_lock() {
        // if cond { mu.Unlock() } else { /* forgot to unlock */ }; return
        let mut cfg: Cfg<Stmt> = Cfg::new_empty();
        let entry = cfg.push_node(1);
        let unlocked_branch = cfg.push_node(2);
        let forgot_branch = cfg.push_node(3);
        let join = cfg.push_node(4);
        cfg.nodes[entry].stmts = vec![call("mu.Lock", None)];
        cfg.nodes[unlocked_branch].stmts = vec![call("mu.Unlock", None)];
        cfg.nodes[join].stmts = vec![Stmt::Return { value: None }];
        cfg.edges.push((entry, unlocked_branch, EdgeKind::True));
        cfg.edges.push((entry, forgot_branch, EdgeKind::False));
        cfg.edges.push((unlocked_branch, join, EdgeKind::Fallthrough));
        cfg.edges.push((forgot_branch, join, EdgeKind::Fallthrough));
        cfg.entry = entry;
        cfg.exit = join;

        let hits = check(&lock_spec(), &cfg);
        assert_eq!(hits.len(), 1, "got: {hits:#?} — the join's pending fact must be the OR of both incoming branches, still flagging the return even though one branch did unlock");
    }

    #[test]
    fn does_not_flag_a_var_declaration_shaped_like_return() {
        // Sanity check that RhsShape::Var assignments don't accidentally
        // participate in the pending lattice at all.
        let cfg = straight_line(vec![call("DocumentBuilderFactory.newInstance", Some("dbf")), Stmt::Assign { lhs: "x".to_string(), rhs: RhsShape::Var("dbf".to_string()) }]);
        assert!(check(&xxe_spec(), &cfg).is_empty());
    }
}

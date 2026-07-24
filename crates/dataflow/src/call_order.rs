//! A generic, declarative "call ordering" engine — closes a gap the
//! taint engine structurally can't cover: taint tracks *data* flowing
//! from a source to a sink through variables; this tracks a *control*
//! fact — has some earlier call happened, on this path, without an
//! intervening call that would make it safe — with no variable/receiver
//! identity involved at all. That's exactly the shape of two real
//! vulnerability/bug classes surveyed in `RULE_ENGINE_RESEARCH.md`'s
//! rule-set enrichment pass and explicitly declined there at the time
//! for lack of this primitive:
//!
//! - **XXE** (CWE-611): an XML parser factory (`DocumentBuilderFactory
//!   .newInstance()`, ...) is unsafe by default; a `parse()` call reached
//!   without an intervening secure-configuration call (`setFeature`,
//!   `setExpandEntityReferences(false)`, ...) anywhere earlier on that
//!   path is the vulnerability CodeQL's XXE queries look for.
//! - **Unreleased lock** (CWE-459-adjacent): a `Lock.lock()` call
//!   obligates a matching `unlock()` before the function returns; a
//!   `return`/`throw` reached while that obligation is still open is a
//!   real correctness bug (the lock leaks, every future caller
//!   deadlocks).
//!
//! Both are "did event A happen without event B happening before event
//! C" — modeled here as one boolean fact (`pending`) per CFG path, seeded
//! by an `after` call, cleared by an `unless` call, and checked against
//! either a `before` call or every `Stmt::Return`. This is deliberately
//! **not variable/receiver-tracked** — unlike `taint::TaintFacts`
//! (keyed per-variable) this is a single flag for the whole function,
//! the same simplification `go_typed_nil_interface_return`'s summary
//! walk and the taint engine's own `compute_source_summary` already make
//! elsewhere in this crate. The tradeoff is real and worth stating
//! plainly: a function juggling *two* independent resources of the same
//! kind (two locks, two parser factories) can produce a false negative
//! (one gets configured/unlocked, masking the other still being
//! unsafe/held) — narrower than a full per-object points-to analysis,
//! but the same "structural heuristic over full precision" tradeoff this
//! whole crate already makes (see `taint`'s and `lib.rs`'s own module
//! docs). Rules built on this engine should be scoped to functions where
//! that single-resource-at-a-time assumption is realistic (a
//! parse-one-document handler, a lock-a-critical-section method) — not a
//! reason to avoid the primitive, since the alternative today is no
//! detection at all for this vulnerability shape.

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
}

/// Runs `spec` against one function's already-lowered CFG. See the module
/// doc comment for the single-boolean-fact tradeoff this makes — no
/// interprocedural variant exists (or is planned) since the obligation
/// this models is meant to resolve within one function, not across a
/// call boundary.
pub fn check(spec: &CallOrderSpec, cfg: &Cfg<Stmt>) -> Vec<CallOrderHit> {
    let out_facts = solver::solve(cfg, |_id, node: &CfgNode<Stmt>, in_fact: &Pending| {
        Pending(node.stmts.iter().fold(in_fact.0, |pending, stmt| apply(spec, pending, stmt)))
    });

    let mut hits = Vec::new();
    for (node_id, node) in cfg.nodes.iter().enumerate() {
        let preds = cfg.predecessors(node_id);
        let mut pending = preds.iter().any(|&p| out_facts[p].0);
        for stmt in &node.stmts {
            match stmt {
                Stmt::Call { target: crate::cfg::CallTarget::Named(name), .. } if pending && spec.before.iter().any(|p| p.matches(name)) => {
                    hits.push(CallOrderHit { trigger_call: name.clone(), node: node_id, source_line: node.source_line });
                }
                Stmt::Return { .. } if pending && spec.check_before_return => {
                    hits.push(CallOrderHit { trigger_call: "return".to_string(), node: node_id, source_line: node.source_line });
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
        }
    }

    fn lock_spec() -> CallOrderSpec {
        CallOrderSpec {
            rule_id: "test-unreleased-lock".to_string(),
            after: vec![NamePattern::suffix("Lock")],
            unless: vec![NamePattern::suffix("Unlock")],
            before: vec![],
            check_before_return: true,
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
        Stmt::Call { target: CallTarget::Named(name.to_string()), args: vec![], assigned_to: assigned_to.map(str::to_string) }
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

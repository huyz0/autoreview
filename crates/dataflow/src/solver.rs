//! A standard forward worklist fixpoint solver over a `Cfg`. Generic over
//! any `Lattice` and a per-node transfer function — each rule supplies its
//! own small lattice (see `crates/dataflow/src/lower/*.rs` and
//! `crates/core/src/analyzers/dataflow_check.rs` for concrete uses) rather
//! than this crate defining one shared general-purpose lattice.

use std::collections::VecDeque;

use crate::cfg::{Cfg, CfgNode, NodeId};
use crate::lattice::Lattice;

/// Runs the transfer function to a fixpoint and returns the fact live at
/// the *exit* of each node (i.e. after that node's statements have been
/// applied). A node's in-fact is the join of all its predecessors'
/// out-facts — nodes with no predecessors (the entry, and any
/// unreachable-by-construction node) start from `F::bottom()`. `transfer`
/// receives the node's own id so a rule can special-case the entry node
/// (e.g. seed initial facts) without needing a separate pre-pass.
pub fn solve<S, F: Lattice>(cfg: &Cfg<S>, transfer: impl Fn(NodeId, &CfgNode<S>, &F) -> F) -> Vec<F> {
    let mut out_facts: Vec<F> = vec![F::bottom(); cfg.nodes.len()];
    let mut worklist: VecDeque<NodeId> = (0..cfg.nodes.len()).collect();
    let mut queued: Vec<bool> = vec![true; cfg.nodes.len()];

    while let Some(node) = worklist.pop_front() {
        queued[node] = false;
        let preds = cfg.predecessors(node);
        let in_fact = preds.iter().fold(F::bottom(), |acc, &p| acc.join(&out_facts[p]));
        let new_out = transfer(node, &cfg.nodes[node], &in_fact);
        if new_out != out_facts[node] {
            out_facts[node] = new_out;
            for succ in cfg.successors(node) {
                if !queued[succ] {
                    queued[succ] = true;
                    worklist.push_back(succ);
                }
            }
        }
    }

    out_facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfg::{EdgeKind, Stmt};

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Reached(bool);

    impl Lattice for Reached {
        fn bottom() -> Self {
            Reached(false)
        }
        fn join(&self, other: &Self) -> Self {
            Reached(self.0 || other.0)
        }
    }

    /// A transfer function for the `Reached` lattice: the entry node seeds
    /// itself as reached; every other node just propagates whatever
    /// reached it (a plain "is this node on any path from entry" analysis).
    fn reachability(entry: NodeId) -> impl Fn(NodeId, &CfgNode<Stmt>, &Reached) -> Reached {
        move |node, _cfg_node, in_fact| if node == entry { Reached(true) } else { in_fact.clone() }
    }

    #[test]
    fn joins_facts_from_both_branches_of_a_diamond() {
        let mut cfg: Cfg<Stmt> = Cfg::new_empty();
        let entry = cfg.push_node(1);
        let a = cfg.push_node(2);
        let b = cfg.push_node(3);
        let join = cfg.push_node(4);
        cfg.edges.push((entry, a, EdgeKind::True));
        cfg.edges.push((entry, b, EdgeKind::False));
        cfg.edges.push((a, join, EdgeKind::Fallthrough));
        cfg.edges.push((b, join, EdgeKind::Fallthrough));
        cfg.entry = entry;
        cfg.exit = join;

        let facts = solve(&cfg, reachability(entry));
        assert_eq!(facts[join], Reached(true), "join node should be reachable via either branch");
    }

    #[test]
    fn terminates_on_a_loop_back_edge() {
        let mut cfg: Cfg<Stmt> = Cfg::new_empty();
        let header = cfg.push_node(1);
        let body = cfg.push_node(2);
        let after = cfg.push_node(3);
        cfg.edges.push((header, body, EdgeKind::True));
        cfg.edges.push((body, header, EdgeKind::Loop));
        cfg.edges.push((header, after, EdgeKind::False));
        cfg.entry = header;
        cfg.exit = after;

        // Termination itself is the assertion here — a solver that never
        // reaches a fixpoint on a cycle would hang this test.
        let facts = solve(&cfg, reachability(header));
        assert_eq!(facts[after], Reached(true));
        assert_eq!(facts[body], Reached(true));
    }

    #[test]
    fn an_unreachable_node_stays_at_bottom() {
        let mut cfg: Cfg<Stmt> = Cfg::new_empty();
        let entry = cfg.push_node(1);
        let ret = cfg.push_node(2);
        let unreachable = cfg.push_node(3);
        cfg.edges.push((entry, ret, EdgeKind::Fallthrough));
        // `unreachable` has no incoming edges — e.g. dead code after an
        // unconditional early return.
        cfg.entry = entry;
        cfg.exit = ret;

        let facts = solve(&cfg, reachability(entry));
        assert_eq!(facts[unreachable], Reached(false));
        assert_eq!(facts[ret], Reached(true));
    }
}

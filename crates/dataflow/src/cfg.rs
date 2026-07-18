//! A deliberately small, language-agnostic control-flow-graph
//! representation. Not a full IR: `Stmt` only models the handful of
//! statement shapes the dataflow-powered rules actually need to
//! recognize (assignment, return, call, guard, closure capture) — most
//! real statements lower to `Stmt::Other`, present in the graph for
//! control-flow/line-number fidelity but contributing no dataflow facts.
//! No SSA, no points-to/alias analysis.

pub type NodeId = usize;
pub type VarId = String;

#[derive(Debug, Clone)]
pub enum RhsShape {
    /// A bare `nil`/`null` literal.
    NilLiteral,
    /// `x[a:b]` — re-slicing another variable.
    Reslice { of: VarId },
    /// A plain variable reference, `x = y`.
    Var(VarId),
    /// Anything this lowering pass doesn't specifically recognize.
    Unknown,
}

#[derive(Debug, Clone)]
pub enum CallTarget {
    /// `name(...)` or `recv.name(...)` — kept as text; resolution against
    /// a `SymbolIndex` happens one layer up, not inside the CFG itself.
    Named(String),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardOp {
    Equal,
    NotEqual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardAgainst {
    Nil,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosureKind {
    Goroutine,
    Deferred,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Assign { lhs: VarId, rhs: RhsShape },
    Return { value: Option<VarId> },
    Call { target: CallTarget, args: Vec<VarId>, assigned_to: Option<VarId> },
    Guard { var: VarId, op: GuardOp, against: GuardAgainst },
    ClosureCapture { captured: Vec<VarId>, kind: ClosureKind },
    /// Anything not specifically recognized above. Keeps the statement's
    /// raw source text so a rule can still fall back to a textual
    /// whole-word reference check (e.g. "is `full` mentioned here at
    /// all?") without this crate needing a dedicated `Stmt` variant for
    /// every possible use-site shape.
    Other(String),
}

#[derive(Debug, Clone)]
pub struct CfgNode<S> {
    pub stmts: Vec<S>,
    pub source_line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// Straight-line flow, or an unconditional loop back-edge.
    Fallthrough,
    /// The true/false arms of a conditional.
    True,
    False,
    /// A loop's back-edge (re-entering the loop header).
    Loop,
}

#[derive(Debug, Clone)]
pub struct Cfg<S> {
    pub entry: NodeId,
    pub exit: NodeId,
    pub nodes: Vec<CfgNode<S>>,
    pub edges: Vec<(NodeId, NodeId, EdgeKind)>,
}

impl<S> Cfg<S> {
    pub fn predecessors(&self, node: NodeId) -> Vec<NodeId> {
        self.edges.iter().filter(|(_, to, _)| *to == node).map(|(from, _, _)| *from).collect()
    }

    pub fn successors(&self, node: NodeId) -> Vec<NodeId> {
        self.edges.iter().filter(|(from, _, _)| *from == node).map(|(_, to, _)| *to).collect()
    }

    /// A minimal builder: pushes an empty node and returns its id. Callers
    /// (the per-language lowering passes, or tests) fill in `stmts` and
    /// wire `edges` directly — this is intentionally low-level rather than
    /// a fluent builder API, since the lowering passes' control flow is
    /// varied enough that a generic builder would just get in the way.
    pub fn push_node(&mut self, source_line: u32) -> NodeId {
        self.nodes.push(CfgNode { stmts: Vec::new(), source_line });
        self.nodes.len() - 1
    }

    pub fn new_empty() -> Self {
        Cfg { entry: 0, exit: 0, nodes: Vec::new(), edges: Vec::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_predecessors_and_successors() {
        let mut cfg: Cfg<Stmt> = Cfg::new_empty();
        let a = cfg.push_node(1);
        let b = cfg.push_node(2);
        let c = cfg.push_node(3);
        cfg.edges.push((a, b, EdgeKind::True));
        cfg.edges.push((a, c, EdgeKind::False));
        cfg.entry = a;
        cfg.exit = c;

        assert_eq!(cfg.successors(a), vec![b, c]);
        assert_eq!(cfg.predecessors(c), vec![a]);
        assert!(cfg.predecessors(a).is_empty());
    }
}

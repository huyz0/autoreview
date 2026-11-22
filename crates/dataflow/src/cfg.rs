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
    /// `&x` — taking a reference/address of another variable.
    AddressOf { of: VarId },
    /// `a + b + "literal"` — string concatenation. Carries every
    /// identifier operand found anywhere in the expression (over-
    /// approximate — doesn't distinguish which operand is which), used
    /// by the taint engine to propagate taint through a concatenated
    /// value (e.g. a SQL query or file path built from a tainted part).
    Concat { parts: Vec<VarId> },
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum IdentityOperand {
    Binding(String),
    Fresh { name: String, start: usize, age: u8, unit: usize },
    Scoped { name: String, owner: usize, unit: usize },
    CallResult { start: usize, fallback: bool },
    String(String),
    Integer(String),
    Boolean(bool),
}

/// Positional operands are separate from the flattened arguments used for taint.
#[derive(Debug, Clone)]
pub struct CallSite {
    pub start: usize,
    pub receiver: Option<String>,
    pub arguments: Vec<Option<IdentityOperand>>,
    pub(crate) local: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ThrowingCall {
    pub(crate) name: String,
    pub(crate) start: usize,
    pub(crate) site: Option<Box<CallSite>>,
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
pub struct BindingWrite {
    pub(crate) name: String,
    pub(crate) start: usize,
    pub(crate) value: Option<IdentityOperand>,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Assign { lhs: VarId, rhs: RhsShape },
    Return { value: Option<VarId> },
    ReturnIdentity(Option<IdentityOperand>),
    Throw { exception: Option<String>, implicit: bool },
    Rebind(BindingWrite),
    LoopJump { target: NodeId, boundary: NodeId },
    DeferredBlock(Box<Cfg<Stmt>>),
    DeferredCall { target: String, site: Option<Box<CallSite>> },
    Call { target: CallTarget, args: Vec<VarId>, assigned_to: Option<VarId>, site: Option<Box<CallSite>> },
    Guard { var: VarId, op: GuardOp, against: GuardAgainst },
    ClosureCapture { captured: Vec<VarId>, kind: ClosureKind },
    /// Marks the point where `vars` come into scope as a range-loop's own
    /// loop variables (pushed once, at the top of the loop body). Rules
    /// that need to know "is this variable a currently-active loop
    /// variable" (the pre-1.22 capture/address rules) key off this rather
    /// than any generic `Assign`, since an ordinary reassignment
    /// shouldn't be conflated with loop-variable scoping.
    LoopVarBind { vars: Vec<VarId> },
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
    statement_lines: Vec<Option<u32>>,
    pub(crate) condition: Option<(String, bool)>,
    pub(crate) throwing_call: Option<ThrowingCall>,
    pub(crate) loop_condition: bool,
}

impl<S> CfgNode<S> {
    pub(crate) fn record_statement_lines(&mut self, start: usize, line: u32) {
        if start == self.stmts.len() {
            return;
        }
        self.statement_lines.resize(self.stmts.len(), None);
        for slot in &mut self.statement_lines[start..] {
            slot.get_or_insert(line);
        }
    }

    pub(crate) fn statement_line(&self, index: usize) -> u32 {
        self.statement_lines.get(index).copied().flatten().unwrap_or(self.source_line)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// Straight-line flow, or an unconditional loop back-edge.
    Fallthrough,
    /// The true/false arms of a conditional.
    True,
    False,
    Exceptional,
    /// A loop's back-edge (re-entering the loop header).
    Loop,
}

#[derive(Debug, Clone)]
pub struct Cfg<S> {
    pub entry: NodeId,
    pub exit: NodeId,
    pub nodes: Vec<CfgNode<S>>,
    pub edges: Vec<(NodeId, NodeId, EdgeKind)>,
    // Remaining copies of finally blocks for distinct exit paths.
    pub(crate) keyed_flow: Option<usize>,
    pub(crate) keyed_exceptions: bool,
    pub(crate) loop_targets: Vec<(NodeId, NodeId, NodeId)>,
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
        self.nodes.push(CfgNode { stmts: Vec::new(), source_line, statement_lines: Vec::new(), condition: None, throwing_call: None, loop_condition: false });
        self.nodes.len() - 1
    }

    pub(crate) fn emit(&mut self, at: NodeId, stmt: S, line: u32) {
        let node = &mut self.nodes[at];
        let start = node.stmts.len();
        node.stmts.push(stmt);
        node.record_statement_lines(start, line);
    }

    pub fn new_empty() -> Self {
        Cfg { entry: 0, exit: 0, nodes: Vec::new(), edges: Vec::new(), keyed_flow: None, keyed_exceptions: false, loop_targets: Vec::new() }
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

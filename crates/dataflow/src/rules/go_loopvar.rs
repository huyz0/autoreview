//! Real-dataflow rewrites of `loopvar-capture-pre-1.22` and
//! `loopvar-address-pre-1.22` (previously brace-counting text heuristics
//! in `autoreview-core`'s `practices.rs`). Both consume the same
//! `LoopVarBind` fact — "is this variable a currently-active range-loop
//! variable at this program point" — computed by the shared
//! [`ActiveLoopVars`] lattice, and differ only in what they check against
//! it: a closure capturing the variable by reference, vs. `&variable`
//! taken directly in the loop body.
//!
//! Scope carried over unchanged from the old heuristics (this crate
//! doesn't re-litigate it): both rules are gated by the caller on
//! `go.mod` targeting a pre-1.22 Go version, since 1.22 changed
//! `for`/`range` loop variables to per-iteration scope, eliminating the
//! underlying bug.
//!
//! Known limitation carried over from the CFG's lattice, not present in
//! the old text heuristic: a loop variable's `LoopVarBind` fact isn't
//! killed at the loop's exit node, so in principle an unrelated variable
//! *reusing* a former loop variable's name later in the same function
//! could still read as "active" here. Low-likelihood in practice (Go
//! style rarely reuses a loop variable's name for an unrelated purpose
//! immediately after the loop), and a false positive there is no worse
//! than the old heuristic's own scoping imprecision — tracked as a
//! possible refinement, not blocking.

use std::collections::HashSet;

use crate::cfg::{Cfg, CfgNode, ClosureKind, NodeId, RhsShape, Stmt};
use crate::lattice::Lattice;
use crate::solver;

#[derive(Debug, Clone, PartialEq)]
struct ActiveLoopVars(HashSet<String>);

impl Lattice for ActiveLoopVars {
    fn bottom() -> Self {
        ActiveLoopVars(HashSet::new())
    }
    fn join(&self, other: &Self) -> Self {
        ActiveLoopVars(self.0.union(&other.0).cloned().collect())
    }
}

fn apply(facts: &ActiveLoopVars, stmt: &Stmt) -> ActiveLoopVars {
    match stmt {
        Stmt::LoopVarBind { vars } => {
            let mut out = facts.0.clone();
            out.extend(vars.iter().cloned());
            ActiveLoopVars(out)
        }
        // A self-shadow copy (`item := item`) directly in the loop body —
        // not just inside a nested closure — is also a valid fix for the
        // address-of case (it gives `item` a fresh per-iteration binding
        // before `&item` is taken). Remove it from the active set so a
        // subsequent `&item` in the same scope isn't flagged.
        Stmt::Assign { lhs, rhs: RhsShape::Var(rhs) } if lhs == rhs && facts.0.contains(lhs) => {
            let mut out = facts.0.clone();
            out.remove(lhs);
            ActiveLoopVars(out)
        }
        _ => facts.clone(),
    }
}

fn solve_active_vars(cfg: &Cfg<Stmt>) -> Vec<ActiveLoopVars> {
    solver::solve(cfg, |_id, node: &CfgNode<Stmt>, in_fact: &ActiveLoopVars| node.stmts.iter().fold(in_fact.clone(), |facts, stmt| apply(&facts, stmt)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopVarCaptureHit {
    pub var: String,
    pub kind: ClosureKind,
    pub node: NodeId,
    pub source_line: u32,
}

/// `go func() { }` / `defer func() { }` capturing an active loop
/// variable by reference, without shadowing it first.
pub fn check_capture(cfg: &Cfg<Stmt>) -> Vec<LoopVarCaptureHit> {
    let out_facts = solve_active_vars(cfg);
    let mut hits = Vec::new();
    for (node_id, node) in cfg.nodes.iter().enumerate() {
        let preds = cfg.predecessors(node_id);
        let mut facts = preds.iter().fold(ActiveLoopVars::bottom(), |acc, &p| acc.join(&out_facts[p]));
        for stmt in &node.stmts {
            if let Stmt::ClosureCapture { captured, kind } = stmt {
                if let Some(var) = captured.iter().find(|v| facts.0.contains(*v)) {
                    hits.push(LoopVarCaptureHit { var: var.clone(), kind: *kind, node: node_id, source_line: node.source_line });
                }
            }
            facts = apply(&facts, stmt);
        }
    }
    hits
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopVarAddressHit {
    pub var: String,
    pub node: NodeId,
    pub source_line: u32,
}

/// `&loopVar` — an address-of expression referencing an active loop
/// variable, found either as a direct assignment (`p := &loopVar`) or as
/// a call argument (`append(out, &loopVar)`, encoded as `"&loopVar"` in
/// `Stmt::Call`'s `args` — see `lower::go::call_arg_identifiers`).
pub fn check_address(cfg: &Cfg<Stmt>) -> Vec<LoopVarAddressHit> {
    let out_facts = solve_active_vars(cfg);
    let mut hits = Vec::new();
    for (node_id, node) in cfg.nodes.iter().enumerate() {
        let preds = cfg.predecessors(node_id);
        let mut facts = preds.iter().fold(ActiveLoopVars::bottom(), |acc, &p| acc.join(&out_facts[p]));
        for stmt in &node.stmts {
            let found = match stmt {
                Stmt::Assign { rhs: RhsShape::AddressOf { of }, .. } if facts.0.contains(of) => Some(of.clone()),
                Stmt::Call { args, .. } => args.iter().find_map(|a| a.strip_prefix('&').filter(|v| facts.0.contains(*v)).map(|v| v.to_string())),
                _ => None,
            };
            if let Some(var) = found {
                hits.push(LoopVarAddressHit { var, node: node_id, source_line: node.source_line });
            }
            facts = apply(&facts, stmt);
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::go::lower_function;

    fn lower(source: &str) -> Cfg<Stmt> {
        let mut parser = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Go).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let mut cursor = root.walk();
        let fn_node = root.named_children(&mut cursor).find(|n| n.kind() == "function_declaration").expect("no function_declaration found");
        lower_function(source.as_bytes(), fn_node)
    }

    #[test]
    fn flags_a_goroutine_capturing_the_range_loop_variable() {
        let cfg = lower("package p\nfunc f(items []string) {\n\tfor _, item := range items {\n\t\tgo func() {\n\t\t\tprintln(item)\n\t\t}()\n\t}\n}\n");
        let hits = check_capture(&cfg);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
        assert_eq!(hits[0].var, "item");
        assert_eq!(hits[0].kind, ClosureKind::Goroutine);
    }

    #[test]
    fn does_not_flag_a_shadowed_goroutine() {
        let cfg = lower("package p\nfunc f(items []string) {\n\tfor _, item := range items {\n\t\tgo func() {\n\t\t\titem := item\n\t\t\tprintln(item)\n\t\t}()\n\t}\n}\n");
        assert!(check_capture(&cfg).is_empty());
    }

    #[test]
    fn does_not_flag_a_goroutine_passed_the_variable_as_a_parameter() {
        let cfg = lower("package p\nfunc f(items []string) {\n\tfor _, item := range items {\n\t\tgo func(x string) {\n\t\t\tprintln(x)\n\t\t}(item)\n\t}\n}\n");
        assert!(check_capture(&cfg).is_empty());
    }

    #[test]
    fn does_not_flag_a_closure_outside_any_loop() {
        let cfg = lower("package p\nfunc f() {\n\tx := 1\n\tgo func() {\n\t\tprintln(x)\n\t}()\n}\n");
        assert!(check_capture(&cfg).is_empty());
    }

    #[test]
    fn flags_a_deferred_closure_capturing_the_loop_variable() {
        let cfg = lower("package p\nfunc f(items []string) {\n\tfor _, item := range items {\n\t\tdefer func() {\n\t\t\tprintln(item)\n\t\t}()\n\t}\n}\n");
        let hits = check_capture(&cfg);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
        assert_eq!(hits[0].kind, ClosureKind::Deferred);
    }

    #[test]
    fn does_not_flag_a_plain_deferred_call_arguments_are_evaluated_immediately() {
        // `defer println(item)` (no func literal) evaluates `item` right
        // away — Go's own by-value defer-argument semantics make this
        // safe, unlike `defer func() { println(item) }()`.
        let cfg = lower("package p\nfunc f(items []string) {\n\tfor _, item := range items {\n\t\tdefer println(item)\n\t}\n}\n");
        assert!(check_capture(&cfg).is_empty());
    }

    #[test]
    fn flags_the_address_of_a_loop_variable_appended_to_a_slice() {
        let cfg = lower("package p\nfunc f(items []string) []*string {\n\tvar out []*string\n\tfor _, item := range items {\n\t\tout = append(out, &item)\n\t}\n\treturn out\n}\n");
        let hits = check_address(&cfg);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
        assert_eq!(hits[0].var, "item");
    }

    #[test]
    fn flags_a_direct_address_of_assignment() {
        let cfg = lower("package p\nfunc f(items []string) {\n\tfor _, item := range items {\n\t\tp := &item\n\t\t_ = p\n\t}\n}\n");
        let hits = check_address(&cfg);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
    }

    #[test]
    fn does_not_flag_a_shadowed_address_of_a_loop_variable() {
        let cfg = lower("package p\nfunc f(items []string) []*string {\n\tvar out []*string\n\tfor _, item := range items {\n\t\titem := item\n\t\tout = append(out, &item)\n\t}\n\treturn out\n}\n");
        assert!(check_address(&cfg).is_empty(), "got: {:#?}", check_address(&cfg));
    }
}

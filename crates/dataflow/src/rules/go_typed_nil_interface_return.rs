//! Real-dataflow rewrite of `typed-nil-interface-return` (previously a
//! same-function-only text heuristic in `autoreview-core`'s
//! `practices.rs`). This is the first genuinely interprocedural rule in
//! the dataflow crate — see the crate-level docs (`lib.rs`) for what
//! "interprocedural" is scoped to mean here: one hop of same-file
//! call-target resolution propagating a coarse per-function summary, not
//! a full recursive fixpoint.
//!
//! Two-pass design:
//! 1. **Summarize** every pointer-returning function in the file with
//!    [`compute_summary`]: does it have a path from entry to `return`
//!    that returns a variable still tracked as a nil literal (a bare
//!    `var e *T` with no initializer that's never proven non-nil on that
//!    path)? Calls inside *this* pass are treated as unknown/not-nil —
//!    deliberately not recursive, to keep the analysis a single, bounded
//!    pass per the "one hop" scope.
//! 2. **Check** every function declaring an `error` return with
//!    [`check`], using the *same* nil-tracking lattice but this time
//!    resolving calls against the summaries computed in pass 1 — so a
//!    `return e` is flagged whether `e` came from a local `var e *T` (the
//!    old heuristic's only case) or from `e := helper()` where `helper`'s
//!    own summary says it may return a nil pointer (the new,
//!    interprocedural case the old heuristic structurally couldn't see).
//!
//! Fixes the documented false negative: `func helper() *MyErr {...}`
//! called as `func Do() error { e := helper(); return e }` is now caught.
//! An unresolved call (a method call, a call to a function not in this
//! file) stays an unknown boundary and is never flagged — precision over
//! recall, matching this project's stance throughout.

use std::collections::HashMap;

use crate::cfg::{Cfg, CfgNode, GuardAgainst, GuardOp, NodeId, RhsShape, Stmt};
use crate::lattice::Lattice;
use crate::solver;

#[derive(Debug, Clone, PartialEq)]
struct NilFacts(HashMap<String, bool>);

impl Lattice for NilFacts {
    fn bottom() -> Self {
        NilFacts(HashMap::new())
    }
    fn join(&self, other: &Self) -> Self {
        let mut out = self.0.clone();
        for (k, v) in &other.0 {
            let entry = out.entry(k.clone()).or_insert(false);
            *entry = *entry || *v;
        }
        NilFacts(out)
    }
}

/// `resolve_call` decides what a call assigned to a variable does to that
/// variable's nil-ness: `None` (pass 1, summary computation — calls are
/// always an unknown/not-nil boundary) or `Some(&summaries)` (pass 2, the
/// real check — resolve one hop against already-computed summaries).
fn apply(facts: &NilFacts, stmt: &Stmt, summaries: Option<&HashMap<String, bool>>) -> NilFacts {
    let mut out = facts.0.clone();
    match stmt {
        Stmt::Assign { lhs, rhs: RhsShape::NilLiteral } => {
            out.insert(lhs.clone(), true);
        }
        Stmt::Assign { lhs, rhs: RhsShape::Var(v) } => {
            let val = out.get(v).copied().unwrap_or(false);
            out.insert(lhs.clone(), val);
        }
        Stmt::Assign { lhs, .. } => {
            out.insert(lhs.clone(), false);
        }
        Stmt::Call { target: crate::cfg::CallTarget::Named(name), assigned_to: Some(v), .. } => {
            let risky = summaries.and_then(|s| s.get(name)).copied().unwrap_or(false);
            out.insert(v.clone(), risky);
        }
        Stmt::Call { assigned_to: Some(v), .. } => {
            out.insert(v.clone(), false);
        }
        Stmt::Guard { var, op: GuardOp::NotEqual, against: GuardAgainst::Nil } => {
            out.insert(var.clone(), false);
        }
        Stmt::Guard { var, op: GuardOp::Equal, against: GuardAgainst::Nil } => {
            out.insert(var.clone(), true);
        }
        _ => {}
    }
    NilFacts(out)
}

fn solve_and_walk(cfg: &Cfg<Stmt>, summaries: Option<&HashMap<String, bool>>) -> Vec<(NodeId, usize, String)> {
    // Returns every `(node, stmt_index, var)` at which a `Return { value:
    // Some(var) }` is reached with `var` currently tracked as nil-possible.
    let out_facts = solver::solve(cfg, |_id, node: &CfgNode<Stmt>, in_fact: &NilFacts| node.stmts.iter().fold(in_fact.clone(), |acc, stmt| apply(&acc, stmt, summaries)));

    let mut hits = Vec::new();
    for (node_id, node) in cfg.nodes.iter().enumerate() {
        let preds = cfg.predecessors(node_id);
        let mut facts = preds.iter().fold(NilFacts::bottom(), |acc, &p| acc.join(&out_facts[p]));
        for (stmt_idx, stmt) in node.stmts.iter().enumerate() {
            if let Stmt::Return { value: Some(v) } = stmt {
                if facts.0.get(v).copied().unwrap_or(false) {
                    hits.push((node_id, stmt_idx, v.clone()));
                }
            }
            facts = apply(&facts, stmt, summaries);
        }
    }
    hits
}

/// Pass 1: does this function have a path that returns a variable still
/// tracked as possibly nil? Used to summarize pointer-returning helper
/// functions for pass 2's interprocedural lookup.
pub fn compute_summary(cfg: &Cfg<Stmt>) -> bool {
    !solve_and_walk(cfg, None).is_empty()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedNilInterfaceReturnHit {
    pub var: String,
    pub node: NodeId,
    pub source_line: u32,
}

/// Pass 2: the real check, run against a function that declares an
/// `error` return. `summaries` should map function name → `may return a
/// nil typed pointer`, from [`compute_summary`] over every other
/// same-file function.
pub fn check(cfg: &Cfg<Stmt>, summaries: &HashMap<String, bool>) -> Vec<TypedNilInterfaceReturnHit> {
    solve_and_walk(cfg, Some(summaries)).into_iter().map(|(node, _stmt_idx, var)| TypedNilInterfaceReturnHit { var, node, source_line: cfg.nodes[node].source_line }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::go::lower_function;

    fn lower(source: &str, fn_name: &str) -> Cfg<Stmt> {
        let mut parser = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Go).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let mut cursor = root.walk();
        let fn_node = root
            .named_children(&mut cursor)
            .find(|n| n.kind() == "function_declaration" && n.child_by_field_name("name").is_some_and(|name| name.utf8_text(source.as_bytes()).unwrap() == fn_name))
            .unwrap_or_else(|| panic!("no function named {fn_name} found"));
        lower_function(source.as_bytes(), fn_node)
    }

    #[test]
    fn flags_the_same_function_case_a_local_nil_pointer_returned_as_error() {
        let src = "package p\nfunc do() error {\n\tvar e *myError\n\tif somethingBad {\n\t\te = &myError{}\n\t}\n\treturn e\n}\n";
        let cfg = lower(src, "do");
        let hits = check(&cfg, &HashMap::new());
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
        assert_eq!(hits[0].var, "e");
    }

    #[test]
    fn does_not_flag_the_guarded_nil_check_idiom() {
        let src = "package p\nfunc do() error {\n\tvar e *myError\n\tif somethingBad {\n\t\te = &myError{}\n\t}\n\tif e != nil {\n\t\treturn e\n\t}\n\treturn nil\n}\n";
        let cfg = lower(src, "do");
        let hits = check(&cfg, &HashMap::new());
        assert!(hits.is_empty(), "got: {hits:#?}");
    }

    #[test]
    fn flags_the_interprocedural_case_a_risky_helper_returned_as_error() {
        let src = "package p\nfunc helper() *myError {\n\tvar e *myError\n\treturn e\n}\nfunc Do() error {\n\te := helper()\n\treturn e\n}\n";
        let helper_cfg = lower(src, "helper");
        assert!(compute_summary(&helper_cfg), "helper's own summary should be risky");

        let mut summaries = HashMap::new();
        summaries.insert("helper".to_string(), true);
        let do_cfg = lower(src, "Do");
        let hits = check(&do_cfg, &summaries);
        assert_eq!(hits.len(), 1, "got: {hits:#?} — same-function heuristic structurally couldn't see this");
        assert_eq!(hits[0].var, "e");
    }

    #[test]
    fn does_not_flag_a_call_to_an_unresolved_unknown_function() {
        // `helper` isn't in `summaries` at all (e.g. defined in another
        // file/package this pass doesn't see) — must not flag, per the
        // "unknown boundary" precision stance.
        let src = "package p\nfunc Do() error {\n\te := helper()\n\treturn e\n}\n";
        let cfg = lower(src, "Do");
        let hits = check(&cfg, &HashMap::new());
        assert!(hits.is_empty(), "got: {hits:#?}");
    }

    #[test]
    fn does_not_flag_a_helper_whose_own_summary_is_safe() {
        let src = "package p\nfunc helper() *myError {\n\treturn &myError{}\n}\nfunc Do() error {\n\te := helper()\n\treturn e\n}\n";
        let helper_cfg = lower(src, "helper");
        assert!(!compute_summary(&helper_cfg));

        let mut summaries = HashMap::new();
        summaries.insert("helper".to_string(), false);
        let do_cfg = lower(src, "Do");
        let hits = check(&do_cfg, &summaries);
        assert!(hits.is_empty(), "got: {hits:#?}");
    }
}

//! A generic, declarative taint-tracking engine — the same shape as
//! Semgrep's free-tier `mode: taint` (sources/sinks/sanitizers,
//! auto-propagating through assignments and calls), reimplemented on
//! this crate's own CFG/solver rather than ported code. Deliberately
//! scoped to match what Semgrep's own OSS taint mode does: **strictly
//! intraprocedural** (single function, no cross-function/cross-file
//! taint tracking — Semgrep itself Pro-gates that, and it's resource-
//! heavy even there).
//!
//! Unlike the earlier dataflow rules (`append-shared-backing-array`,
//! `typed-nil-interface-return`, `go_loopvar`), which are each their own
//! hand-written lattice + transfer function, taint rules are
//! **declarative**: a [`TaintSpec`] is a plain data table (source/sink/
//! sanitizer name patterns), and this module's generic [`check`]
//! function runs it. Adding a new taint rule for a new sink/source
//! family (e.g. a future Java SQL-injection rule) means adding a new
//! `TaintSpec` value, not new lattice/solver code — the same tradeoff
//! ast-grep's YAML rules made over hand-rolled Rust analyzers, applied
//! here to the dataflow side.
//!
//! Matching is syntactic (name-based), same precision level as Semgrep's
//! own pattern-based sink/source matching — there's no type resolution
//! telling us `r`'s static type is `*http.Request`, so a source pattern
//! like `"FormValue"` matches any call whose qualified name ends in
//! `.FormValue` (see [`NamePattern`]), not specifically
//! `http.Request.FormValue`. This is the same tradeoff Semgrep's own
//! syntactic sink/source patterns make.

use std::collections::HashMap;
use std::sync::Arc;

use regex::Regex;

use crate::cfg::{Cfg, CfgNode, NodeId, Stmt};
use crate::lattice::Lattice;
use crate::solver;

/// A name pattern for matching a `Stmt::Call`'s target name (as lowered
/// by e.g. `lower::go::call_target_name`: `"name"` for a bare call,
/// `"operand.field"` for a selector call). Owned (not `&'static str`)
/// since specs are now built at runtime from YAML rule files, not written
/// as Rust consts.
#[derive(Debug, Clone)]
pub enum NamePattern {
    /// Exact-or-trailing-`.method` match — i.e. `Suffix("FormValue")`
    /// matches both a bare `FormValue(...)` and any `X.FormValue(...)`,
    /// regardless of what `X` is (no type resolution available). The
    /// original (and still default) matcher.
    Suffix(String),
    /// A regex tested against the call's full lowered name — added for
    /// sink/source families a suffix match can't express in one rule
    /// (e.g. "any `db.Query*` variant"), matching how frequently
    /// Semgrep's own sink/source patterns are regex-shaped.
    Regex(Arc<Regex>),
}

impl NamePattern {
    pub fn suffix(name: impl Into<String>) -> Self {
        NamePattern::Suffix(name.into())
    }

    pub fn regex(pattern: &str) -> Result<Self, regex::Error> {
        Ok(NamePattern::Regex(Arc::new(Regex::new(pattern)?)))
    }

    fn matches(&self, call_name: &str) -> bool {
        match self {
            NamePattern::Suffix(name) => call_name == name || call_name.strip_suffix(name.as_str()).is_some_and(|prefix| prefix.ends_with('.')),
            NamePattern::Regex(re) => re.is_match(call_name),
        }
    }
}

/// A sink's dangerous argument positions — `None` means any argument
/// reaching this call is dangerous (matches Semgrep's default `...`
/// sink-argument behavior); `Some(positions)` restricts the check to
/// specific zero-indexed argument slots.
#[derive(Debug, Clone)]
pub struct TaintSink {
    pub call: NamePattern,
    pub tainted_arg_positions: Option<Vec<usize>>,
}

#[derive(Debug, Clone)]
pub struct TaintSpec {
    pub rule_id: String,
    /// A call matching one of these makes its assigned variable tainted
    /// (Semgrep's `pattern-sources`).
    pub sources: Vec<NamePattern>,
    /// A call matching one of these, reached with a tainted argument in
    /// a dangerous position, is a hit (Semgrep's `pattern-sinks`).
    pub sinks: Vec<TaintSink>,
    /// A call matching one of these clears taint on its assigned
    /// variable (Semgrep's `pattern-sanitizers`) — note this only
    /// sanitizes the *assigned* variable, not variables passed as
    /// arguments; matches this crate's "sanitizer wraps and reassigns"
    /// idiom (`clean := sanitize(dirty)`), not an in-place mutation.
    pub sanitizers: Vec<NamePattern>,
}

#[derive(Debug, Clone, PartialEq)]
struct TaintFacts(HashMap<String, bool>);

impl Lattice for TaintFacts {
    fn bottom() -> Self {
        TaintFacts(HashMap::new())
    }
    fn join(&self, other: &Self) -> Self {
        let mut out = self.0.clone();
        for (k, v) in &other.0 {
            let entry = out.entry(k.clone()).or_insert(false);
            *entry = *entry || *v;
        }
        TaintFacts(out)
    }
}

fn is_tainted(facts: &TaintFacts, var: &str) -> bool {
    facts.0.get(var).copied().unwrap_or(false)
}

/// `summaries`, when `Some`, maps a same-file/same-package/imported-
/// package function's bare name to "does it have a path where a taint
/// source reaches its own return value" (see [`compute_source_summary`]
/// below) — rule engine roadmap item 6 (`RULE_ENGINE_RESEARCH.md`): the
/// existing "unrecognized call auto-propagates taint from a tainted
/// argument" default only ever sees taint that's already visible in
/// *this* function's own facts. It structurally cannot see a source
/// hidden entirely inside a niladic (or argument-independent) helper —
/// `func getUserID() string { return req.FormValue("id") }` called as
/// `id := getUserID()` has no tainted argument for the default rule to
/// notice at all. `summaries` closes exactly that gap, the same "one
/// hop of same-file/same-package/cross-package resolution propagating a
/// coarse per-function summary" scope every other interprocedural rule
/// in this crate already uses — not a general interprocedural fixpoint.
/// `None` preserves this function's prior behavior exactly (existing
/// intraprocedural-only callers are unaffected).
fn apply(spec: &TaintSpec, facts: &TaintFacts, stmt: &Stmt, summaries: Option<&HashMap<String, bool>>) -> TaintFacts {
    let mut out = facts.0.clone();
    match stmt {
        Stmt::Call { target: crate::cfg::CallTarget::Named(name), args, assigned_to: Some(v) } => {
            if spec.sanitizers.iter().any(|p| p.matches(name)) {
                out.insert(v.clone(), false);
            } else if spec.sources.iter().any(|p| p.matches(name)) {
                out.insert(v.clone(), true);
            } else {
                // Default auto-propagation: taint flows through an
                // unrecognized call if any of its (identifier) arguments
                // are already tainted — matches Semgrep's own default
                // taint-mode propagation through function calls. ORed
                // with the interprocedural summary lookup: either one
                // alone is enough to taint the result.
                let propagated_from_args = args.iter().any(|a| out.get(a.trim_start_matches('&')).copied().unwrap_or(false));
                let propagated_from_summary = summaries.and_then(|s| s.get(name)).copied().unwrap_or(false);
                out.insert(v.clone(), propagated_from_args || propagated_from_summary);
            }
        }
        Stmt::Assign { lhs, rhs: crate::cfg::RhsShape::Var(rhs) } => {
            let val = out.get(rhs).copied().unwrap_or(false);
            out.insert(lhs.clone(), val);
        }
        Stmt::Assign { lhs, rhs: crate::cfg::RhsShape::Concat { parts } } => {
            // A query/path/command string built by concatenating a
            // tainted part (`"SELECT ... " + userInput`) is itself
            // tainted — the classic indirect-injection shape none of
            // the direct source-to-sink rules above would catch on
            // their own.
            let tainted = parts.iter().any(|p| out.get(p).copied().unwrap_or(false));
            out.insert(lhs.clone(), tainted);
        }
        Stmt::Assign { lhs, .. } => {
            out.insert(lhs.clone(), false);
        }
        _ => {}
    }
    TaintFacts(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaintHit {
    pub sink_call: String,
    pub tainted_arg: String,
    pub node: NodeId,
    pub source_line: u32,
}

/// Runs `spec` against one function's already-lowered CFG, purely
/// intraprocedurally (`summaries: None` — see [`check_with_summaries`]
/// for the interprocedural variant). Kept as its own entry point rather
/// than a thin `check_with_summaries(spec, cfg, None)` wrapper only for
/// call-site brevity — every existing caller stays unchanged.
pub fn check(spec: &TaintSpec, cfg: &Cfg<Stmt>) -> Vec<TaintHit> {
    check_with_summaries(spec, cfg, None)
}

/// Does this function have a path from entry to `return` where a taint
/// source reaches the returned value? Used to summarize same-file/same-
/// package/imported-package helper functions for
/// [`check_with_summaries`]'s interprocedural lookup — same "compute a
/// per-function summary once, consult it via the same lattice machinery"
/// shape as `go_typed_nil_interface_return::compute_summary` and
/// `java_kotlin_npe_risk::compute_summary`.
pub fn compute_source_summary(spec: &TaintSpec, cfg: &Cfg<Stmt>) -> bool {
    let out_facts = solver::solve(cfg, |_id, node: &CfgNode<Stmt>, in_fact: &TaintFacts| node.stmts.iter().fold(in_fact.clone(), |facts, stmt| apply(spec, &facts, stmt, None)));

    for (node_id, node) in cfg.nodes.iter().enumerate() {
        let preds = cfg.predecessors(node_id);
        let mut facts = preds.iter().fold(TaintFacts::bottom(), |acc, &p| acc.join(&out_facts[p]));
        for stmt in &node.stmts {
            if let Stmt::Return { value: Some(v) } = stmt {
                if is_tainted(&facts, v) {
                    return true;
                }
            }
            facts = apply(spec, &facts, stmt, None);
        }
    }
    false
}

/// Runs `spec` against one function's already-lowered CFG, resolving an
/// unrecognized call's interprocedural taint against `summaries` (see
/// `apply`'s own doc comment on exactly what gap this closes and how it
/// stays scoped).
pub fn check_with_summaries(spec: &TaintSpec, cfg: &Cfg<Stmt>, summaries: Option<&HashMap<String, bool>>) -> Vec<TaintHit> {
    let out_facts = solver::solve(cfg, |_id, node: &CfgNode<Stmt>, in_fact: &TaintFacts| node.stmts.iter().fold(in_fact.clone(), |facts, stmt| apply(spec, &facts, stmt, summaries)));

    let mut hits = Vec::new();
    for (node_id, node) in cfg.nodes.iter().enumerate() {
        let preds = cfg.predecessors(node_id);
        let mut facts = preds.iter().fold(TaintFacts::bottom(), |acc, &p| acc.join(&out_facts[p]));
        for stmt in &node.stmts {
            if let Stmt::Call { target: crate::cfg::CallTarget::Named(name), args, .. } = stmt {
                // Every matching sink is checked, not just the first — a
                // rule authoring two sink entries for the same call name
                // (e.g. one general, one position-restricted) would
                // otherwise silently only enforce the first one.
                for sink in spec.sinks.iter().filter(|s| s.call.matches(name)) {
                    let candidates: Vec<(usize, &String)> = args.iter().enumerate().collect();
                    let dangerous = candidates.into_iter().filter(|(idx, _)| sink.tainted_arg_positions.as_ref().is_none_or(|positions| positions.contains(idx)));
                    for (_, arg) in dangerous {
                        let var = arg.trim_start_matches('&');
                        if is_tainted(&facts, var) {
                            hits.push(TaintHit { sink_call: name.clone(), tainted_arg: var.to_string(), node: node_id, source_line: node.source_line });
                        }
                    }
                }
            }
            facts = apply(spec, &facts, stmt, summaries);
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> TaintSpec {
        TaintSpec {
            rule_id: "test-taint".to_string(),
            sources: vec![NamePattern::suffix("FormValue")],
            sinks: vec![TaintSink { call: NamePattern::suffix("exec.Command"), tainted_arg_positions: None }],
            sanitizers: vec![NamePattern::suffix("sanitize")],
        }
    }

    fn lower(source: &str) -> Cfg<Stmt> {
        let mut parser = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Go).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let mut cursor = root.walk();
        let fn_node = root.named_children(&mut cursor).find(|n| n.kind() == "function_declaration").expect("no function_declaration found");
        crate::lower::go::lower_function(source.as_bytes(), fn_node)
    }

    #[test]
    fn flags_a_source_reaching_a_sink_directly() {
        let cfg = lower("package p\nfunc f(r *Req) {\n\tcmd := r.FormValue(\"cmd\")\n\texec.Command(\"sh\", \"-c\", cmd)\n}\n");
        let hits = check(&spec(), &cfg);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
        assert_eq!(hits[0].tainted_arg, "cmd");
    }

    #[test]
    fn flags_taint_propagated_through_an_intermediate_assignment() {
        let cfg = lower("package p\nfunc f(r *Req) {\n\tcmd := r.FormValue(\"cmd\")\n\tuserCmd := cmd\n\texec.Command(\"sh\", \"-c\", userCmd)\n}\n");
        let hits = check(&spec(), &cfg);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
    }

    #[test]
    fn does_not_flag_an_untainted_argument() {
        let cfg = lower("package p\nfunc f() {\n\texec.Command(\"ls\", \"-la\")\n}\n");
        let hits = check(&spec(), &cfg);
        assert!(hits.is_empty(), "got: {hits:#?}");
    }

    #[test]
    fn does_not_flag_after_the_sanitizer_wraps_the_value() {
        let cfg = lower("package p\nfunc f(r *Req) {\n\tcmd := r.FormValue(\"cmd\")\n\tclean := sanitize(cmd)\n\texec.Command(\"sh\", \"-c\", clean)\n}\n");
        let hits = check(&spec(), &cfg);
        assert!(hits.is_empty(), "got: {hits:#?}");
    }

    #[test]
    fn flags_a_source_reaching_an_exec_cmd_struct_literals_path_field() {
        let mut spec = spec();
        spec.sinks.push(TaintSink { call: NamePattern::suffix("exec.Cmd{Path}"), tainted_arg_positions: None });
        let cfg = lower("package p\nfunc f(r *Req) {\n\tcmd := r.FormValue(\"cmd\")\n\tc := &exec.Cmd{Path: cmd}\n\t_ = c\n}\n");
        let hits = check(&spec, &cfg);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
        assert_eq!(hits[0].tainted_arg, "cmd");
        assert_eq!(hits[0].sink_call, "exec.Cmd{Path}");
    }

    #[test]
    fn does_not_flag_an_untainted_exec_cmd_struct_literals_path_field() {
        let mut spec = spec();
        spec.sinks.push(TaintSink { call: NamePattern::suffix("exec.Cmd{Path}"), tainted_arg_positions: None });
        let cfg = lower("package p\nfunc f() {\n\tc := &exec.Cmd{Path: \"/bin/ls\"}\n\t_ = c\n}\n");
        let hits = check(&spec, &cfg);
        assert!(hits.is_empty(), "got: {hits:#?}");
    }

    #[test]
    fn does_not_flag_a_call_matching_neither_source_nor_sink_pattern() {
        let cfg = lower("package p\nfunc f() {\n\tx := os.Getenv(\"PATH\")\n\texec.Command(\"echo\", x)\n}\n");
        // os.Getenv isn't a declared source, and taint-through-call
        // propagation only applies to *unrecognized* calls whose args
        // are already tainted — os.Getenv's result starts untainted, so
        // this correctly stays silent.
        let hits = check(&spec(), &cfg);
        assert!(hits.is_empty(), "got: {hits:#?}");
    }

    #[test]
    fn compute_source_summary_is_true_when_a_source_reaches_the_return() {
        let cfg = lower("package p\nfunc getUserID(r *Req) string {\n\tid := r.FormValue(\"id\")\n\treturn id\n}\n");
        assert!(compute_source_summary(&spec(), &cfg));
    }

    #[test]
    fn compute_source_summary_is_false_when_no_source_reaches_the_return() {
        let cfg = lower("package p\nfunc getUserID() string {\n\treturn \"anonymous\"\n}\n");
        assert!(!compute_source_summary(&spec(), &cfg));
    }

    #[test]
    fn check_with_summaries_flags_a_niladic_call_to_a_summarized_source_wrapper() {
        // The exact gap this closes: getUserID() takes no arguments at
        // all, so the existing "unrecognized call auto-propagates from a
        // tainted argument" default has nothing to look at — only the
        // summary lookup can see that this call's *result* is tainted.
        let cfg = lower("package p\nfunc f() {\n\tid := getUserID()\n\texec.Command(\"sh\", \"-c\", id)\n}\n");
        let mut summaries = HashMap::new();
        summaries.insert("getUserID".to_string(), true);
        let hits = check_with_summaries(&spec(), &cfg, Some(&summaries));
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
        assert_eq!(hits[0].tainted_arg, "id");
    }

    #[test]
    fn check_without_summaries_does_not_flag_the_same_niladic_call() {
        // Regression guard: plain `check` (summaries: None) must keep its
        // exact prior behavior — this is the false negative that
        // motivated check_with_summaries, not something check itself
        // should start catching.
        let cfg = lower("package p\nfunc f() {\n\tid := getUserID()\n\texec.Command(\"sh\", \"-c\", id)\n}\n");
        let hits = check(&spec(), &cfg);
        assert!(hits.is_empty(), "got: {hits:#?}");
    }

    #[test]
    fn check_with_summaries_does_not_flag_a_call_to_an_unsummarized_function() {
        let cfg = lower("package p\nfunc f() {\n\tid := getUserID()\n\texec.Command(\"sh\", \"-c\", id)\n}\n");
        let summaries = HashMap::new();
        let hits = check_with_summaries(&spec(), &cfg, Some(&summaries));
        assert!(hits.is_empty(), "got: {hits:#?} — an empty/missing summaries entry is an unknown boundary, never flagged");
    }

    #[test]
    fn check_with_summaries_still_respects_a_sanitizer_after_a_summarized_source() {
        let cfg = lower("package p\nfunc f() {\n\tid := getUserID()\n\tclean := sanitize(id)\n\texec.Command(\"sh\", \"-c\", clean)\n}\n");
        let mut summaries = HashMap::new();
        summaries.insert("getUserID".to_string(), true);
        let hits = check_with_summaries(&spec(), &cfg, Some(&summaries));
        assert!(hits.is_empty(), "got: {hits:#?}");
    }
}

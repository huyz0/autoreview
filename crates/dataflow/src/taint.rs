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

fn apply(spec: &TaintSpec, facts: &TaintFacts, stmt: &Stmt) -> TaintFacts {
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
                // taint-mode propagation through function calls.
                let propagated = args.iter().any(|a| out.get(a.trim_start_matches('&')).copied().unwrap_or(false));
                out.insert(v.clone(), propagated);
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

/// Runs `spec` against one function's already-lowered CFG.
pub fn check(spec: &TaintSpec, cfg: &Cfg<Stmt>) -> Vec<TaintHit> {
    let out_facts = solver::solve(cfg, |_id, node: &CfgNode<Stmt>, in_fact: &TaintFacts| node.stmts.iter().fold(in_fact.clone(), |facts, stmt| apply(spec, &facts, stmt)));

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
            facts = apply(spec, &facts, stmt);
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
    fn does_not_flag_a_call_matching_neither_source_nor_sink_pattern() {
        let cfg = lower("package p\nfunc f() {\n\tx := os.Getenv(\"PATH\")\n\texec.Command(\"echo\", x)\n}\n");
        // os.Getenv isn't a declared source, and taint-through-call
        // propagation only applies to *unrecognized* calls whose args
        // are already tainted — os.Getenv's result starts untainted, so
        // this correctly stays silent.
        let hits = check(&spec(), &cfg);
        assert!(hits.is_empty(), "got: {hits:#?}");
    }
}

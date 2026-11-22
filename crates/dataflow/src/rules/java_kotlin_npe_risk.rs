//! Interprocedural NPE-risk check, shared by Java and Kotlin: a helper
//! function has a path that returns `null`; a caller assigns the result to
//! a variable and dereferences it (calls a method on it) without a null
//! check in between. This is the Java/Kotlin analog of Go's typed-nil
//! footgun `go_typed_nil_interface_return` already catches — same
//! "one hop of same-file/same-package call-target resolution propagating a
//! coarse per-function summary, not a full recursive fixpoint" scope (see
//! that module's docs), applied to the far more common NPE bug class
//! instead of Go's own interface-nil quirk. Works for both languages
//! unchanged because both lowerers (`lower::java`, `lower::kotlin`) now
//! produce the same `Cfg<Stmt>` shapes this needs: `RhsShape::NilLiteral`
//! for `null`, a `CallTarget::Named("recv.method")` qualified name for a
//! method call with a plain-identifier receiver, and `Stmt::Guard{against:
//! Nil}` for an `x != null`/`x == null` condition.
//!
//! Two-pass design:
//! 1. **Summarize** every function with [`compute_summary`]: does it have
//!    a path from entry to `return` that returns a variable still tracked
//!    as nil (a bare `null` literal, never proven non-null on that path)?
//!    Calls inside *this* pass are an unknown/not-nil boundary — not
//!    recursive, matching Go's rule.
//! 2. **Check** every function with [`check`], using the same nil-tracking
//!    lattice but resolving calls against pass 1's summaries: a variable
//!    assigned from a risky call, then used as a method-call receiver
//!    before any `x != null`/`x == null` guard clears it, is flagged.
//!
//! An unresolved call (a method call, a call to a function this pass
//! doesn't see) stays an unknown boundary and is never flagged —
//! precision over recall, matching this project's stance throughout.
//!
//! **A Kotlin-specific caveat, confirmed via a real end-to-end review
//! during development**: this rule's summary-computation pass only
//! recognizes a variable as nil-tracked when it's assigned a literal
//! `null` (`RhsShape::NilLiteral`), which in real, *compiling* Kotlin can
//! only happen for an explicitly `?`-typed declaration (`val e: String? =
//! null`). Kotlin's own compiler already rejects a bare (non-`?.`/`!!`)
//! method call on such a variable — meaning most of this rule's
//! detectable-in-practice surface for pure Kotlin-to-Kotlin calls is code
//! that wouldn't compile anyway, and so would never reach a real review
//! (`toString()`, the one exception — see `null_safe_methods` below — is
//! genuinely reachable). This isn't a false-positive risk (the check
//! doesn't fire on real, compiling code where it doesn't apply), but it
//! does mean the rule's realistic value for Kotlin is narrower than for
//! Java, where there's no compile-time null-safety at all. The mechanism
//! still has real value for Kotlin: it's built to also catch Java-interop
//! platform types (a Java method's return type with unknown nullability,
//! which Kotlin's compiler does *not* enforce a null-check on) — this
//! lowering doesn't yet distinguish those from an ordinary typed
//! parameter, so that surface isn't covered today, but the underlying
//! two-pass design already generalizes to it without further schema
//! changes once that's worth building.

use std::collections::{HashMap, HashSet};

use crate::cfg::{CallSite, CallTarget, Cfg, CfgNode, GuardAgainst, GuardOp, NodeId, RhsShape, Stmt};
use crate::lattice::Lattice;
use crate::solver;

#[derive(Debug, Clone, Default, PartialEq)]
struct NilFacts {
    possible: HashSet<String>,
    // The intersection of bindings that hold the selected call's result.
    // None is the lattice bottom, or a scan with no selected source.
    source_aliases: Option<HashSet<String>>,
}

impl Lattice for NilFacts {
    fn bottom() -> Self {
        Self::default()
    }

    fn join(&self, other: &Self) -> Self {
        let source_aliases = match (&self.source_aliases, &other.source_aliases) {
            (Some(left), Some(right)) => Some(left.intersection(right).cloned().collect()),
            (left, right) => left.as_ref().or(right.as_ref()).cloned(),
        };
        Self { possible: self.possible.union(&other.possible).cloned().collect(), source_aliases }
    }
}

impl NilFacts {
    fn bind(&mut self, name: &str, possible: bool, aliases_source: bool) {
        set_membership(&mut self.possible, name, possible);
        if let Some(aliases) = &mut self.source_aliases {
            set_membership(aliases, name, aliases_source);
        }
    }

    fn is_source_alias(&self, name: &str) -> bool {
        self.source_aliases.as_ref().is_some_and(|aliases| aliases.contains(name))
    }
}

fn set_membership(names: &mut HashSet<String>, name: &str, included: bool) {
    if included {
        names.insert(name.to_owned());
    } else {
        names.remove(name);
    }
}

type ResolveCall<'a> = dyn Fn(&str, Option<&CallSite>) -> bool + 'a;

struct NullSources<'a> {
    call: &'a ResolveCall<'a>,
    include_literals: bool,
}

fn apply(facts: &NilFacts, stmt: &Stmt, sources: &NullSources<'_>) -> NilFacts {
    let mut out = facts.clone();
    match stmt {
        Stmt::Assign { lhs, rhs } => {
            let (possible, aliases_source) = match rhs {
                RhsShape::NilLiteral => (sources.include_literals, false),
                RhsShape::Var(var) => (facts.possible.contains(var), facts.is_source_alias(var)),
                _ => (false, false),
            };
            out.bind(lhs, possible, aliases_source);
        }
        Stmt::Call { target: CallTarget::Named(name), assigned_to: Some(var), site, .. } => {
            let risky = (sources.call)(name, site.as_deref());
            out.bind(var, risky, risky);
        }
        Stmt::Call { assigned_to: Some(var), .. } => out.bind(var, false, false),
        Stmt::Guard { var, op: GuardOp::NotEqual, against: GuardAgainst::Nil } => {
            if facts.is_source_alias(var) {
                out.possible.clear();
            } else {
                out.possible.remove(var);
            }
        }
        Stmt::Guard { var, op: GuardOp::Equal, against: GuardAgainst::Nil } if sources.include_literals => {
            out.possible.insert(var.clone());
        }
        _ => {}
    }
    out
}

fn solve(cfg: &Cfg<Stmt>, sources: &NullSources<'_>, reachable: &[bool]) -> Vec<NilFacts> {
    solver::solve(cfg, |id, node: &CfgNode<Stmt>, in_fact: &NilFacts| {
        if !reachable[id] {
            return NilFacts::bottom();
        }
        let mut facts = in_fact.clone();
        if id == cfg.entry && !sources.include_literals {
            facts.source_aliases = Some(HashSet::new());
        }
        node.stmts.iter().fold(facts, |acc, stmt| apply(&acc, stmt, sources))
    })
}

fn visit_statements(
    cfg: &Cfg<Stmt>,
    sources: &NullSources<'_>,
    mut visit: impl FnMut(NodeId, usize, &Stmt, &NilFacts),
) {
    let reachable = cfg.reachable();
    let out_facts = solve(cfg, sources, &reachable);
    for (id, node) in cfg.nodes.iter().enumerate().filter(|(id, _)| reachable[*id]) {
        let mut facts = cfg.predecessors(id).iter().fold(NilFacts::bottom(), |acc, &p| acc.join(&out_facts[p]));
        for (index, stmt) in node.stmts.iter().enumerate() {
            visit(id, index, stmt, &facts);
            facts = apply(&facts, stmt, sources);
        }
    }
}

fn risky_use(
    stmt: &Stmt, facts: &NilFacts, null_safe_methods: &[&str], node: NodeId, source_line: u32,
) -> Option<NpeRiskHit> {
    let Stmt::Call { target: CallTarget::Named(name), site, .. } = stmt else { return None };
    let (receiver, method) = name.split_once('.')?;
    (facts.possible.contains(receiver) && !null_safe_methods.contains(&method)).then(|| NpeRiskHit {
        var: receiver.to_owned(),
        call_name: name.clone(),
        node,
        source_line,
        use_span: site.as_ref().map(|site| (site.start, site.end)),
    })
}

fn walk_risky_dereferences(cfg: &Cfg<Stmt>, sources: &NullSources<'_>, null_safe_methods: &[&str]) -> Vec<NpeRiskHit> {
    let mut hits = Vec::new();
    visit_statements(cfg, sources, |node, index, stmt, facts| {
        hits.extend(risky_use(stmt, facts, null_safe_methods, node, cfg.nodes[node].statement_line(index)));
    });
    hits
}

/// Pass 1: does this function have a path that returns a variable still
/// tracked as possibly null? Used to summarize helper functions for pass
/// 2's interprocedural lookup.
pub fn compute_summary(cfg: &Cfg<Stmt>) -> bool {
    let mut nullable = false;
    let sources = NullSources { call: &|_, _| false, include_literals: true };
    visit_statements(cfg, &sources, |_, _, stmt, facts| {
        if let Stmt::Return { value: Some(var) } = stmt {
            nullable |= facts.possible.contains(var);
        }
    });
    nullable
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpeRiskHit {
    pub var: String,
    pub call_name: String,
    pub node: NodeId,
    pub source_line: u32,
    pub use_span: Option<(usize, usize)>,
}

/// Pass 2: the real check, using `summaries` (from [`compute_summary`] over
/// every other function this pass resolved) to catch a variable assigned
/// from a risky call and then dereferenced, not just an explicit `null`
/// literal dereferenced directly. Pass `null_safe_methods` per
/// empty for Java or `&["toString"]` for Kotlin, where the nullable receiver
/// extension returns the literal string "null".
pub fn check(cfg: &Cfg<Stmt>, summaries: &HashMap<String, bool>, null_safe_methods: &[&str]) -> Vec<NpeRiskHit> {
    let call = |name: &str, _: Option<&CallSite>| summaries.get(name).copied().unwrap_or(false);
    walk_risky_dereferences(cfg, &NullSources { call: &call, include_literals: true }, null_safe_methods)
}

/// Follows null only from the resolved call site. Other nullable values
/// must not make a changed callee look responsible for the dereference.
/// Its call site must execute at most once on a path so all tracked aliases
/// refer to the same result.
pub fn check_call_source(cfg: &Cfg<Stmt>, span: (usize, usize)) -> Vec<NpeRiskHit> {
    let call = |_: &str, site: Option<&CallSite>| site.is_some_and(|site| (site.start, site.end) == span);
    walk_risky_dereferences(cfg, &NullSources { call: &call, include_literals: false }, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lower_java(source: &str, fn_name: &str) -> Cfg<Stmt> {
        let mut parser = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Java).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        fn find<'a>(node: tree_sitter::Node<'a>, name: &str, source: &[u8]) -> Option<tree_sitter::Node<'a>> {
            if node.kind() == "method_declaration" && node.child_by_field_name("name").is_some_and(|n| n.utf8_text(source).unwrap() == name) {
                return Some(node);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(found) = find(child, name, source) {
                    return Some(found);
                }
            }
            None
        }
        let fn_node = find(root, fn_name, source.as_bytes()).unwrap_or_else(|| panic!("no method named {fn_name} found"));
        crate::lower::java::lower_function(source.as_bytes(), fn_node)
    }

    fn lower_kotlin(source: &str, fn_name: &str) -> Cfg<Stmt> {
        let mut parser = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Kotlin).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        fn find<'a>(node: tree_sitter::Node<'a>, name: &str, source: &[u8]) -> Option<tree_sitter::Node<'a>> {
            if node.kind() == "function_declaration" && node.child_by_field_name("name").is_some_and(|n| n.utf8_text(source).unwrap() == name) {
                return Some(node);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(found) = find(child, name, source) {
                    return Some(found);
                }
            }
            None
        }
        let fn_node = find(root, fn_name, source.as_bytes()).unwrap_or_else(|| panic!("no function named {fn_name} found"));
        crate::lower::kotlin::lower_function(source.as_bytes(), fn_node)
    }

    #[test]
    fn java_flags_a_direct_null_assignment_dereferenced_without_a_check() {
        let src = "class C {\n    void f() {\n        Object e = null;\n        e.toString();\n    }\n}\n";
        let cfg = lower_java(src, "f");
        let hits = check(&cfg, &HashMap::new(), &[]);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
        assert_eq!(hits[0].var, "e");
        assert_eq!(hits[0].call_name, "e.toString");
    }

    #[test]
    fn java_does_not_flag_a_guarded_dereference() {
        let src = "class C {\n    void f() {\n        Object e = null;\n        if (e != null) {\n            e.toString();\n        }\n    }\n}\n";
        let cfg = lower_java(src, "f");
        let hits = check(&cfg, &HashMap::new(), &[]);
        assert!(hits.is_empty(), "got: {hits:#?}");
    }

    #[test]
    fn java_flags_the_interprocedural_case_a_risky_helper_dereferenced() {
        let src = "class C {\n    Object helper() {\n        Object e = null;\n        return e;\n    }\n    void f() {\n        Object e = helper();\n        e.toString();\n    }\n}\n";
        let helper_cfg = lower_java(src, "helper");
        assert!(compute_summary(&helper_cfg), "helper's own summary should be risky");

        let mut summaries = HashMap::new();
        summaries.insert("helper".to_string(), true);
        let f_cfg = lower_java(src, "f");
        let hits = check(&f_cfg, &summaries, &[]);
        assert_eq!(hits.len(), 1, "got: {hits:#?} — same-function heuristic structurally couldn't see this");
        assert_eq!(hits[0].var, "e");
    }

    #[test]
    fn java_does_not_flag_an_unresolved_helper_call() {
        let src = "class C {\n    void f() {\n        Object e = helper();\n        e.toString();\n    }\n}\n";
        let cfg = lower_java(src, "f");
        let hits = check(&cfg, &HashMap::new(), &[]);
        assert!(hits.is_empty(), "got: {hits:#?}");
    }

    #[test]
    fn java_does_not_flag_a_helper_whose_own_summary_is_safe() {
        let src = "class C {\n    Object helper() {\n        return new Object();\n    }\n    void f() {\n        Object e = helper();\n        e.toString();\n    }\n}\n";
        let mut summaries = HashMap::new();
        summaries.insert("helper".to_string(), false);
        let f_cfg = lower_java(src, "f");
        let hits = check(&f_cfg, &summaries, &[]);
        assert!(hits.is_empty(), "got: {hits:#?}");
    }

    #[test]
    fn kotlin_flags_a_direct_null_assignment_dereferenced_without_a_check() {
        // A member *call* (`e.toString()`), not a bare property access
        // (`e.length`) — only a call lowers to `Stmt::Call`, which is what
        // this rule's dereference-walk inspects.
        let src = "class C {\n    fun f() {\n        val e: String? = null\n        e.toString()\n    }\n}\n";
        let cfg = lower_kotlin(src, "f");
        let hits = check(&cfg, &HashMap::new(), &[]);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
        assert_eq!(hits[0].var, "e");
        assert_eq!(hits[0].call_name, "e.toString");
    }

    #[test]
    fn kotlin_does_not_flag_a_guarded_dereference() {
        let src = "class C {\n    fun f() {\n        val e: String? = null\n        if (e != null) {\n            e.toString()\n        }\n    }\n}\n";
        let cfg = lower_kotlin(src, "f");
        let hits = check(&cfg, &HashMap::new(), &[]);
        assert!(hits.is_empty(), "got: {hits:#?}");
    }

    #[test]
    fn kotlin_flags_the_interprocedural_case_a_risky_helper_dereferenced() {
        let src = "class C {\n    fun helper(): String? {\n        val e: String? = null\n        return e\n    }\n    fun f() {\n        val e = helper()\n        e.toString()\n    }\n}\n";
        let helper_cfg = lower_kotlin(src, "helper");
        assert!(compute_summary(&helper_cfg), "helper's own summary should be risky");

        let mut summaries = HashMap::new();
        summaries.insert("helper".to_string(), true);
        let f_cfg = lower_kotlin(src, "f");
        let hits = check(&f_cfg, &summaries, &[]);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
        assert_eq!(hits[0].var, "e");
    }

    #[test]
    fn null_safe_methods_excludes_a_matching_method_but_not_others() {
        // Kotlin's Any?.toString() is a real, unconditional null-safe
        // stdlib extension — a caller passing "toString" here must not
        // flag it, while any other method name on the identical nil-
        // tracked variable still must.
        let src = "class C {\n    fun f() {\n        val e: String? = null\n        e.toString()\n    }\n}\n";
        let cfg = lower_kotlin(src, "f");
        assert!(check(&cfg, &HashMap::new(), &["toString"]).is_empty(), "toString must be excluded when passed in null_safe_methods");
        assert_eq!(check(&cfg, &HashMap::new(), &[]).len(), 1, "toString must still be flagged when null_safe_methods is empty");

        let src2 = "class C {\n    fun f() {\n        val e: String? = null\n        e.length()\n    }\n}\n";
        let cfg2 = lower_kotlin(src2, "f");
        assert_eq!(check(&cfg2, &HashMap::new(), &["toString"]).len(), 1, "a different method name must not be excluded by an unrelated entry");
    }
}

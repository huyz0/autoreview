//! Real-dataflow rewrite of `append-shared-backing-array` (previously a
//! same-function text heuristic in `autoreview-core`'s `practices.rs`).
//!
//! Fixes two concrete gaps the text heuristic had:
//! - **False positive**: the old version associated *any* later
//!   `append(sub, ...)` textually found after a `sub := full[a:b]` line,
//!   even if `sub` had been reassigned to something unrelated in between.
//!   Here, any non-reslice assignment to `sub` kills its `ReslicedFrom`
//!   binding via the lattice's transfer function, so a reassigned `sub`
//!   no longer carries a stale association.
//! - **False negative / precision gap**: the old version checked whether
//!   `full` was textually present *anywhere* in the remaining lines of
//!   the function, with no awareness of branches — it could both miss a
//!   use inside a conditional the line-scan's flat forward walk
//!   mis-scoped, and over-fire on an unrelated shadowed `full` in a
//!   sibling block. Here, "is `full` still used" is a real forward
//!   reachability walk over the CFG's remaining nodes.
//!
//! Known limitation carried over from the CFG's own scope: no
//! interprocedural piece here (this rule doesn't need one — the original
//! heuristic was already same-function-scoped) and no capacity/escape
//! analysis, so this is still a heuristic, just a structurally sounder
//! one. Facts are a MAY-analysis (union across branches via `Binding`'s
//! three-valued join, collapsing to `Ambiguous`/top on any disagreement)
//! — precision-leaning by design: a disagreement between branches means
//! "don't trust this binding," not "assume the riskier one."

use std::collections::{HashMap, HashSet};

use crate::cfg::{Cfg, CfgNode, NodeId, RhsShape, Stmt};
use crate::lattice::Lattice;
use crate::solver;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Binding {
    None,
    Reslice(String),
    /// Disagreement across joined branches, or any other kind of
    /// assignment this rule doesn't specifically track — "don't trust
    /// this variable's binding here."
    Ambiguous,
}

impl Binding {
    fn join(&self, other: &Binding) -> Binding {
        match (self, other) {
            (Binding::None, Binding::None) => Binding::None,
            (Binding::Reslice(a), Binding::Reslice(b)) if a == b => Binding::Reslice(a.clone()),
            _ => Binding::Ambiguous,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Facts(HashMap<String, Binding>);

impl Lattice for Facts {
    fn bottom() -> Self {
        Facts(HashMap::new())
    }
    fn join(&self, other: &Self) -> Self {
        let keys: HashSet<&String> = self.0.keys().chain(other.0.keys()).collect();
        let mut out = HashMap::new();
        for key in keys {
            let a = self.0.get(key).cloned().unwrap_or(Binding::None);
            let b = other.0.get(key).cloned().unwrap_or(Binding::None);
            out.insert(key.clone(), a.join(&b));
        }
        Facts(out)
    }
}

fn apply(facts: &Facts, stmt: &Stmt) -> Facts {
    let mut out = facts.0.clone();
    match stmt {
        Stmt::Assign { lhs, rhs: RhsShape::Reslice { of } } => {
            out.insert(lhs.clone(), Binding::Reslice(of.clone()));
        }
        Stmt::Assign { lhs, .. } => {
            out.insert(lhs.clone(), Binding::Ambiguous);
        }
        _ => {}
    }
    Facts(out)
}

fn is_self_append(stmt: &Stmt) -> Option<&str> {
    if let Stmt::Call { target: crate::cfg::CallTarget::Named(name), args, assigned_to: Some(v) } = stmt {
        if name == "append" && args.iter().any(|a| a == v) {
            return Some(v.as_str());
        }
    }
    None
}

fn mentions(stmt: &Stmt, var: &str) -> bool {
    match stmt {
        Stmt::Assign { rhs: RhsShape::Var(v), .. } => v == var,
        Stmt::Assign { rhs: RhsShape::Reslice { of }, .. } => of == var,
        Stmt::Return { value: Some(v) } => v == var,
        Stmt::Call { args, .. } => args.iter().any(|a| a == var),
        Stmt::Other(text) => contains_whole_word(text, var),
        _ => false,
    }
}

fn contains_whole_word(haystack: &str, word: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(word) {
        let abs = start + pos;
        let before_ok = abs == 0 || !(bytes[abs - 1] as char).is_alphanumeric() && bytes[abs - 1] != b'_';
        let after = abs + word.len();
        let after_ok = after >= bytes.len() || (!(bytes[after] as char).is_alphanumeric() && bytes[after] != b'_');
        if before_ok && after_ok {
            return true;
        }
        start = abs + word.len();
        if start >= haystack.len() {
            break;
        }
    }
    false
}

/// Is `var` referenced in `node` (from `from_stmt_index` onward) or any
/// node reachable from it? A plain forward BFS/DFS over CFG successors —
/// no liveness/must-reach refinement, matching this rule's MAY-analysis
/// stance throughout.
fn used_after(cfg: &Cfg<Stmt>, node: NodeId, from_stmt_index: usize, var: &str, visited: &mut HashSet<NodeId>) -> bool {
    if !visited.insert(node) {
        return false;
    }
    if cfg.nodes[node].stmts.iter().skip(from_stmt_index).any(|s| mentions(s, var)) {
        return true;
    }
    cfg.successors(node).into_iter().any(|succ| used_after(cfg, succ, 0, var, visited))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendSharedBackingArrayHit {
    pub sub: String,
    pub full: String,
    pub node: NodeId,
    pub source_line: u32,
}

/// Runs the rewritten check against an already-lowered function CFG.
pub fn check(cfg: &Cfg<Stmt>) -> Vec<AppendSharedBackingArrayHit> {
    let out_facts = solver::solve(cfg, |_id, node: &CfgNode<Stmt>, in_fact: &Facts| {
        node.stmts.iter().fold(in_fact.clone(), |acc, stmt| apply(&acc, stmt))
    });

    let mut hits = Vec::new();
    for (node_id, node) in cfg.nodes.iter().enumerate() {
        let preds = cfg.predecessors(node_id);
        let mut facts = preds.iter().fold(Facts::bottom(), |acc, &p| acc.join(&out_facts[p]));

        for (stmt_idx, stmt) in node.stmts.iter().enumerate() {
            if let Some(sub) = is_self_append(stmt) {
                if let Some(Binding::Reslice(full)) = facts.0.get(sub) {
                    let full = full.clone();
                    let mut visited = HashSet::new();
                    if used_after(cfg, node_id, stmt_idx + 1, &full, &mut visited) {
                        hits.push(AppendSharedBackingArrayHit { sub: sub.to_string(), full, node: node_id, source_line: node.source_line });
                    }
                }
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
    fn flags_append_when_full_is_used_afterward() {
        let cfg = lower("package p\nfunc f(full []int) []int {\n\tsub := full[2:4]\n\tsub = append(sub, 9)\n\tprintln(full[0])\n\treturn sub\n}\n");
        let hits = check(&cfg);
        assert_eq!(hits.len(), 1, "got: {hits:#?}");
        assert_eq!(hits[0].full, "full");
        assert_eq!(hits[0].sub, "sub");
    }

    #[test]
    fn does_not_flag_when_full_is_never_used_again() {
        let cfg = lower("package p\nfunc f(full []int) []int {\n\tsub := full[2:4]\n\tsub = append(sub, 9)\n\treturn sub\n}\n");
        let hits = check(&cfg);
        assert!(hits.is_empty(), "got: {hits:#?}");
    }

    #[test]
    fn does_not_flag_after_sub_is_reassigned_to_something_unrelated() {
        // The precision fix: `sub` is reassigned before the append, so the
        // append is no longer associated with the original `full` reslice.
        let cfg = lower(
            "package p\nfunc f(full []int, other []int) []int {\n\tsub := full[2:4]\n\tsub = other\n\tsub = append(sub, 9)\n\tprintln(full[0])\n\treturn sub\n}\n",
        );
        let hits = check(&cfg);
        assert!(hits.is_empty(), "got: {hits:#?}");
    }
}

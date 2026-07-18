//! Turns `autoreview-dataflow`'s CFG-based rule checks into reportable
//! findings — same separation as `symindex_check.rs` vs `autoreview-
//! symindex`: the dataflow crate has no knowledge of the schema's
//! `Finding` type, so that mapping lives here.
//!
//! Phase 3 of the dataflow rollout: Go-only, one rule
//! (`append-shared-backing-array`), a drop-in replacement for the
//! text-heuristic version previously in `analyzers::practices` — same
//! rule id, so this is not new rule surface, just a sounder
//! implementation. `run_practices_check`'s Go branch no longer calls the
//! old `detect_append_shared_backing_array` for this reason.

use std::path::Path;

use autoreview_dataflow::rules::go_append_shared_backing_array;
use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

fn make_finding(rule_id: &str, path: &str, line: u32, title: String, message: String) -> AgentFinding {
    AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-dataflow".to_string(), rule_id: Some(rule_id.to_string()), aspect: None, backend: None },
        category: "style".to_string(),
        severity: Severity::Low,
        confidence: 1.0,
        title,
        message,
        location: Location { path: path.to_string(), range: LocationRange { start_line: line, end_line: Some(line), ..Default::default() }, snippet: String::new(), side: Side::New },
        related_locations: None,
        suggestion: None,
        tags: None,
        meta: None,
        suggested_patch: None,
    }
}

/// Every top-level `func`/method declaration in a parsed Go file.
fn go_functions(tree: &tree_sitter::Tree) -> Vec<tree_sitter::Node<'_>> {
    let mut out = Vec::new();
    let mut cursor = tree.root_node().walk();
    for node in tree.root_node().named_children(&mut cursor) {
        if node.kind() == "function_declaration" || node.kind() == "method_declaration" {
            out.push(node);
        }
    }
    out
}

fn run_append_shared_backing_array(path: &str, content: &str) -> Vec<AgentFinding> {
    let Some(mut parser) = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Go) else { return Vec::new() };
    let Some(tree) = parser.parse(content, None) else { return Vec::new() };
    let mut findings = Vec::new();
    for fn_node in go_functions(&tree) {
        let cfg = autoreview_dataflow::lower::go::lower_function(content.as_bytes(), fn_node);
        for hit in go_append_shared_backing_array::check(&cfg) {
            findings.push(make_finding(
                "append-shared-backing-array",
                path,
                hit.source_line,
                format!("`{} = append({}, ...)` may overwrite `{}`'s backing array", hit.sub, hit.sub, hit.full),
                format!(
                    "`{}` was created by re-slicing `{}` (`{} := {}[...]`), and `{}` is still used later in this function. `append({}, ...)` may reuse `{}`'s shared backing array when there's spare capacity, silently overwriting memory `{}` still references. Use `{} := append([]T{{}}, {}[a:b]...)` (or `copy`) to force a fresh backing array if `{}` needs to stay untouched, or restructure so `{}` doesn't outlive its use.",
                    hit.sub, hit.full, hit.sub, hit.full, hit.full, hit.sub, hit.sub, hit.full, hit.sub, hit.full, hit.full, hit.sub
                ),
            ));
        }
    }
    findings
}

/// Runs all dataflow-powered checks against one changed file's current
/// content. Go-only for now (Phase 3); Java/Kotlin land once their
/// lowering passes do (Phase 5/6).
pub fn run_dataflow_check(repo_root: &Path, changed_files: &[String]) -> Vec<AgentFinding> {
    changed_files
        .iter()
        .filter(|path| path.ends_with(".go"))
        .filter_map(|path| {
            let full_path = repo_root.join(path);
            let content = std::fs::read_to_string(&full_path).ok()?;
            Some(run_append_shared_backing_array(path, &content))
        })
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_append_that_may_overwrite_a_reused_slices_backing_array() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.go"), "package main\n\nfunc f(full []int) []int {\n\tsub := full[2:4]\n\tsub = append(sub, 9)\n\tprintln(full[0])\n\treturn sub\n}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()]);
        assert_eq!(findings.len(), 1, "got: {findings:#?}");
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("append-shared-backing-array"));
    }

    #[test]
    fn does_not_flag_after_sub_is_reassigned() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc f(full []int, other []int) []int {\n\tsub := full[2:4]\n\tsub = other\n\tsub = append(sub, 9)\n\tprintln(full[0])\n\treturn sub\n}\n",
        )
        .unwrap();
        let findings = run_dataflow_check(dir.path(), &["main.go".to_string()]);
        assert!(findings.is_empty(), "got: {findings:#?}");
    }

    #[test]
    fn skips_non_go_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Foo.java"), "class Foo {}\n").unwrap();
        let findings = run_dataflow_check(dir.path(), &["Foo.java".to_string()]);
        assert!(findings.is_empty());
    }
}

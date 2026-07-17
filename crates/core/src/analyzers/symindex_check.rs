//! Turns `autoreview-symindex`'s whole-repo symbol index into reportable
//! findings — the crate itself has no knowledge of the schema's `Finding`
//! type, per its own module docs, so that mapping lives here (same
//! separation as `archgraph_check.rs` vs `autoreview-archgraph`).
//!
//! Phase 3 of the cross-file symbol index plan: wires only Message Chains,
//! deliberately the smallest slice, to validate the whole plumbing
//! (crate -> mapping layer -> Stage 1) before Feature Envy and Data Clumps
//! build on top in later phases, extending this same file.
//!
//! Deliberately diff-relevant, not a whole-repo audit dump: the index is
//! built over the *entire* repo (a chain can't be meaningfully scoped to
//! one file — well, chains actually can be, but the index itself is built
//! whole-repo per the plan's own scoping decision so later phases sharing
//! this same index construction get cross-file visibility), but this only
//! reports a chain anchored in a file the current diff actually touched.

use std::collections::HashSet;
use std::path::Path;

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

/// Fowler's own illustrative example (`a.b().c().d()`) is depth 3 — that's
/// the threshold, not an arbitrarily higher bar, since a shorter chain is
/// often unremarkable delegation.
const MIN_CHAIN_DEPTH: usize = 3;

fn path_str(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Runs the symbol-index queries over the whole repo and reports only
/// results anchored in a file the current diff touched. Returns an empty
/// list (not an error) when the diff touches no `.go`/`.java` file — this
/// is Go/Java-only, per the crate's own scoping (Kotlin's tree-sitter
/// grammar is pinned incompatibly, same constraint already documented in
/// `patch_check.rs`).
pub fn run_symindex_check(repo_root: &Path, changed_files: &[String]) -> Vec<AgentFinding> {
    let relevant_changed: Vec<&String> = changed_files.iter().filter(|f| f.ends_with(".go") || f.ends_with(".java")).collect();
    if relevant_changed.is_empty() {
        return Vec::new();
    }

    let index = autoreview_symindex::build_index(repo_root);
    let chains = autoreview_symindex::find_message_chains(&index, MIN_CHAIN_DEPTH);

    let changed_set: HashSet<&String> = relevant_changed.into_iter().collect();

    chains.into_iter().filter(|c| changed_set.contains(&path_str(&c.file))).map(chain_to_finding).collect()
}

fn chain_to_finding(chain: autoreview_symindex::ChainFinding) -> AgentFinding {
    let path = path_str(&chain.file);
    let chain_text = chain.chain_text.clone();
    AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-symindex".to_string(), rule_id: Some("message-chain".to_string()), aspect: None, backend: None },
        category: "design".to_string(),
        severity: Severity::Low,
        confidence: 1.0,
        title: "Long message chain".to_string(),
        message: format!(
            "`{chain_text}` in `{}.{}` chains {} calls deep off a single expression. Each link couples this code to the internal structure of every intermediate object (Fowler's Message Chains smell / Law of Demeter) — consider adding a method on the immediate collaborator that hides the chain. Heuristic, name-based match — not resolved against imports/types, so a coincidentally similar-looking chain could be a false positive; Kotlin files aren't covered (tree-sitter-kotlin is incompatible with this project's pinned tree-sitter version).",
            chain.owner_type, chain.method, chain.depth
        ),
        location: Location { path, range: LocationRange { start_line: chain.line, ..Default::default() }, snippet: chain_text, side: Side::New },
        related_locations: None,
        suggestion: None,
        tags: None,
        meta: None,
        suggested_patch: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(dir: &Path, rel_path: &str, content: &str) {
        let path = dir.join(rel_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn returns_empty_when_no_relevant_files_changed() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "main.go", "package main\n\nfunc main() {}\n");
        let findings = run_symindex_check(dir.path(), &["README.md".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn reports_a_message_chain_that_touches_a_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "Widget.java",
            "class Widget {\n    Owner owner;\n    String chain() {\n        return owner.getAddress().getCity().toUpperCase();\n    }\n}\n",
        );
        let findings = run_symindex_check(dir.path(), &["Widget.java".to_string()]);
        assert_eq!(findings.len(), 1, "got: {findings:#?}");
        assert_eq!(findings[0].category, "design");
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("message-chain"));
        assert!(findings[0].message.contains("3 calls deep"));
    }

    #[test]
    fn does_not_report_a_chain_the_diff_never_touches() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "Widget.java",
            "class Widget {\n    Owner owner;\n    String chain() {\n        return owner.getAddress().getCity().toUpperCase();\n    }\n}\n",
        );
        write_file(dir.path(), "Unrelated.java", "class Unrelated {\n    void f() {}\n}\n");
        let findings = run_symindex_check(dir.path(), &["Unrelated.java".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_report_a_chain_below_the_depth_threshold() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "Widget.java", "class Widget {\n    Owner owner;\n    String f() {\n        return owner.getAddress().getCity();\n    }\n}\n");
        let findings = run_symindex_check(dir.path(), &["Widget.java".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn reports_a_go_message_chain_that_touches_a_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "widget.go", "package main\n\ntype Widget struct{}\n\nfunc (w *Widget) Chain() string {\n\treturn w.Sub().GetCity().ToUpperCase()\n}\n");
        let findings = run_symindex_check(dir.path(), &["widget.go".to_string()]);
        assert_eq!(findings.len(), 1, "got: {findings:#?}");
    }
}

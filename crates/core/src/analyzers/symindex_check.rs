//! Turns `autoreview-symindex`'s whole-repo symbol index into reportable
//! findings — the crate itself has no knowledge of the schema's `Finding`
//! type, per its own module docs, so that mapping lives here (same
//! separation as `archgraph_check.rs` vs `autoreview-archgraph`).
//!
//! Phase 3 wired Message Chains (the smallest slice, to validate the whole
//! plumbing first); Phase 4 adds Feature Envy on the same shape. Data
//! Clumps lands in a later phase, extending this same file.
//!
//! Deliberately diff-relevant, not a whole-repo audit dump: the index is
//! built over the *entire* repo (per the plan's whole-repo-always scoping
//! decision — Feature Envy inherently needs to see the envied type's own
//! file, not just the envious method's file), but this only reports a
//! result anchored in a file the current diff actually touched.

use std::collections::HashSet;
use std::path::Path;

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};
use autoreview_symindex::SymbolIndex;

/// Fowler's own illustrative example (`a.b().c().d()`) is depth 3 — that's
/// the threshold, not an arbitrarily higher bar, since a shorter chain is
/// often unremarkable delegation.
const MIN_CHAIN_DEPTH: usize = 3;

/// A method needs at least this many accesses to a single foreign type
/// before Feature Envy is even considered — below this, it's ordinary,
/// unremarkable collaboration, not envy.
const MIN_FOREIGN_ACCESSES: usize = 3;
/// ...and that count must exceed the method's own-field access count by at
/// least this much, so a method that's genuinely balanced between its own
/// state and a collaborator's doesn't trip the check.
const FEATURE_ENVY_MARGIN: i64 = 2;

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
    let changed_set: HashSet<&String> = relevant_changed.into_iter().collect();

    let index = autoreview_symindex::build_index(repo_root);

    let chains = autoreview_symindex::find_message_chains(&index, MIN_CHAIN_DEPTH);
    let envy = autoreview_symindex::find_feature_envy(&index, MIN_FOREIGN_ACCESSES, FEATURE_ENVY_MARGIN);

    let mut findings: Vec<AgentFinding> = chains.into_iter().filter(|c| changed_set.contains(&path_str(&c.file))).map(chain_to_finding).collect();
    findings.extend(envy.into_iter().filter(|e| changed_set.contains(&path_str(&e.file))).map(|e| envy_to_finding(&index, e)));
    findings
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

/// Populates `related_locations` with the envied type's own declaration
/// site (when it's found elsewhere in the index) — a deliberate
/// improvement over `archgraph_check.rs`'s own precedent of only naming
/// related locations in message-text prose, since `AgentFinding` already
/// has a structured field for exactly this.
fn envy_to_finding(index: &SymbolIndex, envy: autoreview_symindex::FeatureEnvyFinding) -> AgentFinding {
    let path = path_str(&envy.file);
    let related_locations = index.find_type(&envy.envied_type).map(|t| {
        vec![Location {
            path: path_str(&t.file),
            range: LocationRange { start_line: t.start_line, ..Default::default() },
            snippet: envy.envied_type.clone(),
            side: Side::New,
        }]
    });
    AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-symindex".to_string(), rule_id: Some("feature-envy".to_string()), aspect: None, backend: None },
        category: "design".to_string(),
        severity: Severity::Low,
        confidence: 1.0,
        title: "Possible Feature Envy".to_string(),
        message: format!(
            "`{}.{}` accesses `{}` {} times (via a parameter) versus {} access(es) to its own fields — it may belong on `{}` instead (Fowler's Feature Envy smell). Heuristic, name-based match — parameter types aren't resolved against imports, and only direct parameter accesses are counted (not locals reassigned from a parameter/field), so this may under- or over-count; Kotlin files aren't covered (tree-sitter-kotlin is incompatible with this project's pinned tree-sitter version).",
            envy.owner_type, envy.method, envy.envied_type, envy.envied_access_count, envy.own_access_count, envy.envied_type
        ),
        location: Location { path, range: LocationRange { start_line: envy.line, ..Default::default() }, snippet: format!("{}.{}", envy.owner_type, envy.method), side: Side::New },
        related_locations,
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

    #[test]
    fn reports_a_cross_file_feature_envy_that_touches_a_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "Widget.java",
            "class Widget {\n    int quantity;\n    int total(Customer c) {\n        int a = c.getBalance();\n        int b = c.getFee();\n        int d = c.getTax();\n        return a + b + d;\n    }\n}\n",
        );
        write_file(dir.path(), "Customer.java", "class Customer {\n    int balance;\n}\n");

        let findings = run_symindex_check(dir.path(), &["Widget.java".to_string()]);
        let envy: Vec<_> = findings.iter().filter(|f| f.source.rule_id.as_deref() == Some("feature-envy")).collect();
        assert_eq!(envy.len(), 1, "got: {findings:#?}");
        assert_eq!(envy[0].category, "design");
        assert!(envy[0].message.contains("Customer"));
        // The envied type's own declaration lives in a *different* file
        // than the one the diff touched — proving the index really is
        // built whole-repo, not scoped to changed_files, while the
        // *reporting* still only fires because Widget.java (the envious
        // method's own file) is in the diff.
        let related = envy[0].related_locations.as_ref().expect("expected related_locations to be populated");
        assert_eq!(related[0].path, "Customer.java");
    }

    #[test]
    fn does_not_report_feature_envy_below_the_threshold() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "Widget.java", "class Widget {\n    int total(Customer c) {\n        return c.getBalance();\n    }\n}\n");
        let findings = run_symindex_check(dir.path(), &["Widget.java".to_string()]);
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("feature-envy")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_report_feature_envy_the_diff_never_touches() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "Widget.java",
            "class Widget {\n    int total(Customer c) {\n        int a = c.getBalance();\n        int b = c.getFee();\n        int d = c.getTax();\n        return a + b + d;\n    }\n}\n",
        );
        write_file(dir.path(), "Unrelated.java", "class Unrelated {\n    void f() {}\n}\n");
        let findings = run_symindex_check(dir.path(), &["Unrelated.java".to_string()]);
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("feature-envy")));
    }
}

//! Turns `autoreview-symindex`'s whole-repo symbol index into reportable
//! findings — the crate itself has no knowledge of the schema's `Finding`
//! type, per its own module docs, so that mapping lives here (same
//! separation as `archgraph_check.rs` vs `autoreview-archgraph`).
//!
//! Phase 3 wired Message Chains (the smallest slice, to validate the whole
//! plumbing first); Phase 4 added Feature Envy; Phase 5 adds Data Clumps —
//! all three on the same bail-empty/whole-repo-build/diff-scope-filter
//! shape.
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

/// A recurring parameter group needs at least this many params before it's
/// distinctive enough to be worth flagging (a 1- or 2-param match is too
/// common to be meaningful) and must recur across at least this many
/// distinct methods.
const MIN_CLUMP_LEN: usize = 3;
const MIN_CLUMP_METHODS: usize = 3;

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
    let clumps = autoreview_symindex::find_data_clumps(&index, MIN_CLUMP_LEN, MIN_CLUMP_METHODS, autoreview_symindex::ClumpScope::WholeIndex);

    let mut findings: Vec<AgentFinding> = chains.into_iter().filter(|c| changed_set.contains(&path_str(&c.file))).map(chain_to_finding).collect();
    findings.extend(envy.into_iter().filter(|e| changed_set.contains(&path_str(&e.file))).map(|e| envy_to_finding(&index, e)));
    findings.extend(clumps.iter().filter_map(|c| clump_to_finding(c, &changed_set)));
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

/// A data clump has no single "file" (it spans multiple methods, often
/// multiple files) — anchors the finding at whichever member method
/// actually lives in a changed file (mirroring `archgraph_check.rs`'s own
/// anchor-at-a-changed-file convention for a multi-location result), and
/// lists every other member as a `related_locations` entry. Returns `None`
/// if no member touches a changed file at all (nothing to anchor on).
fn clump_to_finding(clump: &autoreview_symindex::DataClumpFinding, changed_set: &HashSet<&String>) -> Option<AgentFinding> {
    let anchor = clump.methods.iter().find(|m| changed_set.contains(&path_str(&m.file)))?;

    let related_locations: Vec<Location> = clump
        .methods
        .iter()
        .filter(|m| !(m.owner_type == anchor.owner_type && m.method == anchor.method && m.file == anchor.file))
        .map(|m| Location { path: path_str(&m.file), range: LocationRange { start_line: m.line, ..Default::default() }, snippet: format!("{}.{}", m.owner_type, m.method), side: Side::New })
        .collect();

    let signature_text = clump.signature.iter().map(|s| format!("{}: {}", s.name, s.type_text)).collect::<Vec<_>>().join(", ");
    let methods_text = clump.methods.iter().map(|m| format!("{}.{}", m.owner_type, m.method)).collect::<Vec<_>>().join(", ");

    Some(AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-symindex".to_string(), rule_id: Some("data-clump".to_string()), aspect: None, backend: None },
        category: "design".to_string(),
        severity: Severity::Low,
        confidence: 1.0,
        title: "Recurring parameter group (Data Clump)".to_string(),
        message: format!(
            "The parameter group ({signature_text}) appears identically across {} methods: {methods_text}. Consider consolidating these into their own small object (Fowler's Data Clumps smell). Heuristic, name-based match — parameter types aren't resolved against imports, so a coincidentally identical group in genuinely unrelated code could be a false positive; Kotlin files aren't covered (tree-sitter-kotlin is incompatible with this project's pinned tree-sitter version).",
            clump.methods.len()
        ),
        location: Location { path: path_str(&anchor.file), range: LocationRange { start_line: anchor.line, ..Default::default() }, snippet: signature_text, side: Side::New },
        related_locations: if related_locations.is_empty() { None } else { Some(related_locations) },
        suggestion: None,
        tags: None,
        meta: None,
        suggested_patch: None,
    })
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

    #[test]
    fn reports_a_data_clump_recurring_across_three_files_anchored_at_the_changed_one() {
        let dir = tempfile::tempdir().unwrap();
        let sig = "String name, int id, boolean active";
        write_file(dir.path(), "A.java", &format!("class A {{\n    void one({sig}) {{}}\n}}\n"));
        write_file(dir.path(), "B.java", &format!("class B {{\n    void two({sig}) {{}}\n}}\n"));
        write_file(dir.path(), "C.java", &format!("class C {{\n    void three({sig}) {{}}\n}}\n"));

        let findings = run_symindex_check(dir.path(), &["A.java".to_string()]);
        let clumps: Vec<_> = findings.iter().filter(|f| f.source.rule_id.as_deref() == Some("data-clump")).collect();
        assert_eq!(clumps.len(), 1, "got: {findings:#?}");
        assert_eq!(clumps[0].location.path, "A.java");
        let related = clumps[0].related_locations.as_ref().expect("expected related_locations");
        assert_eq!(related.len(), 2, "expected B and C as related locations, got: {related:#?}");
    }

    #[test]
    fn does_not_report_a_data_clump_the_diff_never_touches() {
        let dir = tempfile::tempdir().unwrap();
        let sig = "String name, int id, boolean active";
        write_file(dir.path(), "A.java", &format!("class A {{\n    void one({sig}) {{}}\n}}\n"));
        write_file(dir.path(), "B.java", &format!("class B {{\n    void two({sig}) {{}}\n}}\n"));
        write_file(dir.path(), "C.java", &format!("class C {{\n    void three({sig}) {{}}\n}}\n"));
        write_file(dir.path(), "Unrelated.java", "class Unrelated {\n    void f() {}\n}\n");

        let findings = run_symindex_check(dir.path(), &["Unrelated.java".to_string()]);
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("data-clump")));
    }

    #[test]
    fn does_not_report_a_data_clump_below_the_min_methods_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let sig = "String name, int id, boolean active";
        write_file(dir.path(), "A.java", &format!("class A {{\n    void one({sig}) {{}}\n}}\n"));
        write_file(dir.path(), "B.java", &format!("class B {{\n    void two({sig}) {{}}\n}}\n"));

        let findings = run_symindex_check(dir.path(), &["A.java".to_string()]);
        assert!(findings.iter().all(|f| f.source.rule_id.as_deref() != Some("data-clump")));
    }
}

//! Turns `autoreview-archgraph`'s Go-only import-cycle detection into
//! reportable findings — the crate itself has no knowledge of the schema's
//! `Finding` type, per its own module docs, so that mapping lives here.
//!
//! Deliberately diff-relevant, not a whole-repo audit dump: archgraph
//! builds the graph over the *entire* repo (a cycle can't be seen from one
//! file), but this only reports a cycle when at least one of its packages
//! contains a file the current diff actually touched — otherwise every
//! `autoreview diff` on an unrelated change would repeat the same
//! pre-existing cycles it can't do anything about in this review.

use std::collections::HashSet;
use std::path::Path;

use autoreview_archgraph::{build_go_import_graph, detect_cycles, discover_go_module_path};
use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

fn package_for_file(repo_root: &Path, file: &str, module_path: &str) -> Option<String> {
    let rel = Path::new(file);
    let parent = rel.parent()?;
    let _ = repo_root;
    if parent.as_os_str().is_empty() {
        Some(module_path.to_string())
    } else {
        Some(format!("{module_path}/{}", parent.to_string_lossy().replace('\\', "/")))
    }
}

/// Runs archgraph's cycle detection over the whole repo and reports any
/// cycle that includes a package the current diff touched. Returns an
/// empty list (not an error) when the repo isn't a Go module at all — this
/// is Go-only, per the plan's own scoping.
pub fn run_archgraph_check(repo_root: &Path, changed_files: &[String]) -> Vec<AgentFinding> {
    let Some(module_path) = discover_go_module_path(repo_root) else { return Vec::new() };
    let changed_go_files: Vec<&String> = changed_files.iter().filter(|f| f.ends_with(".go")).collect();
    if changed_go_files.is_empty() {
        return Vec::new();
    }

    let graph = build_go_import_graph(repo_root, &module_path);
    let cycles = detect_cycles(&graph);
    if cycles.is_empty() {
        return Vec::new();
    }

    let changed_packages: HashSet<String> = changed_go_files.iter().filter_map(|f| package_for_file(repo_root, f, &module_path)).collect();

    let mut findings = Vec::new();
    let mut reported: HashSet<Vec<String>> = HashSet::new();

    for cycle in &cycles {
        let cycle_packages: HashSet<&String> = cycle.iter().collect();
        if !changed_packages.iter().any(|p| cycle_packages.contains(p)) {
            continue;
        }
        // Cycles are reported starting from whichever node the DFS visited
        // first, which can differ run to run if edge iteration order
        // changes — normalize by rotating to start at the lexicographically
        // smallest package before dedup-checking, so the same real cycle
        // doesn't get reported twice under two different rotations.
        let mut canonical = cycle[..cycle.len() - 1].to_vec();
        let min_idx = canonical.iter().enumerate().min_by_key(|(_, s)| s.as_str()).map(|(i, _)| i).unwrap_or(0);
        canonical.rotate_left(min_idx);
        if !reported.insert(canonical.clone()) {
            continue;
        }

        // Anchor the finding at one of the actually-changed files that
        // belongs to a package in this cycle.
        let anchor_file = changed_go_files.iter().find(|f| package_for_file(repo_root, f, &module_path).map(|p| cycle_packages.contains(&p)).unwrap_or(false));
        let Some(anchor_file) = anchor_file else { continue };

        let cycle_description = cycle.join(" -> ");
        findings.push(AgentFinding {
            source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-archgraph".to_string(), rule_id: Some("import-cycle".to_string()), aspect: None, backend: None },
            category: "architecture".to_string(),
            severity: Severity::High,
            confidence: 1.0,
            title: "Import cycle between packages".to_string(),
            message: format!(
                "This diff touches a package involved in a circular import: {cycle_description}. Cyclic package dependencies make it impossible to reason about either package in isolation and often signal a missing shared abstraction — extract the shared pieces into a new package both can depend on."
            ),
            location: Location { path: (*anchor_file).clone(), range: LocationRange { start_line: 1, ..Default::default() }, snippet: cycle_description, side: Side::New },
            related_locations: None,
            suggestion: None,
            tags: None,
            meta: None,
            suggested_patch: None,
        });
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_go_file(dir: &Path, rel_path: &str, content: &str) {
        let path = dir.join(rel_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn returns_empty_when_repo_is_not_a_go_module() {
        let dir = tempfile::tempdir().unwrap();
        write_go_file(dir.path(), "main.go", "package main\n");
        let findings = run_archgraph_check(dir.path(), &["main.go".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn returns_empty_when_no_go_files_changed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module github.com/example/myapp\n").unwrap();
        let findings = run_archgraph_check(dir.path(), &["README.md".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn reports_a_cycle_that_touches_a_changed_package() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module github.com/example/myapp\n").unwrap();
        write_go_file(dir.path(), "internal/a/a.go", "package a\n\nimport \"github.com/example/myapp/internal/b\"\n\nfunc F() { b.G() }\n");
        write_go_file(dir.path(), "internal/b/b.go", "package b\n\nimport \"github.com/example/myapp/internal/a\"\n\nfunc G() { a.F() }\n");

        let findings = run_archgraph_check(dir.path(), &["internal/a/a.go".to_string()]);
        assert_eq!(findings.len(), 1, "got: {findings:#?}");
        assert_eq!(findings[0].category, "architecture");
        assert!(findings[0].message.contains("internal/a"));
        assert!(findings[0].message.contains("internal/b"));
    }

    #[test]
    fn does_not_report_a_cycle_the_diff_never_touches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module github.com/example/myapp\n").unwrap();
        write_go_file(dir.path(), "internal/a/a.go", "package a\n\nimport \"github.com/example/myapp/internal/b\"\n\nfunc F() { b.G() }\n");
        write_go_file(dir.path(), "internal/b/b.go", "package b\n\nimport \"github.com/example/myapp/internal/a\"\n\nfunc G() { a.F() }\n");
        write_go_file(dir.path(), "internal/unrelated/u.go", "package unrelated\n\nfunc H() {}\n");

        let findings = run_archgraph_check(dir.path(), &["internal/unrelated/u.go".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_report_anything_for_an_acyclic_graph() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module github.com/example/myapp\n").unwrap();
        write_go_file(dir.path(), "internal/a/a.go", "package a\n\nimport \"github.com/example/myapp/internal/b\"\n\nfunc F() { b.G() }\n");
        write_go_file(dir.path(), "internal/b/b.go", "package b\n\nfunc G() {}\n");

        let findings = run_archgraph_check(dir.path(), &["internal/a/a.go".to_string()]);
        assert!(findings.is_empty());
    }
}

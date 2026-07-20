//! Turns `autoreview-archgraph`'s import-cycle detection into reportable
//! findings — the crate itself has no knowledge of the schema's `Finding`
//! type, per its own module docs, so that mapping lives here.
//!
//! Deliberately diff-relevant, not a whole-repo audit dump: archgraph
//! builds the graph over the *entire* repo (a cycle can't be seen from one
//! file), but this only reports a cycle when at least one of its packages
//! contains a file the current diff actually touched — otherwise every
//! `autoreview diff` on an unrelated change would repeat the same
//! pre-existing cycles it can't do anything about in this review. Go and
//! Java/Kotlin are checked independently (two separate graphs, two
//! separate package-resolution strategies — see `autoreview_archgraph`'s
//! own module doc for why) and their findings are simply concatenated.

use std::collections::HashSet;
use std::path::Path;

use autoreview_archgraph::{build_go_import_graph, build_java_kotlin_import_graph, declared_package, detect_cycles, discover_go_module_path, ImportGraph};
use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

fn go_package_for_file(file: &str, module_path: &str) -> Option<String> {
    let parent = Path::new(file).parent()?;
    if parent.as_os_str().is_empty() {
        Some(module_path.to_string())
    } else {
        Some(format!("{module_path}/{}", parent.to_string_lossy().replace('\\', "/")))
    }
}

fn java_kotlin_package_for_file(repo_root: &Path, file: &str) -> Option<String> {
    let content = std::fs::read_to_string(repo_root.join(file)).ok()?;
    declared_package(&content)
}

/// Runs cycle detection over `graph` and reports any cycle that includes
/// a package one of `changed_files` belongs to (via `package_for_file`),
/// anchoring each finding at one of those changed files. Shared between
/// the Go and Java/Kotlin call sites below — only the graph and the
/// package-resolution strategy differ between them.
fn report_cycles_touching_changed_files(graph: &ImportGraph, changed_files: &[&String], package_for_file: impl Fn(&str) -> Option<String>) -> Vec<AgentFinding> {
    let cycles = detect_cycles(graph);
    if cycles.is_empty() {
        return Vec::new();
    }

    let changed_packages: HashSet<String> = changed_files.iter().filter_map(|f| package_for_file(f)).collect();

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
        let anchor_file = changed_files.iter().find(|f| package_for_file(f).map(|p| cycle_packages.contains(&p)).unwrap_or(false));
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

/// Runs archgraph's cycle detection over the whole repo (Go, then
/// Java/Kotlin) and reports any cycle that includes a package the current
/// diff touched. A repo with neither a `go.mod` nor any `.java`/`.kt`
/// files contributes no findings from either half.
pub fn run_archgraph_check(repo_root: &Path, changed_files: &[String]) -> Vec<AgentFinding> {
    let mut findings = Vec::new();

    if let Some(module_path) = discover_go_module_path(repo_root) {
        let changed_go_files: Vec<&String> = changed_files.iter().filter(|f| f.ends_with(".go")).collect();
        if !changed_go_files.is_empty() {
            let graph = build_go_import_graph(repo_root, &module_path);
            findings.extend(report_cycles_touching_changed_files(&graph, &changed_go_files, |f| go_package_for_file(f, &module_path)));
        }
    }

    let changed_java_kotlin_files: Vec<&String> = changed_files.iter().filter(|f| f.ends_with(".java") || f.ends_with(".kt")).collect();
    if !changed_java_kotlin_files.is_empty() {
        let graph = build_java_kotlin_import_graph(repo_root);
        findings.extend(report_cycles_touching_changed_files(&graph, &changed_java_kotlin_files, |f| java_kotlin_package_for_file(repo_root, f)));
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

    fn write_source_file(dir: &Path, rel_path: &str, content: &str) {
        let path = dir.join(rel_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn returns_empty_when_no_java_or_kotlin_files_changed() {
        let dir = tempfile::tempdir().unwrap();
        let findings = run_archgraph_check(dir.path(), &["README.md".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn reports_a_java_cycle_that_touches_a_changed_package() {
        let dir = tempfile::tempdir().unwrap();
        write_source_file(dir.path(), "src/com/example/a/A.java", "package com.example.a;\n\nimport com.example.b.B;\n\nclass A {\n    B b;\n}\n");
        write_source_file(dir.path(), "src/com/example/b/B.java", "package com.example.b;\n\nimport com.example.a.A;\n\nclass B {\n    A a;\n}\n");

        let findings = run_archgraph_check(dir.path(), &["src/com/example/a/A.java".to_string()]);
        assert_eq!(findings.len(), 1, "got: {findings:#?}");
        assert_eq!(findings[0].category, "architecture");
        assert!(findings[0].message.contains("com.example.a"));
        assert!(findings[0].message.contains("com.example.b"));
    }

    #[test]
    fn reports_a_kotlin_to_java_cycle_that_touches_a_changed_package() {
        let dir = tempfile::tempdir().unwrap();
        write_source_file(dir.path(), "src/com/example/a/A.kt", "package com.example.a\n\nimport com.example.b.B\n\nclass A {\n    val b: B? = null\n}\n");
        write_source_file(dir.path(), "src/com/example/b/B.java", "package com.example.b;\n\nimport com.example.a.A;\n\nclass B {\n    A a;\n}\n");

        let findings = run_archgraph_check(dir.path(), &["src/com/example/a/A.kt".to_string()]);
        assert_eq!(findings.len(), 1, "got: {findings:#?}");
    }

    #[test]
    fn does_not_report_a_java_cycle_the_diff_never_touches() {
        let dir = tempfile::tempdir().unwrap();
        write_source_file(dir.path(), "src/com/example/a/A.java", "package com.example.a;\n\nimport com.example.b.B;\n\nclass A {\n    B b;\n}\n");
        write_source_file(dir.path(), "src/com/example/b/B.java", "package com.example.b;\n\nimport com.example.a.A;\n\nclass B {\n    A a;\n}\n");
        write_source_file(dir.path(), "src/com/example/unrelated/U.java", "package com.example.unrelated;\n\nclass U {}\n");

        let findings = run_archgraph_check(dir.path(), &["src/com/example/unrelated/U.java".to_string()]);
        assert!(findings.is_empty());
    }
}

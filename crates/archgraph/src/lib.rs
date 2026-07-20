//! Track 1 Tier 2 of the rule-pack expansion plan — archgraph, the real
//! cross-file dependency graph Tier 1's per-file import scan structurally
//! can't be: a whole-repo package import graph, cycle detection, and
//! fan-in/fan-out metrics. Go-only first slice, per the plan's own scoping
//! ("simplest module resolution, no classpath ambiguity").
//!
//! Java/Kotlin followed as a second slice (`build_java_kotlin_import_graph`)
//! with a deliberately different package-resolution strategy than Go's:
//! Go's package is its file's *directory* (a language guarantee), but
//! Java/Kotlin's package is whatever the file's own `package` statement
//! declares — directory layout conventionally mirrors it but isn't
//! guaranteed the way Go's is, so this reads the declaration directly
//! rather than inferring from the path. That also removes the need for a
//! `go.mod`-style "what's our module path" discovery step: a package is
//! "internal" simply by being declared somewhere in this repo's own
//! source tree (collected in one pass), no external manifest to parse.
//!
//! Deliberately a pure graph library with no knowledge of `autoreview-
//! schema`'s `Finding` type — the plan's repo layout describes this crate
//! as generic dependency-graph analysis; turning a cycle/metric into a
//! reportable finding is `autoreview-core`'s job, which already depends on
//! both this crate and the schema crate.

use std::collections::{HashMap, HashSet};
use std::path::Path;

/// A package-level import graph: each node is a package's full import path
/// (module path + relative directory), each edge an internal import (an
/// import resolving to another package in this same module — external/
/// stdlib imports aren't part of the graph at all, since there's nothing
/// to detect a cycle or layering violation *within this repo* about them).
#[derive(Debug, Clone, Default)]
pub struct ImportGraph {
    pub edges: HashMap<String, HashSet<String>>,
}

impl ImportGraph {
    pub fn packages(&self) -> impl Iterator<Item = &String> {
        self.edges.keys()
    }

    pub fn fan_out(&self, package: &str) -> usize {
        self.edges.get(package).map(|s| s.len()).unwrap_or(0)
    }

    pub fn fan_in(&self, package: &str) -> usize {
        self.edges.values().filter(|targets| targets.contains(package)).count()
    }
}

/// Reads `go.mod`'s `module` directive to get this repo's own import-path
/// prefix — needed to tell an internal import (`<module>/internal/foo`)
/// apart from an external one (`github.com/someone/else`). Returns `None`
/// if there's no `go.mod` (not a Go module, or Go isn't in play at all),
/// which callers use to skip archgraph entirely rather than error.
pub fn discover_go_module_path(repo_root: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(repo_root.join("go.mod")).ok()?;
    contents.lines().find_map(|line| line.trim().strip_prefix("module ").map(|m| m.trim().to_string()))
}

/// Every import path a Go file declares, single-line or block form —
/// `pub` so other resolution needs (e.g. `dataflow_check.rs`'s cross-
/// package interprocedural summaries for `typed-nil-interface-return`)
/// can reuse the same parsing instead of re-deriving it.
pub fn extract_go_imports(content: &str) -> Vec<String> {
    let mut imports = Vec::new();
    let mut in_block = false;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if let Some(rest) = line.strip_prefix("import ") {
            let rest = rest.trim();
            if rest == "(" {
                in_block = true;
                continue;
            }
            if let Some(path) = extract_quoted(rest) {
                imports.push(path);
            }
            continue;
        }
        if in_block {
            if line == ")" {
                in_block = false;
                continue;
            }
            if let Some(path) = extract_quoted(line) {
                imports.push(path);
            }
        }
    }
    imports
}

fn extract_quoted(s: &str) -> Option<String> {
    let s = s.trim();
    let start = s.find('"')?;
    let end = s[start + 1..].find('"')? + start + 1;
    Some(s[start + 1..end].to_string())
}

const SKIP_DIRS: &[&str] = &[".git", "vendor", "node_modules", "testdata", "target", "build", ".gradle", "out"];

fn walk_go_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if SKIP_DIRS.contains(&name) {
                continue;
            }
            walk_go_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("go") {
            out.push(path);
        }
    }
}

/// Builds the whole-repo package import graph: every `.go` file's directory
/// becomes a package node (Go's own package-per-directory model), every
/// import resolving under `module_path` becomes an edge. This is
/// deliberately whole-repo, not diff-scoped — a cycle or a layering
/// violation that a Tier 1 per-file check can't see almost always involves
/// files the current diff didn't touch at all.
pub fn build_go_import_graph(repo_root: &Path, module_path: &str) -> ImportGraph {
    let mut files = Vec::new();
    walk_go_files(repo_root, &mut files);

    let mut graph = ImportGraph::default();

    for file in &files {
        let Ok(rel) = file.strip_prefix(repo_root) else { continue };
        let Some(parent) = rel.parent() else { continue };
        let package = if parent.as_os_str().is_empty() { module_path.to_string() } else { format!("{module_path}/{}", parent.to_string_lossy().replace('\\', "/")) };

        let Ok(content) = std::fs::read_to_string(file) else { continue };
        let entry = graph.edges.entry(package.clone()).or_default();
        for import in extract_go_imports(&content) {
            if import.starts_with(module_path) && import != package {
                entry.insert(import);
            }
        }
    }

    graph
}

/// Extracts a file's own `package foo.bar;` (Java) / `package foo.bar`
/// (Kotlin, no semicolon required) declaration — public since
/// `autoreview-core`'s finding-mapping layer needs the same extraction to
/// resolve a *changed* file's own package (mirroring how Go's
/// `package_for_file` lives outside this crate, except Java/Kotlin's needs
/// this crate's own parsing rather than a pure path computation).
pub fn declared_package(content: &str) -> Option<String> {
    content.lines().map(str::trim).find_map(|line| line.strip_prefix("package ").map(|rest| rest.trim_end_matches(';').trim().to_string()))
}

/// Extracts every `import foo.bar.Baz;`/`import foo.bar.*;` (Java) or
/// `import foo.bar.Baz`/`import foo.bar.*` (Kotlin) target — `import
/// static foo.Bar.baz;`'s `static` keyword is stripped so the member's
/// owning package still resolves correctly.
fn extract_java_kotlin_imports(content: &str) -> Vec<String> {
    content
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("import ").map(|rest| rest.strip_prefix("static ").unwrap_or(rest).trim_end_matches(';').trim().to_string()))
        .collect()
}

/// An import target's own package: `foo.bar.Baz` -> `foo.bar`, and a
/// wildcard `foo.bar.*` -> `foo.bar` directly (no trailing member to
/// strip).
fn import_package(import_path: &str) -> String {
    match import_path.strip_suffix(".*") {
        Some(prefix) => prefix.to_string(),
        None => match import_path.rfind('.') {
            Some(idx) => import_path[..idx].to_string(),
            None => import_path.to_string(),
        },
    }
}

fn walk_java_kotlin_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if SKIP_DIRS.contains(&name) {
                continue;
            }
            walk_java_kotlin_files(&path, out);
        } else if matches!(path.extension().and_then(|e| e.to_str()), Some("java") | Some("kt")) {
            out.push(path);
        }
    }
}

/// Builds the whole-repo Java/Kotlin package import graph. Unlike Go's
/// directory-derived package, a package here is whatever each file's own
/// `package` statement declares (see this module's doc comment) — a
/// package is "internal" simply by being declared somewhere in this
/// repo's own `.java`/`.kt` files, discovered in the same walk rather than
/// needing an external module-path manifest. Files with no `package`
/// statement (Java's default/unnamed package, or a parse miss) are
/// skipped — there's no package name to anchor a node on.
pub fn build_java_kotlin_import_graph(repo_root: &Path) -> ImportGraph {
    let mut files = Vec::new();
    walk_java_kotlin_files(repo_root, &mut files);

    let mut file_packages: Vec<(String, String)> = Vec::new();
    let mut declared_packages: HashSet<String> = HashSet::new();
    for file in &files {
        let Ok(content) = std::fs::read_to_string(file) else { continue };
        let Some(package) = declared_package(&content) else { continue };
        declared_packages.insert(package.clone());
        file_packages.push((package, content));
    }

    let mut graph = ImportGraph::default();
    for (package, content) in &file_packages {
        let entry = graph.edges.entry(package.clone()).or_default();
        for import in extract_java_kotlin_imports(content) {
            let target = import_package(&import);
            if target != *package && declared_packages.contains(&target) {
                entry.insert(target);
            }
        }
    }

    graph
}

/// Finds all simple cycles in the import graph via DFS with an on-stack
/// path tracker. Returns each cycle as the sequence of packages in it
/// (first package repeated at the end for readability, e.g. `[a, b, c, a]`).
/// Not deduplicated across rotations/direction — a genuine cycle a->b->c->a
/// only gets discovered once per distinct starting node reachable from the
/// initial DFS roots, which in practice (iterating all nodes as roots,
/// skipping already-visited ones) reports each cycle exactly once.
pub fn detect_cycles(graph: &ImportGraph) -> Vec<Vec<String>> {
    let mut cycles = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut on_stack: Vec<String> = Vec::new();
    let mut on_stack_set: HashSet<String> = HashSet::new();

    let mut nodes: Vec<&String> = graph.packages().collect();
    nodes.sort();

    for start in nodes {
        if visited.contains(start) {
            continue;
        }
        dfs(start, graph, &mut visited, &mut on_stack, &mut on_stack_set, &mut cycles);
    }

    cycles
}

fn dfs(node: &str, graph: &ImportGraph, visited: &mut HashSet<String>, on_stack: &mut Vec<String>, on_stack_set: &mut HashSet<String>, cycles: &mut Vec<Vec<String>>) {
    visited.insert(node.to_string());
    on_stack.push(node.to_string());
    on_stack_set.insert(node.to_string());

    if let Some(targets) = graph.edges.get(node) {
        let mut sorted_targets: Vec<&String> = targets.iter().collect();
        sorted_targets.sort();
        for target in sorted_targets {
            if on_stack_set.contains(target) {
                let cycle_start = on_stack.iter().position(|n| n == target).unwrap();
                let mut cycle: Vec<String> = on_stack[cycle_start..].to_vec();
                cycle.push(target.clone());
                cycles.push(cycle);
            } else if !visited.contains(target) {
                dfs(target, graph, visited, on_stack, on_stack_set, cycles);
            }
        }
    }

    on_stack.pop();
    on_stack_set.remove(node);
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
    fn discover_go_module_path_reads_the_module_directive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module github.com/example/myapp\n\ngo 1.22\n").unwrap();
        assert_eq!(discover_go_module_path(dir.path()), Some("github.com/example/myapp".to_string()));
    }

    #[test]
    fn discover_go_module_path_returns_none_without_a_go_mod() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(discover_go_module_path(dir.path()), None);
    }

    #[test]
    fn build_go_import_graph_creates_edges_only_for_internal_imports() {
        let dir = tempfile::tempdir().unwrap();
        write_go_file(dir.path(), "internal/a/a.go", "package a\n\nimport (\n\t\"fmt\"\n\t\"github.com/example/myapp/internal/b\"\n)\n\nfunc F() { fmt.Println(b.G()) }\n");
        write_go_file(dir.path(), "internal/b/b.go", "package b\n\nfunc G() int { return 1 }\n");

        let graph = build_go_import_graph(dir.path(), "github.com/example/myapp");
        let a = "github.com/example/myapp/internal/a";
        let b = "github.com/example/myapp/internal/b";
        assert!(graph.edges.get(a).unwrap().contains(b));
        assert!(!graph.edges.get(a).unwrap().contains("fmt"), "stdlib imports must not become graph edges");
    }

    #[test]
    fn detect_cycles_finds_a_real_two_package_cycle() {
        let dir = tempfile::tempdir().unwrap();
        write_go_file(dir.path(), "internal/a/a.go", "package a\n\nimport \"github.com/example/myapp/internal/b\"\n\nfunc F() { b.G() }\n");
        write_go_file(dir.path(), "internal/b/b.go", "package b\n\nimport \"github.com/example/myapp/internal/a\"\n\nfunc G() { a.F() }\n");

        let graph = build_go_import_graph(dir.path(), "github.com/example/myapp");
        let cycles = detect_cycles(&graph);
        assert_eq!(cycles.len(), 1, "got: {cycles:#?}");
        assert_eq!(cycles[0].len(), 3, "a -> b -> a should be a 2-node cycle plus the repeated start: {:#?}", cycles[0]);
    }

    #[test]
    fn detect_cycles_returns_empty_for_an_acyclic_graph() {
        let dir = tempfile::tempdir().unwrap();
        write_go_file(dir.path(), "internal/a/a.go", "package a\n\nimport \"github.com/example/myapp/internal/b\"\n\nfunc F() { b.G() }\n");
        write_go_file(dir.path(), "internal/b/b.go", "package b\n\nfunc G() {}\n");

        let graph = build_go_import_graph(dir.path(), "github.com/example/myapp");
        assert!(detect_cycles(&graph).is_empty());
    }

    #[test]
    fn fan_in_and_fan_out_count_edges_correctly() {
        let dir = tempfile::tempdir().unwrap();
        write_go_file(dir.path(), "internal/a/a.go", "package a\n\nimport \"github.com/example/myapp/internal/c\"\n\nfunc F() { c.H() }\n");
        write_go_file(dir.path(), "internal/b/b.go", "package b\n\nimport \"github.com/example/myapp/internal/c\"\n\nfunc G() { c.H() }\n");
        write_go_file(dir.path(), "internal/c/c.go", "package c\n\nfunc H() {}\n");

        let graph = build_go_import_graph(dir.path(), "github.com/example/myapp");
        let c = "github.com/example/myapp/internal/c";
        assert_eq!(graph.fan_in(c), 2);
        assert_eq!(graph.fan_out(c), 0);
        let a = "github.com/example/myapp/internal/a";
        assert_eq!(graph.fan_out(a), 1);
    }

    #[test]
    fn walk_go_files_skips_vendor_and_git_directories() {
        let dir = tempfile::tempdir().unwrap();
        write_go_file(dir.path(), "internal/a/a.go", "package a\n");
        write_go_file(dir.path(), "vendor/github.com/other/pkg.go", "package pkg\n");
        write_go_file(dir.path(), ".git/objects/fake.go", "package fake\n");

        let graph = build_go_import_graph(dir.path(), "github.com/example/myapp");
        assert_eq!(graph.packages().count(), 1);
    }

    fn write_source_file(dir: &Path, rel_path: &str, content: &str) {
        let path = dir.join(rel_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn declared_package_reads_the_package_statement() {
        assert_eq!(declared_package("package com.example.foo;\n\nclass Foo {}\n"), Some("com.example.foo".to_string()));
        assert_eq!(declared_package("package com.example.foo\n\nclass Foo\n"), Some("com.example.foo".to_string()), "Kotlin has no trailing semicolon");
        assert_eq!(declared_package("class Foo {}\n"), None);
    }

    #[test]
    fn build_java_kotlin_import_graph_creates_edges_only_for_internal_packages() {
        let dir = tempfile::tempdir().unwrap();
        write_source_file(dir.path(), "src/com/example/a/A.java", "package com.example.a;\n\nimport com.example.b.B;\nimport java.util.List;\n\nclass A {\n    B b;\n    List<String> items;\n}\n");
        write_source_file(dir.path(), "src/com/example/b/B.java", "package com.example.b;\n\nclass B {}\n");

        let graph = build_java_kotlin_import_graph(dir.path());
        assert!(graph.edges.get("com.example.a").unwrap().contains("com.example.b"));
        assert!(!graph.edges.get("com.example.a").unwrap().contains("java.util"), "stdlib imports must not become graph edges");
    }

    #[test]
    fn build_java_kotlin_import_graph_resolves_a_wildcard_import() {
        let dir = tempfile::tempdir().unwrap();
        write_source_file(dir.path(), "src/com/example/a/A.java", "package com.example.a;\n\nimport com.example.b.*;\n\nclass A {\n    B b;\n}\n");
        write_source_file(dir.path(), "src/com/example/b/B.java", "package com.example.b;\n\nclass B {}\n");

        let graph = build_java_kotlin_import_graph(dir.path());
        assert!(graph.edges.get("com.example.a").unwrap().contains("com.example.b"));
    }

    #[test]
    fn build_java_kotlin_import_graph_works_across_java_and_kotlin_files_in_the_same_package() {
        let dir = tempfile::tempdir().unwrap();
        write_source_file(dir.path(), "src/com/example/a/A.kt", "package com.example.a\n\nimport com.example.b.B\n\nclass A {\n    val b: B? = null\n}\n");
        write_source_file(dir.path(), "src/com/example/b/B.java", "package com.example.b;\n\nclass B {}\n");

        let graph = build_java_kotlin_import_graph(dir.path());
        assert!(graph.edges.get("com.example.a").unwrap().contains("com.example.b"));
    }

    #[test]
    fn detect_cycles_finds_a_real_two_package_java_cycle() {
        let dir = tempfile::tempdir().unwrap();
        write_source_file(dir.path(), "src/com/example/a/A.java", "package com.example.a;\n\nimport com.example.b.B;\n\nclass A {\n    B b;\n}\n");
        write_source_file(dir.path(), "src/com/example/b/B.java", "package com.example.b;\n\nimport com.example.a.A;\n\nclass B {\n    A a;\n}\n");

        let graph = build_java_kotlin_import_graph(dir.path());
        let cycles = detect_cycles(&graph);
        assert_eq!(cycles.len(), 1, "got: {cycles:#?}");
    }
}

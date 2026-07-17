//! Whole-repo, tree-sitter-based, name-only symbol index — the building
//! block for cross-file design-smell detection (Feature Envy, Message
//! Chains, Data Clumps) that single-file ast-grep patterns and the
//! hand-rolled line-scan analyzers (`complexity.rs`, `duplication.rs`)
//! structurally can't see. Deliberately unresolved/heuristic (no type
//! resolution, no classpath/module-graph loading) — see the plan's
//! "Cross-file symbol index" section for the tiered rationale (a real
//! compiler-frontend backend is future work, not attempted here).
//!
//! Kotlin is out of scope: `tree-sitter-kotlin` pins an incompatible
//! `tree-sitter` version (see `autoreview-core`'s `patch_check.rs` for the
//! same, previously-documented constraint). This crate covers Go and Java.
//!
//! Mirrors `autoreview-archgraph`'s own separation of concerns: a pure
//! data-structure + query library with no dependency on `autoreview-schema`
//! — converting a query result into an `AgentFinding` is a separate
//! concern living in `autoreview-core`.

pub mod extract;
pub mod model;
pub mod queries;

use std::path::{Path, PathBuf};

pub use model::{AccessRef, CallChain, ForeignAccessRef, MethodDecl, NamedSlot, SymbolIndex, TypeDecl};
pub use queries::{find_data_clumps, find_feature_envy, find_message_chains, ChainFinding, ClumpMember, ClumpScope, DataClumpFinding, FeatureEnvyFinding};

/// Same skip-list as `autoreview-archgraph`'s own whole-repo walk — this
/// crate deliberately doesn't depend on archgraph (staying a pure,
/// standalone library per its own module docs), so the list is duplicated
/// rather than shared, same as `architecture.rs`'s own documented
/// duplication of import-extraction logic vs archgraph's.
const SKIP_DIRS: &[&str] = &[".git", "vendor", "node_modules", "testdata", "target", "build", "dist"];

/// A single pathological generated file (a vendored parser, a huge fixture)
/// shouldn't blow up index-build time — skipped, not truncated, since a
/// partial parse of a huge file is more likely to mislead than help.
const MAX_FILE_BYTES: u64 = 1_000_000;

fn walk_source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if SKIP_DIRS.contains(&name) {
                continue;
            }
            walk_source_files(&path, out);
        } else {
            let is_relevant = matches!(path.extension().and_then(|e| e.to_str()), Some("go") | Some("java"));
            if is_relevant {
                out.push(path);
            }
        }
    }
}

/// Builds the whole-repo symbol index — every `.go`/`.java` file under
/// `repo_root`, unconditionally, not scoped to a diff's changed files (a
/// cross-file smell is invisible if only one side of it is indexed). See
/// the plan's "whole-repo, always" scoping decision for why this differs
/// from `duplication.rs`'s changed-files-only cross-file variant: a symbol
/// index build is one linear parse-and-extract pass per file, not a
/// pairwise comparison, so it doesn't carry that same cost concern.
pub fn build_index(repo_root: &Path) -> SymbolIndex {
    let mut files = Vec::new();
    walk_source_files(repo_root, &mut files);

    let mut types = Vec::new();
    for file in &files {
        let Ok(metadata) = std::fs::metadata(file) else { continue };
        if metadata.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(file) else { continue };
        // TypeDecl.file must be repo-relative — callers (the diff-scoped
        // mapping layer in autoreview-core) compare it against
        // git-diff-relative changed-file paths.
        let rel_file = file.strip_prefix(repo_root).unwrap_or(file);
        types.extend(extract::extract_file(rel_file, &content));
    }

    SymbolIndex { types }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn build_index_finds_types_across_both_languages_whole_repo() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "internal/a/widget.go", "package a\n\ntype Widget struct {\n\tX int\n}\n");
        write_file(dir.path(), "src/Customer.java", "class Customer {\n    int balance;\n}\n");
        let index = build_index(dir.path());
        assert!(index.find_type("Widget").is_some());
        assert!(index.find_type("Customer").is_some());
    }

    #[test]
    fn build_index_skips_vendor_and_git_directories() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "vendor/thing/widget.go", "package thing\n\ntype Vendored struct{}\n");
        write_file(dir.path(), ".git/hooks/widget.go", "package hooks\n\ntype Hidden struct{}\n");
        write_file(dir.path(), "real/widget.go", "package real\n\ntype Real struct{}\n");
        let index = build_index(dir.path());
        assert!(index.find_type("Vendored").is_none());
        assert!(index.find_type("Hidden").is_none());
        assert!(index.find_type("Real").is_some());
    }

    #[test]
    fn build_index_ignores_irrelevant_file_extensions() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "README.md", "# hi\n");
        write_file(dir.path(), "notes.kt", "class Widget\n");
        let index = build_index(dir.path());
        assert!(index.types.is_empty());
    }

    #[test]
    fn build_index_on_an_empty_repo_returns_an_empty_index() {
        let dir = tempfile::tempdir().unwrap();
        assert!(build_index(dir.path()).types.is_empty());
    }

    #[test]
    fn type_decl_file_paths_are_repo_relative_not_absolute() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "internal/a/widget.go", "package a\n\ntype Widget struct {\n\tX int\n}\n");
        let index = build_index(dir.path());
        let widget = index.find_type("Widget").unwrap();
        assert_eq!(widget.file, Path::new("internal/a/widget.go"));
    }
}

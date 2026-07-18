//! Per-language CST → `Cfg` lowering. Empty scaffold for now (Phase 2 of
//! the dataflow rollout ships the language-agnostic CFG/lattice/solver
//! core only) — `go.rs` lands in Phase 3 alongside the
//! `append-shared-backing-array` rewrite, `java.rs` in Phase 5, `kotlin.rs`
//! in Phase 6. Each language module's entry point will have the shape:
//!
//! ```ignore
//! pub fn lower_function(tree: &tree_sitter::Tree, source: &[u8], fn_node: tree_sitter::Node) -> Cfg<Stmt>
//! ```
//!
//! called once per function/method already discovered by
//! `autoreview_symindex`'s existing method enumeration — this crate
//! deliberately does not re-walk the repo to "find every function" a
//! second time.

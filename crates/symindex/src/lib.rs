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

pub use model::{AccessRef, CallChain, ForeignAccessRef, MethodDecl, NamedSlot, SymbolIndex, TypeDecl};

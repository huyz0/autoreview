//! Multi-language interprocedural dataflow analysis for `autoreview`.
//!
//! Scope, stated explicitly since "dataflow" and "interprocedural" are
//! easy to over-promise: this crate builds a small, language-agnostic
//! control-flow graph (`cfg`) per function/method, runs a standard forward
//! worklist fixpoint solver (`solver`) over a rule-supplied lattice
//! (`lattice`), and resolves cross-function calls through
//! `autoreview-symindex`'s existing whole-repo `SymbolIndex` — same-file,
//! then same-package/type, then an explicit "unknown boundary" that stops
//! fact propagation rather than guessing. "Interprocedural" here means one
//! hop of call-target resolution propagating a coarse per-function
//! summary (see `crates/core/src/analyzers/dataflow_check.rs`'s
//! `typed-nil-interface-return` rewrite for a concrete example), not a
//! full recursive interprocedural fixpoint across an arbitrary call
//! chain, and there is no SSA, points-to, or alias analysis anywhere in
//! this crate.
//!
//! Per-language lowering (CST → `Cfg`) lives under `lower/`, kept
//! separate from `autoreview-symindex::extract` even though both walk the
//! same tree-sitter grammars — `symindex::extract` parses-then-flattens
//! and discards the tree, which is architecturally incompatible with the
//! *retained*, structured `Cfg` this crate needs to build and keep around
//! for the solver to walk.

pub mod cfg;
pub mod lattice;
pub mod lower;
pub mod rules;
pub mod solver;
pub mod taint;

pub use cfg::{Cfg, CfgNode, EdgeKind, Stmt};
pub use lattice::Lattice;

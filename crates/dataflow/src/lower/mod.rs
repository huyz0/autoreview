//! Per-language CST → `Cfg` lowering. `go.rs` (Phase 3) backs three
//! shipped rules; `java.rs` and `kotlin.rs` (Phases 6/7) were architectural
//! completion of the generic CFG core, since proven out by real taint
//! rules too (see `dataflow_check.rs`). `javascript.rs` is one module
//! covering both JavaScript and TypeScript — `tree-sitter-typescript`'s
//! grammar is built directly on `tree-sitter-javascript`'s, so the same
//! field-based node access works against source parsed with either.
//!
//! Each language module's entry point is called once per function/method
//! already discovered by `autoreview_symindex`'s existing method
//! enumeration — this crate deliberately does not re-walk the repo to
//! "find every function" a second time.

pub mod go;
pub mod java;
pub mod javascript;
pub mod kotlin;

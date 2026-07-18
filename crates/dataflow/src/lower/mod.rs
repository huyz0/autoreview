//! Per-language CST → `Cfg` lowering. `go.rs` lands in Phase 3 alongside
//! the `append-shared-backing-array` rewrite; `java.rs`/`kotlin.rs` are
//! later-phase scaffold (Phase 5/6) — not yet implemented.
//!
//! Each language module's entry point is called once per function/method
//! already discovered by `autoreview_symindex`'s existing method
//! enumeration — this crate deliberately does not re-walk the repo to
//! "find every function" a second time.

pub mod go;

//! Per-language CST → `Cfg` lowering. `go.rs` (Phase 3) backs three
//! shipped rules; `java.rs` and `kotlin.rs` (Phases 6/7) are
//! architectural completion of the generic CFG core across all three
//! languages, per the project's "design for all three from the start"
//! scope — no Java/Kotlin-specific dataflow rule exists yet, so these
//! modules are proven via their own lowering tests rather than an
//! end-to-end rule.
//!
//! Each language module's entry point is called once per function/method
//! already discovered by `autoreview_symindex`'s existing method
//! enumeration — this crate deliberately does not re-walk the repo to
//! "find every function" a second time.

pub mod go;
pub mod java;
pub mod kotlin;

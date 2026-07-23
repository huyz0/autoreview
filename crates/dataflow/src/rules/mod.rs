//! Concrete dataflow-powered rules. Each rule owns its own `Lattice` impl
//! and transfer function — see `lattice.rs`'s module docs for why this
//! crate doesn't try to share one general-purpose lattice across rules.
//!
//! Taint rules used to live here too (`go_command_injection_taint.rs` and
//! friends), one hand-written `TaintSpec` constant per file. They're now
//! declarative YAML under `crates/core/rules-builtin/` (`kind: taint`),
//! loaded at runtime by `crates/core/src/analyzers/taint_rules.rs` — see
//! that module and `taint.rs`'s own docs for why.

pub mod go_append_shared_backing_array;
pub mod go_loopvar;
pub mod go_typed_nil_interface_return;
pub mod java_kotlin_npe_risk;

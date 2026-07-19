//! Concrete dataflow-powered rules. Each rule owns its own `Lattice` impl
//! and transfer function — see `lattice.rs`'s module docs for why this
//! crate doesn't try to share one general-purpose lattice across rules.

pub mod go_append_shared_backing_array;
pub mod go_command_injection_taint;
pub mod go_loopvar;
pub mod go_path_traversal_taint;
pub mod go_sql_injection_taint;
pub mod go_typed_nil_interface_return;

//! Self-improving rule factory (M3): mine recurring agent findings into
//! deterministic-rule candidates, then (later stages, not yet built) draft,
//! bench, shadow, and promote them. Only `mine` is implemented so far —
//! see this module's `mine` submodule for the clustering algorithm and
//! `CandidateSeed`'s docs for exactly what "recurring" means here.

pub mod draft;
pub mod mine;

//! Self-improving rule factory (M3): mine recurring agent findings (or,
//! opt-in, recurring human PR review comments via `mine_from_comments`)
//! into deterministic-rule candidates, then draft, bench, shadow, and
//! promote them. See `mine`'s submodule docs for the clustering algorithm
//! and `CandidateSeed`'s docs for exactly what "recurring" means here.

pub mod bench;
pub mod category_heuristics;
pub mod draft;
pub mod existing_rules;
pub mod mine;
pub mod mine_from_bugfix_commits;
pub mod mine_from_code;
pub mod mine_from_comments;
pub mod shadow;

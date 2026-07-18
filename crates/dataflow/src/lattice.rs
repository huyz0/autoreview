//! The join-semilattice contract each rule's dataflow facts must satisfy
//! for the worklist solver in `solver.rs` to reach a fixpoint. Kept
//! minimal on purpose — no meet/top, no lattice-combinator helpers beyond
//! what a monotone forward analysis actually needs.

pub trait Lattice: Clone + PartialEq {
    /// The "no information yet" value — what an unreached node starts at.
    fn bottom() -> Self;

    /// Merges facts from two predecessors. Must be monotone (joining never
    /// loses information already established) for the solver to terminate.
    fn join(&self, other: &Self) -> Self;
}

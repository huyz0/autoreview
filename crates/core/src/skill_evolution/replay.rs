//! Pure comparison logic for the skill-evolution replay-bench harness. The
//! actual replay mechanics — checking out a historical commit into an
//! isolated `git worktree`, writing a candidate `instructions.md` override,
//! running the specialist twice (old vs candidate) — live in the CLI layer
//! (`commands::skills_bench`), since they're I/O/process-heavy and specific
//! to how `diff`'s specialist-invocation plumbing is already wired there.
//! This module only answers: "given what a run flagged and what we already
//! know the ground truth was, did the candidate do better?"

use autoreview_schema::AgentFinding;

use crate::report::dedupe::title_similarity;
use crate::storage::history_store::KnownVerdict;

/// How similar a re-run finding's title/message needs to be to a known
/// verdict's, to count as "the same finding recurring" — re-running an LLM
/// specialist never reproduces identical wording, so this can't be an exact
/// match.
const MATCH_THRESHOLD: f64 = 0.5;

fn finding_matches(finding: &AgentFinding, known: &KnownVerdict) -> bool {
    let a = format!("{} {}", finding.title, finding.message);
    let b = format!("{} {}", known.title, known.message);
    title_similarity(&a, &b) >= MATCH_THRESHOLD
}

/// Whether any finding in `run_findings` looks like the same issue as `known`.
fn still_flagged(known: &KnownVerdict, run_findings: &[AgentFinding]) -> bool {
    run_findings.iter().any(|f| finding_matches(f, known))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayComparison {
    pub fp_total: usize,
    pub fp_resolved: usize,
    pub tp_total: usize,
    pub tp_lost: Vec<String>,
}

impl ReplayComparison {
    /// The plan's gate: candidate must resolve >= 70% of the FPs it was
    /// drafted to fix, and lose zero confirmed TPs — a hard gate, not a
    /// score, since losing a real finding to quiet noise is the exact
    /// failure mode the plan's own prior-art research warned about.
    pub fn passes_gate(&self) -> bool {
        if self.tp_total > 0 && !self.tp_lost.is_empty() {
            return false;
        }
        if self.fp_total == 0 {
            return false;
        }
        (self.fp_resolved as f64 / self.fp_total as f64) >= 0.70
    }
}

/// Compares a candidate skill's replay findings against the known ground
/// truth for one run. `known_verdicts` are findings a human already judged
/// `fp` or `tp` for the *old* skill; `candidate_findings` is what the
/// *candidate* (with the proposed instruction edit) flagged on the same
/// replayed diff. Pure — no I/O, directly testable against literal fixtures.
pub fn compare_replay(known_verdicts: &[KnownVerdict], candidate_findings: &[AgentFinding]) -> ReplayComparison {
    let mut comparison = ReplayComparison::default();
    for known in known_verdicts {
        match known.verdict.as_str() {
            "fp" => {
                comparison.fp_total += 1;
                if !still_flagged(known, candidate_findings) {
                    comparison.fp_resolved += 1;
                }
            }
            "tp" => {
                comparison.tp_total += 1;
                if !still_flagged(known, candidate_findings) {
                    comparison.tp_lost.push(known.title.clone());
                }
            }
            _ => {}
        }
    }
    comparison
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_schema::{FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

    fn known(title: &str, message: &str, verdict: &str) -> KnownVerdict {
        KnownVerdict { title: title.to_string(), message: message.to_string(), fingerprint: "fp1".to_string(), verdict: verdict.to_string() }
    }

    fn finding(title: &str, message: &str) -> AgentFinding {
        AgentFinding {
            source: FindingSource { kind: FindingSourceKind::Agent, tool: "claude".into(), rule_id: None, aspect: Some("correctness".into()), backend: None },
            category: "correctness".to_string(),
            severity: Severity::Medium,
            confidence: 0.8,
            title: title.to_string(),
            message: message.to_string(),
            location: Location { path: "a.go".into(), range: LocationRange { start_line: 1, ..Default::default() }, snippet: "x".into(), side: Side::New },
            related_locations: None,
            suggestion: None,
            tags: None,
            meta: None,
            suggested_patch: None,
        }
    }

    #[test]
    fn counts_a_resolved_fp_when_the_candidate_no_longer_flags_it() {
        let known_verdicts = vec![known("Missing null check", "Parameter x is not null-checked before use", "fp")];
        let comparison = compare_replay(&known_verdicts, &[]);
        assert_eq!(comparison.fp_total, 1);
        assert_eq!(comparison.fp_resolved, 1);
    }

    #[test]
    fn does_not_count_an_fp_as_resolved_if_the_candidate_still_flags_it() {
        let known_verdicts = vec![known("Missing null check", "Parameter x is not null-checked before use", "fp")];
        let candidate = vec![finding("Missing null check", "Parameter x is not null-checked before use")];
        let comparison = compare_replay(&known_verdicts, &candidate);
        assert_eq!(comparison.fp_resolved, 0);
    }

    #[test]
    fn flags_a_lost_tp_when_the_candidate_stops_flagging_a_confirmed_true_positive() {
        let known_verdicts = vec![known("SQL injection risk", "raw query built from input", "tp")];
        let comparison = compare_replay(&known_verdicts, &[]);
        assert_eq!(comparison.tp_lost, vec!["SQL injection risk".to_string()]);
    }

    #[test]
    fn does_not_flag_a_tp_as_lost_if_the_candidate_still_flags_it() {
        let known_verdicts = vec![known("SQL injection risk", "raw query built from input", "tp")];
        let candidate = vec![finding("SQL injection risk", "raw query built from input")];
        let comparison = compare_replay(&known_verdicts, &candidate);
        assert!(comparison.tp_lost.is_empty());
    }

    #[test]
    fn passes_gate_when_most_fps_resolved_and_no_tps_lost() {
        let comparison = ReplayComparison { fp_total: 10, fp_resolved: 8, tp_total: 5, tp_lost: vec![] };
        assert!(comparison.passes_gate());
    }

    #[test]
    fn fails_gate_on_any_lost_tp_regardless_of_fp_resolution_rate() {
        let comparison = ReplayComparison { fp_total: 10, fp_resolved: 10, tp_total: 5, tp_lost: vec!["real bug".to_string()] };
        assert!(!comparison.passes_gate(), "zero tolerance for a lost TP, per the plan");
    }

    #[test]
    fn fails_gate_when_fp_resolution_rate_is_below_70_percent() {
        let comparison = ReplayComparison { fp_total: 10, fp_resolved: 6, tp_total: 0, tp_lost: vec![] };
        assert!(!comparison.passes_gate());
    }

    #[test]
    fn fails_gate_when_there_are_no_fps_to_resolve_at_all() {
        let comparison = ReplayComparison { fp_total: 0, fp_resolved: 0, tp_total: 3, tp_lost: vec![] };
        assert!(!comparison.passes_gate(), "nothing to prove the candidate actually helps");
    }
}

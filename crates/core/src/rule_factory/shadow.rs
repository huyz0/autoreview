//! Rule-factory shadow mode: pure promotion/demotion decision logic and
//! agreement classification, kept free of any I/O or `HistoryStore`
//! dependency so the thresholds are directly testable. The orchestration
//! (loading shadow rule files, running them, recording firings) lives in
//! `analyzers::shadow_rules`.
//!
//! Per the plan's Self-improvement section: promote when firings >= 20,
//! distinct runs >= 5, agent-agreement ratio >= 0.90, zero user `--fp`
//! feedback, and >= 7 days since entering shadow (so one large refactor
//! commit can't manufacture 20 firings in an afternoon). Demote on 3 total
//! user `--fp` feedbacks.

pub const MIN_FIRINGS: u32 = 20;
pub const MIN_DISTINCT_RUNS: usize = 5;
pub const MIN_AGREEMENT_RATIO: f64 = 0.90;
pub const MIN_DAYS_IN_SHADOW: i64 = 7;
pub const DEMOTE_FP_THRESHOLD: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agreement {
    Agreed,
    Disagreed,
    NoSignal,
}

impl Agreement {
    pub fn as_str(&self) -> &'static str {
        match self {
            Agreement::Agreed => "agreed",
            Agreement::Disagreed => "disagreed",
            Agreement::NoSignal => "no_signal",
        }
    }
}

/// A minimal view of an agent finding, just what agreement classification
/// needs — decoupled from `autoreview_schema::Finding` so this stays
/// trivially unit-testable with plain literals.
pub struct AgentFindingRef<'a> {
    pub category: &'a str,
    pub path: &'a str,
    pub line: u32,
}

/// How close two findings' locations need to be (in lines) to count as
/// "the same spot" — small enough that unrelated findings on the same file
/// don't accidentally count as agreement.
const LINE_PROXIMITY: u32 = 3;

/// Classifies whether a shadow-rule firing agrees with this run's agent
/// findings: `Agreed` if a same-category agent finding landed within
/// `LINE_PROXIMITY` lines of the same file; `Disagreed` if the aspect ran
/// (at least one agent finding exists in this category, anywhere) but
/// didn't flag this spot; `NoSignal` if the aspect wasn't summoned at all
/// this run (no agent findings in this category anywhere), since a
/// specialist that never looked shouldn't count as disagreement.
pub fn classify_agreement(category: &str, path: &str, line: u32, agent_findings_this_run: &[AgentFindingRef]) -> Agreement {
    let same_category: Vec<&AgentFindingRef> = agent_findings_this_run.iter().filter(|f| f.category == category).collect();
    if same_category.is_empty() {
        return Agreement::NoSignal;
    }
    let agrees = same_category.iter().any(|f| f.path == path && f.line.abs_diff(line) <= LINE_PROXIMITY);
    if agrees {
        Agreement::Agreed
    } else {
        Agreement::Disagreed
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PromotionInputs {
    pub firings: u32,
    pub distinct_runs: usize,
    pub agent_agreed: u32,
    pub agent_disagreed: u32,
    pub user_fp_count: u32,
    pub days_since_valid_from: i64,
}

/// Whether a shadow rule clears every promotion gate. All five conditions
/// are independent hard gates, not a weighted score — matching the plan's
/// own "all hold" phrasing, since a rule that's fast but disagreed-with, or
/// popular but user-flagged, shouldn't promote just because one number is
/// good.
pub fn should_promote(inputs: &PromotionInputs) -> bool {
    if inputs.firings < MIN_FIRINGS {
        return false;
    }
    if inputs.distinct_runs < MIN_DISTINCT_RUNS {
        return false;
    }
    if inputs.user_fp_count > 0 {
        return false;
    }
    if inputs.days_since_valid_from < MIN_DAYS_IN_SHADOW {
        return false;
    }
    let total_signal = inputs.agent_agreed + inputs.agent_disagreed;
    if total_signal == 0 {
        // No agent ever ran alongside this rule to agree or disagree —
        // can't judge an agreement ratio from zero signal, so this can
        // never promote purely on firing volume alone.
        return false;
    }
    let ratio = inputs.agent_agreed as f64 / total_signal as f64;
    ratio >= MIN_AGREEMENT_RATIO
}

/// Demotion is a single hard trigger — three user false-positive reports —
/// independent of everything else a promoted rule has going for it.
pub fn should_demote(user_fp_count: u32) -> bool {
    user_fp_count >= DEMOTE_FP_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing_inputs() -> PromotionInputs {
        PromotionInputs { firings: 20, distinct_runs: 5, agent_agreed: 18, agent_disagreed: 2, user_fp_count: 0, days_since_valid_from: 7 }
    }

    #[test]
    fn promotes_when_every_gate_clears() {
        assert!(should_promote(&passing_inputs()));
    }

    #[test]
    fn does_not_promote_below_minimum_firings() {
        let mut inputs = passing_inputs();
        inputs.firings = 19;
        assert!(!should_promote(&inputs));
    }

    #[test]
    fn does_not_promote_below_minimum_distinct_runs() {
        let mut inputs = passing_inputs();
        inputs.distinct_runs = 4;
        assert!(!should_promote(&inputs));
    }

    #[test]
    fn does_not_promote_with_any_user_fp_feedback() {
        let mut inputs = passing_inputs();
        inputs.user_fp_count = 1;
        assert!(!should_promote(&inputs));
    }

    #[test]
    fn does_not_promote_before_the_minimum_shadow_period() {
        let mut inputs = passing_inputs();
        inputs.days_since_valid_from = 6;
        assert!(!should_promote(&inputs));
    }

    #[test]
    fn does_not_promote_below_the_agreement_ratio() {
        let mut inputs = passing_inputs();
        inputs.agent_agreed = 10;
        inputs.agent_disagreed = 10;
        assert!(!should_promote(&inputs));
    }

    #[test]
    fn does_not_promote_on_zero_agreement_signal_even_with_high_firings() {
        let mut inputs = passing_inputs();
        inputs.agent_agreed = 0;
        inputs.agent_disagreed = 0;
        assert!(!should_promote(&inputs));
    }

    #[test]
    fn demotes_at_exactly_three_fp_reports() {
        assert!(should_demote(3));
        assert!(!should_demote(2));
    }

    #[test]
    fn classifies_agreement_when_agent_flags_the_same_spot() {
        let agents = vec![AgentFindingRef { category: "correctness", path: "a.go", line: 12 }];
        assert_eq!(classify_agreement("correctness", "a.go", 10, &agents), Agreement::Agreed);
    }

    #[test]
    fn classifies_disagreement_when_agent_ran_but_missed_the_spot() {
        let agents = vec![AgentFindingRef { category: "correctness", path: "a.go", line: 100 }];
        assert_eq!(classify_agreement("correctness", "a.go", 10, &agents), Agreement::Disagreed);
    }

    #[test]
    fn classifies_disagreement_when_agent_flagged_a_different_file() {
        let agents = vec![AgentFindingRef { category: "correctness", path: "b.go", line: 10 }];
        assert_eq!(classify_agreement("correctness", "a.go", 10, &agents), Agreement::Disagreed);
    }

    #[test]
    fn classifies_no_signal_when_the_category_never_ran_this_turn() {
        let agents = vec![AgentFindingRef { category: "design", path: "a.go", line: 10 }];
        assert_eq!(classify_agreement("correctness", "a.go", 10, &agents), Agreement::NoSignal);
    }

    #[test]
    fn classifies_no_signal_when_no_agents_ran_at_all() {
        assert_eq!(classify_agreement("correctness", "a.go", 10, &[]), Agreement::NoSignal);
    }
}

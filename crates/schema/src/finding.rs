use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Blocker,
    High,
    Medium,
    Low,
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FindingSourceKind {
    Analyzer,
    Agent,
    LearnedRule,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingSource {
    pub kind: FindingSourceKind,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LocationRange {
    pub start_line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_col: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_col: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    New,
    Old,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub path: String,
    pub range: LocationRange,
    pub snippet: String,
    pub side: Side,
}

/// A human's verdict on a reported finding, recorded via `autoreview
/// feedback <id> --<flag>`. Deliberately five variants, not a plain
/// true/false-positive binary — modeled on Aviator Verify's waiver
/// taxonomy (`false_positive`/`doesnt_apply`/`accepted_risk`/
/// `fix_in_followup`), extended with a generic `TruePositive` since
/// autoreview's fp/tp flow is a rule-precision signal first and a
/// merge-waiver record second, and not every confirmation needs a
/// specific downstream reason attached.
///
/// The distinction that actually matters downstream: `FalsePositive` is
/// evidence the *rule itself* misjudged this case (counts toward
/// `rule_factory::shadow`'s demotion gate); `DoesntApply` says the rule is
/// valid in general but this instance doesn't apply (does *not* count as
/// evidence against the rule — a categorically different signal, per
/// Aviator's own stated semantics). `TruePositive`/`AcceptedRisk`/
/// `FixInFollowup` all confirm the finding was correct; they only differ
/// in what happens next, which matters for the audit trail but not for
/// rule precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackVerdict {
    FalsePositive,
    TruePositive,
    DoesntApply,
    AcceptedRisk,
    FixInFollowup,
}

impl FeedbackVerdict {
    /// The canonical string stored in `HistoryStore`'s `feedback.verdict`
    /// column and used in SQL `WHERE` filters — kept in sync with the
    /// `#[serde(rename_all = "snake_case")]` above intentionally, so
    /// serialization and storage never drift apart.
    pub fn as_str(self) -> &'static str {
        match self {
            FeedbackVerdict::FalsePositive => "false_positive",
            FeedbackVerdict::TruePositive => "true_positive",
            FeedbackVerdict::DoesntApply => "doesnt_apply",
            FeedbackVerdict::AcceptedRisk => "accepted_risk",
            FeedbackVerdict::FixInFollowup => "fix_in_followup",
        }
    }

    /// A short, human-readable label for terminal output — distinct from
    /// `as_str()`, which is the stable storage/wire format.
    pub fn label(self) -> &'static str {
        match self {
            FeedbackVerdict::FalsePositive => "false positive",
            FeedbackVerdict::TruePositive => "true positive",
            FeedbackVerdict::DoesntApply => "doesn't apply",
            FeedbackVerdict::AcceptedRisk => "accepted risk",
            FeedbackVerdict::FixInFollowup => "fix in follow-up",
        }
    }

    /// Evidence the rule itself is wrong (a demotion signal) — see the
    /// type's own doc comment for why `DoesntApply` is deliberately
    /// excluded.
    pub fn is_false_positive_like(self) -> bool {
        matches!(self, FeedbackVerdict::FalsePositive)
    }

    /// Confirms the finding was correct, regardless of what happens next.
    pub fn is_true_positive_like(self) -> bool {
        matches!(self, FeedbackVerdict::TruePositive | FeedbackVerdict::AcceptedRisk | FeedbackVerdict::FixInFollowup)
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "false_positive" => Some(FeedbackVerdict::FalsePositive),
            "true_positive" => Some(FeedbackVerdict::TruePositive),
            "doesnt_apply" => Some(FeedbackVerdict::DoesntApply),
            "accepted_risk" => Some(FeedbackVerdict::AcceptedRisk),
            "fix_in_followup" => Some(FeedbackVerdict::FixInFollowup),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PatchSafety {
    SafeAutofix,
    NeedsReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    pub safety: PatchSafety,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingFingerprints {
    pub primary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub id: String,
    pub fingerprints: FindingFingerprints,
    pub source: FindingSource,
    pub category: String,
    pub severity: Severity,
    /// 0.0-1.0; analyzers are always 1.0, agents self-report and the planner clamps.
    pub confidence: f64,
    pub title: String,
    pub message: String,
    pub location: Location,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_locations: Option<Vec<Location>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<Suggestion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<HashMap<String, serde_json::Value>>,
}

/// The looser shape a specialist agent emits in its final fenced JSON block:
/// no id/fingerprints (computed downstream), self-reported confidence/range.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentFinding {
    pub source: FindingSource,
    pub category: String,
    pub severity: Severity,
    pub confidence: f64,
    pub title: String,
    pub message: String,
    pub location: Location,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_locations: Option<Vec<Location>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<Suggestion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<HashMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_patch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentOutput {
    pub findings: Vec<AgentFinding>,
}

/// A Finding with id/fingerprints attached, produced by `assign_fingerprints`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintedFinding {
    pub id: String,
    pub fingerprints: FindingFingerprints,
    #[serde(flatten)]
    pub finding: AgentFinding,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_round_trips_through_as_str_and_parse() {
        for verdict in [FeedbackVerdict::FalsePositive, FeedbackVerdict::TruePositive, FeedbackVerdict::DoesntApply, FeedbackVerdict::AcceptedRisk, FeedbackVerdict::FixInFollowup] {
            assert_eq!(FeedbackVerdict::parse(verdict.as_str()), Some(verdict));
        }
    }

    #[test]
    fn parse_rejects_an_unknown_string() {
        assert_eq!(FeedbackVerdict::parse("fp"), None, "legacy short forms are not accepted — as_str()'s canonical strings are the only valid input");
        assert_eq!(FeedbackVerdict::parse("bogus"), None);
    }

    #[test]
    fn only_false_positive_is_false_positive_like() {
        assert!(FeedbackVerdict::FalsePositive.is_false_positive_like());
        for verdict in [FeedbackVerdict::TruePositive, FeedbackVerdict::DoesntApply, FeedbackVerdict::AcceptedRisk, FeedbackVerdict::FixInFollowup] {
            assert!(!verdict.is_false_positive_like(), "{verdict:?} must not count as false-positive-like");
        }
    }

    #[test]
    fn true_positive_accepted_risk_and_fix_in_followup_are_true_positive_like_but_doesnt_apply_is_not() {
        for verdict in [FeedbackVerdict::TruePositive, FeedbackVerdict::AcceptedRisk, FeedbackVerdict::FixInFollowup] {
            assert!(verdict.is_true_positive_like(), "{verdict:?} must count as true-positive-like");
        }
        assert!(!FeedbackVerdict::DoesntApply.is_true_positive_like());
        assert!(!FeedbackVerdict::FalsePositive.is_true_positive_like());
    }

    #[test]
    fn serde_uses_the_same_snake_case_strings_as_as_str() {
        for verdict in [FeedbackVerdict::FalsePositive, FeedbackVerdict::TruePositive, FeedbackVerdict::DoesntApply, FeedbackVerdict::AcceptedRisk, FeedbackVerdict::FixInFollowup] {
            let json = serde_json::to_string(&verdict).unwrap();
            assert_eq!(json, format!("\"{}\"", verdict.as_str()), "serde wire format must match as_str()'s storage format");
        }
    }
}

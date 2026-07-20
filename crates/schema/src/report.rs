use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::finding::Finding;
use crate::spec::CriterionResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Quick,
    Standard,
    Deep,
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tier::Quick => write!(f, "quick"),
            Tier::Standard => write!(f, "standard"),
            Tier::Deep => write!(f, "deep"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffStats {
    pub files: u32,
    pub additions: u32,
    pub deletions: u32,
    pub languages: HashMap<String, u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTarget {
    pub repo_root: String,
    pub base_ref: String,
    pub head_ref: String,
    pub diff_stats: DiffStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriageSignalScore {
    pub signal: String,
    pub points: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecialistPlanEntry {
    pub aspect: String,
    pub triggered_by: Vec<String>,
    pub model: String,
    pub max_turns: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanBudgets {
    pub max_agents: u32,
    pub total_token_cap: u64,
    pub wall_clock_sec: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewPlan {
    pub tier: Tier,
    pub score: f64,
    pub signals: Vec<TriageSignalScore>,
    pub specialists: Vec<SpecialistPlanEntry>,
    pub budgets: PlanBudgets,
    #[serde(default)]
    pub overrides: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SuppressedReason {
    Duplicate,
    Baseline,
    BelowConfidence,
    ShadowRule,
    EmbeddingFpMatch,
    /// The Stage-3.5 verify pass judged this finding against the diff and
    /// voted to refute it — see the plan's "grounded generation + separate
    /// judge" section.
    Refuted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuppressedFinding {
    pub finding: Finding,
    pub reason: SuppressedReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostEntry {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usd: Option<f64>,
    pub wall_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCosts {
    pub total: CostEntry,
    pub per_stage: HashMap<String, CostEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gate {
    pub passed: bool,
    pub failed_conditions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSummary {
    pub by_severity: HashMap<String, u32>,
    pub by_category: HashMap<String, u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<Gate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewReport {
    pub schema_version: String,
    pub run_id: String,
    pub created_at: String,
    pub target: ReviewTarget,
    pub plan: ReviewPlan,
    pub findings: Vec<Finding>,
    pub suppressed: Vec<SuppressedFinding>,
    pub costs: RunCosts,
    pub summary: ReviewSummary,
    /// Empty when no `.autoreview/spec.md` was present (the common case
    /// today) or its Acceptance Criteria section had nothing to check —
    /// additive field, `#[serde(default)]` so older `report.json` files
    /// (and any external tooling reading `schema_version: "1"`) keep
    /// parsing without needing to know about this.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spec_verdicts: Vec<CriterionResult>,
}

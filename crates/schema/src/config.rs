use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelAlias {
    Cheap,
    Standard,
    Deep,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriageTierScoreConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_score: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriageSignalWeight {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_line: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_file: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_branch: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_finding_per_kloc: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub points: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cap: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriageTiers {
    #[serde(default = "default_quick_tier")]
    pub quick: TriageTierScoreConfig,
    #[serde(default = "default_standard_tier")]
    pub standard: TriageTierScoreConfig,
    #[serde(default)]
    pub deep: TriageTierScoreConfig,
}

fn default_quick_tier() -> TriageTierScoreConfig {
    TriageTierScoreConfig { max_score: Some(20.0) }
}
fn default_standard_tier() -> TriageTierScoreConfig {
    TriageTierScoreConfig { max_score: Some(60.0) }
}

impl Default for TriageTiers {
    fn default() -> Self {
        Self {
            quick: default_quick_tier(),
            standard: default_standard_tier(),
            deep: TriageTierScoreConfig::default(),
        }
    }
}

pub fn default_sensitive_paths() -> Vec<String> {
    [
        "**/auth*",
        "**/auth*/**",
        "**/crypto*",
        "**/crypto*/**",
        "**/*secret*",
        "**/payment*",
        "**/payment*/**",
        "**/migration*",
        "**/migration*/**",
        ".github/workflows/**",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

pub fn default_signal_weights() -> HashMap<String, TriageSignalWeight> {
    let mut m = HashMap::new();
    m.insert("linesChanged".to_string(), TriageSignalWeight { per_line: Some(0.15), cap: Some(30.0), ..Default::default() });
    m.insert("filesChanged".to_string(), TriageSignalWeight { per_file: Some(1.0), cap: Some(15.0), ..Default::default() });
    m.insert("sensitivePathHit".to_string(), TriageSignalWeight { points: Some(25.0), ..Default::default() });
    m.insert("dependencyChange".to_string(), TriageSignalWeight { points: Some(15.0), ..Default::default() });
    m.insert("ciOrInfraChange".to_string(), TriageSignalWeight { points: Some(15.0), ..Default::default() });
    m.insert("complexityDelta".to_string(), TriageSignalWeight { per_branch: Some(0.5), cap: Some(15.0), ..Default::default() });
    m.insert("analyzerDensity".to_string(), TriageSignalWeight { per_finding_per_kloc: Some(2.0), cap: Some(10.0), ..Default::default() });
    m.insert("noTestsWithSource".to_string(), TriageSignalWeight { points: Some(10.0), ..Default::default() });
    m
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriageConfig {
    #[serde(default)]
    pub tiers: TriageTiers,
    #[serde(default = "default_signal_weights")]
    pub signals: HashMap<String, TriageSignalWeight>,
    #[serde(default = "default_sensitive_paths")]
    pub sensitive_paths: Vec<String>,
}

impl Default for TriageConfig {
    fn default() -> Self {
        Self {
            tiers: TriageTiers::default(),
            signals: default_signal_weights(),
            sensitive_paths: default_sensitive_paths(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerAgentBudget {
    pub max_turns: u32,
    pub model: ModelAlias,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalation_model: Option<ModelAlias>,
}

fn default_max_concurrency() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TierBudget {
    pub max_agents: u32,
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: u32,
    pub per_agent: PerAgentBudget,
    pub total_token_cap: u64,
    pub wall_clock_sec: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsConfig {
    pub cheap: String,
    pub standard: String,
    pub deep: String,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self { cheap: "haiku".into(), standard: "sonnet".into(), deep: "opus".into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetTiers {
    pub quick: TierBudget,
    pub standard: TierBudget,
    pub deep: TierBudget,
}

impl Default for BudgetTiers {
    fn default() -> Self {
        Self {
            quick: TierBudget {
                max_agents: 1,
                max_concurrency: 1,
                per_agent: PerAgentBudget { max_turns: 4, model: ModelAlias::Cheap, escalation_model: None },
                total_token_cap: 80_000,
                wall_clock_sec: 120,
            },
            standard: TierBudget {
                max_agents: 3,
                max_concurrency: 3,
                per_agent: PerAgentBudget { max_turns: 10, model: ModelAlias::Standard, escalation_model: None },
                total_token_cap: 400_000,
                wall_clock_sec: 480,
            },
            deep: TierBudget {
                max_agents: 6,
                max_concurrency: 3,
                per_agent: PerAgentBudget { max_turns: 25, model: ModelAlias::Standard, escalation_model: Some(ModelAlias::Deep) },
                total_token_cap: 1_500_000,
                wall_clock_sec: 1800,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetsConfig {
    #[serde(default)]
    pub models: ModelsConfig,
    #[serde(default)]
    pub tiers: BudgetTiers,
}

impl Default for BudgetsConfig {
    fn default() -> Self {
        Self { models: ModelsConfig::default(), tiers: BudgetTiers::default() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ContextProviderConfig {
    Docs { paths: Vec<String> },
    GitHistory,
    Command {
        run: String,
        #[serde(default = "default_command_input")]
        input: String,
    },
    Mcp {
        server: String,
        #[serde(default)]
        use_for: Vec<String>,
    },
}

fn default_command_input() -> String {
    "changed-files".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextConfig {
    #[serde(default)]
    pub providers: Vec<ContextProviderConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SyncMode {
    #[default]
    None,
    Git,
    Remote,
}

fn default_sync_branch() -> String {
    "autoreview-history".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageSyncConfig {
    #[serde(default)]
    pub mode: SyncMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default = "default_sync_branch")]
    pub branch: String,
}

impl Default for StorageSyncConfig {
    fn default() -> Self {
        Self { mode: SyncMode::None, location: None, branch: default_sync_branch() }
    }
}

fn default_threshold() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_dir: Option<String>,
    #[serde(default)]
    pub sync: StorageSyncConfig,
    #[serde(default = "default_threshold")]
    pub fp_block_threshold: u32,
    #[serde(default = "default_threshold")]
    pub tp_override_threshold: u32,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self { history_dir: None, sync: StorageSyncConfig::default(), fp_block_threshold: 3, tp_override_threshold: 3 }
    }
}

fn default_noisy_categories() -> Vec<String> {
    vec!["style".to_string()]
}

/// Config for the Stage-3.5 judge pass (see the plan's prior-art research
/// section: a separate verifier over generated findings measurably beats
/// generation alone at killing false positives). Skipped entirely in `quick`
/// tier by the caller — this only configures *which* findings get judged
/// when the pass does run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Analyzer categories with a known false-positive history worth a
    /// second look; agent findings at high/blocker severity are always
    /// verified regardless of category.
    #[serde(default = "default_noisy_categories")]
    pub noisy_categories: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self { enabled: true, noisy_categories: default_noisy_categories() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoreviewConfig {
    #[serde(default)]
    pub triage: TriageConfig,
    #[serde(default)]
    pub budgets: BudgetsConfig,
    #[serde(default)]
    pub context: ContextConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub verify: VerifyConfig,
}

impl Default for AutoreviewConfig {
    fn default() -> Self {
        Self {
            triage: TriageConfig::default(),
            budgets: BudgetsConfig::default(),
            context: ContextConfig::default(),
            storage: StorageConfig::default(),
            verify: VerifyConfig::default(),
        }
    }
}

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

/// Which `AgentBackend` implementation drives Stage 3 specialists (and the
/// Stage 3.5 verify pass / triage classifier, which reuse the same trait).
/// Per the plan's Milestones: proving the abstraction with a second backend
/// was originally scoped for M3 ("raw Anthropic API backend, proves the
/// abstraction") — landed here instead via `pi` and a local-LLM backend,
/// since the trait was already shaped for exactly this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AgentBackendKind {
    #[default]
    ClaudeCode,
    Pi,
    LocalLlm,
}

fn default_local_llm_base_url() -> String {
    "http://localhost:8080/v1".to_string()
}
fn default_local_llm_model() -> String {
    "local-model".to_string()
}

/// Settings for the local-LLM backend — an OpenAI-compatible
/// `/v1/chat/completions` endpoint, the contract llama.cpp's `llama-server`
/// exposes (also LM Studio, vLLM). No tool access: see the backend's own
/// module docs for why.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalLlmConfig {
    #[serde(default = "default_local_llm_base_url")]
    pub base_url: String,
    #[serde(default = "default_local_llm_model")]
    pub model: String,
}

impl Default for LocalLlmConfig {
    fn default() -> Self {
        Self { base_url: default_local_llm_base_url(), model: default_local_llm_model() }
    }
}

fn default_embedding_base_url() -> String {
    "http://localhost:8080/v1".to_string()
}
fn default_embedding_model() -> String {
    "local-embed".to_string()
}
fn default_embedding_curl_binary() -> String {
    "curl".to_string()
}

/// Settings for the Stage 4 embedding-similarity noise filter — an
/// OpenAI-compatible `/v1/embeddings` endpoint, same contract llama.cpp's
/// `llama-server --embedding` mode exposes. Reuses the curl-shell-out
/// pattern from `LocalLlmConfig` rather than a distinct HTTP client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_embedding_base_url")]
    pub base_url: String,
    #[serde(default = "default_embedding_model")]
    pub model: String,
    #[serde(default = "default_embedding_curl_binary")]
    pub curl_binary: String,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self { enabled: false, base_url: default_embedding_base_url(), model: default_embedding_model(), curl_binary: default_embedding_curl_binary() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentsConfig {
    #[serde(default)]
    pub backend: AgentBackendKind,
    /// `--provider` for the `pi` backend, e.g. `"anthropic"` or `"openai"` —
    /// `None` lets `pi` resolve the bare model id against whatever provider
    /// is already logged in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pi_provider: Option<String>,
    #[serde(default)]
    pub local_llm: LocalLlmConfig,
    #[serde(default)]
    pub embedding: EmbeddingConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetsConfig {
    #[serde(default)]
    pub models: ModelsConfig,
    #[serde(default)]
    pub tiers: BudgetTiers,
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
    // "design"/"performance" cover the symindex heuristic rules
    // (message-chain, feature-envy, data-clump) and the nested-loop/
    // object-in-loop rules — the pack's first non-type-resolved,
    // higher-false-positive-rate deterministic rules, so they get a
    // cheap-model double-check by default rather than only on request.
    vec!["style".to_string(), "design".to_string(), "performance".to_string()]
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

/// Opt-in Tier 4 real-semantic backend for `autoreview-symindex` (see
/// SESSION_NOTES.md follow-up #3): layered *on top of* the default
/// heuristic tree-sitter tier, not a replacement — off by default because
/// it shells out to a real Go toolchain (`go run`, which needs network
/// access the first time to resolve `golang.org/x/tools/go/packages`) and
/// only type-checks cleanly-building repos. Java's equivalent
/// (JavaParser + javaparser-symbol-solver) needs Maven dependency
/// resolution and is deliberately not built yet — this only covers Go.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymindexConfig {
    #[serde(default)]
    pub tier4_go: bool,
}

/// One sequential `gh api .../comments` call per PR (see
/// `mine_from_comments::mine_from_pr_comments`) took ~2-3s/PR against a
/// real, active public repo (verified manually — 40 PRs took 103s) — this
/// default trades a first run staying under ~2 minutes against scanning
/// enough history to actually find recurring patterns. A repo that wants
/// deeper history should raise this explicitly, accepting the longer wait.
fn default_lookback_prs() -> usize {
    30
}

fn default_gh_binary() -> String {
    "gh".to_string()
}

/// Opt-in second input source for `rule_factory::mine`'s clustering,
/// alongside autoreview's own past agent findings: recurring human PR
/// review comments, mined via the `gh` CLI. Off by default because it
/// shells out to `gh api` against GitHub (needs `gh auth login` done
/// already, and makes real, sequential network calls per `autoreview
/// rules mine --from-comments` invocation — see `default_lookback_prs`'s
/// own doc comment for measured latency) — modeled on Aviator Verify's
/// "mine invariants from PR history" input, per SESSION_NOTES.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MineFromCommentsConfig {
    #[serde(default)]
    pub enabled: bool,
    /// How many of the repo's most recently merged PRs to scan — a fixed
    /// count, not a date window, so this stays a bounded number of `gh`
    /// calls (list PRs once, fetch comments once per PR) regardless of how
    /// active the repo is. Roughly linear in runtime — see
    /// `default_lookback_prs`'s doc comment for the measured rate.
    #[serde(default = "default_lookback_prs")]
    pub lookback_prs: usize,
    #[serde(default = "default_gh_binary")]
    pub gh_binary: String,
}

impl Default for MineFromCommentsConfig {
    fn default() -> Self {
        Self { enabled: false, lookback_prs: default_lookback_prs(), gh_binary: default_gh_binary() }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    #[serde(default)]
    pub agents: AgentsConfig,
    #[serde(default)]
    pub symindex: SymindexConfig,
    #[serde(default)]
    pub mine_from_comments: MineFromCommentsConfig,
    #[serde(default)]
    pub mine_from_bugfix_commits: crate::mining_config::MineFromBugfixCommitsConfig,
}

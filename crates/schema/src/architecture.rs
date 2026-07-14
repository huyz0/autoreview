use serde::{Deserialize, Serialize};

/// A named layer in the codebase, identified by path globs — inspired by
/// Python's `import-linter` and JS's `dependency-cruiser`, both of which
/// prove layer rules don't strictly need a full dependency graph, just
/// import-statement inspection per file. See the plan's Track 1 Tier 1.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchitectureLayer {
    pub name: String,
    #[serde(rename = "match")]
    pub match_globs: Vec<String>,
}

/// A single forbidden-dependency rule: files in the `from` layer must not
/// import anything resolving to one of the `to` layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchitectureForbidRule {
    pub from: String,
    pub to: Vec<String>,
}

/// One entry under `rules:` — currently always a `forbid` rule, wrapped so
/// the YAML shape has room for other rule kinds later without a breaking
/// change (`allow`-only layers, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchitectureRuleEntry {
    pub forbid: ArchitectureForbidRule,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArchitectureConfig {
    #[serde(default)]
    pub layers: Vec<ArchitectureLayer>,
    #[serde(default)]
    pub rules: Vec<ArchitectureRuleEntry>,
}

/// The root of `.autoreview/architecture.yaml` — wraps everything under a
/// top-level `architecture:` key per the plan's own example, deliberately a
/// separate, optional, opt-in file from `config.yaml`: a repo with no
/// architecture.yaml gets no layer checking at all, since there's no sane
/// generic default for what a repo's layers are.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArchitectureFile {
    #[serde(default)]
    pub architecture: ArchitectureConfig,
}

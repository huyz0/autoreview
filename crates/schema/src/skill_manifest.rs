use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillTriggers {
    #[serde(default)]
    pub globs: Vec<String>,
    #[serde(default)]
    pub signals: Vec<String>,
    #[serde(default)]
    pub always: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillTools {
    #[serde(default = "default_true")]
    pub read: bool,
    #[serde(default = "default_true")]
    pub grep: bool,
    #[serde(default)]
    pub bash: Vec<String>,
}

impl Default for SkillTools {
    fn default() -> Self {
        Self { read: true, grep: true, bash: Vec::new() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CostClass {
    Quick,
    Moderate,
    Expensive,
}

impl Default for CostClass {
    fn default() -> Self {
        CostClass::Moderate
    }
}

fn default_languages() -> Vec<String> {
    vec!["*".to_string()]
}

fn default_output_contract() -> String {
    "findings-json-v1".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillManifest {
    pub id: String,
    pub title: String,
    pub version: String,
    pub categories: Vec<String>,
    #[serde(default = "default_languages")]
    pub languages: Vec<String>,
    #[serde(default)]
    pub triggers: SkillTriggers,
    #[serde(default)]
    pub cost_class: CostClass,
    #[serde(default)]
    pub tools: SkillTools,
    #[serde(default = "default_output_contract")]
    pub output_contract: String,
}

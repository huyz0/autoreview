use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactChecksConfig {
    #[serde(default)]
    pub immutable_github_action_refs: bool,
    #[serde(default)]
    pub transactional_postgres_migrations: Vec<String>,
    #[serde(default)]
    pub locale_catalogs: Vec<LocaleCatalogConfig>,
    #[serde(default)]
    pub protobuf_compatibility: Vec<String>,
    #[serde(default)]
    pub outbox_operations: Vec<OutboxOperationConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboxOperationConfig {
    pub paths: Vec<String>,
    pub database_write: ImportedOperation,
    pub broker_publish: ImportedOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedOperation {
    pub module: String,
    pub export: String,
    #[serde(default)]
    pub members: Vec<String>,
    pub first_argument: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocaleCatalogConfig {
    pub reference: String,
    pub translations: Vec<String>,
}

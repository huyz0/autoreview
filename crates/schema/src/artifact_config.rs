use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactChecksConfig {
    #[serde(default)]
    pub immutable_github_action_refs: bool,
    #[serde(default)]
    pub github_actions_script_inputs: bool,
    #[serde(default)]
    pub github_actions_least_privilege: bool,
    pub npm_advisory_snapshot: Option<String>,
    #[serde(default)]
    pub transactional_postgres_migrations: Vec<String>,
    #[serde(default)]
    pub locale_catalogs: Vec<LocaleCatalogConfig>,
    #[serde(default)]
    pub protobuf_compatibility: Vec<String>,
    #[serde(default)]
    pub outbox_operations: Vec<OutboxOperationConfig>,
    #[serde(default)]
    pub express_route_policies: Vec<ExpressRoutePolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpressRoutePolicy {
    pub paths: Vec<String>,
    pub method: String,
    pub route: String,
    pub authorization: Option<RouteAuthorization>,
    pub workload: Option<RouteWorkloadLimit>,
    pub shared_resource: Option<RouteSharedResource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteAuthorization {
    pub entity: String,
    pub verifier: ImportedOperation,
    pub protected_operation: ImportedOperation,
    #[serde(default)]
    pub verifier_key_argument: usize,
    #[serde(default)]
    pub operation_key_argument: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteWorkloadLimit {
    pub operation: ImportedOperation,
    #[serde(default)]
    pub options_argument: usize,
    pub limit_field: String,
    pub maximum: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteSharedResource {
    pub resource: String,
    pub check: ImportedOperation,
    pub mutation: ImportedOperation,
    #[serde(default)]
    pub check_key_argument: usize,
    #[serde(default)]
    pub mutation_key_argument: usize,
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

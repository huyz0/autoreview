//! Schema for external "rule packs" — third-party rule bundles a repo can
//! register without forking the binary. Two files: `.autoreview/
//! rulepacks.yaml` (this repo's registration list, `RulePacksFile`) and
//! each pack's own `rulepack.yaml` at its root (`RulePackManifest`,
//! deliberately the same flat-id/free-string-version shape
//! `SkillManifest` already uses — no namespace/semver enforcement, matching
//! this project's existing precedent for this kind of identity).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RulePacksFile {
    #[serde(default)]
    pub packs: Vec<RulePackConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RulePackConfig {
    pub id: String,
    pub source: RulePackSourceConfig,
    /// `full` (default): the pack's rules run exactly like builtin ones —
    /// no staging gate. `shadow`: the pack's rules still run (so their
    /// findings are computed and provenance-tagged the same way), but the
    /// findings are suppressed from the surfaced report — same "runs, but
    /// doesn't count yet" posture as a human-authored
    /// `.autoreview/rules/shadow/` rule. Unlike that mechanism, a shadow-
    /// trust pack rule isn't (yet) tracked toward automatic promotion —
    /// this is a manual, config-level trust decision, not a firing-history
    /// gate.
    #[serde(default)]
    pub trust: RulePackTrust,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RulePackTrust {
    #[default]
    Full,
    Shadow,
}

/// `kind: local` resolves `path` relative to the repo root — no caching,
/// no network. `kind: git` clones/checks-out `url` (optionally pinned to
/// `ref`, optionally scoped to `subpath` within the repo) into a shared
/// local cache before it's readable as a directory the same way a local
/// pack is.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum RulePackSourceConfig {
    Local {
        path: String,
    },
    Git {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        r#ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subpath: Option<String>,
    },
}

/// The pack's own self-declared identity, read from `rulepack.yaml` at its
/// root. The registered `id` in `rulepacks.yaml` and this manifest's `id`
/// must match — enforced by the loader, not this schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RulePackManifest {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_local_source_pack() {
        let yaml = "packs:\n  - id: acme-security\n    source:\n      kind: local\n      path: ../shared-rules/acme-security\n";
        let file: RulePacksFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(file.packs.len(), 1);
        assert_eq!(file.packs[0].id, "acme-security");
        match &file.packs[0].source {
            RulePackSourceConfig::Local { path } => assert_eq!(path, "../shared-rules/acme-security"),
            other => panic!("expected Local, got {other:?}"),
        }
    }

    #[test]
    fn parses_a_git_source_pack_with_optional_fields() {
        let yaml = "packs:\n  - id: acme-perf\n    source:\n      kind: git\n      url: https://github.com/acme/perf-rules\n      ref: v1.2.0\n      subpath: rules/go\n";
        let file: RulePacksFile = serde_yaml::from_str(yaml).unwrap();
        match &file.packs[0].source {
            RulePackSourceConfig::Git { url, r#ref, subpath } => {
                assert_eq!(url, "https://github.com/acme/perf-rules");
                assert_eq!(r#ref.as_deref(), Some("v1.2.0"));
                assert_eq!(subpath.as_deref(), Some("rules/go"));
            }
            other => panic!("expected Git, got {other:?}"),
        }
    }

    #[test]
    fn git_source_ref_and_subpath_are_optional() {
        let yaml = "packs:\n  - id: acme-perf\n    source:\n      kind: git\n      url: https://github.com/acme/perf-rules\n";
        let file: RulePacksFile = serde_yaml::from_str(yaml).unwrap();
        match &file.packs[0].source {
            RulePackSourceConfig::Git { r#ref, subpath, .. } => {
                assert!(r#ref.is_none());
                assert!(subpath.is_none());
            }
            other => panic!("expected Git, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_packs_file_parses_to_an_empty_list() {
        let file: RulePacksFile = serde_yaml::from_str("packs: []\n").unwrap();
        assert!(file.packs.is_empty());
    }

    #[test]
    fn parses_a_rule_pack_manifest() {
        let yaml = "id: acme-security\nversion: \"1.0.0\"\ndescription: \"ACME's internal security rule pack\"\n";
        let manifest: RulePackManifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(manifest.id, "acme-security");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.description, "ACME's internal security rule pack");
    }

    #[test]
    fn rule_pack_manifest_description_defaults_to_empty_when_absent() {
        let manifest: RulePackManifest = serde_yaml::from_str("id: x\nversion: \"1.0.0\"\n").unwrap();
        assert_eq!(manifest.description, "");
    }

    #[test]
    fn round_trips_through_serde_yaml() {
        let file = RulePacksFile {
            packs: vec![
                RulePackConfig { id: "a".to_string(), source: RulePackSourceConfig::Local { path: "../a".to_string() }, trust: RulePackTrust::Full },
                RulePackConfig { id: "b".to_string(), source: RulePackSourceConfig::Git { url: "https://example.com/b".to_string(), r#ref: Some("main".to_string()), subpath: None }, trust: RulePackTrust::Shadow },
            ],
        };
        let yaml = serde_yaml::to_string(&file).unwrap();
        let parsed: RulePacksFile = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.packs.len(), 2);
        assert_eq!(parsed.packs[1].trust, RulePackTrust::Shadow);
    }

    #[test]
    fn trust_defaults_to_full_when_absent() {
        let yaml = "packs:\n  - id: a\n    source:\n      kind: local\n      path: ../a\n";
        let file: RulePacksFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(file.packs[0].trust, RulePackTrust::Full);
    }

    #[test]
    fn trust_shadow_parses_explicitly() {
        let yaml = "packs:\n  - id: a\n    source:\n      kind: local\n      path: ../a\n    trust: shadow\n";
        let file: RulePacksFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(file.packs[0].trust, RulePackTrust::Shadow);
    }
}

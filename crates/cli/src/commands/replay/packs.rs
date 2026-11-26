use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Context;
use autoreview_core::ResolvedRulePack;
use autoreview_schema::RulePackTrust;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::Suite;

pub(super) struct FrozenRules {
    pub(super) packs: Vec<ResolvedRulePack>,
    pub(super) errors: BTreeMap<String, String>,
    pub(super) snapshots: Vec<RuleSnapshot>,
    _directory: Option<tempfile::TempDir>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RuleSnapshot {
    id: String,
    pack_id: String,
    sha256: String,
}

// Freeze only requested custom definitions once so both revisions use the
// same rule text. Built-in-only replay and merge gates retain their old path.
pub(super) fn freeze_custom_rules(repo: &Path, suite: &Suite) -> anyhow::Result<FrozenRules> {
    let ids: BTreeSet<_> = suite.cases.iter().filter(|case| case.rule.check_replay_support(&[]).is_err()
        && autoreview_core::find_rule_definition(&case.rule.id, &[]).is_none())
        .map(|case| case.rule.id.clone()).collect();
    if ids.is_empty() { return Ok(FrozenRules { packs: Vec::new(), errors: BTreeMap::new(), snapshots: Vec::new(), _directory: None }); }
    let config = autoreview_core::rule_packs_config_path(repo);
    let configured = match autoreview_core::load_rule_packs_config(&config).context("failed to load replay rule packs") {
        Ok(configured) => configured,
        Err(error) => return Ok(failed_rules(ids, format!("{error:#}"))),
    };
    let cache = autoreview_core::default_rule_packs_cache_root();
    let resolved: anyhow::Result<Vec<_>> = autoreview_core::resolve_rule_packs(repo, &cache, &configured).into_iter()
        .map(|(id, result)| result.with_context(|| format!("failed to resolve replay rule pack '{id}'")))
        .collect();
    let packs = match resolved { Ok(packs) => packs, Err(error) => return Ok(failed_rules(ids, format!("{error:#}"))) };
    let summaries = autoreview_core::load_existing_rule_summaries(&packs);
    let directory = tempfile::tempdir().context("cannot create frozen replay rule directory")?;
    let mut count = 0;
    let mut errors = BTreeMap::new();
    let mut snapshots = Vec::new();
    for id in ids {
        let matches = summaries.iter().filter(|rule| rule.id == id).count();
        if matches > 1 {
            errors.insert(id.clone(), format!("custom replay rule '{id}' is ambiguous across registered packs"));
            continue;
        }
        let Some(definition) = autoreview_core::find_rule_definition(&id, &packs) else { continue };
        if definition.kind != "call-sequence" {
            errors.insert(id.clone(), format!("custom replay rule '{id}' must be a call-sequence rule"));
            continue;
        }
        if let Err(error) = autoreview_core::analyzers::call_order_rules::validate_call_sequence(&definition.yaml) {
            errors.insert(id.clone(), format!("custom replay rule '{id}' is invalid: {error:#}"));
            continue;
        }
        snapshots.push(RuleSnapshot { id, pack_id: definition.source_label, sha256: format!("{:x}", Sha256::digest(definition.yaml.as_bytes())) });
        std::fs::write(directory.path().join(format!("rule-{count}.yml")), definition.yaml)?;
        count += 1;
    }
    let frozen = (count > 0).then(|| ResolvedRulePack { id: "replay-frozen".into(), local_path: directory.path().to_owned(), trust: RulePackTrust::Full });
    Ok(FrozenRules { packs: frozen.into_iter().collect(), errors, snapshots, _directory: Some(directory) })
}

fn failed_rules(ids: BTreeSet<String>, error: String) -> FrozenRules {
    FrozenRules { packs: Vec::new(), errors: ids.into_iter().map(|id| (id, error.clone())).collect(), snapshots: Vec::new(), _directory: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suite() -> Suite {
        serde_json::from_value(serde_json::json!({"cases":[{"name":"test","rule":{"tool":"autoreview-dataflow","id":"custom-test"},"before":{"commit":"a","path":"a.go","lines":[]},"after":{"commit":"b","path":"a.go","lines":[]}}]})).unwrap()
    }

    fn pack(root: &Path, id: &str) -> std::path::PathBuf {
        let path = root.join(id);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("rulepack.yaml"), format!("id: {id}\nversion: '1'\n")).unwrap();
        std::fs::write(path.join("rule.yml"), "id: custom-test\nlanguage: Go\ncategory: correctness\nseverity: warning\nkind: call-sequence\nmessage: original\ncontract:\n  mode: must-use\n  import: os\n  methods: [Rename]\n").unwrap();
        path
    }

    #[test]
    fn freezes_selected_rule_and_rejects_duplicate_ids() {
        let repo = tempfile::tempdir().unwrap();
        let first = pack(repo.path(), "first");
        let config = autoreview_core::rule_packs_config_path(repo.path());
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "packs:\n  - id: first\n    source: { kind: local, path: first }\n").unwrap();
        let frozen = freeze_custom_rules(repo.path(), &suite()).unwrap();
        std::fs::write(first.join("rule.yml"), "id: custom-test\nmessage: changed\n").unwrap();
        let definition = autoreview_core::find_rule_definition("custom-test", &frozen.packs).unwrap();
        assert!(definition.yaml.contains("message: original"));
        std::fs::write(first.join("rule.yml"), "id: custom-test\nlanguage: Go\ncategory: correctness\nkind: call-sequence\nmessage: broken\n").unwrap();
        assert!(freeze_custom_rules(repo.path(), &suite()).unwrap().errors["custom-test"].contains("invalid"));
        std::fs::write(first.join("rule.yml"), "id: custom-test\nlanguage: Go\ncategory: correctness\nkind: pattern\nrule: { pattern: 'bad()' }\n").unwrap();
        assert!(freeze_custom_rules(repo.path(), &suite()).unwrap().errors["custom-test"].contains("must be a call-sequence"));
        pack(repo.path(), "second");
        std::fs::write(first.join("rule.yml"), &definition.yaml).unwrap();
        std::fs::write(&config, "packs:\n  - id: first\n    source: { kind: local, path: first }\n  - id: second\n    source: { kind: local, path: second }\n").unwrap();
        assert!(freeze_custom_rules(repo.path(), &suite()).unwrap().errors["custom-test"].contains("ambiguous"));
    }
}

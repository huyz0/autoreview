//! Loads function-wide and keyed call-sequence rules from builtin and registered packs.

use serde::Deserialize;

use autoreview_dataflow::call_order::{CallOrderSpec, CollectionSelector, EntryIdentity};
use autoreview_dataflow::taint::NamePattern;
use autoreview_schema::Severity;

use super::ast_grep::{pack_rule_provenance, rule_roots, walk_rule_contents, RuleMeta};
use crate::rule_packs::ResolvedRulePack;

fn severity_from_str(s: &str) -> Severity {
    match s {
        "error" => Severity::High,
        "warning" => Severity::Medium,
        "info" => Severity::Low,
        "hint" => Severity::Info,
        _ => Severity::Medium,
    }
}

/// `call: <name>` or `callRegex: <pattern>` — same shape as
/// `taint_rules::PatternYaml`, duplicated rather than shared across the
/// two loader modules since each is a small, self-contained ~5-line type
/// and a shared module for just this would be more indirection than the
/// two call sites justify.
#[derive(Debug, Deserialize)]
struct PatternYaml {
    call: Option<String>,
    #[serde(rename = "callRegex")]
    call_regex: Option<String>,
}

impl PatternYaml {
    fn into_name_pattern(self) -> anyhow::Result<NamePattern> {
        match (self.call, self.call_regex) {
            (Some(name), None) => Ok(NamePattern::suffix(name)),
            (None, Some(pattern)) => NamePattern::regex(&pattern).map_err(|e| anyhow::anyhow!("invalid callRegex {pattern:?}: {e}")),
            (None, None) => anyhow::bail!("an after/unless/before entry needs either `call` or `callRegex`"),
            (Some(_), Some(_)) => anyhow::bail!("an after/unless/before entry can't set both `call` and `callRegex`"),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdentityYaml {
    #[serde(default)]
    receiver: bool,
    collection_argument: Option<usize>,
    key_argument: usize,
}

impl IdentityYaml {
    fn into_selector(self) -> anyhow::Result<EntryIdentity> {
        let collection = match (self.receiver, self.collection_argument) {
            (true, None) => CollectionSelector::Receiver,
            (false, Some(index)) if index != self.key_argument => CollectionSelector::Argument(index),
            (false, Some(_)) => anyhow::bail!("identity collectionArgument and keyArgument must select different arguments"),
            _ => anyhow::bail!("identity needs exactly one of receiver: true or collectionArgument"),
        };
        Ok(EntryIdentity { collection, key_argument: self.key_argument })
    }
}

#[derive(Debug, Deserialize)]
struct CallOrderRuleYaml {
    #[serde(flatten)]
    common: RuleMeta,
    #[serde(default)]
    language: String,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    after: Vec<PatternYaml>,
    #[serde(default)]
    unless: Vec<PatternYaml>,
    #[serde(default)]
    before: Vec<PatternYaml>,
    #[serde(default, rename = "checkBeforeReturn")]
    check_before_return: bool,
    identity: Option<IdentityYaml>,
}

pub type CallOrderRuleDef = super::dataflow_check::DataflowRuleDef<CallOrderSpec>;

fn parse_call_order_rule(contents: &str) -> Option<CallOrderRuleDef> {
    let yaml: CallOrderRuleYaml = serde_yaml::from_str(contents).ok()?;
    if yaml.common.kind != "call-sequence" {
        return None;
    }

    let after: Vec<NamePattern> = yaml.after.into_iter().filter_map(|p| p.into_name_pattern().ok()).collect();
    let unless: Vec<NamePattern> = yaml.unless.into_iter().filter_map(|p| p.into_name_pattern().ok()).collect();
    let before: Vec<NamePattern> = yaml.before.into_iter().filter_map(|p| p.into_name_pattern().ok()).collect();

    Some(CallOrderRuleDef {
        id: yaml.common.id.clone(),
        language: yaml.language,
        category: yaml.common.category,
        severity: severity_from_str(&yaml.severity),
        message: yaml.message,
        spec: CallOrderSpec {
            rule_id: yaml.common.id,
            after,
            unless,
            before,
            check_before_return: yaml.check_before_return,
            identity: yaml.identity.map(IdentityYaml::into_selector).transpose().ok()?,
        },
        pack_id: None,
        metadata: yaml.common.metadata,
    })
}

/// Uses the runtime identity schema when validating a rule pack.
pub fn validate_identity(contents: &str) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct IdentityBlock {
        identity: Option<IdentityYaml>,
    }
    let block: IdentityBlock = serde_yaml::from_str(contents)?;
    block.identity.map(IdentityYaml::into_selector).transpose()?;
    Ok(())
}

/// Every `kind: call-sequence` rule declared in `rules-builtin/` plus any
/// registered pack, across all languages — same "callers filter by
/// `language` themselves" convention as `taint_rules::load_taint_rules`.
pub fn load_call_order_rules(registered_packs: &[ResolvedRulePack]) -> Vec<CallOrderRuleDef> {
    let roots = rule_roots(registered_packs);
    let provenance = pack_rule_provenance(registered_packs);
    let mut defs = Vec::new();
    walk_rule_contents(&roots, &mut |contents| {
        if let Some(mut def) = parse_call_order_rule(contents) {
            def.pack_id = provenance.get(&def.id).cloned();
            defs.push(def);
        }
    });
    defs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_call_sequence_rule() {
        let yaml = r#"
id: test-xxe
language: Java
category: security
severity: warning
kind: call-sequence
message: "unsafe XML parser"
after:
  - call: newInstance
unless:
  - call: setFeature
before:
  - call: parse
"#;
        let def = parse_call_order_rule(yaml).unwrap();
        assert_eq!(def.id, "test-xxe");
        assert_eq!(def.language, "Java");
        assert!(!def.spec.check_before_return);
    }

    #[test]
    fn parses_check_before_return() {
        let yaml = r#"
id: test-lock
language: Java
category: correctness
severity: warning
kind: call-sequence
message: "unreleased lock"
after:
  - call: lock
unless:
  - call: unlock
checkBeforeReturn: true
"#;
        let def = parse_call_order_rule(yaml).unwrap();
        assert!(def.spec.check_before_return);
        assert!(def.spec.before.is_empty());
    }

    #[test]
    fn validates_identity_selectors_consistently_with_rule_loading() {
        let rule = "id: sample\nkind: call-sequence\ncategory: correctness\n";
        let valid = [
            ("{receiver: true, keyArgument: 0}", CollectionSelector::Receiver, 0),
            ("{collectionArgument: 0, keyArgument: 2}", CollectionSelector::Argument(0), 2),
        ];
        for (identity, collection, key_argument) in valid {
            let contents = format!("{rule}identity: {identity}\n");
            assert!(validate_identity(&contents).is_ok());
            assert_eq!(parse_call_order_rule(&contents).unwrap().spec.identity, Some(EntryIdentity { collection, key_argument }));
        }
        for identity in [
            "{receiver: true, collectionArgument: 0, keyArgument: 1}",
            "{collectionArgument: 1, keyArgument: 1}",
            "{keyArgument: 0}",
            "{receiver: true}",
            "{receiver: true, keyArgument: -1}",
            "{receiver: true, keyArgument: 0, unknown: true}",
        ] {
            let contents = format!("{rule}identity: {identity}\n");
            assert!(validate_identity(&contents).is_err(), "{identity}");
            assert!(parse_call_order_rule(&contents).is_none(), "{identity}");
        }
    }

    #[test]
    fn returns_none_for_a_non_call_sequence_rule() {
        let yaml = "id: unrelated\nkind: taint\ncategory: security\n";
        assert!(parse_call_order_rule(yaml).is_none());
    }
}

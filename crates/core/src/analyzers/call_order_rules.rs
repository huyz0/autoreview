//! Loads function-wide and keyed call-sequence rules from builtin and registered packs.

use serde::Deserialize;

use autoreview_dataflow::call_order::{CallOrderSpec, CollectionSelector, ContractLanguage, ContractMode, EntryIdentity, ResultContract};
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
#[serde(rename_all = "kebab-case")]
enum ContractModeYaml {
    MustUse,
    MustComplete,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractYaml {
    mode: ContractModeYaml,
    import: String,
    methods: Vec<String>,
    #[serde(default)]
    borrows: Vec<String>,
}

impl ContractYaml {
    fn resolve(self, language: &str) -> anyhow::Result<ResultContract> {
        let language = match language {
            "Go" => ContractLanguage::Go,
            "Java" => ContractLanguage::Java,
            _ => anyhow::bail!("call-sequence contracts currently support Go and Java"),
        };
        let mode = match self.mode {
            ContractModeYaml::MustUse => ContractMode::MustUse,
            ContractModeYaml::MustComplete => ContractMode::MustComplete,
        };
        anyhow::ensure!(!self.import.trim().is_empty(), "contract import must name the package or class");
        anyhow::ensure!(!self.methods.is_empty() && self.methods.iter().chain(&self.borrows).all(|name| {
            !name.is_empty() && name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        }), "contract methods and borrows must contain literal method names");
        Ok(ResultContract { mode, language, import: self.import, methods: self.methods, borrows: self.borrows })
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
    contract: Option<ContractYaml>,
}

pub type CallOrderRuleDef = super::dataflow_check::DataflowRuleDef<CallOrderSpec>;

fn parse_call_order_rule(contents: &str) -> Option<CallOrderRuleDef> {
    let yaml: CallOrderRuleYaml = serde_yaml::from_str(contents).ok()?;
    if yaml.common.kind != "call-sequence" {
        return None;
    }

    let yaml = validate_shape(yaml).ok()?;
    let check_before_return = yaml.contract.as_ref().is_some_and(|contract| matches!(contract.mode, ContractModeYaml::MustComplete))
        || yaml.check_before_return;
    let contract = yaml.contract.map(|contract| contract.resolve(&yaml.language)).transpose().ok()?;
    let patterns = |values: Vec<PatternYaml>| -> Option<Vec<NamePattern>> {
        values.into_iter().map(|entry| entry.into_name_pattern().ok()).collect()
    };
    let after = patterns(yaml.after)?;
    let unless = patterns(yaml.unless)?;
    let before = patterns(yaml.before)?;

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
            check_before_return,
            identity: yaml.identity.map(IdentityYaml::into_selector).transpose().ok()?,
            contract,
        },
        pack_id: None,
        metadata: yaml.common.metadata,
    })
}

fn validate_shape(yaml: CallOrderRuleYaml) -> anyhow::Result<CallOrderRuleYaml> {
    if let Some(contract) = &yaml.contract {
        anyhow::ensure!(yaml.identity.is_none() && yaml.after.is_empty() && yaml.before.is_empty() && !yaml.check_before_return,
            "contract replaces identity, after, before, and checkBeforeReturn");
        match contract.mode {
            ContractModeYaml::MustUse => anyhow::ensure!(yaml.unless.is_empty() && contract.borrows.is_empty(),
                "must-use does not take unless or borrows"),
            ContractModeYaml::MustComplete => anyhow::ensure!(!yaml.unless.is_empty(),
                "must-complete requires an unless list of completion methods"),
        }
    } else {
        anyhow::ensure!(!yaml.after.is_empty(), "call-sequence requires a nonempty after list");
        anyhow::ensure!(!yaml.unless.is_empty(), "call-sequence requires a nonempty unless list");
    }
    Ok(yaml)
}

pub fn validate_call_sequence(contents: &str) -> anyhow::Result<()> {
    let rule = validate_shape(serde_yaml::from_str(contents)?)?;
    if let Some(identity) = rule.identity {
        identity.into_selector()?;
    }
    if let Some(contract) = rule.contract {
        contract.resolve(&rule.language)?;
    }
    for pattern in rule.after.into_iter().chain(rule.unless).chain(rule.before) {
        pattern.into_name_pattern()?;
    }
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
        let rule = "id: sample\nkind: call-sequence\ncategory: correctness\nafter: [{call: add}]\nunless: [{call: remove}]\n";
        let valid = [
            ("{receiver: true, keyArgument: 0}", CollectionSelector::Receiver, 0),
            ("{collectionArgument: 0, keyArgument: 2}", CollectionSelector::Argument(0), 2),
        ];
        for (identity, collection, key_argument) in valid {
            let contents = format!("{rule}identity: {identity}\n");
            assert!(validate_call_sequence(&contents).is_ok());
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
            assert!(validate_call_sequence(&contents).is_err(), "{identity}");
            assert!(parse_call_order_rule(&contents).is_none(), "{identity}");
        }
    }

    #[test]
    fn validates_contract_mode_and_completion_methods() {
        let required = "id: demo\nkind: call-sequence\nlanguage: Go\ncategory: correctness\n";
        let source = "  import: os\n  methods: [Rename]\n";
        let check = |mode, extensions| {
            let data = format!("{required}contract:\n  mode: {mode}\n{source}{extensions}");
            assert_eq!(validate_call_sequence(&data).is_ok(), parse_call_order_rule(&data).is_some());
            validate_call_sequence(&data).is_ok()
        };
        assert!(check("must-use", ""));
        assert!(!check("must-use", "unless: [{call: Close}]\n"));
        assert!(!check("must-complete", ""));
        assert!(check("must-complete", "unless: [{call: Close}]\n"));
        assert!(!check("must-complete", "unless: [{callRegex: '['}]\n"));
    }

    #[test]
    fn returns_none_for_a_non_call_sequence_rule() {
        let yaml = "id: unrelated\nkind: taint\ncategory: security\n";
        assert!(parse_call_order_rule(yaml).is_none());
    }
}

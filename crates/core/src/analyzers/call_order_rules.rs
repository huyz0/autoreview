//! Loads `kind: call-sequence` rule files out of the same `rules-builtin/`
//! tree `ast_grep.rs`/`taint_rules.rs` load from — see those modules' own
//! docs for why one directory serves multiple execution backends. This
//! backend runs `autoreview_dataflow::call_order`, the "did an `after`
//! call happen without an intervening `unless` call before a `before`
//! call or a `return`" primitive — see that module's own doc comment for
//! exactly what gap this closes (XXE, unreleased locks) that the taint
//! engine's data-flow model structurally can't express.

use serde::Deserialize;

use autoreview_dataflow::call_order::CallOrderSpec;
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
}

pub struct CallOrderRuleDef {
    pub id: String,
    pub language: String,
    pub category: String,
    pub severity: Severity,
    /// May contain a `{trigger_call}` placeholder, substituted by the
    /// caller from a `CallOrderHit`.
    pub message: String,
    pub spec: CallOrderSpec,
    pub pack_id: Option<String>,
    /// The rule's own `metadata:` block (CWE/OWASP/confidence). Carried
    /// here so it can reach `AgentFinding.meta` — `RuleMetadataBlock`'s
    /// docs promise it "flows verbatim" into findings, which was true for
    /// pattern rules but silently false here: four builtin call-sequence
    /// rules declare CWE mappings that were parsed and then dropped.
    pub metadata: Option<super::ast_grep::RuleMetadataBlock>,
}

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
        spec: CallOrderSpec { rule_id: yaml.common.id, after, unless, before, check_before_return: yaml.check_before_return },
        pack_id: None,
        metadata: yaml.common.metadata,
    })
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
    fn returns_none_for_a_non_call_sequence_rule() {
        let yaml = "id: unrelated\nkind: taint\ncategory: security\n";
        assert!(parse_call_order_rule(yaml).is_none());
    }
}

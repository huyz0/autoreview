//! Loads `kind: taint` rule files out of the same `rules-builtin/` tree
//! `ast_grep.rs` embeds — see that module's `RuleMeta`/`extract_pattern_rules`
//! docs for why one directory serves multiple execution backends. This
//! module is the taint backend's own loader: it deserializes each file's
//! `sources:`/`sinks:`/`sanitizers:` body into an
//! `autoreview_dataflow::taint::TaintSpec` and pairs it with the common
//! `id`/`language`/`category`/`severity`/`message` fields
//! `crates/core/src/analyzers/dataflow_check.rs` needs to build findings.

use serde::Deserialize;

use autoreview_dataflow::taint::{NamePattern, TaintSink, TaintSpec};
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

/// `call: <name>` or `callRegex: <pattern>` — exactly one of the two.
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
            (None, None) => anyhow::bail!("a source/sink/sanitizer entry needs either `call` or `callRegex`"),
            (Some(_), Some(_)) => anyhow::bail!("a source/sink/sanitizer entry can't set both `call` and `callRegex`"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct SinkYaml {
    #[serde(flatten)]
    pattern: PatternYaml,
    #[serde(rename = "argPositions")]
    arg_positions: Option<Vec<usize>>,
}

#[derive(Debug, Deserialize)]
struct TaintRuleYaml {
    #[serde(flatten)]
    common: RuleMeta,
    #[serde(default)]
    language: String,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    sources: Vec<PatternYaml>,
    #[serde(default)]
    sinks: Vec<SinkYaml>,
    #[serde(default)]
    sanitizers: Vec<PatternYaml>,
}

/// A loaded `kind: taint` rule, ready to hand to
/// `autoreview_dataflow::taint::check`.
pub struct TaintRuleDef {
    pub id: String,
    pub language: String,
    pub category: String,
    pub severity: Severity,
    /// May contain `{tainted_arg}`/`{sink_call}` placeholders, substituted
    /// by the caller from a `TaintHit`.
    pub message: String,
    pub spec: TaintSpec,
    /// `Some(packId)` when this rule came from a registered pack rather
    /// than the embedded builtin tree — see `ast_grep::pack_rule_provenance`.
    pub pack_id: Option<String>,
}

fn parse_taint_rule(contents: &str) -> Option<TaintRuleDef> {
    let yaml: TaintRuleYaml = serde_yaml::from_str(contents).ok()?;
    if yaml.common.kind != "taint" {
        return None;
    }

    let sources: Vec<NamePattern> = yaml.sources.into_iter().filter_map(|p| p.into_name_pattern().ok()).collect();
    let sinks: Vec<TaintSink> = yaml
        .sinks
        .into_iter()
        .filter_map(|s| {
            let call = s.pattern.into_name_pattern().ok()?;
            Some(TaintSink { call, tainted_arg_positions: s.arg_positions })
        })
        .collect();
    let sanitizers: Vec<NamePattern> = yaml.sanitizers.into_iter().filter_map(|p| p.into_name_pattern().ok()).collect();

    Some(TaintRuleDef {
        id: yaml.common.id.clone(),
        language: yaml.language,
        category: yaml.common.category,
        severity: severity_from_str(&yaml.severity),
        message: yaml.message,
        spec: TaintSpec { rule_id: yaml.common.id, sources, sinks, sanitizers },
        pack_id: None,
    })
}

/// Every `kind: taint` rule declared in `rules-builtin/` plus any
/// registered pack, across all languages — callers filter by `language`
/// themselves (see `dataflow_check.rs::run_dataflow_check`), the same way
/// `run_ast_grep` leaves language dispatch to file extension rather than
/// filtering here.
pub fn load_taint_rules(registered_packs: &[ResolvedRulePack]) -> Vec<TaintRuleDef> {
    let roots = rule_roots(registered_packs);
    let provenance = pack_rule_provenance(registered_packs);
    let mut defs = Vec::new();
    walk_rule_contents(&roots, &mut |contents| {
        if let Some(mut def) = parse_taint_rule(contents) {
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
    fn loads_the_three_builtin_go_taint_rules() {
        let rules = load_taint_rules(&[]);
        let ids: Vec<&str> = rules.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"go-command-injection-taint"), "got: {ids:?}");
        assert!(ids.contains(&"go-sql-injection-taint"), "got: {ids:?}");
        assert!(ids.contains(&"go-path-traversal-taint"), "got: {ids:?}");
    }

    #[test]
    fn a_pattern_kind_rule_is_not_loaded_as_a_taint_rule() {
        let rules = load_taint_rules(&[]);
        assert!(!rules.iter().any(|r| r.id == "go-weak-hash"), "a plain ast-grep rule must not parse as a taint rule");
    }

    #[test]
    fn command_injection_taint_rule_has_the_expected_sinks() {
        let rules = load_taint_rules(&[]);
        let rule = rules.iter().find(|r| r.id == "go-command-injection-taint").expect("rule should load");
        assert_eq!(rule.category, "security");
        assert_eq!(rule.severity, Severity::High);
        assert_eq!(rule.spec.sinks.len(), 5);
        assert_eq!(rule.spec.sources.len(), 2);
    }

    #[test]
    fn parse_taint_rule_rejects_a_call_and_call_regex_together() {
        let contents = "id: bad\nkind: taint\nsources:\n  - call: FormValue\n    callRegex: \".*\"\nsinks: []\n";
        // `into_name_pattern` returning Err for the ambiguous entry means
        // it's silently dropped from `sources` rather than failing the
        // whole rule load — verify the rule still loads but with 0 sources
        // rather than panicking or including a bogus pattern.
        let def = parse_taint_rule(contents).expect("rule should still parse structurally");
        assert!(def.spec.sources.is_empty(), "the ambiguous call+callRegex entry should be dropped, not included");
    }

    #[test]
    fn a_builtin_taint_rule_has_no_pack_id() {
        let rules = load_taint_rules(&[]);
        let rule = rules.iter().find(|r| r.id == "go-command-injection-taint").expect("rule should load");
        assert!(rule.pack_id.is_none());
    }

    #[test]
    fn a_pack_sourced_taint_rule_carries_its_pack_id() {
        let root = tempfile::tempdir().unwrap();
        let pack_dir = root.path().join("pack");
        std::fs::create_dir_all(&pack_dir).unwrap();
        std::fs::write(pack_dir.join("rulepack.yaml"), "id: acme-taint\nversion: \"1.0.0\"\n").unwrap();
        std::fs::write(
            pack_dir.join("env-taint.yml"),
            "id: acme-env-taint\nkind: taint\nlanguage: Go\ncategory: security\nseverity: error\nmessage: m\nsources:\n  - call: Getenv\nsinks:\n  - call: Println\nsanitizers: []\n",
        )
        .unwrap();
        let packs = vec![crate::rule_packs::ResolvedRulePack { id: "acme-taint".to_string(), local_path: pack_dir, trust: autoreview_schema::RulePackTrust::Full }];

        let rules = load_taint_rules(&packs);
        let rule = rules.iter().find(|r| r.id == "acme-env-taint").expect("pack rule should load");
        assert_eq!(rule.pack_id.as_deref(), Some("acme-taint"));
    }
}

//! Loads `kind: threshold` rule files out of the same `rules-builtin/`
//! tree `ast_grep.rs` embeds and `taint_rules.rs` mines for `kind: taint`
//! — see those modules' docs for why one directory serves multiple
//! execution backends. A threshold rule externalizes a numeric limit for
//! a metric `complexity.rs` already computes internally (it doesn't
//! invent new computation); this module's job is just resolving those
//! limits from YAML into a `complexity::ComplexityThresholds` the
//! existing check functions read instead of a hardcoded Rust constant.
//!
//! Every bare `metric > threshold` comparison in `complexity.rs` is
//! YAML-configurable this way, including `data-class`/`utility-class-
//! public-constructor`'s own accessor/static-method-count floors (see
//! `complexity::ComplexityThresholds`'s own docs for why those two still
//! carry surrounding hardcoded Rust logic despite the numeric limit
//! itself being YAML-configurable).

use std::collections::HashMap;

use serde::Deserialize;

use autoreview_schema::Severity;

use super::ast_grep::{pack_rule_provenance, rule_roots, walk_rule_contents, RuleMeta};
use super::complexity::ComplexityThresholds;
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

#[derive(Debug, Deserialize)]
struct ThresholdRuleYaml {
    #[serde(flatten)]
    common: RuleMeta,
    #[serde(default)]
    severity: String,
    metric: String,
    threshold: usize,
}

pub struct ThresholdRuleDef {
    pub id: String,
    pub metric: String,
    pub threshold: usize,
    #[allow(dead_code)] // not yet consumed — complexity.rs's own make_finding still hardcodes category/severity, matching this phase's stated scope (threshold value only)
    pub category: String,
    #[allow(dead_code)]
    pub severity: Severity,
    /// `Some(packId)` when this rule came from a registered pack rather
    /// than the embedded builtin tree — keyed by the rule's own `id`
    /// (looked up via `ast_grep::pack_rule_provenance`), NOT `metric`,
    /// since a pack can name its rule anything while still overriding a
    /// builtin metric name (e.g. `id: acme-tight-cyclomatic-complexity`,
    /// `metric: cyclomatic-complexity`).
    pub pack_id: Option<String>,
}

fn parse_threshold_rule(contents: &str) -> Option<ThresholdRuleDef> {
    let yaml: ThresholdRuleYaml = serde_yaml::from_str(contents).ok()?;
    if yaml.common.kind != "threshold" {
        return None;
    }
    Some(ThresholdRuleDef { id: yaml.common.id.clone(), metric: yaml.metric, threshold: yaml.threshold, category: yaml.common.category, severity: severity_from_str(&yaml.severity), pack_id: None })
}

/// Every `kind: threshold` rule declared in `rules-builtin/` plus any
/// registered pack.
pub fn load_threshold_rules(registered_packs: &[ResolvedRulePack]) -> Vec<ThresholdRuleDef> {
    let roots = rule_roots(registered_packs);
    let provenance = pack_rule_provenance(registered_packs);
    let mut defs = Vec::new();
    walk_rule_contents(&roots, &mut |contents| {
        if let Some(mut def) = parse_threshold_rule(contents) {
            def.pack_id = provenance.get(&def.id).cloned();
            defs.push(def);
        }
    });
    defs
}

/// Builds a `ComplexityThresholds` starting from its defaults, overridden
/// per matching `metric:` name by any loaded `kind: threshold` rule — a
/// metric with no matching rule keeps its default, so this never panics
/// or leaves a threshold unset. Also returns a `metric -> packId` map for
/// provenance tagging (only present for metrics a registered pack, not
/// the builtin tree, overrode).
pub fn resolve_complexity_thresholds_with_provenance(registered_packs: &[ResolvedRulePack]) -> (ComplexityThresholds, HashMap<String, String>) {
    let mut thresholds = ComplexityThresholds::default();
    let mut pack_ids_by_metric = HashMap::new();
    for rule in load_threshold_rules(registered_packs) {
        if let Some(pack_id) = &rule.pack_id {
            pack_ids_by_metric.insert(rule.metric.clone(), pack_id.clone());
        }
        match rule.metric.as_str() {
            "cyclomatic-complexity" => thresholds.cyclomatic_complexity = rule.threshold,
            "too-many-returns" => thresholds.too_many_returns = rule.threshold,
            "cognitive-complexity" => thresholds.cognitive_complexity = rule.threshold,
            "long-method" => thresholds.long_method = rule.threshold,
            "long-parameter-list" => thresholds.long_parameter_list = rule.threshold,
            "deep-nesting" => thresholds.deep_nesting = rule.threshold,
            "god-class" => thresholds.god_class = rule.threshold,
            "large-switch" => thresholds.large_switch = rule.threshold,
            "complex-interface" => thresholds.complex_interface = rule.threshold,
            "data-class-min-accessors" => thresholds.data_class_min_accessors = rule.threshold,
            "utility-class-min-static-methods" => thresholds.utility_class_min_static_methods = rule.threshold,
            _ => {}
        }
    }
    (thresholds, pack_ids_by_metric)
}

/// `resolve_complexity_thresholds_with_provenance` without the provenance
/// map, for callers (most existing tests, and any caller not tagging
/// findings with pack ids) that only need the resolved thresholds.
pub fn resolve_complexity_thresholds(registered_packs: &[ResolvedRulePack]) -> ComplexityThresholds {
    resolve_complexity_thresholds_with_provenance(registered_packs).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_the_builtin_threshold_rules() {
        let rules = load_threshold_rules(&[]);
        let ids: Vec<&str> = rules.iter().map(|r| r.id.as_str()).collect();
        for expected in ["cyclomatic-complexity", "too-many-returns", "cognitive-complexity", "long-method", "long-parameter-list", "deep-nesting", "god-class", "large-switch", "complex-interface"] {
            assert!(ids.contains(&expected), "got: {ids:?}");
        }
    }

    #[test]
    fn a_taint_kind_rule_is_not_loaded_as_a_threshold_rule() {
        let rules = load_threshold_rules(&[]);
        assert!(!rules.iter().any(|r| r.id == "go-command-injection-taint"), "a kind: taint rule must not parse as kind: threshold");
    }

    #[test]
    fn resolve_complexity_thresholds_reflects_the_builtin_yaml_values() {
        let thresholds = resolve_complexity_thresholds(&[]);
        // Matches cyclomatic-complexity.yml / too-many-returns.yml's
        // declared `threshold:` values — if those files change, this
        // test's expectation should change with them, on purpose (it's
        // proving the YAML is actually read, not asserting a specific
        // number is sacred).
        assert_eq!(thresholds.cyclomatic_complexity, 10);
        assert_eq!(thresholds.too_many_returns, 4);
    }

    #[test]
    fn a_builtin_threshold_rule_has_no_pack_id() {
        let rules = load_threshold_rules(&[]);
        let rule = rules.iter().find(|r| r.id == "cyclomatic-complexity").expect("rule should load");
        assert!(rule.pack_id.is_none());
    }

    #[test]
    fn a_pack_sourced_threshold_rule_carries_its_pack_id_keyed_by_metric_not_id() {
        // The pack rule's own id differs from the metric it overrides,
        // exactly the case pack_ids_by_metric (keyed by metric, built from
        // pack_id looked up by id) needs to handle correctly.
        let root = tempfile::tempdir().unwrap();
        let pack_dir = root.path().join("pack");
        std::fs::create_dir_all(&pack_dir).unwrap();
        std::fs::write(pack_dir.join("rulepack.yaml"), "id: acme-thresholds\nversion: \"1.0.0\"\n").unwrap();
        std::fs::write(
            pack_dir.join("tight.yml"),
            "id: acme-tight-cyclomatic-complexity\nkind: threshold\nlanguage: Go\ncategory: correctness\nseverity: warning\nmetric: cyclomatic-complexity\nthreshold: 2\nmessage: m\n",
        )
        .unwrap();
        let packs = vec![crate::rule_packs::ResolvedRulePack { id: "acme-thresholds".to_string(), local_path: pack_dir, trust: autoreview_schema::RulePackTrust::Full }];

        let (thresholds, pack_ids_by_metric) = resolve_complexity_thresholds_with_provenance(&packs);
        assert_eq!(thresholds.cyclomatic_complexity, 2, "the pack's threshold value should win");
        assert_eq!(pack_ids_by_metric.get("cyclomatic-complexity").map(String::as_str), Some("acme-thresholds"));
    }
}

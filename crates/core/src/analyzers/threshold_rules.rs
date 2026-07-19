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
//! YAML-configurable this way (see `complexity::ComplexityThresholds`'s own
//! docs for the two — `data-class`/`utility-class-public-constructor` —
//! that stay hardcoded because they're entangled with more than a single
//! numeric limit).

use serde::Deserialize;

use autoreview_schema::Severity;

use super::ast_grep::{walk_rule_contents, RuleMeta, BUILTIN_RULES};
use super::complexity::ComplexityThresholds;

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
}

fn parse_threshold_rule(contents: &str) -> Option<ThresholdRuleDef> {
    let yaml: ThresholdRuleYaml = serde_yaml::from_str(contents).ok()?;
    if yaml.common.kind != "threshold" {
        return None;
    }
    Some(ThresholdRuleDef { id: yaml.common.id.clone(), metric: yaml.metric, threshold: yaml.threshold, category: yaml.common.category, severity: severity_from_str(&yaml.severity) })
}

/// Every `kind: threshold` rule declared in `rules-builtin/`.
pub fn load_threshold_rules() -> Vec<ThresholdRuleDef> {
    let mut defs = Vec::new();
    walk_rule_contents(&BUILTIN_RULES, &mut |contents| {
        if let Some(def) = parse_threshold_rule(contents) {
            defs.push(def);
        }
    });
    defs
}

/// Builds a `ComplexityThresholds` starting from its defaults, overridden
/// per matching `metric:` name by any loaded `kind: threshold` rule — a
/// metric with no matching rule keeps its default, so this never panics
/// or leaves a threshold unset.
pub fn resolve_complexity_thresholds() -> ComplexityThresholds {
    let mut thresholds = ComplexityThresholds::default();
    for rule in load_threshold_rules() {
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
            _ => {}
        }
    }
    thresholds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_the_builtin_threshold_rules() {
        let rules = load_threshold_rules();
        let ids: Vec<&str> = rules.iter().map(|r| r.id.as_str()).collect();
        for expected in ["cyclomatic-complexity", "too-many-returns", "cognitive-complexity", "long-method", "long-parameter-list", "deep-nesting", "god-class", "large-switch", "complex-interface"] {
            assert!(ids.contains(&expected), "got: {ids:?}");
        }
    }

    #[test]
    fn a_taint_kind_rule_is_not_loaded_as_a_threshold_rule() {
        let rules = load_threshold_rules();
        assert!(!rules.iter().any(|r| r.id == "go-command-injection-taint"), "a kind: taint rule must not parse as kind: threshold");
    }

    #[test]
    fn resolve_complexity_thresholds_reflects_the_builtin_yaml_values() {
        let thresholds = resolve_complexity_thresholds();
        // Matches cyclomatic-complexity.yml / too-many-returns.yml's
        // declared `threshold:` values — if those files change, this
        // test's expectation should change with them, on purpose (it's
        // proving the YAML is actually read, not asserting a specific
        // number is sacred).
        assert_eq!(thresholds.cyclomatic_complexity, 10);
        assert_eq!(thresholds.too_many_returns, 4);
    }
}

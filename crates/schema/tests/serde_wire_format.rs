//! Wire-format tests for the schema crate. These exist because every struct
//! in this crate got a blanket `#[serde(rename_all = "camelCase")]` pass
//! applied mechanically (to match the plan's documented JSON field names
//! like `runId`, `schemaVersion`) — that rename was never directly verified
//! before this suite, only indirectly through core/cli code that happened
//! to keep working. A wrong or missing rename here would silently produce
//! `report.json` files with the wrong field names.

use autoreview_schema::{
    AgentFinding, AutoreviewConfig, Finding, FindingFingerprints, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side,
};

fn sample_finding() -> Finding {
    Finding {
        id: "f-abc123".into(),
        fingerprints: FindingFingerprints { primary: "abc123".into(), secondary: None },
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "ast-grep".into(), rule_id: Some("go-no-self-comparison".into()), aspect: None, backend: None },
        category: "correctness".into(),
        severity: Severity::Medium,
        confidence: 1.0,
        title: "t".into(),
        message: "m".into(),
        location: Location { path: "a.go".into(), range: LocationRange { start_line: 1, start_col: None, end_line: None, end_col: None }, snippet: "x".into(), side: Side::New },
        related_locations: None,
        suggestion: None,
        tags: None,
        meta: None,
    }
}

#[test]
fn finding_serializes_with_camel_case_field_names() {
    let json = serde_json::to_value(sample_finding()).unwrap();

    // Top-level: snake_case Rust field names must not leak into the wire format.
    assert!(json.get("fingerprints").is_some());
    assert!(json.get("source").is_some());
    assert!(json.get("location").is_some());

    // Nested structs are the actual risk area — this is what the mechanical
    // rename pass touched.
    assert!(json["source"].get("ruleId").is_some(), "expected camelCase 'ruleId', got: {:?}", json["source"]);
    assert!(json["source"].get("rule_id").is_none(), "snake_case 'rule_id' leaked into the wire format");
    assert!(json["location"]["range"].get("startLine").is_some());
    assert!(json["location"]["range"].get("start_line").is_none());
}

#[test]
fn finding_round_trips_through_json_without_loss() {
    let original = sample_finding();
    let json = serde_json::to_string(&original).unwrap();
    let parsed: Finding = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.id, original.id);
    assert_eq!(parsed.fingerprints.primary, original.fingerprints.primary);
    assert_eq!(parsed.source.rule_id, original.source.rule_id);
    assert_eq!(parsed.location.range.start_line, original.location.range.start_line);
    assert_eq!(parsed.severity, original.severity);
}

#[test]
fn severity_serializes_as_lowercase_strings_matching_the_plan_yaml() {
    assert_eq!(serde_json::to_value(Severity::Blocker).unwrap(), "blocker");
    assert_eq!(serde_json::to_value(Severity::High).unwrap(), "high");
    assert_eq!(serde_json::to_value(Severity::Info).unwrap(), "info");
}

#[test]
fn finding_source_kind_serializes_as_kebab_case() {
    assert_eq!(serde_json::to_value(FindingSourceKind::LearnedRule).unwrap(), "learned-rule");
}

#[test]
fn agent_finding_parses_the_exact_shape_specialists_are_told_to_emit() {
    // This is the literal example embedded in the OUTPUT_CONTRACT prompt text
    // (crates/core/src/skills/mod.rs) — if this ever stops parsing, every
    // specialist's compiled instructions are lying about the contract.
    let json = r#"{
        "source": { "kind": "agent", "tool": "claude-code", "aspect": "security" },
        "category": "security",
        "severity": "high",
        "confidence": 0.8,
        "title": "Unvalidated redirect target",
        "message": "explanation",
        "location": {
            "path": "src/redirect.ts",
            "range": { "startLine": 10, "endLine": 12 },
            "snippet": "res.redirect(target)",
            "side": "new"
        },
        "suggestion": {
            "description": "validate the target",
            "safety": "needs-review"
        }
    }"#;
    let finding: AgentFinding = serde_json::from_str(json).unwrap();
    assert_eq!(finding.source.aspect.as_deref(), Some("security"));
    assert_eq!(finding.location.range.start_line, 10);
    assert_eq!(finding.suggestion.unwrap().description, "validate the target");
}

#[test]
fn empty_config_document_parses_to_full_working_defaults() {
    // A bare `autoreview diff` in a repo with no .autoreview/config.yaml must
    // still work — this is the load-bearing guarantee behind that.
    let config: AutoreviewConfig = serde_yaml::from_str("{}").unwrap();

    assert_eq!(config.triage.tiers.quick.max_score, Some(20.0));
    assert_eq!(config.budgets.models.cheap, "haiku");
    assert_eq!(config.budgets.tiers.quick.max_agents, 1);
    assert_eq!(config.budgets.tiers.deep.max_agents, 6);
    assert!(config.triage.sensitive_paths.iter().any(|p| p == "**/auth*/**"), "sensitive path defaults should include the auth*/** directory form, not just the auth* leaf form");
    assert_eq!(config.storage.fp_block_threshold, 3);
}

#[test]
fn config_yaml_uses_camel_case_keys_matching_the_plan() {
    // The plan's own config.yaml examples use camelCase (e.g. `costClass`,
    // `maxTurns`, `totalTokenCap`). A config file authored against that
    // documentation must actually parse.
    let yaml = r#"
budgets:
  tiers:
    quick:
      maxAgents: 2
      perAgent:
        maxTurns: 5
        model: cheap
      totalTokenCap: 100000
      wallClockSec: 90
    standard:
      maxAgents: 3
      perAgent: { maxTurns: 10, model: standard }
      totalTokenCap: 400000
      wallClockSec: 480
    deep:
      maxAgents: 6
      perAgent: { maxTurns: 25, model: standard, escalationModel: deep }
      totalTokenCap: 1500000
      wallClockSec: 1800
"#;
    let config: AutoreviewConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(config.budgets.tiers.quick.max_agents, 2);
    assert_eq!(config.budgets.tiers.quick.per_agent.max_turns, 5);
    assert_eq!(config.budgets.tiers.quick.total_token_cap, 100_000);
}

#[test]
fn skill_manifest_parses_camel_case_yaml_fields() {
    let yaml = r#"
id: security
title: Security review
version: 0.1.0
categories: [security]
costClass: expensive
outputContract: findings-json-v1
triggers:
  signals: [sensitivePathHit]
"#;
    let manifest: autoreview_schema::SkillManifest = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(manifest.cost_class, autoreview_schema::CostClass::Expensive);
    assert_eq!(manifest.triggers.signals, vec!["sensitivePathHit".to_string()]);
}

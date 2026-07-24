//! Compares whatever linter/static-analysis config a repo already has
//! (`.golangci.yml`, `.eslintrc.{json,yml,yaml}`, checkstyle XML, detekt
//! YAML) against autoreview's own shipped rule catalog — deliberately
//! **not** named `mine_from_*` (matching `mine_from_code.rs`'s own
//! honest precedent for a report-only output): there's no natural
//! `CandidateSeed` this maps to, so it prints a comparison for a human
//! to read instead of feeding the mine -> draft -> bench -> shadow
//! pipeline.
//!
//! Two findings, printed with the more actionable one first:
//! - **A linter check the team explicitly disabled, that autoreview
//!   still covers** — the standing false-positive signal worth acting
//!   on: if a team turned a check off elsewhere for a reason, an
//!   autoreview rule catching the same thing is an unwanted surprise for
//!   them, not new information.
//! - **A linter check present with no obvious autoreview equivalent** —
//!   a real coverage gap, lower-confidence (the match is fuzzy either
//!   way) but still worth a human's glance.
//!
//! The disabled/covered match is fuzzy by necessity: comparing a short,
//! often-cryptic linter check name (`"errcheck"`) against a short
//! autoreview rule id (`"go-empty-error-check"`) via the same
//! `title_similarity` trigram-Jaccard `mine_candidates` already uses,
//! but at a **much lower bar** (`GAP_REPORT_SIMILARITY_THRESHOLD`, not
//! `mine::SIMILARITY_THRESHOLD`) — two short strings share fewer
//! trigrams by construction than two full prose snippets do, so the same
//! 0.55 bar that works for clustering full findings would almost never
//! fire here. A human reads this report; a modest false-positive rate
//! costs a skimmed extra line, not a wrongly-shipped rule, which is why
//! a looser bar is the right tradeoff here specifically (unlike the
//! dedup gate in `existing_rules.rs`, which reuses the strict bar because
//! it silently drops candidates rather than just listing them).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::report::dedupe::title_similarity;
use crate::rule_factory::existing_rules::ExistingRuleSummary;

const GAP_REPORT_SIMILARITY_THRESHOLD: f64 = 0.15;

#[derive(Debug, Clone, PartialEq)]
pub struct LinterCheckStatus {
    pub name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisabledMatch {
    pub linter_check: String,
    pub autoreview_rule_id: String,
    pub similarity: f64,
}

#[derive(Debug, Clone)]
pub struct ConfigGapReport {
    pub tool: String,
    pub config_path: PathBuf,
    pub disabled_matches: Vec<DisabledMatch>,
    pub uncovered_checks: Vec<String>,
}

fn normalize_for_matching(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { ' ' }).collect()
}

fn best_rule_match(check_name: &str, existing: &[ExistingRuleSummary]) -> Option<DisabledMatch> {
    let normalized_check = normalize_for_matching(check_name);
    existing
        .iter()
        .filter_map(|rule| {
            let similarity = title_similarity(&normalized_check, &normalize_for_matching(&rule.id));
            (similarity >= GAP_REPORT_SIMILARITY_THRESHOLD).then_some(DisabledMatch { linter_check: check_name.to_string(), autoreview_rule_id: rule.id.clone(), similarity })
        })
        .max_by(|a, b| a.similarity.partial_cmp(&b.similarity).unwrap_or(std::cmp::Ordering::Equal))
}

fn build_report(tool: &str, config_path: &Path, checks: &[LinterCheckStatus], existing: &[ExistingRuleSummary]) -> ConfigGapReport {
    let mut disabled_matches = Vec::new();
    let mut uncovered_checks = Vec::new();
    for check in checks {
        match best_rule_match(&check.name, existing) {
            Some(m) if !check.enabled => disabled_matches.push(m),
            Some(_) => {}
            None if check.enabled => uncovered_checks.push(check.name.clone()),
            None => {}
        }
    }
    disabled_matches.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
    ConfigGapReport { tool: tool.to_string(), config_path: config_path.to_path_buf(), disabled_matches, uncovered_checks }
}

// --- golangci-lint (.golangci.yml, v2 shape: linters.enable/linters.disable) ---

#[derive(Debug, Default, Deserialize)]
struct GolangciLinters {
    #[serde(default)]
    enable: Vec<String>,
    #[serde(default)]
    disable: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct GolangciConfig {
    #[serde(default)]
    linters: GolangciLinters,
}

/// Only reads the config's **explicit** `enable`/`disable` lists — not
/// golangci-lint's own implicit default-linter set (`govet`, `errcheck`,
/// `staticcheck`, `unused`, `ineffassign` as of v2, enabled even with an
/// empty `linters:` section). A documented, deliberate limitation:
/// replicating golangci-lint's own default-resolution logic here would
/// need to track its release-to-release default-set changes, which isn't
/// this report's job — an explicit `enable`/`disable` list is exactly the
/// team's own stated intent, the actual signal this report cares about.
fn parse_golangci_config(yaml: &str) -> Vec<LinterCheckStatus> {
    let Ok(config) = serde_yaml::from_str::<GolangciConfig>(yaml) else { return Vec::new() };
    config
        .linters
        .enable
        .into_iter()
        .map(|name| LinterCheckStatus { name, enabled: true })
        .chain(config.linters.disable.into_iter().map(|name| LinterCheckStatus { name, enabled: false }))
        .collect()
}

// --- ESLint (.eslintrc.json / .eslintrc.yml / .eslintrc.yaml) ---

/// A rule's configured severity in ESLint's own shape: a bare string/
/// number (`"error"`, `"off"`, `2`, `0`), or a 2-element array whose
/// first element is that same severity (`["error", {...options}]`).
/// `.eslintrc.js`/`.cjs` (executable JS, not data) are out of scope —
/// this project has no JS runtime to evaluate one, stated plainly rather
/// than silently under-delivering.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum EslintSeverity {
    Bare(serde_json::Value),
    WithOptions(Vec<serde_json::Value>),
}

impl EslintSeverity {
    fn is_off(&self) -> bool {
        let head = match self {
            EslintSeverity::Bare(v) => v,
            EslintSeverity::WithOptions(items) => match items.first() {
                Some(v) => v,
                None => return true,
            },
        };
        match head {
            serde_json::Value::String(s) => s == "off",
            serde_json::Value::Number(n) => n.as_i64() == Some(0),
            _ => false,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct EslintConfig {
    #[serde(default)]
    rules: std::collections::HashMap<String, EslintSeverity>,
}

fn parse_eslintrc(contents: &str, is_yaml: bool) -> Vec<LinterCheckStatus> {
    let parsed: Result<EslintConfig, _> = if is_yaml { serde_yaml::from_str(contents).map_err(|e| e.to_string()) } else { serde_json::from_str(contents).map_err(|e| e.to_string()) };
    let Ok(config) = parsed else { return Vec::new() };
    config.rules.into_iter().map(|(name, severity)| LinterCheckStatus { enabled: !severity.is_off(), name }).collect()
}

// --- Checkstyle (checkstyle.xml) ---

/// Checkstyle has no "disabled" concept the way the others do — a
/// `<module>` is either present in the file (active) or absent
/// (inactive, but then it's simply not in this list at all, not
/// distinguishable from "never considered"). Every module this finds is
/// therefore treated as `enabled: true` — this source only ever
/// contributes to `uncovered_checks`, never `disabled_matches`.
fn parse_checkstyle_config(xml: &str) -> Vec<LinterCheckStatus> {
    let Ok(doc) = roxmltree::Document::parse(xml) else { return Vec::new() };
    doc.descendants()
        .filter(|n| n.has_tag_name("module"))
        .filter_map(|n| n.attribute("name"))
        // "Checker" and "TreeWalker" are checkstyle's own required
        // structural wrapper modules, not real checks — skipping them
        // avoids two guaranteed, meaningless "uncovered" lines on every
        // real checkstyle config.
        .filter(|name| !matches!(*name, "Checker" | "TreeWalker"))
        .map(|name| LinterCheckStatus { name: name.to_string(), enabled: true })
        .collect()
}

// --- detekt (detekt.yml / config/detekt/detekt.yml) ---

#[derive(Debug, Default, Deserialize)]
struct DetektRuleConfig {
    #[serde(default = "default_true")]
    active: bool,
}

fn default_true() -> bool {
    true
}

/// detekt's config nests `<ruleSet>: {<ruleName>: {active: bool, ...}}`
/// — flattened here to plain check names (the rule set name isn't
/// carried through, since autoreview's own rule ids don't encode a
/// detekt rule-set concept to match against either).
fn parse_detekt_config(yaml: &str) -> Vec<LinterCheckStatus> {
    let Ok(top): Result<std::collections::HashMap<String, std::collections::HashMap<String, DetektRuleConfig>>, _> = serde_yaml::from_str(yaml) else { return Vec::new() };
    top.into_values().flat_map(|rules| rules.into_iter().map(|(name, cfg)| LinterCheckStatus { name, enabled: cfg.active })).collect()
}

/// `(relative config path, tool label, parser)` — one entry per
/// supported linter config file.
type LinterConfigCandidate = (&'static str, &'static str, fn(&str) -> Vec<LinterCheckStatus>);

/// Locates and parses whichever supported linter config files exist
/// under `repo_root`, returning one `ConfigGapReport` per file found —
/// a repo with no such files at all gets an empty `Vec`, not an error.
pub fn compare_linter_configs_to_rule_catalog(repo_root: &Path, existing: &[ExistingRuleSummary]) -> Vec<ConfigGapReport> {
    let mut reports = Vec::new();

    let candidates: &[LinterConfigCandidate] = &[
        (".golangci.yml", "golangci-lint", parse_golangci_config),
        (".golangci.yaml", "golangci-lint", parse_golangci_config),
        (".eslintrc.json", "eslint", |s| parse_eslintrc(s, false)),
        (".eslintrc.yml", "eslint", |s| parse_eslintrc(s, true)),
        (".eslintrc.yaml", "eslint", |s| parse_eslintrc(s, true)),
        ("config/checkstyle/checkstyle.xml", "checkstyle", parse_checkstyle_config),
        ("checkstyle.xml", "checkstyle", parse_checkstyle_config),
        ("config/detekt/detekt.yml", "detekt", parse_detekt_config),
        ("detekt.yml", "detekt", parse_detekt_config),
    ];

    for (rel_path, tool, parse) in candidates {
        let config_path = repo_root.join(rel_path);
        let Ok(contents) = std::fs::read_to_string(&config_path) else { continue };
        let checks = parse(&contents);
        if checks.is_empty() {
            continue;
        }
        reports.push(build_report(tool, &config_path, &checks, existing));
    }
    reports
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str) -> ExistingRuleSummary {
        ExistingRuleSummary { id: id.to_string(), category: "correctness".to_string(), message: String::new() }
    }

    #[test]
    fn parses_a_real_golangci_v2_config() {
        let yaml = "version: \"2\"\nlinters:\n  enable:\n    - unused\n    - gocyclo\n  disable:\n    - errcheck\n";
        let checks = parse_golangci_config(yaml);
        assert!(checks.contains(&LinterCheckStatus { name: "unused".to_string(), enabled: true }));
        assert!(checks.contains(&LinterCheckStatus { name: "errcheck".to_string(), enabled: false }));
    }

    #[test]
    fn parses_eslint_bare_off_and_error_severities() {
        let json = r#"{"rules": {"no-unused-vars": "error", "eqeqeq": "off", "no-console": 0}}"#;
        let checks = parse_eslintrc(json, false);
        assert!(checks.contains(&LinterCheckStatus { name: "no-unused-vars".to_string(), enabled: true }));
        assert!(checks.contains(&LinterCheckStatus { name: "eqeqeq".to_string(), enabled: false }));
        assert!(checks.contains(&LinterCheckStatus { name: "no-console".to_string(), enabled: false }));
    }

    #[test]
    fn parses_eslint_array_with_options_severity() {
        let json = r#"{"rules": {"quotes": ["error", "single"]}}"#;
        let checks = parse_eslintrc(json, false);
        assert_eq!(checks, vec![LinterCheckStatus { name: "quotes".to_string(), enabled: true }]);
    }

    #[test]
    fn parses_eslintrc_yaml_form() {
        let yaml = "rules:\n  no-var: error\n";
        let checks = parse_eslintrc(yaml, true);
        assert_eq!(checks, vec![LinterCheckStatus { name: "no-var".to_string(), enabled: true }]);
    }

    #[test]
    fn parses_checkstyle_modules_and_skips_structural_wrappers() {
        let xml = r#"<?xml version="1.0"?>
<module name="Checker">
    <module name="TreeWalker">
        <module name="UnusedImports"/>
        <module name="EmptyBlock"/>
    </module>
</module>"#;
        let checks = parse_checkstyle_config(xml);
        let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"UnusedImports"), "got: {names:?}");
        assert!(names.contains(&"EmptyBlock"), "got: {names:?}");
        assert!(!names.contains(&"Checker"), "got: {names:?}");
        assert!(!names.contains(&"TreeWalker"), "got: {names:?}");
        assert!(checks.iter().all(|c| c.enabled), "every checkstyle module is treated as enabled");
    }

    #[test]
    fn parses_detekt_nested_ruleset_config() {
        let yaml = "style:\n  MagicNumber:\n    active: false\n  ForbiddenComment:\n    active: true\n";
        let checks = parse_detekt_config(yaml);
        assert!(checks.contains(&LinterCheckStatus { name: "MagicNumber".to_string(), enabled: false }));
        assert!(checks.contains(&LinterCheckStatus { name: "ForbiddenComment".to_string(), enabled: true }));
    }

    #[test]
    fn detekt_check_defaults_to_active_when_the_field_is_absent() {
        let yaml = "style:\n  MagicNumber: {}\n";
        let checks = parse_detekt_config(yaml);
        assert_eq!(checks, vec![LinterCheckStatus { name: "MagicNumber".to_string(), enabled: true }]);
    }

    #[test]
    fn build_report_surfaces_a_disabled_check_matching_an_existing_rule() {
        let checks = vec![LinterCheckStatus { name: "weak-random-for-tokens".to_string(), enabled: false }];
        let existing = vec![rule("go-weak-random-for-tokens")];
        let report = build_report("golangci-lint", Path::new(".golangci.yml"), &checks, &existing);
        assert_eq!(report.disabled_matches.len(), 1, "got: {report:#?}");
        assert_eq!(report.disabled_matches[0].autoreview_rule_id, "go-weak-random-for-tokens");
        assert!(report.uncovered_checks.is_empty());
    }

    #[test]
    fn build_report_surfaces_an_enabled_check_with_no_match_as_uncovered() {
        let checks = vec![LinterCheckStatus { name: "completely-unrelated-xyz".to_string(), enabled: true }];
        let existing = vec![rule("go-weak-random-for-tokens")];
        let report = build_report("golangci-lint", Path::new(".golangci.yml"), &checks, &existing);
        assert!(report.disabled_matches.is_empty());
        assert_eq!(report.uncovered_checks, vec!["completely-unrelated-xyz".to_string()]);
    }

    #[test]
    fn compare_linter_configs_returns_empty_when_no_config_files_exist() {
        let dir = tempfile::tempdir().unwrap();
        assert!(compare_linter_configs_to_rule_catalog(dir.path(), &[]).is_empty());
    }

    #[test]
    fn compare_linter_configs_finds_and_reports_a_real_golangci_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".golangci.yml"), "version: \"2\"\nlinters:\n  disable:\n    - weak-random-for-tokens\n").unwrap();
        let existing = vec![rule("go-weak-random-for-tokens")];
        let reports = compare_linter_configs_to_rule_catalog(dir.path(), &existing);
        assert_eq!(reports.len(), 1, "got: {reports:#?}");
        assert_eq!(reports[0].tool, "golangci-lint");
        assert_eq!(reports[0].disabled_matches.len(), 1);
    }
}

//! Data-driven conformance test for every builtin ast-grep rule: each rule
//! gets a positive fixture (must fire, exactly once, with the right rule id)
//! and a negative fixture (must not fire at all). This is the same
//! self-test discipline the plan's rule-factory bench stage calls for
//! ("100% on its own positive/negative test files") applied to the builtin
//! rules we ship today — adding a new rule means adding rule YAML +
//! fixture files, not touching this test file.
//!
//! Rules are discovered by walking `rules-builtin/` at test time (not via
//! the `include_dir!` embed used at build time for the shipped binary — an
//! external integration test can just read the crate's own source tree off
//! disk). Each rule's `id`/`language` drives which fixture files it expects
//! at `tests/fixtures/rules/<rule-id>/{positive,negative}.<ext>` — a missing
//! fixture pair fails the test for that rule, so a new rule without
//! fixtures can't silently ship untested.
//!
//! Requires the real `ast-grep` binary; skips (not fails) when it's absent,
//! so this suite is safe to run anywhere but exercises real integration
//! wherever the tool is installed.

use std::path::{Path, PathBuf};
use std::process::Command;

use autoreview_core::run_ast_grep;
use serde::Deserialize;

fn default_kind() -> String {
    "pattern".to_string()
}

#[derive(Debug, Deserialize)]
struct RuleMeta {
    id: String,
    language: String,
    #[serde(default = "default_kind")]
    kind: String,
}

fn ast_grep_available() -> bool {
    Command::new("ast-grep").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn rules_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("rules-builtin")
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("rules")
}

/// Only `kind: pattern` (or `kind`-absent) rules — this test drives
/// `run_ast_grep`, which can't run a `kind: taint`/`kind: threshold` rule
/// at all (see `extract_pattern_rules`'s docs in `ast_grep.rs` for why
/// those files are invisible to the ast-grep subprocess in the first
/// place). Non-pattern rules get their own conformance test —
/// `taint_rule_conformance.rs` for `kind: taint`.
fn discover_rules() -> Vec<RuleMeta> {
    let mut rules = Vec::new();
    walk(&rules_dir(), &mut rules);
    rules.retain(|r| r.kind == "pattern");
    rules.sort_by(|a, b| a.id.cmp(&b.id));
    rules
}

fn walk(dir: &Path, out: &mut Vec<RuleMeta>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("yml") {
            continue;
        }
        let contents = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        let meta: RuleMeta = serde_yaml::from_str(&contents).unwrap_or_else(|e| panic!("failed to parse {} as rule metadata: {e}", path.display()));
        out.push(meta);
    }
}

fn extension_for_language(language: &str) -> &'static str {
    match language {
        "Go" => "go",
        "Java" => "java",
        "Kotlin" => "kt",
        "TypeScript" => "ts",
        "Tsx" => "tsx",
        "JavaScript" => "js",
        other => panic!("rule_pack_conformance doesn't know the fixture extension for language {other:?} — add it to extension_for_language"),
    }
}

fn scan_filename_for_language(language: &str) -> &'static str {
    match language {
        "Go" => "main.go",
        "Java" => "Sample.java",
        "Kotlin" => "Sample.kt",
        "TypeScript" => "sample.ts",
        "Tsx" => "Sample.tsx",
        "JavaScript" => "sample.js",
        other => panic!("rule_pack_conformance doesn't know the scan filename for language {other:?} — add it to scan_filename_for_language"),
    }
}

fn write_single_file(filename: &str, contents: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(filename), contents).unwrap();
    dir
}

#[test]
fn every_builtin_rule_fires_on_its_positive_fixture_and_stays_silent_on_its_negative_fixture() {
    if !ast_grep_available() {
        eprintln!("skipping rule_pack_conformance: ast-grep not on PATH");
        return;
    }

    let rules = discover_rules();
    assert!(!rules.is_empty(), "expected to discover at least one rule under {}", rules_dir().display());

    let mut failures = Vec::new();

    for rule in &rules {
        let ext = extension_for_language(&rule.language);
        let filename = scan_filename_for_language(&rule.language);
        let fixture_dir = fixtures_dir().join(&rule.id);
        let positive_path = fixture_dir.join(format!("positive.{ext}"));
        let negative_path = fixture_dir.join(format!("negative.{ext}"));

        let (Ok(positive), Ok(negative)) = (std::fs::read_to_string(&positive_path), std::fs::read_to_string(&negative_path)) else {
            failures.push(format!("{}: missing fixture files (expected {} and {})", rule.id, positive_path.display(), negative_path.display()));
            continue;
        };

        let positive_dir = write_single_file(filename, &positive);
        let positive_findings = run_ast_grep(positive_dir.path(), &[filename.to_string()], &[]).unwrap();
        let positive_matches: Vec<_> = positive_findings.iter().filter(|f| f.source.rule_id.as_deref() == Some(rule.id.as_str())).collect();
        if positive_matches.len() != 1 {
            failures.push(format!(
                "{}: expected exactly 1 match on positive fixture, got {} (all findings: {:?})",
                rule.id,
                positive_matches.len(),
                positive_findings.iter().map(|f| f.source.rule_id.clone()).collect::<Vec<_>>()
            ));
        }

        let negative_dir = write_single_file(filename, &negative);
        let negative_findings = run_ast_grep(negative_dir.path(), &[filename.to_string()], &[]).unwrap();
        let negative_matches: Vec<_> = negative_findings.iter().filter(|f| f.source.rule_id.as_deref() == Some(rule.id.as_str())).collect();
        if !negative_matches.is_empty() {
            failures.push(format!("{}: expected 0 matches on negative fixture, got {}", rule.id, negative_matches.len()));
        }
    }

    assert!(failures.is_empty(), "rule conformance failures ({} rule(s) checked):\n{}", rules.len(), failures.join("\n"));
}

#[test]
fn every_rule_declares_a_category() {
    for rule in discover_rules() {
        // Re-parse fully this time (discover_rules only pulls id/language) to
        // assert the category field specifically — a rule with no category
        // silently falls back to "correctness" in production, which is
        // surprising enough to want a hard test failure instead.
        let path = find_rule_file(&rule.id).unwrap_or_else(|| panic!("could not relocate rule file for {}", rule.id));
        let contents = std::fs::read_to_string(&path).unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
        assert!(value.get("category").and_then(|c| c.as_str()).is_some(), "{}: rule file has no `category` field ({})", rule.id, path.display());
    }
}

fn find_rule_file(rule_id: &str) -> Option<PathBuf> {
    fn walk_find(dir: &Path, rule_id: &str) -> Option<PathBuf> {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = walk_find(&path, rule_id) {
                    return Some(found);
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("yml") {
                if let Ok(contents) = std::fs::read_to_string(&path) {
                    if let Ok(meta) = serde_yaml::from_str::<RuleMeta>(&contents) {
                        if meta.id == rule_id {
                            return Some(path);
                        }
                    }
                }
            }
        }
        None
    }
    walk_find(&rules_dir(), rule_id)
}

/// Every `category: security` rule must carry a CWE mapping, whatever its
/// `kind`. CWE IDs are how a security finding gets triaged, mapped to
/// compliance requirements, and understood by downstream SARIF consumers,
/// so a security rule without one is materially less useful than one with
/// it. 37 rules were missing this when the guard was added; the point of
/// the guard is that the next one can't ship silently.
#[test]
fn every_security_rule_declares_a_cwe_mapping() {
    #[derive(Debug, serde::Deserialize)]
    struct SecurityRuleMeta {
        id: String,
        #[serde(default)]
        category: String,
        #[serde(default)]
        metadata: Option<MetaBlock>,
    }
    #[derive(Debug, serde::Deserialize)]
    struct MetaBlock {
        #[serde(default)]
        cwe: Vec<String>,
    }

    fn walk(dir: &Path, out: &mut Vec<(PathBuf, SecurityRuleMeta)>) {
        for entry in std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
                continue;
            }
            let is_yaml = path.extension().and_then(|e| e.to_str()).map(|e| e == "yml" || e == "yaml").unwrap_or(false);
            if !is_yaml || path.file_name().and_then(|n| n.to_str()) == Some("rulepack.yaml") {
                continue;
            }
            let contents = std::fs::read_to_string(&path).unwrap();
            if let Ok(meta) = serde_yaml::from_str::<SecurityRuleMeta>(&contents) {
                out.push((path, meta));
            }
        }
    }

    let mut rules = Vec::new();
    walk(&rules_dir(), &mut rules);
    let security: Vec<_> = rules.iter().filter(|(_, r)| r.category == "security").collect();
    assert!(!security.is_empty(), "expected the builtin pack to contain security rules");

    let missing: Vec<String> = security
        .iter()
        .filter(|(_, r)| r.metadata.as_ref().map(|m| m.cwe.is_empty()).unwrap_or(true))
        .map(|(p, r)| format!("{} ({})", r.id, p.display()))
        .collect();
    assert!(missing.is_empty(), "{} security rule(s) declare no cwe metadata:\n{}", missing.len(), missing.join("\n"));
}

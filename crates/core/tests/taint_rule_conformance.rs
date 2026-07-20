//! Conformance test for every `kind: taint` rule, parallel to
//! `rule_pack_conformance.rs`'s pattern-rule version — same fixture
//! convention (`tests/fixtures/rules/<rule-id>/{positive,negative}.<ext>`),
//! but driven through `autoreview_core::run_dataflow_check` (the real
//! end-to-end path: parse, lower, run every loaded taint spec) instead of
//! `run_ast_grep`, since a `kind: taint` rule has no `rule:` block for the
//! ast-grep subprocess to understand at all.

use std::path::{Path, PathBuf};

use autoreview_core::run_dataflow_check;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct RuleMeta {
    id: String,
    language: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    category: String,
}

fn rules_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("rules-builtin")
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("rules")
}

fn discover_taint_rules() -> Vec<RuleMeta> {
    let mut rules = Vec::new();
    walk(&rules_dir(), &mut rules);
    rules.retain(|r| r.kind == "taint");
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
        let Ok(meta) = serde_yaml::from_str::<RuleMeta>(&contents) else { continue };
        out.push(meta);
    }
}

fn extension_for_language(language: &str) -> &'static str {
    match language {
        "Go" => "go",
        "Java" => "java",
        "Kotlin" => "kt",
        other => panic!("taint_rule_conformance doesn't know the fixture extension for language {other:?} — add it to extension_for_language"),
    }
}

fn scan_filename_for_language(language: &str) -> &'static str {
    match language {
        "Go" => "main.go",
        "Java" => "Main.java",
        "Kotlin" => "Main.kt",
        other => panic!("taint_rule_conformance doesn't know the scan filename for language {other:?} — add it to scan_filename_for_language"),
    }
}

fn write_single_file(filename: &str, contents: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(filename), contents).unwrap();
    dir
}

#[test]
fn every_taint_rule_fires_on_its_positive_fixture_and_stays_silent_on_its_negative_fixture() {
    let rules = discover_taint_rules();
    assert!(!rules.is_empty(), "expected to discover at least one kind: taint rule under {}", rules_dir().display());

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
        let positive_findings = run_dataflow_check(positive_dir.path(), &[filename.to_string()], &[]);
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
        let negative_findings = run_dataflow_check(negative_dir.path(), &[filename.to_string()], &[]);
        let negative_matches: Vec<_> = negative_findings.iter().filter(|f| f.source.rule_id.as_deref() == Some(rule.id.as_str())).collect();
        if !negative_matches.is_empty() {
            failures.push(format!("{}: expected 0 matches on negative fixture, got {}", rule.id, negative_matches.len()));
        }
    }

    assert!(failures.is_empty(), "taint rule conformance failures ({} rule(s) checked):\n{}", rules.len(), failures.join("\n"));
}

#[test]
fn every_taint_rule_declares_a_category() {
    for rule in discover_taint_rules() {
        assert!(!rule.category.is_empty(), "{}: kind: taint rule has no category field", rule.id);
    }
}

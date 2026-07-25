//! Conformance test for every `kind: call-sequence` rule — same fixture
//! convention and structure as `taint_rule_conformance.rs`, duplicated
//! (not shared) for the same reason that file is its own from
//! `rule_pack_conformance.rs`: each `kind` is a distinct execution
//! backend with its own discovery filter, and the three files are small
//! enough that a shared harness would be more indirection than the
//! duplication costs.

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

fn discover_call_order_rules() -> Vec<RuleMeta> {
    let mut rules = Vec::new();
    walk(&rules_dir(), &mut rules);
    rules.retain(|r| r.kind == "call-sequence");
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
        "Java" => "java",
        "Kotlin" => "kt",
        other => panic!("call_order_rule_conformance doesn't know the fixture extension for language {other:?} — add it to extension_for_language"),
    }
}

fn scan_filename_for_language(language: &str) -> &'static str {
    match language {
        "Java" => "Main.java",
        "Kotlin" => "Main.kt",
        other => panic!("call_order_rule_conformance doesn't know the scan filename for language {other:?} — add it to scan_filename_for_language"),
    }
}

fn write_single_file(filename: &str, contents: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(filename), contents).unwrap();
    dir
}

#[test]
fn every_call_order_rule_fires_on_its_positive_fixture_and_stays_silent_on_its_negative_fixture() {
    let rules = discover_call_order_rules();
    assert!(!rules.is_empty(), "expected to discover at least one kind: call-sequence rule under {}", rules_dir().display());

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

    assert!(failures.is_empty(), "call-sequence rule conformance failures ({} rule(s) checked):\n{}", rules.len(), failures.join("\n"));
}

#[test]
fn every_call_order_rule_declares_a_category() {
    for rule in discover_call_order_rules() {
        assert!(!rule.category.is_empty(), "{}: kind: call-sequence rule has no category field", rule.id);
    }
}

/// Regression test for a real silent-drop bug: `RuleMetadataBlock`
/// documents itself as flowing "verbatim into `AgentFinding.meta`", and
/// that held for pattern rules but not for call-sequence ones — the
/// loader parsed the block and then discarded it, so the CWE mappings
/// these rules declare never reached a report. Asserts against a real
/// rule's real fixture rather than a synthetic one, so it also fails if
/// somebody strips the metadata out of the rule file itself.
#[test]
fn a_call_order_rules_declared_cwe_reaches_the_finding() {
    let rule_id = "java-xxe-unconfigured-parser";
    let rule = discover_call_order_rules().into_iter().find(|r| r.id == rule_id).expect("java-xxe-unconfigured-parser is a builtin call-sequence rule");
    let filename = scan_filename_for_language(&rule.language);

    let positive = std::fs::read_to_string(fixtures_dir().join(rule_id).join(format!("positive.{}", extension_for_language(&rule.language)))).expect("positive fixture exists");
    let dir = write_single_file(filename, &positive);
    let findings = run_dataflow_check(dir.path(), &[filename.to_string()], &[]);

    let finding = findings.iter().find(|f| f.source.rule_id.as_deref() == Some(rule_id)).expect("the positive fixture must fire this rule");
    let meta = finding.meta.as_ref().unwrap_or_else(|| panic!("{rule_id} declares a metadata block, so its finding must carry meta"));
    let cwe = meta.get("cwe").unwrap_or_else(|| panic!("{rule_id} declares cwe metadata; got meta keys {:?}", meta.keys().collect::<Vec<_>>()));
    assert_eq!(cwe, &serde_json::json!(["CWE-611"]), "the rule's declared CWE must reach the finding unchanged");
}

//! Conformance test for every `kind: call-sequence` rule — same fixture
//! convention and structure as `taint_rule_conformance.rs`, duplicated
//! (not shared) for the same reason that file is its own from
//! `rule_pack_conformance.rs`: each `kind` is a distinct execution
//! backend with its own discovery filter, and the three files are small
//! enough that a shared harness would be more indirection than the
//! duplication costs.

use std::path::{Path, PathBuf};

use autoreview_core::{run_dataflow_check, run_dataflow_check_with_coverage};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct RuleMeta {
    id: String,
    language: String,
    #[serde(default)]
    category: String,
    contract: Option<ContractMeta>,
}

#[derive(Debug, Deserialize)]
struct ContractMeta { source: Option<String> }

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("rules")
}

fn discover_call_order_rules() -> Vec<RuleMeta> {
    autoreview_core::list_builtin_rules().into_iter()
        .filter(|entry| entry.kind == "call-sequence")
        .map(|entry| {
            let definition = autoreview_core::find_rule_definition(&entry.id, &[]).unwrap();
            serde_yaml::from_str(&definition.yaml).unwrap()
        })
        .collect()
}

fn extension_for_language(language: &str) -> &'static str {
    match language {
        "Go" => "go",
        "Java" => "java",
        "Kotlin" => "kt",
        "Rust" => "rs",
        "JavaScript" => "js",
        "TypeScript" => "ts",
        "Tsx" => "tsx",
        other => panic!("missing call-sequence fixture extension for {other:?}"),
    }
}

fn scan_filename_for_language(language: &str) -> String {
    format!("Main.{}", extension_for_language(language))
}

fn write_single_file(filename: &str, contents: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(filename), contents).unwrap();
    dir
}

#[test]
fn every_call_order_rule_fires_on_its_positive_fixture_and_stays_silent_on_its_negative_fixture() {
    let rules = discover_call_order_rules();
    assert!(!rules.is_empty(), "expected built-in call-sequence rules");

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

        // Typed sources require a configured trusted checker and project setup;
        // keyed_imports tests run these fixtures with that setup.
        if matches!(rule.contract.as_ref().and_then(|contract| contract.source.as_deref()), Some("typed-promise-factory" | "typed-go-error")) {
            continue;
        }

        let positive_dir = write_single_file(&filename, &positive);
        let positive_findings = run_dataflow_check(positive_dir.path(), std::slice::from_ref(&filename), &[]);
        let positive_matches: Vec<_> = positive_findings.iter().filter(|f| f.source.rule_id.as_deref() == Some(rule.id.as_str())).collect();
        if positive_matches.len() != 1 {
            failures.push(format!(
                "{}: expected exactly 1 match on positive fixture, got {} (all findings: {:?})",
                rule.id,
                positive_matches.len(),
                positive_findings.iter().map(|f| f.source.rule_id.clone()).collect::<Vec<_>>()
            ));
        }

        let negative_dir = write_single_file(&filename, &negative);
        let negative_findings = run_dataflow_check(negative_dir.path(), std::slice::from_ref(&filename), &[]);
        let negative_matches: Vec<_> = negative_findings.iter().filter(|f| f.source.rule_id.as_deref() == Some(rule.id.as_str())).collect();
        if !negative_matches.is_empty() {
            failures.push(format!("{}: expected 0 matches on negative fixture, got {}", rule.id, negative_matches.len()));
        }
    }

    assert!(failures.is_empty(), "call-sequence rule conformance failures ({} rule(s) checked):\n{}", rules.len(), failures.join("\n"));
}

#[test]
fn completion_requires_the_same_returned_handle_across_files() {
    let factory = r#"package archive

import (
    "compress/gzip"
    "io"
)

func Create(out io.Writer) *gzip.Writer {
    return gzip.NewWriter(out)
}

func Finish(writer *gzip.Writer) {
    writer.Close()
}
"#;
    let caller = r#"package jobs

import (
    "compress/gzip"
    "example.com/project/archive"
    "io"
)

func Write(out io.Writer, other *gzip.Writer) {
    original := archive.Create(out)
    alias := original
    defer archive.Finish(other)
    alias.Write([]byte("payload"))
}
"#;
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("archive");
    std::fs::create_dir(&archive).unwrap();
    std::fs::write(root.path().join("go.mod"), "module example.com/project\n\ngo 1.22\n").unwrap();
    std::fs::write(archive.join("writer.go"), factory).unwrap();
    let path = root.path().join("jobs.go");
    std::fs::write(&path, caller).unwrap();
    let scan = || run_dataflow_check(root.path(), &["jobs.go".into()], &[]).into_iter()
        .filter(|hit| hit.source.rule_id.as_deref() == Some("go-gzip-writer-unclosed"))
        .collect::<Vec<_>>();
    let findings = scan();
    assert_eq!(findings.len(), 1, "{findings:?}");
    let related = findings[0].related_locations.as_ref().unwrap();
    assert_eq!(related[0].path, "archive/writer.go");
    assert!(related[0].snippet.contains("gzip.NewWriter"));

    std::fs::write(path, caller.replace("Finish(other)", "Finish(alias)")).unwrap();
    assert!(scan().is_empty());
}

#[test]
fn unsupported_contract_handle_storage_reports_incomplete_analysis() {
    let source = "package example\nimport (\"compress/gzip\"; \"io\")\nfunc run(out io.Writer, holder *Holder) {\n holder.writer = gzip.NewWriter(out)\n}\n";
    let root = write_single_file("main.go", source);
    let check = || autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["main.go".into()], &[]);
    let result = check();
    assert!(result.coverage.iter().any(|issue| issue.reason == "unmodeled_contract_handle"
        && issue.rule.as_deref() == Some("go-gzip-writer-unclosed") && issue.path.as_deref() == Some("main.go") && issue.line == Some(4)), "{:?}", result.coverage);
    std::fs::write(root.path().join("main.go"), "package example\nimport (\"compress/gzip\"; \"io\")\nfunc run(out io.Writer) { w := gzip.NewWriter(out); w.Close() }\n").unwrap();
    assert!(!check().coverage.iter().any(|issue| issue.reason == "unmodeled_contract_handle"));
    std::fs::write(root.path().join("main.go"), "package example\nimport \"compress/gzip\"\nfunc run(gzip Factory, holder *Holder) { holder.writer = gzip.NewWriter(nil) }\n").unwrap();
    assert!(!check().coverage.iter().any(|issue| issue.reason == "unmodeled_contract_handle"));
    std::fs::write(root.path().join("go.mod"), "module example.test/project\n\ngo 1.22\n").unwrap();
    std::fs::create_dir(root.path().join("helper")).unwrap();
    std::fs::write(root.path().join("helper/writer.go"), "package helper\nimport (\"compress/gzip\"; \"io\")\nfunc Make(out io.Writer, holder *Holder) {\n holder.writer = gzip.NewWriter(out)\n}\n").unwrap();
    std::fs::write(root.path().join("main.go"), "package example\nimport (\"example.test/project/helper\"; \"io\")\nfunc run(out io.Writer, holder *Holder) { helper.Make(out, holder) }\n").unwrap();
    let result = check();
    assert!(result.coverage.iter().any(|issue| issue.reason == "unmodeled_contract_handle"
        && issue.rule.as_deref() == Some("go-gzip-writer-unclosed") && issue.path.as_deref() == Some("helper/writer.go") && issue.line == Some(4)), "{:?}", result.coverage);
    std::fs::write(root.path().join("main.go"), "package example\nimport \"example.test/project/helper\"\nfunc run() {}\n").unwrap();
    assert!(!check().coverage.iter().any(|issue| issue.reason == "unmodeled_contract_handle"));
}

fn conditional_test_result(root: &tempfile::TempDir, path: &str) -> autoreview_core::DataflowReviewResult {
    autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &[path.into()], &[])
}

fn conditional_finding_fixture(path: &str, rule: &str, source: &str) -> tempfile::TempDir {
    let root = write_single_file(path, source);
    let result = conditional_test_result(&root, path);
    let hits: Vec<_> = result.findings.iter().filter(|hit| hit.source.rule_id.as_deref() == Some(rule)).collect();
    assert_eq!(hits.len(), 1, "{path}: {hits:#?} {:?}", result.coverage);
    assert_eq!(hits[0].location.range.start_line, 3);
    root
}

#[test]
fn discarded_conditional_bindings_do_not_consume_results() {
    for (path, rule) in [("main.js", "javascript-node-fs-discarded-promise"), ("main.ts", "typescript-node-fs-discarded-promise"), ("main.tsx", "typescript-node-fs-discarded-promise")] {
        let source = "import { rename } from 'node:fs/promises';\nexport async function run(a, b, flag) {\n const pending = rename(a, b);\n flag ? pending : null;\n}\n";
        let root = conditional_finding_fixture(path, rule, source);
        let check = || conditional_test_result(&root, path);
        let observed = source.replace("\n}\n", "\n await pending;\n}\n");
        std::fs::write(root.path().join(path), observed).unwrap();
        let result = check();
        assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
        let helper = "import { rename } from 'node:fs/promises';\nfunction ignore(value, flag) { flag ? value : null; }\nexport function run(a, b, flag) { const pending = rename(a, b); ignore(pending, flag); }\n";
        std::fs::write(root.path().join(path), helper).unwrap();
        let result = check();
        assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
        let branch = source.replace("\n}\n", "\n if (flag) await pending;\n}\n");
        std::fs::write(root.path().join(path), branch).unwrap();
        let result = check();
        assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
        let imported = "import { rename } from 'node:fs/promises';\nimport { ignore } from './helper';\nexport function run(a, b, flag) { const pending = rename(a, b); ignore(pending, flag); }\n";
        std::fs::write(root.path().join(path), imported).unwrap();
        let extension = path.rsplit('.').next().unwrap();
        std::fs::write(root.path().join(format!("helper.{extension}")), "export function ignore(value, flag) { flag ? value : null; }\n").unwrap();
        let result = check();
        assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
        std::fs::write(root.path().join(path), source.replace("flag ?", "check(flag) ?")).unwrap();
        let result = check();
        assert!(result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
        let short_circuit = "import { rename } from 'node:fs/promises';\nexport async function run(a, b, flag) {\n const pending = rename(a, b);\n flag && await pending;\n}\n";
        std::fs::write(root.path().join(path), short_circuit).unwrap();
        let result = check();
        assert!(result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
    }
}

#[test]
fn partial_result_observation_respects_correlated_guards() {
    for (path, rule) in [("main.js", "javascript-node-fs-discarded-promise"), ("main.ts", "typescript-node-fs-discarded-promise"), ("main.tsx", "typescript-node-fs-discarded-promise")] {
        let source = "import { rename } from 'node:fs/promises';\nexport async function run(a, b, flag) {\n let pending;\n if (flag) pending = rename(a, b);\n if (flag) await pending;\n}\n";
        let root = write_single_file(path, source);
        let check = || conditional_test_result(&root, path);
        let result = check();
        assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
        std::fs::write(root.path().join(path), source.replace("if (flag) await", "if (!flag) await")).unwrap();
        let result = check();
        assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
        let both = "import { rename } from 'node:fs/promises';\nexport async function run(a, b, flag) {\n const pending = rename(a, b);\n if (flag) await pending; else await pending;\n}\n";
        std::fs::write(root.path().join(path), both).unwrap();
        let result = check();
        assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
        let alias = both.replace("if (flag) await pending; else await pending;", "const alias = pending; if (flag) await alias;");
        std::fs::write(root.path().join(path), alias).unwrap();
        let result = check();
        assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
        let reassigned = source.replace("if (flag) await pending;", "flag = !flag; if (!flag) await pending;");
        std::fs::write(root.path().join(path), reassigned).unwrap();
        let result = check();
        assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
        assert!(result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
    }
}

fn assert_partial_helper_coverage(root: &tempfile::TempDir, path: &str, rule: &str) {
    let result = conditional_test_result(root, path);
    assert!(result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
}

fn assert_go_observed_and_conditional(source: &str, conditional: &str) {
    let path = "main.go";
    let rule = "go-os-rename-discarded-error";
    let root = write_single_file(path, source);
    let result = conditional_test_result(&root, path);
    assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{:?}", result.findings);
    assert!(!result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{:?}", result.coverage);
    std::fs::write(root.path().join(path), conditional).unwrap();
    let result = conditional_test_result(&root, path);
    assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)) || result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{:?} {:?}", result.findings, result.coverage);
}

#[test]
fn go_result_observed_after_callback_argument() {
    let source = "package main\nimport \"os\"\nfunc run(a, b string, flag bool) error {\n err := os.Rename(func() string { if flag { return a }; return b }(), b)\n return err\n}\n";
    assert_go_observed_and_conditional(source, &source.replace("return err", "if flag { return err }; return nil"));
}

#[test]
fn go_short_circuit_guard_observes_only_the_left_operand() {
    let source = "package main\nimport \"os\"\nfunc run(a, b string, flag bool) error {\n err := os.Rename(a, b)\n if err != nil && flag { return err }; return nil\n}\n";
    assert_go_observed_and_conditional(source, &source.replace("err != nil && flag", "flag && err != nil"));
}

fn assert_partial_helper_source(path: &str, rule: &str, source: &str) {
    let root = write_single_file(path, source);
    assert_partial_helper_coverage(&root, path, rule);
}

#[test]
fn partial_helper_observation_reports_incomplete_analysis() {
    for (path, rule) in [("main.js", "javascript-node-fs-discarded-promise"), ("main.ts", "typescript-node-fs-discarded-promise"), ("main.tsx", "typescript-node-fs-discarded-promise")] {
        let source = "import { rename } from 'node:fs/promises';\nfunction consume(value, flag) { if (flag) return value; }\nexport function run(a, b, flag) { const pending = rename(a, b); return consume(pending, flag); }\n";
        let root = write_single_file(path, source);
        assert_partial_helper_coverage(&root, path, rule);
        let direct = source.replace("const pending = rename(a, b); return consume(pending, flag);", "return consume(rename(a, b), flag);");
        std::fs::write(root.path().join(path), &direct).unwrap();
        assert_partial_helper_coverage(&root, path, rule);
        let fixed_direct = direct.replace("if (flag) return value;", "return value;");
        std::fs::write(root.path().join(path), fixed_direct).unwrap();
        let result = conditional_test_result(&root, path);
        assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
        assert!(!result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
        let fixed = source.replace("if (flag) return value;", "if (flag) return value; else return value;");
        std::fs::write(root.path().join(path), fixed).unwrap();
        let result = conditional_test_result(&root, path);
        assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
        assert!(!result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
        let observed_after = source.replace("export function run(a, b, flag) { const pending = rename(a, b); return consume(pending, flag); }", "export async function run(a, b, flag) { const pending = rename(a, b); consume(pending, flag); await pending; }");
        std::fs::write(root.path().join(path), observed_after).unwrap();
        let result = conditional_test_result(&root, path);
        assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
        assert!(!result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
        let conditional_after = source.replace("export function run(a, b, flag) { const pending = rename(a, b); return consume(pending, flag); }", "export async function run(a, b, flag) { const pending = rename(a, b); consume(pending, flag); flag && await pending; }");
        std::fs::write(root.path().join(path), conditional_after).unwrap();
        let result = conditional_test_result(&root, path);
        assert!(result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
        let imported = source.replace("function consume(value, flag) { if (flag) return value; }", "import { consume } from './helper';");
        let extension = path.rsplit('.').next().unwrap();
        std::fs::write(root.path().join(format!("helper.{extension}")), "export function consume(value, flag) { if (flag) return value; }\n").unwrap();
        std::fs::write(root.path().join(path), imported).unwrap();
        assert_partial_helper_coverage(&root, path, rule);
        let imported_direct = direct.replace("function consume(value, flag) { if (flag) return value; }", "import { consume } from './helper';");
        std::fs::write(root.path().join(path), imported_direct).unwrap();
        assert_partial_helper_coverage(&root, path, rule);
    }
}

fn assert_boolean_helper_guard(path: &str, rule: &str, source: &str) -> tempfile::TempDir {
    let root = write_single_file(path, source);
    let result = conditional_test_result(&root, path);
    assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
    assert!(!result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
    std::fs::write(root.path().join(path), source.replace(", true)", ", false)")).unwrap();
    let result = conditional_test_result(&root, path);
    assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
    assert!(!result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
    root
}

#[test]
fn helper_conditional_returns_respect_constant_arguments() {
    for (path, rule) in [("main.js", "javascript-node-fs-discarded-promise"), ("main.ts", "typescript-node-fs-discarded-promise"), ("main.tsx", "typescript-node-fs-discarded-promise")] {
        let source = "import { rename } from 'node:fs/promises';\nfunction consume(value, flag) { if (flag) return value; }\nexport function run(a, b) { return consume(rename(a, b), true); }\n";
        let root = assert_boolean_helper_guard(path, rule, source);
        let later = source.replace("export function run(a, b) { return consume(rename(a, b), true); }", "export async function run(a, b) { const value = rename(a, b); consume(value, false); await value; }");
        std::fs::write(root.path().join(path), later).unwrap();
        assert_no_helper_finding(&root, path, rule);
        let imported = import_guarded_helper(&root, path, source);
        std::fs::write(root.path().join(path), imported.replace(", true)", ", false)")).unwrap();
        let result = conditional_test_result(&root, path);
        assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
        let unknown = source.replace("if (flag) return value;", "if (flag && check()) return value;");
        std::fs::write(root.path().join(path), unknown).unwrap();
        let result = conditional_test_result(&root, path);
        assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
        assert_partial_helper_coverage(&root, path, rule);
    }
}

fn assert_bound_helper_guard(path: &str, rule: &str, source: &str) -> tempfile::TempDir {
    let root = write_single_file(path, source);
    let result = conditional_test_result(&root, path);
    assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
    assert!(!result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
    std::fs::write(root.path().join(path), source.replace("enabled = true", "enabled = false")).unwrap();
    let result = conditional_test_result(&root, path);
    assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
    root
}

fn assert_uncertain_helper_guard(root: &tempfile::TempDir, path: &str, rule: &str) {
    let result = conditional_test_result(root, path);
    assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
    assert_partial_helper_coverage(root, path, rule);
}

#[test]
fn helper_guards_follow_stable_boolean_bindings() {
    for (path, rule) in [("main.js", "javascript-node-fs-discarded-promise"), ("main.ts", "typescript-node-fs-discarded-promise"), ("main.tsx", "typescript-node-fs-discarded-promise")] {
        let source = "import { rename } from 'node:fs/promises';\nfunction consume(value, flag) { if (flag) return value; }\nexport function run(a, b) { const enabled = true; return consume(rename(a, b), enabled); }\n";
        let root = assert_bound_helper_guard(path, rule, source);
        let reassigned = source.replace("const enabled = true;", "let enabled = false; enabled = true;");
        std::fs::write(root.path().join(path), reassigned).unwrap();
        assert_uncertain_helper_guard(&root, path, rule);
        let captured = source.replace("const enabled = true;", "let enabled = false; function update() { enabled = true; } update();");
        std::fs::write(root.path().join(path), captured).unwrap();
        assert_uncertain_helper_guard(&root, path, rule);
    }
}

fn assert_correlated_helper_pair(root: &tempfile::TempDir, path: &str, rule: &str, source: &str) {
    std::fs::write(root.path().join(path), source).unwrap();
    let result = conditional_test_result(root, path);
    assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
    assert!(!result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
    std::fs::write(root.path().join(path), source.replace("if (flag) return consume", "if (!flag) return consume").replace("if flag { return consume", "if !flag { return consume")).unwrap();
    let result = conditional_test_result(root, path);
    assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
}

fn assert_no_helper_finding(root: &tempfile::TempDir, path: &str, rule: &str) {
    let result = conditional_test_result(root, path);
    assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
}

fn import_guarded_helper(root: &tempfile::TempDir, path: &str, source: &str) -> String {
    let extension = path.rsplit('.').next().unwrap();
    std::fs::write(root.path().join(format!("helper.{extension}")), "export function consume(value, flag) { if (flag) return value; }\n").unwrap();
    source.replace("function consume(value, flag) { if (flag) return value; }", "import { consume } from './helper';")
}

fn assert_assigned_helper_pair(root: &tempfile::TempDir, path: &str, rule: &str, source: &str) {
    std::fs::write(root.path().join(path), source).unwrap();
    let result = conditional_test_result(root, path);
    assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
    assert!(!result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
    let bad = source.replace("if (flag) { const pending", "if (!flag) { const pending").replace("if (flag) { ", "if (!flag) { ").replace("if flag { let value", "if !flag { let value");
    std::fs::write(root.path().join(path), bad).unwrap();
    let result = conditional_test_result(root, path);
    assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
}

#[test]
fn caller_and_helper_guards_are_correlated() {
    for (path, rule) in [("main.js", "javascript-node-fs-discarded-promise"), ("main.ts", "typescript-node-fs-discarded-promise"), ("main.tsx", "typescript-node-fs-discarded-promise")] {
        let source = "import { rename } from 'node:fs/promises';\nfunction consume(value, flag) { if (flag) return value; }\nexport function run(a, b, flag) { if (flag) return consume(rename(a, b), flag); }\n";
        let root = write_single_file(path, source);
        assert_correlated_helper_pair(&root, path, rule, source);
        let reassigned = source.replace("if (flag) return consume", "if (flag) { flag = false; return consume").replace("); }\n", "); } }\n");
        std::fs::write(root.path().join(path), reassigned).unwrap();
        let result = conditional_test_result(&root, path);
        assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
        assert_partial_helper_coverage(&root, path, rule);
        let alias = source.replace("if (flag) return consume(rename(a, b), flag);", "const copy = flag; if (flag) return consume(rename(a, b), copy);");
        assert_correlated_helper_pair(&root, path, rule, &alias);
        let assigned = source.replace("if (flag) return consume(rename(a, b), flag);", "if (!flag) { const value = rename(a, b); return consume(value, flag); }");
        std::fs::write(root.path().join(path), assigned).unwrap();
        let result = conditional_test_result(&root, path);
        assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
        assert!(!result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
        let changed = alias.replace("const copy = flag;", "let copy = flag; copy = false;");
        std::fs::write(root.path().join(path), changed).unwrap();
        assert_no_helper_finding(&root, path, rule);
        assert_partial_helper_coverage(&root, path, rule);
        let later = source.replace("export function run(a, b, flag) { if (flag) return consume(rename(a, b), flag); }", "export async function run(a, b, flag) { if (!flag) { const value = rename(a, b); consume(value, flag); await value; } }");
        std::fs::write(root.path().join(path), later).unwrap();
        assert_no_helper_finding(&root, path, rule);
        let returned = source.replace("if (flag) return consume(rename(a, b), flag);", "if (!flag) { const value = rename(a, b); consume(value, flag); return value; }");
        std::fs::write(root.path().join(path), returned).unwrap();
        assert_no_helper_finding(&root, path, rule);
        let opaque = source.replace("if (flag) return consume(rename(a, b), flag);", "if (!flag) { const value = rename(a, b); consume(value, flag); unknown(value); }");
        std::fs::write(root.path().join(path), opaque).unwrap();
        assert_no_helper_finding(&root, path, rule);
        let extension = path.rsplit('.').next().unwrap();
        std::fs::write(root.path().join(format!("helper.{extension}")), "export function consume(value, flag) { if (flag) return value; }\n").unwrap();
        let imported = source.replace("function consume(value, flag) { if (flag) return value; }", "import { consume } from './helper';");
        assert_correlated_helper_pair(&root, path, rule, &imported);
    }
}

#[test]
fn assigned_helper_result_respects_correlated_guards() {
    for (path, rule) in [("main.js", "javascript-node-fs-discarded-promise"), ("main.ts", "typescript-node-fs-discarded-promise"), ("main.tsx", "typescript-node-fs-discarded-promise")] {
        let source = "import { rename } from 'node:fs/promises';\nfunction consume(value, flag) { if (flag) return value; }\nexport async function run(a, b, flag) { if (flag) { const pending = rename(a, b); const forwarded = consume(pending, flag); await forwarded; } }\n";
        let root = write_single_file(path, source);
        assert_assigned_helper_pair(&root, path, rule, source);
        let later = source.replace("if (flag) { const pending", "if (!flag) { const pending").replace("await forwarded;", "await pending;");
        std::fs::write(root.path().join(path), later).unwrap();
        let result = conditional_test_result(&root, path);
        assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
        let overwritten = source.replace("const forwarded = consume(pending, flag); await forwarded;", "let forwarded = consume(pending, flag); forwarded = null; await forwarded;");
        std::fs::write(root.path().join(path), overwritten).unwrap();
        let result = conditional_test_result(&root, path);
        assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
        let returned = source.replace("await forwarded;", "return forwarded;");
        std::fs::write(root.path().join(path), returned).unwrap();
        let result = conditional_test_result(&root, path);
        assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
        let alias = source.replace("await forwarded;", "const copy = forwarded; await copy;");
        std::fs::write(root.path().join(path), alias).unwrap();
        assert_no_helper_finding(&root, path, rule);
        let imported = import_guarded_helper(&root, path, source);
        assert_assigned_helper_pair(&root, path, rule, &imported);
    }
}

#[test]
fn assigned_helper_results_follow_jvm_and_rust_guards() {
    let cases = [
        ("Main.java", "java-discarded-objects-comparison", "import java.util.Objects;\nclass Main { static boolean consume(boolean value, boolean flag) { if (flag) return value; return false; }\nboolean run(Object a, Object b, boolean flag) { if (flag) { boolean value = Objects.equals(a,b); boolean forwarded = consume(value, flag); return forwarded; } return false; } }\n"),
        ("Main.kt", "kotlin-discarded-run-catching-result", "import kotlin.runCatching\nfun consume(value: Result<Int>, flag: Boolean): Result<Int>? { if (flag) return value; return null }\nfun run(flag: Boolean): Result<Int>? { if (flag) { val value = runCatching { 1 }; val forwarded = consume(value, flag); return forwarded; }; return null }\n"),
        ("lib.rs", "rust-discarded-remove-file-result", "use std::fs::remove_file;\nfn consume(value: std::io::Result<()>, flag: bool) -> std::io::Result<()> { if flag { return value; } Ok(()) }\nfn run(path: &str, flag: bool) -> std::io::Result<()> { if flag { let value = remove_file(path); let forwarded = consume(value, flag); return forwarded; } Ok(()) }\n"),
    ];
    for (path, rule, source) in cases {
        let root = write_single_file(path, source);
        assert_assigned_helper_pair(&root, path, rule, source);
    }
}

#[test]
fn locally_assigned_results_follow_correlated_helper_guards() {
    for (path, rule) in [("main.js", "javascript-node-fs-discarded-promise"), ("main.ts", "typescript-node-fs-discarded-promise"), ("main.tsx", "typescript-node-fs-discarded-promise")] {
        let source = "import { rename } from 'node:fs/promises';\nfunction consume(value, flag) { if (flag) return value; }\nexport function run(a, b, flag) { if (flag) { const value = rename(a, b); return consume(value, flag); } }\n";
        let root = write_single_file(path, source);
        let result = conditional_test_result(&root, path);
        assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
        assert!(!result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
        let bad = source.replace("if (flag) { const value", "if (!flag) { const value");
        std::fs::write(root.path().join(path), bad).unwrap();
        let result = conditional_test_result(&root, path);
        assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
        let mixed = source.replace("if (flag) { const value = rename(a, b); return consume(value, flag); }", "const value = rename(a, b); if (flag) return consume(value, flag);");
        std::fs::write(root.path().join(path), mixed).unwrap();
        let result = conditional_test_result(&root, path);
        assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)) || result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
        let another = source.replace("run(a, b, flag)", "run(a, b, flag, other)").replace("return consume(value, flag); } }", "return consume(value, flag); } const remaining = rename(a, b); return consume(remaining, other); }");
        std::fs::write(root.path().join(path), another).unwrap();
        assert_partial_helper_coverage(&root, path, rule);
    }
}

#[test]
fn helper_path_budget_does_not_claim_complete_analysis() {
    let mut source = String::from("import { rename } from 'node:fs/promises';\nfunction consume(value) { return value; }\nexport function run(a, b, flags) {\n");
    for index in 0..14 { source.push_str(&format!(" const f{index} = flags[{index}]; if (f{index}) {{ a = a; }}\n")); }
    source.push_str(" return consume(rename(a, b));\n}\n");
    let root = write_single_file("main.js", &source);
    assert_partial_helper_coverage(&root, "main.js", "javascript-node-fs-discarded-promise");
}

#[test]
fn caller_helper_guard_correlation_across_jvm_and_rust() {
    let cases = [
        ("Main.java", "java-discarded-objects-comparison", "import java.util.Objects;\nclass Main { static boolean consume(boolean value, boolean flag) { if (flag) return value; return false; }\nboolean run(Object a, Object b, boolean flag) { if (flag) return consume(Objects.equals(a,b), flag); return false; } }\n"),
        ("Main.kt", "kotlin-discarded-run-catching-result", "import kotlin.runCatching\nfun consume(value: Result<Int>, flag: Boolean): Result<Int>? { if (flag) return value; return null }\nfun run(flag: Boolean): Result<Int>? { if (flag) return consume(runCatching { 1 }, flag); return null }\n"),
        ("lib.rs", "rust-discarded-remove-file-result", "use std::fs::remove_file;\nfn consume(value: std::io::Result<()>, flag: bool) -> std::io::Result<()> { if flag { return value; } Ok(()) }\nfn run(path: &str, flag: bool) -> std::io::Result<()> { if flag { return consume(remove_file(path), flag); } Ok(()) }\n"),
    ];
    for (path, rule, source) in cases {
        let root = write_single_file(path, source);
        assert_correlated_helper_pair(&root, path, rule, source);
        let assigned = match path {
            "Main.java" => source.replace("if (flag) return consume(Objects.equals(a,b), flag);", "if (flag) { boolean value = Objects.equals(a,b); return consume(value, flag); }"),
            "Main.kt" => source.replace("if (flag) return consume(runCatching { 1 }, flag);", "if (flag) { val value = runCatching { 1 }; return consume(value, flag); }"),
            "lib.rs" => source.replace("if flag { return consume(remove_file(path), flag); }", "if flag { let value = remove_file(path); return consume(value, flag); }"),
            _ => unreachable!(),
        };
        std::fs::write(root.path().join(path), assigned).unwrap();
        let result = conditional_test_result(&root, path);
        assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
        assert!(!result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
    }
}

#[test]
fn helper_guards_follow_stable_boolean_aliases() {
    for (path, rule) in [("main.js", "javascript-node-fs-discarded-promise"), ("main.ts", "typescript-node-fs-discarded-promise"), ("main.tsx", "typescript-node-fs-discarded-promise")] {
        let source = "import { rename } from 'node:fs/promises';\nfunction consume(value, flag) { if (flag) return value; }\nexport function run(a, b) { const enabled = true; const first = enabled; const second = first; return consume(rename(a, b), second); }\n";
        let root = assert_bound_helper_guard(path, rule, source);
        for changed in [
            source.replace("const enabled = true; const first = enabled;", "let enabled = false; enabled = true; const first = enabled;"),
            source.replace("const enabled = true; const first = enabled;", "let enabled = false; const first = enabled; enabled = true;"),
            source.replace("const second = first;", "let second = first; second = true;"),
        ] {
            std::fs::write(root.path().join(path), changed).unwrap();
            assert_uncertain_helper_guard(&root, path, rule);
        }
    }
}

#[test]
fn long_boolean_alias_chains_remain_incomplete() {
    let mut source = String::from("import { rename } from 'node:fs/promises';\nfunction consume(value, flag) { if (flag) return value; }\nexport function run(a, b) { const flag0 = false;\n");
    for index in 1..=64 { source.push_str(&format!("const flag{index} = flag{};\n", index - 1)); }
    source.push_str("return consume(rename(a, b), flag64); }\n");
    let root = write_single_file("main.js", &source);
    assert_uncertain_helper_guard(&root, "main.js", "javascript-node-fs-discarded-promise");
}

#[test]
fn mutable_rust_borrow_keeps_helper_guard_uncertain() {
    let source = "use std::fs::remove_file;\nfn consume(value: std::io::Result<()>, flag: bool) -> std::io::Result<()> { if flag { return value; } Ok(()) }\nfn run(path: &str) -> std::io::Result<()> { let mut enabled = false; let alias = &mut enabled; *alias = true; consume(remove_file(path), enabled) }\n";
    let root = write_single_file("lib.rs", source);
    assert_uncertain_helper_guard(&root, "lib.rs", "rust-discarded-remove-file-result");
}

#[test]
fn helper_stable_guards_follow_jvm_and_rust_bindings() {
    let cases = [
        ("Main.java", "java-discarded-objects-comparison", "import java.util.Objects;\nclass Main { static boolean consume(boolean value, boolean flag) { if (flag) return value; return false; }\nboolean run(Object a, Object b) { final boolean enabled = true; return consume(Objects.equals(a,b), enabled); } }\n"),
        ("Main.kt", "kotlin-discarded-run-catching-result", "import kotlin.runCatching\nfun consume(value: Result<Int>, flag: Boolean): Result<Int>? { if (flag) return value; return null }\nfun run(): Result<Int>? { val enabled = true; return consume(runCatching { 1 }, enabled) }\n"),
        ("lib.rs", "rust-discarded-remove-file-result", "use std::fs::remove_file;\nfn consume(value: std::io::Result<()>, flag: bool) -> std::io::Result<()> { if flag { return value; } Ok(()) }\nfn run(path: &str) -> std::io::Result<()> { let enabled = true; consume(remove_file(path), enabled) }\n"),
    ];
    for (path, rule, source) in cases {
        assert_bound_helper_guard(path, rule, source);
        let aliased = match path {
            "Main.java" => source.replace("return consume(Objects.equals(a,b), enabled)", "final boolean alias = enabled; return consume(Objects.equals(a,b), alias)"),
            "Main.kt" => source.replace("return consume(runCatching { 1 }, enabled)", "val alias = enabled; return consume(runCatching { 1 }, alias)"),
            "lib.rs" => source.replace("consume(remove_file(path), enabled)", "let alias = enabled; consume(remove_file(path), alias)"),
            _ => unreachable!(),
        };
        assert_bound_helper_guard(path, rule, &aliased);
    }
}

#[test]
fn helper_constant_guards_are_resolved_across_languages() {
    let cases = [
        ("Main.java", "java-discarded-objects-comparison", "import java.util.Objects;\nclass Main { static boolean consume(boolean value, boolean flag) { if (flag) return value; return false; }\nboolean run(Object a, Object b) { return consume(Objects.equals(a,b), true); } }\n"),
        ("Main.kt", "kotlin-discarded-run-catching-result", "import kotlin.runCatching\nfun consume(value: Result<Int>, flag: Boolean): Result<Int>? { if (flag) return value; return null }\nfun run(): Result<Int>? { return consume(runCatching { 1 }, true) }\n"),
        ("lib.rs", "rust-discarded-remove-file-result", "use std::fs::remove_file;\nfn consume(value: std::io::Result<()>, flag: bool) -> std::io::Result<()> { if flag { return value; } Ok(()) }\nfn run(path: &str) -> std::io::Result<()> { consume(remove_file(path), true) }\n"),
    ];
    for (path, rule, source) in cases {
        assert_boolean_helper_guard(path, rule, source);
    }
}

#[test]
fn shadowed_go_boolean_does_not_specialize_helper_guard() {
    let source = "package sample\nimport \"os\"\nfunc consume(err error, flag bool) error { if flag { return err }; return nil }\nfunc run(a, b string) error { true := false; return consume(os.Rename(a, b), true) }\n";
    let root = write_single_file("main.go", source);
    let result = conditional_test_result(&root, "main.go");
    assert!(result.coverage.iter().any(|issue| issue.reason == "result_use_path_unmodeled" && issue.rule.as_deref() == Some("go-os-rename-discarded-error")), "{:?} {:?}", result.findings, result.coverage);
}

#[test]
fn partial_helper_observation_reports_incomplete_across_languages() {
    let cases = [
        ("main.go", "go-os-rename-discarded-error", "package sample\nimport \"os\"\nfunc consume(err error, flag bool) error { if flag { return err }; return nil }\nfunc run(a, b string, flag bool) error { err := os.Rename(a, b); return consume(err, flag) }\n"),
        ("Main.java", "java-discarded-objects-comparison", "import java.util.Objects;\nclass Main { static boolean consume(boolean value, boolean flag) { if (flag) return value; return false; }\nboolean run(Object a, Object b, boolean flag) { boolean value = Objects.equals(a,b); return consume(value, flag); } }\n"),
        ("Main.kt", "kotlin-discarded-run-catching-result", "import kotlin.runCatching\nfun consume(value: Result<Int>, flag: Boolean): Int { if (flag) return value.getOrThrow(); return 0 }\nfun run(flag: Boolean): Int { val value = runCatching { 1 }; return consume(value, flag) }\n"),
        ("lib.rs", "rust-discarded-remove-file-result", "use std::fs::remove_file;\nfn consume(value: std::io::Result<()>, flag: bool) -> std::io::Result<()> { if flag { return value; } Ok(()) }\nfn run(path: &str, flag: bool) -> std::io::Result<()> { let value = remove_file(path); consume(value, flag) }\n"),
    ];
    for (path, rule, source) in cases {
        assert_partial_helper_source(path, rule, source);
        let direct = match path {
            "main.go" => source.replace("err := os.Rename(a, b); return consume(err, flag)", "return consume(os.Rename(a, b), flag)"),
            "Main.java" => source.replace("boolean value = Objects.equals(a,b); return consume(value, flag);", "return consume(Objects.equals(a,b), flag);"),
            "Main.kt" => source.replace("val value = runCatching { 1 }; return consume(value, flag)", "return consume(runCatching { 1 }, flag)"),
            "lib.rs" => source.replace("let value = remove_file(path); consume(value, flag)", "consume(remove_file(path), flag)"),
            _ => unreachable!(),
        };
        assert_partial_helper_source(path, rule, &direct);
    }
}

#[test]
fn partial_result_observation_across_languages() {
    let cases = [
        ("main.go", "go-os-rename-discarded-error", "package sample\nimport \"os\"\nfunc run(a, b string, flag bool) error {\n err := os.Rename(a, b)\n if flag { return err }\n return nil\n}\n", 4, ("return nil", "return err")),
        ("Main.java", "java-discarded-objects-comparison", "import java.util.Objects;\nclass Main { boolean run(Object a, Object b, boolean flag) {\n boolean value = Objects.equals(a, b);\n if (flag) return value;\n return false;\n} }\n", 3, ("return false", "return value")),
        ("Main.kt", "kotlin-discarded-run-catching-result", "import kotlin.runCatching\nfun run(flag: Boolean): Int {\n val value = runCatching { 1 }\n if (flag) return value.getOrThrow()\n return 0\n}\n", 3, ("return 0", "return value.getOrThrow()")),
        ("lib.rs", "rust-discarded-remove-file-result", "use std::fs::remove_file;\nfn run(path: &str, flag: bool) -> std::io::Result<()> {\n let result = remove_file(path);\n if flag { return result; }\n Ok(())\n}\n", 3, ("Ok(())", "result")),
    ];
    for (path, rule, source, line, (from, to)) in cases {
        let root = write_single_file(path, source);
        let result = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &[path.into()], &[]);
        assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule) && hit.location.range.start_line == line), "{path}: {:?} {:?}", result.findings, result.coverage);
        std::fs::write(root.path().join(path), source.replace(from, to)).unwrap();
        let result = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &[path.into()], &[]);
        assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
    }
}

#[test]
fn partial_result_observation_reports_its_source_budget() {
    let mut source = String::from("import { rename } from 'node:fs/promises';\nexport async function run(a, b, flag) {\n");
    for index in 0..65 { source.push_str(&format!(" const p{index} = rename(a, b);\n")); }
    source.push_str(" if (flag) await p0;\n");
    for index in 1..65 { source.push_str(&format!(" await p{index};\n")); }
    source.push_str("}\n");
    let root = write_single_file("main.js", &source);
    let result = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["main.js".into()], &[]);
    assert!(result.coverage.iter().any(|issue| issue.reason == "result_use_path_limit" && issue.rule.as_deref() == Some("javascript-node-fs-discarded-promise") && issue.line == Some(2)), "{:?}", result.coverage);
}

#[test]
fn conditional_assignments_track_result_use() {
    for (path, rule) in [("main.js", "javascript-node-fs-discarded-promise"), ("main.ts", "typescript-node-fs-discarded-promise"), ("main.tsx", "typescript-node-fs-discarded-promise")] {
        let source = "import { rename } from 'node:fs/promises';\nexport async function run(a, b, flag) {\n const pending = flag ? rename(a, b) : null;\n}\n";
        let root = conditional_finding_fixture(path, rule, source);
        let check = || conditional_test_result(&root, path);
        std::fs::write(root.path().join(path), source.replace("\n}\n", "\n await pending;\n}\n")).unwrap();
        let result = check();
        assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
        let overwritten = source.replace("\n}\n", "\n pending = null;\n await pending;\n}\n").replace("const pending", "let pending");
        std::fs::write(root.path().join(path), overwritten).unwrap();
        let result = check();
        assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
        std::fs::write(root.path().join(path), source.replace("flag ?", "check(flag) ?")).unwrap();
        let result = check();
        assert!(result.coverage.iter().any(|issue| issue.reason == "unmodeled_conditional_result" && issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
        std::fs::write(root.path().join(path), source.replace(": null;", ": null, other = 0;")).unwrap();
        let result = check();
        assert!(result.coverage.iter().any(|issue| issue.reason == "unmodeled_conditional_result" && issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
        let helper = "import { rename } from 'node:fs/promises';\nexport function make(a, b, flag) {\n const pending = flag ? rename(a, b) : null;\n return pending;\n}\n";
        std::fs::write(root.path().join(path), "import { make } from './helper';\nexport function run(a, b, flag) { make(a, b, flag); }\n").unwrap();
        let extension = path.rsplit('.').next().unwrap();
        std::fs::write(root.path().join(format!("helper.{extension}")), helper).unwrap();
        let result = check();
        assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule) && hit.location.range.start_line == 3), "{path}: {:?} {:?}", result.findings, result.coverage);
    }
}

#[test]
fn conditional_return_results_flow_through_helpers() {
    for (path, rule) in [("main.js", "javascript-node-fs-discarded-promise"), ("main.ts", "typescript-node-fs-discarded-promise"), ("main.tsx", "typescript-node-fs-discarded-promise")] {
        let source = "import { rename } from 'node:fs/promises';\nfunction make(a, b, flag) {\n return flag ? rename(a, b) : null;\n}\nexport function run(a, b, flag) { make(a, b, flag); }\n";
        let root = conditional_finding_fixture(path, rule, source);
        let check = || conditional_test_result(&root, path);
        std::fs::write(root.path().join(path), source.replace("export function run(a, b, flag) { make(a, b, flag); }", "export async function run(a, b, flag) { await make(a, b, flag); }")).unwrap();
        let result = check();
        assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
        let replaced = source.replace("return flag ? rename(a, b) : null;", "try { return flag ? rename(a, b) : null; } finally { return null; }")
            .replace("export function run(a, b, flag) { make(a, b, flag); }", "export async function run(a, b, flag) { await make(a, b, flag); }");
        std::fs::write(root.path().join(path), replaced).unwrap();
        let result = check();
        assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?} {:?}", result.findings, result.coverage);
        std::fs::write(root.path().join(path), source.replace("flag ?", "check(flag) ?")).unwrap();
        let result = check();
        assert!(result.coverage.iter().any(|issue| issue.reason == "unmodeled_conditional_result" && issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
        let helper = "import { rename } from 'node:fs/promises';\nexport function make(a, b, flag) {\n return flag ? rename(a, b)\n   : rename(b, a);\n}\n";
        std::fs::write(root.path().join(path), "import { make } from './helper';\nexport function run(a, b, flag) { make(a, b, flag); }\n").unwrap();
        let extension = path.rsplit('.').next().unwrap();
        std::fs::write(root.path().join(format!("helper.{extension}")), helper).unwrap();
        let result = check();
        let lines: std::collections::BTreeSet<_> = result.findings.iter().filter(|hit| hit.source.rule_id.as_deref() == Some(rule)).map(|hit| hit.location.range.start_line).collect();
        assert_eq!(lines, [3, 4].into_iter().collect(), "{path}: {:?} {:?}", result.findings, result.coverage);
    }
}

#[test]
fn discarded_results_in_conditional_statements_are_tracked() {
    for (path, rule) in [("main.js", "javascript-node-fs-discarded-promise"), ("main.ts", "typescript-node-fs-discarded-promise"), ("main.tsx", "typescript-node-fs-discarded-promise")] {
        let source = "import { rename } from 'node:fs/promises';\nexport function run(a, b, flag) {\n flag ? rename(a, b) : null;\n}\n";
        let root = conditional_finding_fixture(path, rule, source);
        let check = || conditional_test_result(&root, path);
        std::fs::write(root.path().join(path), source.replace("flag ? rename(a, b) : null;", "((flag ? (rename(a, b)) : null));")).unwrap();
        let result = check();
        assert_eq!(result.findings.iter().filter(|hit| hit.source.rule_id.as_deref() == Some(rule)).count(), 1, "{path}: {:?}", result.coverage);
        std::fs::write(root.path().join(path), source.replace("flag ?", "false ?")).unwrap();
        let result = check();
        assert!(!result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule)), "{path}: {:?}", result.findings);
        std::fs::write(root.path().join(path), source.replace("flag ?", "(flag && a) ?")).unwrap();
        let result = check();
        assert!(result.findings.iter().any(|hit| hit.source.rule_id.as_deref() == Some(rule))
            || result.coverage.iter().any(|issue| issue.rule.as_deref() == Some(rule)), "{path}: {:?}", result.coverage);
        std::fs::write(root.path().join(path), source.replace("flag ?", "check(flag) ?")).unwrap();
        let result = check();
        assert!(result.coverage.iter().any(|issue| issue.rule.as_deref() == Some(rule)
            && issue.reason == "unmodeled_conditional_result" && issue.line == Some(3)), "{path}: {:?}", result.coverage);
        std::fs::write(root.path().join(path), source.replace("rename(a, b)", "wrap(rename(a, b))")).unwrap();
        let result = check();
        assert!(result.coverage.iter().any(|issue| issue.rule.as_deref() == Some(rule)
            && issue.reason == "unmodeled_conditional_result" && issue.line == Some(3)), "{path}: {:?}", result.coverage);
    }
}

#[test]
fn unlowered_contract_cleanup_reports_incomplete_analysis() {
    let source = "import java.util.concurrent.CompletableFuture;\nclass Main {\n Object run(Runnable work, boolean flag) {\n  var task = CompletableFuture.runAsync(work);\n  return flag ? task.join() : null;\n }\n}\n";
    let root = write_single_file("Main.java", source);
    let check = || autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["Main.java".into()], &[]);
    let result = check();
    assert!(result.coverage.iter().any(|issue| issue.reason == "unmodeled_cleanup_call"
        && issue.rule.as_deref() == Some("java-completable-future-unobserved") && issue.path.as_deref() == Some("Main.java") && issue.line == Some(5)), "{:?}", result.coverage);
    std::fs::write(root.path().join("Main.java"), "import java.util.concurrent.CompletableFuture;\nclass Main { void run(Runnable work) { var task = CompletableFuture.runAsync(work); task.join(); } }\n").unwrap();
    assert!(!check().coverage.iter().any(|issue| issue.reason == "unmodeled_cleanup_call"));
    std::fs::write(root.path().join("Main.java"), "import java.util.concurrent.CompletableFuture;\nclass Main {\n Object finish(CompletableFuture<Void> task, boolean flag) { return flag ? task.join() : null; }\n void run(Runnable work, boolean flag) { var task = CompletableFuture.runAsync(work); finish(task, flag); }\n}\n").unwrap();
    let result = check();
    assert!(result.coverage.iter().any(|issue| issue.reason == "unmodeled_cleanup_call"
        && issue.rule.as_deref() == Some("java-completable-future-unobserved") && issue.line == Some(3)), "{:?}", result.coverage);
    std::fs::write(root.path().join("Main.java"), "class Main { Object finish(Other task, boolean flag) { return flag ? task.join() : null; } }\n").unwrap();
    assert!(!check().coverage.iter().any(|issue| issue.reason == "unmodeled_cleanup_call"));
}

#[test]
fn unsupported_java_future_storage_reports_incomplete_analysis() {
    let source = "import java.util.concurrent.CompletableFuture;\nclass Main {\n Object pending;\n void run() { pending = CompletableFuture.runAsync(() -> {}); }\n}\n";
    let root = write_single_file("Main.java", source);
    let result = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["Main.java".into()], &[]);
    assert!(result.coverage.iter().any(|issue| issue.reason == "unmodeled_contract_handle"
        && issue.rule.as_deref() == Some("java-completable-future-unobserved") && issue.path.as_deref() == Some("Main.java") && issue.line == Some(4)), "{:?}", result.coverage);
}

#[test]
fn every_call_order_rule_declares_a_category() {
    for rule in discover_call_order_rules() {
        assert!(!rule.category.is_empty(), "{}: kind: call-sequence rule has no category field", rule.id);
    }
}

fn assert_unresolved_receiver_cases(cases: &[(&str, &str, &str, u32)]) {
    for &(path, rule, source, line) in cases {
        let root = write_single_file(path, source);
        let result = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &[path.into()], &[]);
        assert!(result.coverage.iter().any(|issue| issue.reason == "unresolved_result_receiver" && issue.rule.as_deref() == Some(rule) && issue.path.as_deref() == Some(path) && issue.line == Some(line)), "{path}: {:?}", result.coverage);
    }
}

#[test]
fn unresolved_receivers_track_bindings_across_languages() {
    let cases = [
        ("main.go", "go-os-rename-discarded-error", "package main\nimport \"os\"\nfunc run(a, b string) { err := os.Rename(a, b); unknown(err) }\n", 3),
        ("main.ts", "typescript-node-fs-discarded-promise", "import { rename } from 'node:fs/promises';\nfunction run(a, b) { const p = rename(a, b); unknown(p); }\n", 2),
        ("main.tsx", "typescript-node-fs-discarded-promise", "import { rename } from 'node:fs/promises';\nfunction run(a, b) { const p = rename(a, b); unknown(p); }\n", 2),
        ("Main.java", "java-discarded-objects-comparison", "import java.util.Objects;\nclass Main { void run(Object a, Object b) { boolean equal = Objects.equals(a, b); unknown(equal); } }\n", 2),
        ("main.kt", "kotlin-discarded-run-catching-result", "import kotlin.runCatching\nfun run() { val value = runCatching { 1 }; unknown(value) }\n", 2),
        ("lib.rs", "rust-discarded-remove-file-result", "use std::fs::remove_file;\nfn run(path: &str) { let value = remove_file(path); unknown(value); }\n", 2),
    ];
    assert_unresolved_receiver_cases(&cases);
}

fn assert_web_receiver_lines(source: &str, reason: &str, expected: &[u32]) {
    for (ext, rule) in [("js", "javascript-node-fs-discarded-promise"), ("ts", "typescript-node-fs-discarded-promise"), ("tsx", "typescript-node-fs-discarded-promise")] {
        let path = format!("main.{ext}");
        let root = write_single_file(&path, source);
        let result = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), std::slice::from_ref(&path), &[]);
        let lines: std::collections::BTreeSet<_> = result.coverage.iter().filter(|issue|
            issue.reason == reason && issue.rule.as_deref() == Some(rule)).filter_map(|issue| issue.line).collect();
        assert_eq!(lines, expected.iter().copied().collect(), "{ext}: {:?}", result.coverage);
    }
}

#[test]
fn unresolved_receivers_report_results_wrapped_in_web_arrays() {
    let source = "import { rename } from 'node:fs/promises';\nfunction consume(values) {}\nfunction array(a, b) { unknown([rename(a, b)]); }\nfunction spread(a, b) { unknown(...[rename(a, b)]); }\nfunction local(a, b) { consume([rename(a, b)]); }\nasync function observed(a, b) { unknown([await rename(a, b)]); }\nfunction noReceiver(a, b) { return [rename(a, b)]; }\n";
    assert_web_receiver_lines(source, "wrapped_result_receiver", &[3, 4, 5]);
}

#[test]
fn unresolved_receivers_track_assigned_results_in_web_arrays() {
    let source = "import { rename } from 'node:fs/promises';\nfunction local(values) {}\nfunction direct(a, b) { const p = rename(a, b); unknown([p]); }\nfunction alias(a, b) { const p = rename(a, b); const q = p; unknown(...[q]); }\nfunction resolved(a, b) { const p = rename(a, b); local([p]); }\nfunction overwritten(a, b) { let p = rename(a, b); p = null; unknown([p]); }\nasync function observed(a, b) { const p = rename(a, b); unknown([await p]); }\nfunction branch(a, b, flag) { const p = rename(a, b); if (flag) unknown([p]); }\n";
    assert_web_receiver_lines(source, "unresolved_result_receiver", &[3, 4, 5, 8]);
}

#[test]
fn unresolved_receivers_track_results_in_stored_arrays() {
    let source = "import { rename } from 'node:fs/promises';\nfunction direct(a, b) { const values = [rename(a, b)]; unknown(values); }\nfunction assigned(a, b) { const p = rename(a, b); const values = [p]; unknown(values); }\nfunction alias(a, b) { const p = rename(a, b); const values = [p]; const q = values; unknown(q); }\nfunction replaced(a, b) { const p = rename(a, b); let values = [p]; values = []; unknown(values); }\nfunction valueReplaced(a, b) { let p = rename(a, b); const values = [p]; p = null; unknown(values); }\nasync function observed(a, b) { const p = rename(a, b); const values = [await p]; unknown(values); }\nfunction branch(a, b, flag) { const p = rename(a, b); const values = [p]; if (flag) unknown(values); }\nfunction mixed(a, b) { const p = rename(a, b); const values = [p, 1, null]; unknown(values); }\n";
    assert_web_receiver_lines(source, "unresolved_result_receiver", &[2, 3, 4, 6, 8, 9]);
}

#[test]
fn mixed_array_elements_preserve_result_and_helper_flow() {
    let source = "import { rename } from 'node:fs/promises';\nfunction make(a, b) { return rename(a, b); }\nfunction mixed(a, b) { const p = rename(a, b); const values = [p, build()]; unknown(values); }\nfunction helper(a, b) { const values = [make(a, b), build()]; unknown(values); }\nasync function used(a, b) { const p = rename(a, b); const values = [p, await p]; unknown(values); }\nfunction sideEffect(a, b) { const p = rename(a, b); const values = [send(p), 1]; return values; }\n";
    assert_web_receiver_lines(source, "unresolved_result_receiver", &[2, 3, 6]);
    let clean = "import { rename } from 'node:fs/promises';\nfunction make(a, b) { return rename(a, b); }\nfunction run(a, b) { return [make(a, b), build()]; }\n";
    let root = write_single_file("main.js", clean);
    assert_helper_locations(root.path(), "main.js", "javascript-node-fs-discarded-promise", &[]);
}

#[test]
fn mixed_array_tracks_imported_result_helpers() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("helper.js"), "import { rename } from 'node:fs/promises';\nexport function make(a, b) { return rename(a, b); }\n").unwrap();
    std::fs::write(root.path().join("main.js"), "import { make } from './helper';\nfunction run(a, b) { const values = [make(a, b), build()]; unknown(values); }\n").unwrap();
    let result = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["main.js".into()], &[]);
    assert!(result.coverage.iter().any(|issue| issue.reason == "unresolved_result_receiver"
        && issue.rule.as_deref() == Some("javascript-node-fs-discarded-promise")
        && issue.path.as_deref() == Some("helper.js") && issue.line == Some(2)), "{:?}", result.coverage);
}

#[test]
fn stored_array_result_uncertainty_crosses_helper_files() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("helper.js"), "import { rename } from 'node:fs/promises';\nexport function make(a, b) { return [rename(a, b)]; }\nexport function send(values) { unknown(values); }\n").unwrap();
    std::fs::write(root.path().join("main.js"), "import { rename } from 'node:fs/promises';\nimport { make, send } from './helper';\nfunction passed(a, b) { const p = rename(a, b); const values = [p]; send(values); }\nfunction returned(a, b) { const values = make(a, b); unknown(values); }\n").unwrap();
    let result = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["main.js".into()], &[]);
    let locations: std::collections::BTreeSet<_> = result.coverage.iter().filter(|issue|
        issue.reason == "unresolved_result_receiver" && issue.rule.as_deref() == Some("javascript-node-fs-discarded-promise"))
        .map(|issue| (issue.path.as_deref().unwrap(), issue.line.unwrap())).collect();
    assert_eq!(locations, std::collections::BTreeSet::from([("main.js", 3), ("helper.js", 2)]), "{:?}", result.coverage);
}

#[test]
fn wrapped_assigned_results_propagate_through_imported_helpers() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("helper.js"), "export function send(value) { relay(value); }\nfunction relay(value) { unknown([value]); }\n").unwrap();
    std::fs::write(root.path().join("main.js"), "import { rename } from 'node:fs/promises';\nimport { send } from './helper';\nfunction run(a, b) { const p = rename(a, b); send(p); }\n").unwrap();
    let result = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["main.js".into()], &[]);
    assert!(result.coverage.iter().any(|issue| issue.reason == "unresolved_result_receiver"
        && issue.rule.as_deref() == Some("javascript-node-fs-discarded-promise")
        && issue.path.as_deref() == Some("main.js") && issue.line == Some(3)), "{:?}", result.coverage);
}

#[test]
fn unresolved_receivers_propagate_through_imported_helpers() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("helper.js"), "import { rename } from 'node:fs/promises';\nexport function send(value) { relay(value); }\nexport function make(a, b) { return rename(a, b); }\nexport function forward(value) { return value; }\nfunction relay(value) { unknown(value); }\n").unwrap();
    std::fs::write(root.path().join("main.js"), "import { rename } from 'node:fs/promises';\nimport { send, make, forward } from './helper';\nfunction passed(a, b) { const p = rename(a, b); send(p); }\nfunction returned(a, b) { const p = make(a, b); unknown(p); }\nasync function observed(a, b) { await forward(rename(a, b)); }\n").unwrap();
    let result = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["main.js".into()], &[]);
    let locations: std::collections::BTreeSet<_> = result.coverage.iter().filter(|issue| issue.reason == "unresolved_result_receiver"
        && issue.rule.as_deref() == Some("javascript-node-fs-discarded-promise"))
        .map(|issue| (issue.path.as_deref().unwrap(), issue.line.unwrap())).collect();
    assert_eq!(locations, std::collections::BTreeSet::from([("main.js", 3), ("helper.js", 3)]), "{:?}", result.coverage);
    std::fs::write(root.path().join("main.js"), "import { make } from './helper';\nfunction nested(a, b) { unknown(make(a, b)); }\n").unwrap();
    let nested = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["main.js".into()], &[]);
    assert!(nested.coverage.iter().any(|issue| issue.reason == "unresolved_result_receiver"
        && issue.rule.as_deref() == Some("javascript-node-fs-discarded-promise")
        && issue.path.as_deref() == Some("helper.js") && issue.line == Some(3)), "{:?}", nested.coverage);
}

#[test]
fn unresolved_receivers_track_assigned_results_and_reassignments() {
    use std::collections::BTreeSet;
    let source = "import { rename } from 'node:fs/promises';\nfunction externalCase(a, b) { const p = rename(a, b); unknown(p); }\nfunction aliasCase(a, b) { const p = rename(a, b); const q = p; unknown(q); }\nfunction branchCase(a, b, flag) { const p = rename(a, b); if (flag) unknown(p); }\nfunction replaced(a, b) { let p = rename(a, b); p = null; unknown(p); }\nfunction consume(value) { return value; }\nasync function handled(a, b) { await consume(rename(a, b)); }\n";
    let root = write_single_file("main.js", source);
    let result = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["main.js".into()], &[]);
    let lines: BTreeSet<_> = result.coverage.iter().filter(|issue| issue.reason == "unresolved_result_receiver"
        && issue.rule.as_deref() == Some("javascript-node-fs-discarded-promise"))
        .filter_map(|issue| issue.line).collect();
    assert_eq!(lines, BTreeSet::from([2, 3, 4]), "{:?}", result.coverage);
    let observed = "import { rename } from 'node:fs/promises';\nasync function used(a, b) { const p = rename(a, b); await p; unknown(p); }\nasync function branch(a, b, flag) { const p = rename(a, b); if (flag) await p; unknown(p); }\n";
    std::fs::write(root.path().join("main.js"), observed).unwrap();
    let observed = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["main.js".into()], &[]);
    let lines: BTreeSet<_> = observed.coverage.iter().filter(|issue| issue.reason == "unresolved_result_receiver"
        && issue.rule.as_deref() == Some("javascript-node-fs-discarded-promise")).filter_map(|issue| issue.line).collect();
    assert_eq!(lines, BTreeSet::from([3]), "{:?}", observed.coverage);
    let fields = "import { rename } from 'node:fs/promises';\nfunction run(a, b) { const box = { pending: null }; box.pending = rename(a, b); unknown(box.pending); }\nfunction replaced(a, b) { const box = { pending: null }; box.pending = rename(a, b); box.pending = null; unknown(box.pending); }\n";
    std::fs::write(root.path().join("main.js"), fields).unwrap();
    let fields = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["main.js".into()], &[]);
    let lines: BTreeSet<_> = fields.coverage.iter().filter(|issue| issue.reason == "unresolved_result_receiver"
        && issue.rule.as_deref() == Some("javascript-node-fs-discarded-promise")).filter_map(|issue| issue.line).collect();
    assert_eq!(lines, BTreeSet::from([2]), "{:?}", fields.coverage);
    let closures = "import { rename } from 'node:fs/promises';\nfunction called(a, b) { const p = rename(a, b); const f = () => unknown(p); f(); }\nfunction uncalled(a, b) { const p = rename(a, b); const f = () => unknown(p); }\n";
    std::fs::write(root.path().join("main.js"), closures).unwrap();
    let closures = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["main.js".into()], &[]);
    let lines: BTreeSet<_> = closures.coverage.iter().filter(|issue| issue.reason == "unresolved_result_receiver"
        && issue.rule.as_deref() == Some("javascript-node-fs-discarded-promise")).filter_map(|issue| issue.line).collect();
    assert_eq!(lines, BTreeSet::from([2]), "{:?}", closures.coverage);
}

#[test]
fn unresolved_helpers_receiving_direct_results_are_reported() {
    let cases = [
        ("main.go", "go-os-rename-discarded-error", "package main\nimport \"os\"\nfunc run(a, b string) { external(os.Rename(a, b)) }\n", 3),
        ("main.js", "javascript-node-fs-discarded-promise", "import { rename } from 'node:fs/promises';\nfunction run(a, b) { external(rename(a, b)); }\n", 2),
        ("main.ts", "typescript-node-fs-discarded-promise", "import { rename } from 'node:fs/promises';\nfunction run(a, b) { external(rename(a, b)); }\n", 2),
        ("main.tsx", "typescript-node-fs-discarded-promise", "import { rename } from 'node:fs/promises';\nfunction run(a, b) { external(rename(a, b)); }\n", 2),
        ("Main.java", "java-discarded-objects-comparison", "import java.util.Objects;\nclass Main { void run(Object a, Object b) { external(Objects.equals(a, b)); } }\n", 2),
        ("main.kt", "kotlin-discarded-run-catching-result", "import kotlin.runCatching\nfun run() { mystery(runCatching({ 1 })) }\n", 2),
        ("lib.rs", "rust-discarded-remove-file-result", "use std::fs::remove_file;\nfn run(path: &str) { external(remove_file(path)); }\n", 2),
    ];
    assert_unresolved_receiver_cases(&cases);
    let path = "main.js";
    let source = "import { rename } from 'node:fs/promises';\nfunction consume(value) { return value; }\nasync function run(a, b) { await consume(rename(a, b)); other(await rename(a, b)); }\n";
    let root = write_single_file(path, source);
    let result = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &[path.into()], &[]);
    assert!(!result.coverage.iter().any(|issue| issue.reason == "unresolved_result_receiver" && issue.rule.as_deref() == Some("javascript-node-fs-discarded-promise")), "{:?}", result.coverage);
    std::fs::write(root.path().join(path), "import { rename } from 'node:fs/promises';\nfunction run(a, b) { if (false) { unknown(rename(a, b)); } }\n").unwrap();
    let dead = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &[path.into()], &[]);
    assert!(!dead.coverage.iter().any(|issue| issue.reason == "unresolved_result_receiver" && issue.rule.as_deref() == Some("javascript-node-fs-discarded-promise")), "{:?}", dead.coverage);
}

#[test]
fn ignored_results_follow_imported_helper_arguments_and_returns() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("helper")).unwrap();
    std::fs::write(root.path().join("go.mod"), "module example.com/project\n\ngo 1.22\n").unwrap();
    std::fs::write(root.path().join("helper/helper.go"), r#"package helper
import "os"
func Ignore(err error) {}
func Check(err error) { if err != nil { panic(err) } }
func Forward(err error) error { return err }
func Move(a, b string) error { return os.Rename(a, b) }
func Bridge(a, b string) error { return Move(a, b) }
"#).unwrap();
    std::fs::write(root.path().join("main.go"), r#"package main
import (
    "os"
    "example.com/project/helper"
)
func discarded(a, b string) {
    err := os.Rename(a, b)
    helper.Ignore(err)
}
func direct(a, b string) {
    helper.Ignore(os.Rename(a, b))
}

func checked(a, b string) {
    err := os.Rename(a, b)
    helper.Check(err)
}
func forwarded(a, b string) error {
    err := os.Rename(a, b)
    return helper.Forward(err)
}
func ignoredReturn(a, b string) {
    helper.Bridge(a, b)
}
"#).unwrap();
    let found = run_dataflow_check(root.path(), &["main.go".into()], &[]);
    let mut locations: Vec<_> = found.iter()
        .filter(|finding| finding.source.rule_id.as_deref() == Some("go-os-rename-discarded-error"))
        .map(|finding| (finding.location.path.as_str(), finding.location.range.start_line))
        .collect();
    locations.sort_unstable();
    assert_eq!(locations, [("helper/helper.go", 6), ("main.go", 7), ("main.go", 11)], "{found:#?}");
    let helper_finding = found.iter().find(|finding| finding.location.path == "helper/helper.go"
        && finding.source.rule_id.as_deref() == Some("go-os-rename-discarded-error")).unwrap();
    assert!(helper_finding.related_locations.iter().flatten()
        .any(|location| location.path == "main.go" && location.range.start_line == 23), "{helper_finding:#?}");
}

#[test]
fn msw_delay_results_are_tracked_in_web_languages() {
    for (extension, language) in [("js", "javascript"), ("ts", "typescript"), ("tsx", "typescript")] {
        for (module, kind) in [("msw", "msw-delay"), ("msw/utils/delay", "msw-utils-delay")] {
            let rule = format!("{language}-{kind}-unobserved-promise");
            let path = format!("handler.{extension}");
            let source = format!("import {{ delay as pause }} from '{module}';\nimport * as msw from '{module}';\nexport function ignored() {{ const pending = pause(20); }}\nexport function ignoredNamespace() {{ msw.delay(30); }}\nexport async function observed() {{ await pause(40); }}\nexport function forwarded() {{ return pause(50); }}\n");
            let root = write_single_file(&path, &source);
            assert_helper_locations(root.path(), &path, &rule, &[(path.as_str(), 3), (path.as_str(), 4)]);
            let unrelated = "import { delay } from 'other';\nexport function handler() { delay(20); }\n";
            std::fs::write(root.path().join(&path), unrelated).unwrap();
            assert_helper_locations(root.path(), &path, &rule, &[]);
            let shadowed = format!("import {{ delay }} from '{module}';\nexport function handler(delay) {{ delay(20); }}\n");
            std::fs::write(root.path().join(&path), shadowed).unwrap();
            assert_helper_locations(root.path(), &path, &rule, &[]);
        }
    }
}

#[test]
fn ignored_results_follow_helpers_through_web_reexports() {
    for (extension, rule) in [("js", "javascript-node-fs-discarded-promise"), ("ts", "typescript-node-fs-discarded-promise"), ("tsx", "typescript-node-fs-discarded-promise")] {
        let root = tempfile::tempdir().unwrap();
        let helper = format!("helper.{extension}");
        let barrel = format!("index.{extension}");
        let caller = format!("main.{extension}");
        std::fs::write(root.path().join(&helper), "import { rename } from 'node:fs/promises';\nexport function move(a, b) { return rename(a, b); }\n").unwrap();
        std::fs::write(root.path().join(&barrel), "export { move as moveFile } from './helper';\n").unwrap();
        std::fs::write(root.path().join(&caller), "import { moveFile } from './index';\nfunction ignored(a, b) { moveFile(a, b); }\n").unwrap();
        let found = run_dataflow_check(root.path(), std::slice::from_ref(&caller), &[]);
        let finding = found.iter().find(|item| item.source.rule_id.as_deref() == Some(rule)).expect("ignored promise through barrel");
        assert_eq!(finding.location.path, helper);
        assert!(finding.related_locations.iter().flatten().any(|location| location.path == caller), "{found:#?}");
        std::fs::write(root.path().join(&caller), "import { moveFile } from './index';\nasync function handled(a, b) { await moveFile(a, b); }\n").unwrap();
        let clean = run_dataflow_check(root.path(), &[caller], &[]);
        assert!(!clean.iter().any(|item| item.source.rule_id.as_deref() == Some(rule)), "{clean:#?}");
    }
    let root = tempfile::tempdir().unwrap();
    let helper = "import { rename } from 'node:fs/promises';\nexport function move(a, b) { return rename(a, b); }\n";
    std::fs::write(root.path().join("a.js"), helper).unwrap();
    std::fs::write(root.path().join("b.js"), helper).unwrap();
    std::fs::write(root.path().join("index.js"), "export * from './a'; export * from './b';\n").unwrap();
    std::fs::write(root.path().join("main.js"), "import { move } from './index';\nfunction ignored(a, b) { move(a, b); }\n").unwrap();
    let result = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["main.js".into()], &[]);
    assert!(result.coverage.iter().any(|issue| issue.reason == "unresolved_helper_target" && issue.rule.as_deref() == Some("javascript-node-fs-discarded-promise") && issue.path.as_deref() == Some("main.js")), "{:?}", result.coverage);
}

#[test]
fn unresolved_helper_is_reported_without_a_registered_source() {
    for (ext, rule) in [("js", "javascript-node-fs-discarded-promise"), ("ts", "typescript-node-fs-discarded-promise"), ("tsx", "typescript-node-fs-discarded-promise")] {
        let root = tempfile::tempdir().unwrap();
        let entry = format!("main.{ext}");
        for (name, body) in [("a", "return a;"), ("b", "return b;")] {
            std::fs::write(root.path().join(format!("{name}.{ext}")), format!("export function move(a, b) {{ {body} }}\n")).unwrap();
        }
        let index = root.path().join(format!("index.{ext}"));
        std::fs::write(&index, "export * from './a'; export * from './b';\n").unwrap();
        std::fs::write(root.path().join(&entry), "import { move } from './index';\nfunction run(a, b) { move(a, b); }\n").unwrap();
        let check = || autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), std::slice::from_ref(&entry), &[]);
        let result = check();
        assert!(result.coverage.iter().any(|issue| issue.reason == "unresolved_helper_target" && issue.rule.as_deref() == Some(rule) && issue.path.as_deref() == Some(&entry) && issue.line == Some(2)), "{ext}: {:?}", result.coverage);
        std::fs::write(&index, "export * from './a';\n").unwrap();
        let resolved = check();
        assert!(!resolved.coverage.iter().any(|issue| issue.reason == "unresolved_helper_target" && issue.rule.as_deref() == Some(rule)), "{ext}: {:?}", resolved.coverage);
    }
}

#[test]
fn ignored_results_follow_exported_arrow_helpers() {
    for (extension, rule) in [("js", "javascript-node-fs-discarded-promise"), ("ts", "typescript-node-fs-discarded-promise"), ("tsx", "typescript-node-fs-discarded-promise")] {
        let root = tempfile::tempdir().unwrap();
        let caller = format!("main.{extension}");
        let helper = format!("helper.{extension}");
        std::fs::write(root.path().join(&helper), "import { rename } from 'node:fs/promises';\nexport const move = (a, b) => rename(a, b);\n").unwrap();
        std::fs::write(root.path().join(format!("index.{extension}")), "export * from './helper';\n").unwrap();
        std::fs::write(root.path().join(&caller), "import { move } from './index';\nfunction ignored(a, b) { move(a, b); }\n").unwrap();
        let found = run_dataflow_check(root.path(), std::slice::from_ref(&caller), &[]);
        let finding = found.iter().find(|item| item.source.rule_id.as_deref() == Some(rule)).expect("ignored promise from arrow helper");
        assert_eq!(finding.location.path, helper);
        assert!(finding.related_locations.iter().flatten().any(|location| location.path == caller), "{found:#?}");
        std::fs::write(root.path().join(&caller), "import { move } from './index';\nasync function handled(a, b) { await move(a, b); }\n").unwrap();
        let clean = run_dataflow_check(root.path(), &[caller], &[]);
        assert!(!clean.iter().any(|item| item.source.rule_id.as_deref() == Some(rule)), "{clean:#?}");
    }
}

#[test]
fn helper_return_demand_is_kept_separate_for_each_caller() {
    let source = r#"package storage
import "os"
func moveFile(a, b string) error {
    return os.Rename(a, b)
}
func ignored(a, b string) {
    moveFile(a, b)
}
func handled(a, b string) error {
    return moveFile(a, b)
}
func mixed(a, b string) error {
    moveFile(a, b)
    return moveFile(a, b)
}
"#;
    let root = write_single_file("storage.go", source);
    let found = run_dataflow_check(root.path(), &["storage.go".into()], &[]);
    let matches: Vec<_> = found.iter()
        .filter(|finding| finding.source.rule_id.as_deref() == Some("go-os-rename-discarded-error"))
        .map(|finding| finding.location.range.start_line)
        .collect();
    assert_eq!(matches, [4], "{found:#?}");
    let mixed_only = source.replace("func ignored(a, b string) {\n    moveFile(a, b)\n}\n", "");
    let root = write_single_file("storage.go", &mixed_only);
    let found = run_dataflow_check(root.path(), &["storage.go".into()], &[]);
    assert!(found.iter().any(|finding| finding.source.rule_id.as_deref() == Some("go-os-rename-discarded-error")),
        "a used call must not hide a discarded call in the same function: {found:#?}");
}

fn assert_helper_locations(root: &Path, caller: &str, rule: &str, expected: &[(&str, u32)]) {
    let found = run_dataflow_check(root, &[caller.into()], &[]);
    let mut locations: Vec<_> = found.iter()
        .filter(|finding| finding.source.rule_id.as_deref() == Some(rule))
        .map(|finding| (finding.location.path.as_str(), finding.location.range.start_line))
        .collect();
    locations.sort_unstable();
    assert_eq!(locations, expected, "{rule}: {found:#?}");
}

#[test]
fn imported_web_helpers_track_unused_promises_in_all_web_grammars() {
    for (extension, rule) in [
        ("js", "javascript-node-fs-discarded-promise"),
        ("ts", "typescript-node-fs-discarded-promise"),
        ("tsx", "typescript-node-fs-discarded-promise"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let helper = format!("helper.{extension}");
        let caller = format!("worker.{extension}");
        std::fs::write(root.path().join(&helper), r#"import { rename } from "node:fs/promises";
export function discard(value) {}
export function forward(value) { return value; }
export function move(a, b) { return rename(a, b); }
"#).unwrap();
        std::fs::write(root.path().join(&caller), r#"import { rename } from "node:fs/promises";
import { discard, forward, move } from "./helper";
export function unused(a, b) {
  const result = rename(a, b);
  discard(result);
}
export function used(a, b) {
  const result = rename(a, b);
  return forward(result);
}
export function discardedReturn(a, b) {
  move(a, b);
}
"#).unwrap();
        assert_helper_locations(root.path(), &caller, rule, &[(helper.as_str(), 4), (caller.as_str(), 4)]);
    }
}

#[test]
fn imported_jvm_helpers_preserve_result_usage() {
    let cases = [
        (
            "Java", "java-discarded-objects-comparison", "app/Worker.java", "support/Checks.java",
            r#"/*
package misleading.comment;
*/
package support;
import java.util.Objects;
public class Checks {
    public static void discard(boolean value) {}
    public static boolean forward(boolean value) { return value; }
    public static boolean compare(Object a, Object b) { return Objects.equals(a, b); }
}

"#,
            r#"package app;
import java.util.Objects;
import support.Checks;
class Worker {
    void unused(Object a, Object b) {
        boolean same = Objects.equals(a, b);
        Checks.discard(same);
    }
    boolean used(Object a, Object b) {
        boolean same = Objects.equals(a, b);
        return Checks.forward(same);
    }
    void dropped(Object a, Object b) { Checks.compare(a, b); }
}
"#,
            6, 9,
        ),
        (
            "Kotlin", "kotlin-discarded-run-catching-result", "app/Worker.kt", "support/Checks.kt",
            r#"/*
package misleading.comment
*/
package support
import kotlin.runCatching
fun discard(value: Result<Int>) {}
fun forward(value: Result<Int>): Result<Int> = value
fun parse(value: String): Result<Int> = runCatching { value.toInt() }
"#,
            r#"package app
import kotlin.runCatching
import support.discard
import support.forward
import support.parse
fun unused(text: String) {
    val parsed = runCatching { text.toInt() }
    discard(parsed)
}
fun used(text: String): Result<Int> {
    val parsed = runCatching { text.toInt() }
    return forward(parsed)
}
fun dropped(text: String) { parse(text) }
"#,
            7, 8,
        ),
    ];
    for (_language, rule, caller, helper, helper_source, caller_source, caller_line, helper_line) in cases {
        let root = tempfile::tempdir().unwrap();
        for (path, source) in [(caller, caller_source), (helper, helper_source)] {
            let file = root.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, source).unwrap();
        }
        assert_helper_locations(root.path(), caller, rule, &[(caller, caller_line), (helper, helper_line)]);
    }
}

#[test]
fn same_package_kotlin_helpers_ignore_commented_package_text() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("app")).unwrap();
    std::fs::create_dir_all(root.path().join("lib")).unwrap();
    std::fs::write(root.path().join("lib/Helper.kt"), "/*\npackage wrong\n*/\npackage shared\nfun discard(value: Result<Int>) {}\n").unwrap();
    std::fs::write(root.path().join("app/Worker.kt"), "/*\npackage misleading\n*/\npackage shared\nimport kotlin.runCatching\nfun run(text: String) {\n val result = runCatching { text.toInt() }\n discard(result)\n}\n").unwrap();
    assert_helper_locations(root.path(), "app/Worker.kt", "kotlin-discarded-run-catching-result", &[("app/Worker.kt", 7)]);
}

const RUST_RESULT_HELPERS: &str = r#"use std::fs::remove_file;
pub(crate) fn discard(value: std::io::Result<()>) {}
pub(crate) fn forward(value: std::io::Result<()>) -> std::io::Result<()> { value }
pub(crate) fn remove(path: &str) -> std::io::Result<()> { remove_file(path) }
"#;

#[test]
fn rust_local_associated_helpers_preserve_result_usage() {
    let source = "use std::fs::remove_file;\nstruct Files;\nimpl Files {\n    fn remove(path: &str) -> std::io::Result<()> { remove_file(path) }\n    fn discard(value: std::io::Result<()>) {}\n    fn forward(value: std::io::Result<()>) -> std::io::Result<()> { value }\n    fn bridge(path: &str) -> std::io::Result<()> { Self::remove(path) }\n}\nfn ignored(path: &str) { Files::bridge(path); }\nfn observed(path: &str) -> std::io::Result<()> { Files::bridge(path) }\nfn ignored_arg(path: &str) { Files::discard(remove_file(path)); }\nfn observed_arg(path: &str) -> std::io::Result<()> { Files::forward(remove_file(path)) }\n";
    let root = write_single_file("lib.rs", source);
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 4), ("lib.rs", 11)]);
    let head = source.split("fn ignored(path:").next().unwrap();
    for body in [
        "trait Other { fn remove(path: &str); }\nfn run<Files: Other>(path: &str) { Files::remove(path); }\n",
        "fn run(path: &str) { struct Files; impl Files { fn remove(_: &str) {} } Files::remove(path); }\n",
        "mod other { pub struct Files; impl Files { pub fn remove(_: &str) {} } }\nfn run(path: &str) { use other::Files; Files::remove(path); }\n",
    ] {
        std::fs::write(root.path().join("lib.rs"), format!("{head}{body}")).unwrap();
        assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[]);
        let result = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
        assert!(result.coverage.iter().any(|issue| issue.reason == "unresolved_helper_target" && issue.path.as_deref() == Some("lib.rs")), "{body}: {:?}", result.coverage);
    }
    let guarded = source.replace("impl Files {", "#[cfg(feature = \"files\")]\nimpl Files {");
    std::fs::write(root.path().join("lib.rs"), guarded).unwrap();
    let result = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    assert!(result.coverage.iter().any(|issue| issue.reason == "unresolved_helper_target" && issue.path.as_deref() == Some("lib.rs")), "{:?}", result.coverage);
}

#[test]
fn rust_imported_associated_helpers_preserve_result_usage() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/lib.rs"), "mod support; mod worker;\n").unwrap();
    let support = "use std::fs::remove_file;\npub(crate) struct Files;\nimpl Files {\n    pub(crate) fn remove(path: &str) -> std::io::Result<()> { remove_file(path) }\n    pub(crate) fn discard(value: std::io::Result<()>) {}\n    pub(crate) fn forward(value: std::io::Result<()>) -> std::io::Result<()> { value }\n}\n";
    std::fs::write(root.path().join("src/support.rs"), support).unwrap();
    let worker = "use crate::support::Files as Store;\nuse std::fs::remove_file;\nfn ignored(path: &str) { Store::remove(path); }\nfn observed(path: &str) -> std::io::Result<()> { Store::remove(path) }\nfn ignored_arg(path: &str) { Store::discard(remove_file(path)); }\nfn observed_arg(path: &str) -> std::io::Result<()> { Store::forward(remove_file(path)) }\n";
    std::fs::write(root.path().join("src/worker.rs"), worker).unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", &[("src/support.rs", 4), ("src/worker.rs", 5)]);
    let clean = worker.replace("fn ignored(path: &str) { Store::remove(path); }", "fn ignored(path: &str) -> std::io::Result<()> { Store::remove(path) }")
        .replace("fn ignored_arg(path: &str) { Store::discard(remove_file(path)); }", "fn ignored_arg(path: &str) -> std::io::Result<()> { Store::forward(remove_file(path)) }");
    std::fs::write(root.path().join("src/worker.rs"), clean).unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", &[]);

    let shadowed = "use crate::support::Files as Store;\ntrait Other { fn remove(path: &str); }\nfn run<Store: Other>(path: &str) { Store::remove(path); }\n";
    std::fs::write(root.path().join("src/worker.rs"), shadowed).unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", &[]);

    std::fs::write(root.path().join("src/worker.rs"), worker).unwrap();
    for guarded in [support.replace("impl Files {", "#[cfg(feature = \"files\")]\nimpl Files {"),
        support.replace("    pub(crate) fn remove", "    #[cfg(feature = \"files\")]\n    pub(crate) fn remove"),
        "use std::fs::remove_file;\npub(crate) struct Files;\npub(crate) trait Delete { fn remove(path: &str) -> std::io::Result<()>; }\nimpl Delete for Files { fn remove(path: &str) -> std::io::Result<()> { remove_file(path) } }\n".into()] {
        std::fs::write(root.path().join("src/support.rs"), guarded).unwrap();
        let result = autoreview_core::run_dataflow_check_with_coverage(root.path(), root.path(), &["src/worker.rs".into()], &[]);
        assert!(!result.findings.iter().any(|finding| finding.location.path == "src/support.rs" && finding.source.rule_id.as_deref() == Some("rust-discarded-remove-file-result")), "{:#?}", result.findings);
        assert!(result.coverage.iter().any(|issue| matches!(issue.reason.as_str(), "unresolved_rust_module_import" | "unresolved_helper_target") && issue.path.as_deref() == Some("src/worker.rs")), "{:?}", result.coverage);
    }
}

fn associated_coverage_fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("src");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "mod support; mod worker;\n").unwrap();
    std::fs::write(src.join("support.rs"), "pub(crate) struct Files;\nimpl Files { pub(crate) fn known() {} }\n").unwrap();
    root
}

#[test]
fn unresolved_imported_associated_methods_report_coverage() {
    let root = associated_coverage_fixture();
    let src = root.path().join("src");
    for call in ["Store::unknown();", "Store::unknown()", "Store::known(1);", "Store::unknown::<u8>();", "let f = || Store::unknown(); f();", "let _ = async { Store::unknown(); };", "if true { Store::unknown(); }"] {
        std::fs::write(src.join("worker.rs"), format!("use crate::support::Files as Store;\nfn run() {{ {call} }}\n")).unwrap();
        let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/worker.rs".into()], &[]);
        assert!(result.coverage.iter().any(|issue| matches!(issue.reason.as_str(), "unresolved_rust_module_import" | "unresolved_helper_target") && issue.path.as_deref() == Some("src/worker.rs")), "{call}: {:?}", result.coverage);
    }
}

#[test]
fn imported_associated_call_in_skipped_function_reports_coverage() {
    let root = associated_coverage_fixture();
    let src = root.path().join("src");
    for body in ["Store::unknown(); match 1 { value => {} }", "Store::known(); let (macro_pattern!(), b) = (1, 2);"] {
        std::fs::write(src.join("worker.rs"), format!("use crate::support::Files as Store;\nfn run() {{ {body} }}\n")).unwrap();
        let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/worker.rs".into()], &[]);
        assert!(result.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow" && issue.path.as_deref() == Some("src/worker.rs") && issue.line == Some(2) && issue.rule.as_deref() == Some("rust-discarded-remove-file-result")), "{body}: {:?}", result.coverage);
    }
    std::fs::write(src.join("worker.rs"), "use crate::support::Files as Store;\nfn run() { Store::known(); }\n").unwrap();
    let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/worker.rs".into()], &[]);
    assert!(!result.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow"), "{:?}", result.coverage);
}

#[test]
fn skipped_rust_helper_coverage_follows_reachable_calls() {
    let root = associated_coverage_fixture();
    let src = root.path().join("src");
    std::fs::write(src.join("support.rs"), "pub(crate) struct Files;\nimpl Files {\n pub(crate) fn known() { match 1 { value => {} } }\n pub(crate) fn spare() { match 1 { value => {} } }\n}\n").unwrap();
    std::fs::write(src.join("worker.rs"), "use crate::support::Files as Store;\nfn run() { Store::known(); }\n").unwrap();
    let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/worker.rs".into()], &[]);
    let lines: std::collections::BTreeSet<_> = result.coverage.iter().filter(|issue| issue.reason == "unsupported_rust_control_flow"
        && issue.path.as_deref() == Some("src/support.rs") && issue.rule.as_deref() == Some("rust-discarded-remove-file-result"))
        .map(|issue| issue.line).collect();
    assert_eq!(lines, std::collections::BTreeSet::from([Some(3)]), "{:?}", result.coverage);
}

#[test]
fn rust_literal_match_tracks_results_across_arms() {
    let root = write_single_file("lib.rs", "use std::fs::remove_file;\nfn run(path: &str, flag: u8) {\n    match flag { 0 => { remove_file(path); }, _ => {} };\n}\n");
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 3)]);
    let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    assert!(!result.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow"), "{:?}", result.coverage);
    std::fs::write(root.path().join("lib.rs"), "use std::fs::remove_file;\nfn run(path: &str, flag: u8) -> std::io::Result<()> {\n    match flag { 0 => { return remove_file(path); }, _ => {} };\n    Ok(())\n}\n").unwrap();
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[]);
    std::fs::write(root.path().join("lib.rs"), "use std::fs::remove_file;\nfn run(path: &str, flag: u8) {\n    match flag { 0 => {}, _ => { remove_file(path); } };\n}\n").unwrap();
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 3)]);
    for arm in ["remove_file(path)", "{ remove_file(path) }"] {
        std::fs::write(root.path().join("lib.rs"), format!("use std::fs::remove_file;\nfn run(path: &str, flag: u8) {{\n    match flag {{ 0 => {arm}, _ => Ok(()) }};\n}}\n")).unwrap();
        assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 3)]);
    }
}

#[test]
fn rust_exhaustive_local_enum_preserves_arm_calls_and_results() {
    let source = concat!(
        "use std::fs::remove_file;\n",
        "enum Choice { Empty, Tuple(u8, u8), Record { field: u8 } }\n",
        "fn run(choice: Choice) {\n",
        " match choice {\n",
        "  self::Choice::Empty => {},\n",
        "  self::Choice::Tuple(value @ _, _) => { use_value(value); remove_file(\"old.txt\"); },\n",
        "  self :: Choice :: Record { .. } => {},\n",
        " };\n}\n",
        "fn returned(choice: Choice) -> std::io::Result<()> {\n",
        " match choice {\n",
        "  self::Choice::Empty => remove_file(\"other.txt\"),\n",
        "  self::Choice::Tuple(..) => Ok(()),\n",
        "  self::Choice::Record { .. } => Ok(()),\n",
        " }\n}\n",
    );
    let root = write_single_file("lib.rs", source);
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 6)]);
    let report = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    assert!(!report.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow"), "{:?}", report.coverage);
    assert!(report.coverage.iter().any(|issue| issue.reason == "rust_match_binding_projection" && issue.line == Some(4)), "{:?}", report.coverage);
}

#[test]
fn rust_enum_exhaustiveness_requires_unconditional_whole_variants() {
    for (declaration, first, last) in [
        ("enum Choice { A(u8), B, C }", "self::Choice::A(_)", "self::Choice::B"),
        ("enum Choice { A(u8), B }", "self::Choice::A(0)", "self::Choice::B"),
        ("enum Choice { A(u8), B }", "self::Choice::A(_) if check()", "self::Choice::B"),
        ("enum Choice { A(u8), B }", "self::Choice::A(_)", "self::Choice::A(_)"),
        ("enum Choice { A(u8), B }", "#[cfg(test)] self::Choice::A(_)", "self::Choice::B"),
        ("enum Choice { A(u8), #[cfg(test)] B }", "self::Choice::A(_)", "self::Choice::B"),
        ("enum Choice { A(u8), B }", "Choice::A(_)", "Choice::B"),
    ] {
        let source = format!("{declaration}\nfn run(choice: Choice) {{ match choice {{ {first} => {{}}, {last} => {{}} }}; }}\n");
        assert_rust_control_flow_incomplete(&source);
    }
}

#[test]
fn rust_unmodeled_matches_remain_incomplete() {
    for body in ["match x { 0 if check() && defer(|| other()) > 2 => {}, _ => {} };", "match x { _ if check() => {} };", "match x { value => {} };", "match x { 0 => {} };", "match check([risky(); 3]) { _ => {} };", "match x { _ => { let y = defer(|| risky()); } };", "consume([0; match 0 { _ => 3 }]);", "consume({ consume((match x { _ => {} },)) });"] {
        let root = write_single_file("lib.rs", &format!("fn run(x: u8) {{ {body} }}\n"));
        let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
        assert!(result.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow" && issue.path.as_deref() == Some("lib.rs")), "{body}: {:?}", result.coverage);
    }
}

#[test]
fn rust_or_patterns_follow_calls_and_keep_projection_diagnostics() {
    let source = concat!(
        "use std::fs::remove_file;\n",
        "enum Choice { First(u8, u8), Second { key: u8 }, Other }\n",
        "fn run(mut item: Choice, pairs: Vec<Result<u8, u8>>) {\n",
        " match item { Choice::First(ref mut key, _) | Choice::Second { ref mut key } => { consume(key); remove_file(\"one.txt\"); }, _ => {} };\n",
        " if let Choice::First(key, _) | Choice::Second { key } = item && allowed(key) { remove_file(\"two.txt\"); }\n",
        " let (Choice::First(key, _) | Choice::Second { key }) = item else { return };\n",
        " consume(key); remove_file(\"three.txt\");\n",
        " for (Ok(key) | Err(key)) in pairs { consume(key); remove_file(\"four.txt\"); }\n",
        "}\n",
    );
    let root = write_single_file("lib.rs", source);
    let expected = [4, 5, 7, 8].map(|line| ("lib.rs", line));
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &expected);
    let report = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    for (line, reason) in [
        (4, "rust_match_binding_projection"),
        (5, "rust_conditional_binding_projection"),
        (6, "rust_let_else_binding_projection"),
        (8, "rust_destructuring_binding_projection"),
    ] {
        assert!(report.coverage.iter().any(|issue| issue.line == Some(line) && issue.reason == reason), "{line} {reason}: {:?}", report.coverage);
    }
}

#[test]
fn rust_or_patterns_keep_inconsistent_and_guarded_bindings_incomplete() {
    for alternative in [
        "Choice::First(key, _) | Choice::First(_, other)",
        "Choice::First(ref key, _) | Choice::First(_, key)",
        "Choice::First(key, _) | Choice::First(_, key) if allowed(key)",
        "key @ _ | key @ _",
    ] {
        let source = format!("fn run(item: Choice) {{ match item {{ {alternative} => {{}}, _ => {{}} }}; }}\n");
        assert_rust_control_flow_incomplete(&source);
    }
}

fn assert_rust_control_flow_incomplete(source: &str) {
    let root = write_single_file("lib.rs", source);
    let report = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    assert!(report.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow"), "{source}: {:?}", report.coverage);
}

#[test]
fn rust_match_guards_track_nested_patterns_and_projected_names() {
    let source = r#"use std::fs::remove_file;
fn run(item: Option<(String, bool)>, extra: Option<&str>, tag: u8) {
    match item {
        Some((ref path, _)) if let Some(other) = extra && path.len() > 0 => {
            remove_file(path);
            remove_file(other);
        }
        _ => {}
    };
    match tag { 0 | 1 => { remove_file("fallback.txt"); }, _ => {} };
}
"#;
    let root = write_single_file("lib.rs", source);
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 5), ("lib.rs", 6), ("lib.rs", 10)]);
    let report = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    for reason in ["rust_match_binding_projection", "rust_conditional_binding_projection"] {
        assert!(report.coverage.iter().any(|issue| issue.reason == reason), "{:?}", report.coverage);
    }
    assert!(!report.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow"));
}

#[test]
fn rust_guarded_wildcard_let_chain_short_circuits() {
    let source = concat!(
        "fn open(tag: u8) {\n match tag { _ if let Some(_) = begin() => {}, _ => {} };\n}\n",
        "fn short(tag: u8) {\n match tag { _ if false && let Some(_) = begin() => {}, _ => {} };\n}\n",
        "fn closed(tag: u8) {\n match tag { _ if let Some(_) = begin() => { finish(); }, _ => { finish(); } };\n}\n",
    );
    assert_rust_match_cleanup("rust-guarded-wildcard-chain", source);
}

#[test]
fn rust_branch_elements_preserve_order_and_early_exits() {
    let source = concat!(
        "fn open(flag: bool) -> std::io::Result<()> {\n",
        " let _ = [begin(), if flag { token()? } else { fallback() }, finish()];\n Ok(())\n}\n",
        "fn closed(flag: bool) {\n",
        " consume((begin(), (match flag { true => token(), _ => { finish(); return } }), finish()));\n}\n",
        "fn absent(flag: bool) {\n",
        " consume((if flag { return } else { return }, begin()));\n}\n",
        "fn balanced(flag: bool) {\n",
        " let _ = [match flag { true => begin(), _ => token() }, finish()];\n}\n",
    );
    assert_rust_match_cleanup("rust-branch-element-order", source);
}

#[test]
fn rust_branch_elements_preserve_whole_results_and_report_projection_limits() {
    let source = concat!(
        "use std::fs::remove_file;\n",
        "fn dropped(flag: bool) {\n",
        " let _ = [if flag { remove_file(\"array.txt\") } else { return }, Ok(())];\n}\n",
        "fn returned(flag: bool) -> (std::io::Result<()>, ()) {\n",
        " (match flag { true => remove_file(\"tuple.txt\"), _ => Ok(()) }, ())\n}\n",
        "fn exited(flag: bool) {\n",
        " let _ = (remove_file(\"early.txt\"), if flag { return } else { return });\n}\n",
        "fn keep(value: (std::io::Result<()>, ())) -> (std::io::Result<()>, ()) { value }\n",
        "fn forwarded(flag: bool) -> (std::io::Result<()>, ()) {\n",
        " keep((if flag { remove_file(\"forwarded.txt\") } else { Ok(()) }, ()))\n}\n",
    );
    assert_rust_aggregate_results(source, &[("lib.rs", 3), ("lib.rs", 9)], 6);
}

#[test]
fn rust_branch_array_repeats_evaluate_the_operand_before_later_arguments() {
    let source = concat!(
        "fn open(flag: bool) -> std::io::Result<()> {\n",
        " consume(begin(), [if flag { token()? } else { fallback() }; 0], finish());\n Ok(())\n}\n",
        "fn closed(flag: bool) {\n",
        " consume(begin(), [(match flag { true => token(), _ => { finish(); return } }); 3], finish());\n}\n",
        "fn absent(flag: bool) {\n",
        " consume([if flag { return } else { return }; 0], begin());\n}\n",
        "fn balanced<const N: usize>(flag: bool) {\n",
        " let _ = [match flag { true => begin(), _ => token() }; N];\n finish();\n}\n",
    );
    assert_rust_match_cleanup("rust-branch-array-repeat-order", source);
}

#[test]
fn rust_branch_array_repeats_distinguish_an_empty_array_from_a_used_result() {
    let source = concat!(
        "use std::fs::remove_file;\n",
        "fn dropped(flag: bool) {\n",
        " let _ = [if flag { remove_file(\"dropped.txt\") } else { return }; 1];\n}\n",
        "fn kept(flag: bool) -> [std::io::Result<()>; 1] {\n",
        " [match flag { true => remove_file(\"kept.txt\"), _ => Ok(()) }; 1]\n}\n",
        "fn empty(flag: bool) -> [std::io::Result<()>; 0] {\n",
        " [(if flag { remove_file(\"empty.txt\") } else { Ok(()) }); (0x0_usize)]\n}\n",
        "fn passthrough(value: [std::io::Result<()>; 0]) -> [std::io::Result<()>; 0] { value }\n",
        "fn forwarded_empty(flag: bool) -> [std::io::Result<()>; 0] {\n",
        " passthrough([match flag { true => remove_file(\"forwarded.txt\"), _ => Ok(()) }; 0])\n}\n",
    );
    assert_rust_aggregate_results(source, &[("lib.rs", 3), ("lib.rs", 9), ("lib.rs", 13)], 6);
}

fn assert_rust_aggregate_results(source: &str, discarded: &[(&str, u32)], projection_line: u32) {
    let root = write_single_file("lib.rs", source);
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", discarded);
    let report = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    assert!(!report.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow"), "{:?}", report.coverage);
    assert!(report.coverage.iter().any(|issue| issue.reason == "rust_aggregate_element_projection" && issue.line == Some(projection_line)), "{:?}", report.coverage);
}

#[test]
fn rust_reads_only_the_selected_array_or_tuple_element() {
    let source = r#"use std::fs::remove_file;
fn local_array(flag: bool) -> bool {
    let values = [
        if flag { remove_file("used") } else { Ok(()) },
        remove_file("unused"),
    ];
    values[(0x0_usize)].is_ok()
}
fn local_tuple() -> std::io::Result<()> {
    let pair = (remove_file("unused"), remove_file("used"));
    pair.1
}
fn direct_array() -> bool {
    [
        remove_file("unused"),
        remove_file("used"),
    ][1].is_ok()
}
fn direct_tuple(flag: bool) -> std::io::Result<()> {
    (
        remove_file("unused"),
        match flag { true => remove_file("used"), _ => Ok(()) },
    ).1
}
fn repeat(flag: bool) -> bool {
    let values = [if flag { remove_file("used") } else { Ok(()) }; 1];
    values[0].is_ok()
}
fn shadowed() -> (std::io::Result<()>, std::io::Result<()>) {
    let pair = (
        remove_file("used from the first binding"),
        remove_file("unused from the first binding"),
    );
    let earlier = pair.0;
    let pair = (
        remove_file("unused from the second binding"),
        remove_file("used from the second binding"),
    );
    (earlier, pair.1)
}
"#;
    let root = write_single_file("lib.rs", source);
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result",
        &[("lib.rs", 5), ("lib.rs", 10), ("lib.rs", 15), ("lib.rs", 21), ("lib.rs", 32), ("lib.rs", 36)]);
    let report = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    assert!(!report.coverage.iter().any(|issue| matches!(issue.reason.as_str(),
        "unsupported_rust_control_flow" | "rust_aggregate_element_projection")), "{:?}", report.coverage);
}

#[test]
fn rust_element_reads_keep_dynamic_or_escaping_aggregates_incomplete() {
    let source = r#"use std::fs::remove_file;
fn dynamic(flag: bool, index: usize) -> bool {
    let values = [if flag { remove_file("maybe used") } else { Ok(()) }, Ok(())];
    values[index].is_ok()
}
fn escapes(flag: bool) -> bool {
    let pair = (if flag { remove_file("maybe used") } else { Ok(()) }, ());
    let escaped = pair;
    escaped.0.is_ok()
}
fn hidden_macro(flag: bool) {
    let pair = (if flag { remove_file("maybe used") } else { Ok(()) }, ());
    pair.0.is_ok();
    println!("{pair:?}");
}
fn mutated(flag: bool) -> bool {
    let mut pair = (if flag { remove_file("unknown after mutation") } else { Ok(()) }, ());
    pair.0 = Ok(());
    pair.0.is_ok()
}
"#;
    let root = write_single_file("lib.rs", source);
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[]);
    assert_rust_projection_limits(root.path(), "rust_aggregate_element_projection", &[3, 7, 12, 17]);
}

#[test]
fn rust_branch_call_arguments_preserve_evaluation_order_and_early_exits() {
    let source = concat!(
        "fn open(flag: bool) {\n invoke(begin(), if flag { token() } else { return }, finish());\n}\n",
        "fn closed(flag: bool) {\n begin().invoke((match flag { true => token(), _ => { finish(); return } }), finish());\n}\n",
        "fn absent(flag: bool) {\n invoke(token(), if flag { return } else { return }, begin());\n}\n",
        "fn unit(flag: bool) {\n invoke(if flag { begin(); }, finish());\n}\n",
    );
    assert_rust_match_cleanup("rust-branch-argument-order", source);
}

#[test]
fn rust_branch_call_argument_exit_keeps_prior_obligations_visible() {
    let source = "fn open() {\n begin(); finish(if true { return } else { return });\n}\n";
    assert_rust_match_cleanup("rust-branch-argument-skipped-cleanup", source);
}

#[test]
fn rust_branch_call_argument_question_mark_skips_later_cleanup() {
    let source = "fn open(flag: bool) -> std::io::Result<()> {\n invoke(begin(), if flag { token()? } else { fallback() }, finish());\n Ok(())\n}\n";
    assert_rust_match_cleanup("rust-branch-argument-try-cleanup", source);
}

#[test]
fn rust_branch_call_arguments_transfer_only_reachable_results() {
    let source = concat!(
        "use std::fs::remove_file;\n",
        "fn ignored(_value: std::io::Result<()>) {}\n",
        "fn kept(result: std::io::Result<()>, _unit: ()) -> std::io::Result<()> { result }\n",
        "fn open(flag: bool) {\n",
        " ignored(if flag { remove_file(\"one.txt\") } else { return });\n",
        "}\n",
        "fn closed(flag: bool) -> std::io::Result<()> {\n",
        " kept(match flag { true => remove_file(\"two.txt\"), _ => return Ok(()) }, ())\n",
        "}\n",
        "fn exited(flag: bool) -> std::io::Result<()> {\n",
        " kept(remove_file(\"three.txt\"), if flag { return Ok(()) } else { return Ok(()) })\n",
        "}\n",
    );
    let root = write_single_file("lib.rs", source);
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 5), ("lib.rs", 11)]);
    let report = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    assert!(!report.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow"), "{:?}", report.coverage);
}

#[test]
fn rust_branch_initializers_track_results_from_each_reachable_arm() {
    let initializers = [
        "if flag {\n remove_file(\"one.txt\")\n } else {\n remove_file(\"two.txt\")\n }",
        "match flag {\n true => remove_file(\"one.txt\"),\n\n _ => remove_file(\"two.txt\")\n }",
    ];
    for initializer in initializers
        .into_iter()
        .flat_map(|value| [value.to_owned(), format!("(({value}))")])
    {
        let source = format!("use std::fs::remove_file;\nfn run(flag: bool) -> std::io::Result<()> {{\n let result = {initializer};\n Ok(())\n}}\n");
        let root = write_single_file("lib.rs", &source);
        assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 4), ("lib.rs", 6)]);
        std::fs::write(root.path().join("lib.rs"), source.replace(" Ok(())\n", " result\n")).unwrap();
        assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[]);
    }
}

#[test]
fn rust_match_initializer_keeps_a_direct_return_separate() {
    let source = "use std::fs::remove_file;\nfn run(flag: bool) -> std::io::Result<()> {\n let result = match flag {\n  true => remove_file(\"example.txt\"),\n  _ => return Ok(()),\n };\n result\n}\n";
    let root = write_single_file("lib.rs", source);
    assert_rust_result_is_observed(root.path());
}

#[test]
fn rust_nested_struct_and_slice_patterns_analyze_calls_and_report_projection() {
    let source = concat!(
        "use std::fs::remove_file;\n",
        "fn run(entry: Entry, entries: &[Entry], keys: &[u8]) { remove_file(\"old.txt\");\n",
        " let Entry { name: key, tag: _, .. } = entry;\n",
        " for &Entry { name, .. } in entries {}\n",
        " if let Entry { tag: Some(tag), .. } = entry { inspect(tag); }\n",
        " match keys { [key, rest @ ..] => {}, _ => {} };\n",
        " let [head, ..] = keys else { return };\n",
        "}\n",
    );
    let root = write_single_file("lib.rs", source);
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 2)]);
    let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    for (reason, line) in [
        ("rust_destructuring_binding_projection", 3),
        ("rust_destructuring_binding_projection", 4),
        ("rust_conditional_binding_projection", 5),
        ("rust_match_binding_projection", 6),
        ("rust_let_else_binding_projection", 7),
    ] {
        assert!(result.coverage.iter().any(|issue| issue.reason == reason
            && issue.line == Some(line)), "{reason}:{line}: {:?}", result.coverage);
    }
}

#[test]
fn rust_match_arm_prefix_can_exit_before_producing_its_value() {
    let source = concat!(
        "fn open(flag: bool, abort: bool) -> u8 {\n",
        " match flag { true => { begin(); if abort { return 0; } finish(); 1 }, _ => 2 }\n}\n",
        "fn closed(flag: bool, abort: bool) -> u8 {\n",
        " match flag { true => { begin(); if abort { finish(); return 0; } finish(); 1 }, _ => 2 }\n}\n",
    );
    assert_rust_match_cleanup("rust-match-arm-early-exit", source);
}

#[test]
fn rust_tuple_branch_results_evaluate_operands_in_order() {
    let source = concat!(
        "fn open(flag: bool) {\n",
        " let (_, _) = match flag { true => (finish(), -begin()), _ => return };\n}\n",
        "fn closed(flag: bool) {\n",
        " let (_, _) = (if flag { (-begin() + finish(), 0) } else { return });\n}\n",
        "fn assigned(flag: bool) {\n",
        " let mut value = 0; match flag { true => { begin(); value = finish(); }, _ => {} };\n}\n",
    );
    assert_rust_match_cleanup("rust-tuple-operand-order", source);
    let root = write_single_file("lib.rs", source);
    assert_rust_projection_limits(root.path(), "rust_destructuring_binding_projection", &[2, 5]);
}

fn assert_rust_projection_limits(root: &std::path::Path, reason: &str, lines: &[u32]) {
    let report = run_dataflow_check_with_coverage(root, root, &["lib.rs".into()], &[]);
    for &line in lines {
        assert!(report.coverage.iter().any(|issue| issue.reason == reason && issue.line == Some(line)), "{:?}", report.coverage);
    }
}

#[test]
fn rust_eager_projections_evaluate_values_from_left_to_right() {
    let source = concat!(
        "fn open() {\n match [finish(), begin() as usize][0] { _ => {} };\n}\n",
        "fn closed() {\n match [wrap(begin()).field, finish()][0] { _ => {} };\n}\n",
        "fn slice(values: &[usize]) {\n let [first, ..] = &values[*borrow(begin())..finish()] else { return };\n consume(first);\n}\n",
    );
    assert_rust_match_cleanup("rust-eager-projection-order", source);
}

#[test]
fn rust_returned_branch_values_preserve_explicit_and_implicit_exits() {
    let conditional = "if let Some(_) = choice {\n   let result = remove_file(\"one.txt\"); result\n } else {\n   remove_file(\"two.txt\")\n }";
    for returned in [
        conditional.to_owned(),
        format!("return {conditional};"),
        format!("(({conditional}))"),
        format!("return (({conditional}));"),
    ] {
        let source = format!("use std::fs::remove_file;\nfn run(choice: Option<u8>) -> std::io::Result<()> {{\n {returned}\n}}\n");
        let root = write_single_file("lib.rs", &source);
        assert_rust_result_is_observed(root.path());
    }
}

fn assert_rust_result_is_observed(root: &std::path::Path) {
    assert_helper_locations(root, "lib.rs", "rust-discarded-remove-file-result", &[]);
    let report = run_dataflow_check_with_coverage(root, root, &["lib.rs".into()], &[]);
    assert!(!report.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow"), "{:?}", report.coverage);
}

#[test]
fn rust_conditional_bindings_follow_chain_and_else_scopes() {
    let source = r#"use std::fs::remove_file;
fn run(path: Option<Option<&str>>, callback: Option<fn(&str)>) {
    if let Some(candidate) = path
        && let Some(path) = candidate
        && path.len() > 0 {
        remove_file(path);
    } else if let Some(remove_file) = callback {
        remove_file("local.txt");
    } else {
        remove_file("fallback.txt");
    }
    remove_file("after.txt");
}
"#;
    let root = write_single_file("lib.rs", source);
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 6), ("lib.rs", 10), ("lib.rs", 12)]);
    let report = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    let projection: std::collections::BTreeSet<_> = report.coverage.iter()
        .filter(|issue| issue.reason == "rust_conditional_binding_projection").filter_map(|issue| issue.line).collect();
    assert_eq!(projection, [3, 4, 7].into(), "{:?}", report.coverage);
    assert!(!report.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow"));
}

#[test]
fn rust_conditional_loops_and_short_circuits_bound_call_sequence_paths() {
    let source = concat!(
        "fn open() {\n if let Some(_) = begin() && allowed() { finish(); }\n}\n",
        "fn short() {\n if false && let Some(_) = begin() {}\n}\n",
        "fn closed() {\n while let Some(_) = begin() && allowed() { finish(); continue; }\n finish();\n}\n",
    );
    assert_rust_match_cleanup("rust-conditional-cleanup", source);
}

#[test]
fn rust_returned_matches_preserve_results_through_helpers() {
    let source = "use std::fs::remove_file;\nfn remove(path: &str, flag: bool) -> std::io::Result<()> {\n    match flag { true => remove_file(path), _ => { let result = remove_file(path); result } }\n}\nfn run(path: &str, flag: bool) {\n    remove(path, flag);\n}\n";
    let root = write_single_file("lib.rs", source);
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 3)]);
    let clean = source.replace("    remove(path, flag);", "    assert!(remove(path, flag).is_ok());");
    for form in [clean.clone(), clean.replace("    match flag", "    return /* chosen result */ match flag").replace("result } }\n", "result } };\n")] {
        std::fs::write(root.path().join("lib.rs"), form).unwrap();
        assert_rust_result_is_observed(root.path());
    }
}

#[test]
fn rust_let_else_grouped_initializers_keep_early_exits_separate() {
    let source = concat!(
        "fn open(flag: bool) {\n",
        " let Some(_) = (if flag { begin() } else { return }) else { return };\n finish();\n}\n",
        "fn closed(flag: bool) {\n",
        " let Some(_) = (match flag { true => begin(), _ => return }) else { finish(); return };\n finish();\n}\n",
    );
    assert_rust_match_cleanup("rust-let-else-grouped-values", source);
}

#[test]
fn rust_let_else_fallbacks_follow_explicit_exits() {
    let source = concat!(
        "fn open(flag: bool) {\n",
        " let Some(_) = begin() else { if flag { finish(); return; } else { return; } };\n finish();\n}\n",
        "fn closed(flag: bool) {\n",
        " let Some(_) = begin() else { match flag { true => { finish(); return; }, _ => return finish() } };\n finish();\n}\n",
        "fn loop_exits(flag: bool) {\n",
        " loop { let Some(_) = begin() else { finish(); match flag { true => continue, _ => break } }; finish(); break; }\n}\n",
    );
    assert_rust_match_cleanup("rust-let-else-explicit-exits", source);
}

#[test]
fn rust_let_else_macro_divergence_stays_incomplete() {
    let root = write_single_file("lib.rs", "fn run(choice: Option<u8>) {\n let Some(_) = choice else { panic!(\"missing choice\"); };\n}\n");
    let report = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    assert!(report.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow"), "{:?}", report.coverage);
}

#[test]
fn rust_let_else_lowers_both_paths_and_limits_projection() {
    let source = concat!(
        "use std::fs::remove_file;\n",
        "fn run(path: Option<&str>) {\n",
        "    let Some(path) = path else {\n",
        "        remove_file(\"fallback.txt\");\n",
        "        return;\n",
        "    };\n",
        "    remove_file(path);\n",
        "}\n",
    );
    let root = write_single_file("lib.rs", source);
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 4), ("lib.rs", 7)]);
    let scan = || run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    let report = scan();
    assert!(!report.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow"));
    assert!(report.coverage.iter().any(|issue| issue.reason == "rust_let_else_binding_projection" && issue.line == Some(3)));
    let unbound = source.replace("Some(path)", "Some(_)").replace("remove_file(path);", "remove_file(\"present.txt\");");
    std::fs::write(root.path().join("lib.rs"), unbound).unwrap();
    assert!(!scan().coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow" || issue.reason == "rust_let_else_binding_projection"));
}

#[test]
fn rust_tuple_bindings_analyze_calls_without_claiming_component_identity() {
    let source = r#"use std::fs::remove_file;
fn run(pairs: Vec<(&str, u8)>, pair: (&str, (u8, bool))) {
    let (name, (_, _)) = pair;
    remove_file(name);
    for (path, _) in pairs {
        remove_file(path);
    }
}
"#;
    let root = write_single_file("lib.rs", source);
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 4), ("lib.rs", 6)]);
    let report = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    let projections: std::collections::BTreeSet<_> = report.coverage.iter()
        .filter(|issue| issue.reason == "rust_destructuring_binding_projection").filter_map(|issue| issue.line).collect();
    assert_eq!(projections, [3, 5].into(), "{:?}", report.coverage);
    assert!(!report.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow"));
}

#[test]
fn rust_match_bindings_preserve_arm_scope_and_report_projection_limit() {
    let source = "use std::fs::remove_file;\nfn run(path: &str, item: Option<&str>) {\n    let result = remove_file(path);\n    match item { Some(result) => { inspect(result); }, _ => {} };\n}\n";
    let root = write_single_file("lib.rs", source);
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 3)]);
    let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    assert!(result.coverage.iter().any(|issue| issue.reason == "rust_match_binding_projection" && issue.path.as_deref() == Some("lib.rs") && issue.line == Some(4) && issue.rule.as_deref() == Some("rust-discarded-remove-file-result")), "{:?}", result.coverage);
    assert!(!result.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow"), "{:?}", result.coverage);
    let after = "use std::fs::remove_file;\nfn run(path: &str, item: Option<&str>) -> std::io::Result<()> {\n    let result = remove_file(path);\n    match item { Some(result) => { inspect(result); }, _ => {} };\n    result\n}\n";
    std::fs::write(root.path().join("lib.rs"), after).unwrap();
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[]);
    let inside = "use std::fs::remove_file;\nfn run(item: Option<&str>) {\n    match item { Some(path) => { remove_file(path); }, _ => {} };\n}\n";
    std::fs::write(root.path().join("lib.rs"), inside).unwrap();
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 3)]);
    let siblings = "use std::fs::remove_file;\nenum Choice<'a> { Left(&'a str), Right(&'a str), Other }\nfn run(value: Choice<'_>) {\n    match value {\n        Choice::Left(path) => { remove_file(path); },\n        Choice::Right(path) => { remove_file(path); },\n        _ => {}\n    };\n}\n";
    std::fs::write(root.path().join("lib.rs"), siblings).unwrap();
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 5), ("lib.rs", 6)]);
}

#[test]
fn rust_unbound_enum_match_tracks_results() {
    for (pattern, ty) in [("Some(_)", "Option<u8>"), ("Option::Some(_)", "Option<u8>"), ("Mode::Fast", "Mode")] {
        let source = format!("use std::fs::remove_file; enum Mode {{ Fast, Other }}\nfn run(path: &str, value: {ty}) {{\n    match value {{ {pattern} => {{ remove_file(path); }}, _ => {{}} }};\n}}\n");
        let root = write_single_file("lib.rs", &source);
        assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 3)]);
        let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
        assert!(!result.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow"), "{pattern}: {:?}", result.coverage);
    }
}

fn assert_rust_match_cleanup(rule: &str, source: &str) {
    use autoreview_core::ResolvedRulePack;
    use autoreview_schema::RulePackTrust;
    let repo = tempfile::tempdir().unwrap();
    let pack = tempfile::tempdir().unwrap();
    std::fs::write(pack.path().join("rust.yml"), format!("id: {rule}\nkind: call-sequence\nlanguage: Rust\ncategory: correctness\nseverity: warning\nmessage: Finish before leaving\nafter: [{{call: begin}}]\nunless: [{{call: finish}}]\ncheckBeforeReturn: true\n")).unwrap();
    std::fs::write(repo.path().join("lib.rs"), source).unwrap();
    let result = run_dataflow_check_with_coverage(repo.path(), repo.path(), &["lib.rs".into()], &[ResolvedRulePack {
        id: "match-cleanup".into(), local_path: pack.path().to_owned(), trust: RulePackTrust::Full,
    }]);
    let lines: Vec<_> = result.findings.iter().filter(|finding| finding.source.rule_id.as_deref() == Some(rule))
        .map(|finding| finding.location.range.start_line).collect();
    assert_eq!(lines.len(), 1, "{lines:?}: {:#?}", result.findings);
    assert!(lines[0] <= 3, "{lines:?}");
    assert!(!result.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow"), "{:?}", result.coverage);
}

#[test]
fn rust_literal_match_checks_cleanup_on_each_arm() {
    let source = "fn partial() {\n match begin() { Some(_) => { finish(); }, _ => {} };\n}\nfn closed() {\n match begin() { Some(_) => { finish(); }, _ => { finish(); } };\n}\n";
    assert_rust_match_cleanup("rust-match-cleanup", source);
}

#[test]
fn rust_match_guard_side_effects_reach_later_arms() {
    let source = "fn partial(flag: u8) {\n match flag { 0 if begin() => { finish(); }, _ => {} };\n}\nfn clean(flag: u8) {\n match flag { 0 if begin() => { finish(); }, _ => { finish(); } };\n}\n";
    assert_rust_match_cleanup("rust-guard-cleanup", source);
}

#[test]
fn rust_ordinary_if_short_circuits_cleanup_paths() {
    let source = concat!(
        "fn open(flag: bool) {\n if flag && begin() { finish(); }\n}\n",
        "fn closed() {\n if true || begin() {}\n if false && begin() {}\n if !(true || begin()) {}\n}\n",
        "fn both_arms(flag: bool) {\n if flag || begin() { finish(); } else { finish(); }\n}\n",
        "fn nested(flag: bool) {\n match flag { true => { if false && begin() {} }, _ => {} };\n}\n",
    );
    assert_rust_match_cleanup("rust-if-short-circuit", source);
}

#[test]
fn rust_ordinary_while_short_circuits_cleanup_paths() {
    let source = concat!(
        "fn open(flag: bool) {\n while flag && begin() { finish(); break; }\n}\n",
        "fn never_enters() {\n while false && begin() {}\n}\n",
        "fn no_right_operand() {\n while true || begin() { break; }\n}\n",
        "fn closed(flag: bool) {\n while flag && begin() { finish(); break; }\n finish();\n}\n",
    );
    assert_rust_match_cleanup("rust-while-short-circuit", source);
}

#[test]
fn rust_match_guards_short_circuit_before_running_the_right_operand() {
    let source = concat!(
        "fn partial(flag: u8) {\n match flag { 0 if gate() || begin() => {}, _ => {} };\n}\n",
        "fn clean(flag: u8) {\n match flag { 0 if gate() && begin() => { finish(); }, _ => { finish(); } };\n}\n",
        "fn skipped(flag: u8) {\n match flag { 0 if false && begin() => {}, 1 if !(true || begin()) => {}, _ => {} };\n}\n",
    );
    assert_rust_match_cleanup("rust-short-circuit-cleanup", source);
}

#[test]
fn rust_match_guard_tracks_cleanup_through_a_helper() {
    use autoreview_core::ResolvedRulePack;
    use autoreview_schema::RulePackTrust;
    let repo = tempfile::tempdir().unwrap();
    let pack = tempfile::tempdir().unwrap();
    std::fs::write(pack.path().join("rust.yml"), "id: rust-guard-helper\nkind: call-sequence\nlanguage: Rust\ncategory: correctness\nseverity: warning\nmessage: Remove before finishing\nafter: [{call: register}]\nunless: [{call: remove}]\nbefore: [{call: finish}]\nidentity: {receiver: true, keyArgument: 0}\n").unwrap();
    let source = "fn gate(root: &mut Registry, key: &str) -> bool { root.register(key); true }\nfn run(root: &mut Registry, key: &str, flag: u8) {\n match flag { 0 if gate(root, key) => { root.remove(key); }, _ => {} };\n root.finish(key);\n}\n";
    std::fs::write(repo.path().join("lib.rs"), source).unwrap();
    let scan = || run_dataflow_check(repo.path(), &["lib.rs".into()], &[ResolvedRulePack {
        id: "guard-helper".into(), local_path: pack.path().to_owned(), trust: RulePackTrust::Full,
    }]);
    let findings = scan();
    assert_eq!(findings.iter().filter(|finding| finding.source.rule_id.as_deref() == Some("rust-guard-helper")).count(), 1, "{findings:#?}");
    std::fs::write(repo.path().join("lib.rs"), source.replace("_ => {}", "_ => { root.remove(key); }")).unwrap();
    let findings = scan();
    assert!(!findings.iter().any(|finding| finding.source.rule_id.as_deref() == Some("rust-guard-helper")), "{findings:#?}");
}

#[test]
fn rust_proven_pattern_bindings_keep_results_through_arms_and_conditions() {
    let source = r#"use std::fs::remove_file;
fn nested() -> std::io::Result<()> {
    let ((used, _), [unused]) = ((remove_file("used"), 0), [remove_file("unused")]);
    used
}
fn matched() {
    match remove_file("matched") { ignored @ _ => {} };
}
fn condition() -> std::io::Result<()> {
    if let [result @ _] = [remove_file("condition")] { return result; }
    Ok(())
}
fn declaration() -> std::io::Result<()> {
    let (result @ _, _) = (remove_file("declaration"), 0) else { return Ok(()) };
    result
}
"#;
    let root = write_single_file("lib.rs", source);
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 3), ("lib.rs", 7)]);
    let report = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    assert!(report.coverage.is_empty(), "{:?}", report.coverage);
}

#[test]
fn rust_conditional_literal_with_a_constant_keeps_the_failure_path() {
    let source = "use std::fs::remove_file;\nconst KEY: u8 = 1;\nfn run() {\n if let (KEY, _) = (0, 1) {} else { remove_file(\"fallback\"); }\n}\n";
    let root = write_single_file("lib.rs", source);
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 4)]);
    let report = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    assert!(report.coverage.iter().any(|issue| issue.reason == "rust_conditional_binding_projection"), "{:?}", report.coverage);
}

#[test]
fn rust_labeled_jumps_in_loop_conditions_keep_coverage_incomplete() {
    let source = r#"use std::fs::remove_file;
fn own_condition(flag: bool) {
    'outer: while { if flag { break 'outer; } true } {}
    remove_file("after");
}
fn nested_condition(flag: bool) {
    'outer: loop { while { if flag { break 'outer; } true } {} }
    remove_file("after");
}
"#;
    let root = write_single_file("lib.rs", source);
    let report = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    for line in [2, 6] {
        assert!(report.coverage.iter().any(|issue| {
            issue.line == Some(line) && issue.reason == "unsupported_rust_control_flow"
                && issue.rule.as_deref() == Some("rust-discarded-remove-file-result")
        }), "{:?}", report.coverage);
    }
}

#[test]
fn rust_statement_unsafe_blocks_and_reassignment_arms_follow_calls() {
    let source = r#"use std::fs::remove_file;
fn run(flag: bool) {
    unsafe { remove_file("unsafe"); }
    let mut result;
    result = if flag { remove_file("one") } else { remove_file("two") };
    result = match flag { true => remove_file("three"), _ => remove_file("four") };
    let _handled = result.is_ok();
}
"#;
    let root = write_single_file("lib.rs", source);
    let report = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    let mut locations: Vec<_> = report.findings.iter()
        .filter(|finding| finding.source.rule_id.as_deref() == Some("rust-discarded-remove-file-result"))
        .map(|finding| finding.location.range.start_line).collect();
    locations.sort_unstable();
    assert_eq!(locations, [3, 5], "{:?}", report.findings);
    assert!(!report.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow"), "{:?}", report.coverage);
}

#[test]
fn rust_tuple_literal_bindings_track_each_result_in_a_match_arm() {
    let source = r#"use std::fs::remove_file;
fn run(flag: bool) -> std::io::Result<()> {
    match flag {
        true => {
            let (used, unused) = (remove_file("used"), remove_file("unused"));
            let _ok = used.is_ok();
            Ok(())
        }
        _ => Ok(()),
    }
}
"#;
    let root = write_single_file("lib.rs", source);
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 5)]);
    let scan = || run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    assert!(scan().coverage.is_empty(), "{:?}", scan().coverage);
    let clean = source.replace("let _ok = used.is_ok();", "let _ok = used.is_ok();\n            let _also_ok = unused.is_ok();");
    std::fs::write(root.path().join("lib.rs"), clean).unwrap();
    assert!(scan().findings.is_empty());
    assert!(scan().coverage.is_empty());
}

#[test]
fn rust_child_resolves_a_private_associated_alias_from_its_ancestor() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("src");
    std::fs::create_dir_all(src.join("parent")).unwrap();
    let files = [
        ("lib.rs", "mod support; mod parent; mod sibling;\n"),
        ("support.rs", "pub(crate) struct Files;\nimpl Files { pub(crate) fn remove(path: &str) -> std::io::Result<()> { std::fs::remove_file(path) } }\n"),
        ("parent.rs", "mod worker;\nuse crate::support::Files as Store;\n"),
        ("parent/worker.rs", "use super::Store as Disk;\nfn run() { Disk::remove(\"old.txt\"); }\n"),
        ("sibling.rs", "use crate::parent::Store as Disk;\nfn run() { Disk::remove(\"old.txt\"); }\n"),
    ];
    for (path, source) in files {
        std::fs::write(src.join(path), source).unwrap();
    }
    assert_helper_locations(root.path(), "src/parent/worker.rs", "rust-discarded-remove-file-result", &[("src/support.rs", 2)]);
    let scan = |path: &str| run_dataflow_check_with_coverage(root.path(), root.path(), &[path.into()], &[]);
    assert!(scan("src/parent/worker.rs").coverage.is_empty());
    let sibling = scan("src/sibling.rs");
    assert!(sibling.findings.is_empty());
    assert!(sibling.coverage.iter().any(|issue| issue.reason == "unresolved_rust_module_import"));
}

#[test]
fn rust_associated_calls_only_consider_imports_in_their_scope() {
    let root = associated_coverage_fixture();
    let src = root.path().join("src");
    std::fs::write(src.join("support.rs"), "pub(crate) struct Files;\nimpl Files { pub(crate) fn remove(path: &str) -> std::io::Result<()> { std::fs::remove_file(path) } }\n").unwrap();
    let worker = r#"use crate::support::Files;
fn run(path: &str) {
    {
        use std::path::Path as Files;
        Files::new(path);
    }
    use std::{io, collections::HashMap};
    Files::remove(path);
}
"#;
    std::fs::write(src.join("worker.rs"), worker).unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", &[("src/support.rs", 2)]);
    let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/worker.rs".into()], &[]);
    assert!(!result.coverage.iter().any(|issue| issue.reason == "unresolved_helper_target" && issue.line == Some(8)), "{:?}", result.coverage);
    assert!(result.coverage.iter().any(|issue| issue.reason == "unresolved_helper_target" && issue.line == Some(5)), "{:?}", result.coverage);
}

#[test]
fn rust_match_guard_tracks_results_passed_to_helpers() {
    let source = "use std::fs::remove_file;\nfn ignore(_value: std::io::Result<()>) -> bool { true }\nfn run(path: &str, flag: u8) {\n    match flag { 0 if ignore(remove_file(path)) => {}, _ => {} };\n}\n";
    let root = write_single_file("lib.rs", source);
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 4)]);
    let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    assert!(!result.coverage.iter().any(|issue| issue.reason == "unsupported_rust_control_flow"), "{:?}", result.coverage);
    let bound = "use std::fs::remove_file;\nfn allowed(_path: &str) -> bool { true }\nfn run(value: Option<&str>) {\n    match value { Some(path) if allowed(path) => { remove_file(path); }, _ => {} };\n}\n";
    std::fs::write(root.path().join("lib.rs"), bound).unwrap();
    assert_helper_locations(root.path(), "lib.rs", "rust-discarded-remove-file-result", &[("lib.rs", 4)]);
    let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["lib.rs".into()], &[]);
    assert!(result.coverage.iter().any(|issue| issue.reason == "rust_match_binding_projection" && issue.line == Some(4)), "{:?}", result.coverage);
}

#[test]
fn rust_reexported_associated_helpers_preserve_result_usage() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("src");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "mod support; mod facade; mod worker;\n").unwrap();
    std::fs::write(src.join("support.rs"), "use std::fs::remove_file;\npub(crate) struct Files;\nimpl Files {\n    pub(crate) fn remove(path: &str) -> std::io::Result<()> { remove_file(path) }\n    pub(crate) fn forward(value: std::io::Result<()>) -> std::io::Result<()> { value }\n}\n").unwrap();
    std::fs::write(src.join("facade.rs"), "pub(crate) use crate::support::Files as Store;\n").unwrap();
    let worker = "use crate::facade::Store as Disk;\nfn ignored(path: &str) { Disk::remove(path); }\nfn observed(path: &str) -> std::io::Result<()> { Disk::forward(Disk::remove(path)) }\n";
    std::fs::write(src.join("worker.rs"), worker).unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", &[("src/support.rs", 4)]);
    std::fs::write(src.join("worker.rs"), worker.replace("fn ignored(path: &str) { Disk::remove(path); }", "fn ignored(path: &str) -> std::io::Result<()> { Disk::remove(path) }")).unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", &[]);

    std::fs::write(src.join("worker.rs"), worker).unwrap();
    let support = std::fs::read_to_string(src.join("support.rs")).unwrap();
    std::fs::write(src.join("support.rs"), support.replace("{ remove_file(path) }", "{ let _ = format!(\"{path}\"); remove_file(path) }")).unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", &[("src/support.rs", 4)]);
    std::fs::write(src.join("support.rs"), support).unwrap();
    std::fs::write(src.join("lib.rs"), "mod support; mod middle; mod facade; mod worker;\n").unwrap();
    std::fs::write(src.join("middle.rs"), "pub(crate) use crate::support::Files as Inner;\n").unwrap();
    std::fs::write(src.join("facade.rs"), "pub(crate) use crate::middle::Inner as Store;\n").unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", &[("src/support.rs", 4)]);

    for facade in [
        "#[cfg(feature = \"files\")] pub(crate) use crate::support::Files as Store;\n",
        "pub(crate) use crate::support::Files as Store; pub(crate) use crate::middle::Inner as Store;\n",
        "pub(crate) use crate::support::Files as Store; pub(crate) use crate::middle::*;\n",
        "pub(crate) use crate::support::Files as Store; make_items!();\n",
        "pub(crate) use crate::{support::Files as Store, middle::{Inner as Store}};\n",
        "pub(crate) use crate::facade::Store;\n",
    ] {
        std::fs::write(src.join("facade.rs"), facade).unwrap();
        assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", &[]);
        let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/worker.rs".into()], &[]);
        assert!(result.coverage.iter().any(|issue| issue.reason == "unresolved_rust_module_import" && issue.path.as_deref() == Some("src/worker.rs")), "{facade}: {:?}", result.coverage);
    }
}

#[test]
fn imported_rust_helpers_preserve_result_usage() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/support.rs"), RUST_RESULT_HELPERS).unwrap();
    std::fs::write(root.path().join("src/lib.rs"), r#"mod support;
use std::fs::remove_file;
fn unused(path: &str) {
    let result = remove_file(path);
    support::discard(result);
}
fn used(path: &str) -> std::io::Result<()> {
    let result = remove_file(path);
    support::forward(result)
}
fn dropped(path: &str) {
    support::remove(path);
}
"#).unwrap();
    assert_helper_locations(root.path(), "src/lib.rs", "rust-discarded-remove-file-result", &[("src/lib.rs", 4), ("src/support.rs", 4)]);
}

const RUST_WORKER: &str = r#"use crate::support as tasks;
use std::fs::remove_file;
fn unused(path: &str) {
    let result = remove_file(path);
    tasks::discard(result);
}
fn used(path: &str) -> std::io::Result<()> {
    let result = remove_file(path);
    tasks::forward(result)
}
fn dropped(path: &str) { tasks::remove(path); }
"#;

#[test]
fn rust_unresolved_imports_identify_each_used_name() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/lib.rs"),
        "use crate::missing::{first, second, unused};\nfn run() { first(); second(); first(); }\n").unwrap();
    let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/lib.rs".into()], &[]);
    let issues: Vec<_> = result.coverage.iter().filter(|issue| issue.reason == "unresolved_rust_module_import").collect();
    assert_eq!(issues.len(), 2, "{issues:?}");
    for name in ["first", "second"] {
        assert!(issues.iter().any(|issue| issue.line == Some(1) && issue.detail.contains(&format!("`{name}`"))), "{issues:?}");
    }
}

#[test]
fn rust_private_parent_module_imports_preserve_helper_result_usage() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("src");
    std::fs::create_dir_all(src.join("parent")).unwrap();
    std::fs::write(src.join("lib.rs"), "mod support; mod parent;\n").unwrap();
    std::fs::write(src.join("support.rs"), RUST_RESULT_HELPERS).unwrap();
    let worker = RUST_WORKER.replace("use crate::support as tasks;", "use super::tasks;");
    std::fs::write(src.join("parent/worker.rs"), worker).unwrap();
    std::fs::write(src.join("parent.rs"), "use super::support as tasks; mod worker;\n").unwrap();
    assert_helper_locations(root.path(), "src/parent/worker.rs", "rust-discarded-remove-file-result", &[("src/parent/worker.rs", 4), ("src/support.rs", 4)]);
    let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/parent/worker.rs".into()], &[]);
    assert!(!result.coverage.iter().any(|issue| issue.reason == "unresolved_rust_module_import" && issue.path.as_deref() == Some("src/parent/worker.rs")), "{:?}", result.coverage);
    for parent in [
        "#[cfg(feature = \"optional\")] use super::support as tasks; mod worker;\n",
        "use super::support as tasks; use super::other as tasks; mod worker;\n",
        "use super::support as tasks; use super::other::*; mod worker;\n",
        "use super::{support as tasks, other::{x}}; mod worker;\n",
        "use super::support as tasks; generated_items!(); mod worker;\n",
    ] {
        std::fs::write(src.join("parent.rs"), parent).unwrap();
        let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/parent/worker.rs".into()], &[]);
        assert!(result.coverage.iter().any(|issue| issue.reason == "unresolved_rust_module_import" && issue.path.as_deref() == Some("src/parent/worker.rs")), "{parent}: {:?}", result.coverage);
        assert!(!result.findings.iter().any(|finding| finding.source.rule_id.as_deref() == Some("rust-discarded-remove-file-result")), "{parent}: {:?}", result.findings);
    }
}

#[test]
fn rust_module_aliases_preserve_helper_result_usage() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/lib.rs"), "mod support; mod worker;\n").unwrap();
    std::fs::write(root.path().join("src/support.rs"), RUST_RESULT_HELPERS).unwrap();
    std::fs::write(root.path().join("src/worker.rs"), RUST_WORKER).unwrap();
    let expected = &[("src/support.rs", 4), ("src/worker.rs", 4)];
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", expected);
    let clean = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/worker.rs".into()], &[]);
    assert!(!clean.coverage.iter().any(|issue| issue.reason == "unresolved_rust_module_import"));
    let worker = std::fs::read_to_string(root.path().join("src/worker.rs")).unwrap();
    std::fs::write(root.path().join("src/worker.rs"), worker.replace("use crate::support as tasks;", "use crate::support::{self as tasks};")).unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", expected);
    std::fs::write(root.path().join("src/worker.rs"), &worker).unwrap();
    for crate_root in ["mod worker;\n", "#[path = \"alternate.rs\"] mod support; mod worker;\n", "#[cfg(feature = \"optional\")] mod support; mod worker;\n"] {
        std::fs::write(root.path().join("src/lib.rs"), crate_root).unwrap();
        let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/worker.rs".into()], &[]);
        assert!(!result.findings.iter().any(|finding| finding.source.rule_id.as_deref() == Some("rust-discarded-remove-file-result")), "{crate_root}: {:#?}", result.findings);
        assert!(result.coverage.iter().any(|issue| issue.reason == "unresolved_rust_module_import" && issue.path.as_deref() == Some("src/worker.rs") && issue.line == Some(1)), "{crate_root}: {:?}", result.coverage);
    }
    std::fs::write(root.path().join("src/lib.rs"), "mod support; mod worker;\n").unwrap();
    std::fs::write(root.path().join("src/worker.rs"), format!("#[cfg(feature = \"optional\")]\n{worker}")).unwrap();
    let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/worker.rs".into()], &[]);
    assert!(!result.findings.iter().any(|finding| finding.source.rule_id.as_deref() == Some("rust-discarded-remove-file-result")), "{:#?}", result.findings);
    assert!(result.coverage.iter().any(|issue| issue.reason == "unresolved_rust_module_import" && issue.line == Some(2)), "{:?}", result.coverage);
    std::fs::write(root.path().join("src/lib.rs"), "mod group; mod worker;\n").unwrap();
    std::fs::write(root.path().join("src/group.rs"), "pub(crate) mod support;\n").unwrap();
    std::fs::create_dir(root.path().join("src/group")).unwrap();
    std::fs::copy(root.path().join("src/support.rs"), root.path().join("src/group/support.rs")).unwrap();
    std::fs::write(root.path().join("src/worker.rs"), worker.replace("crate::support", "crate::group::support")).unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", &[("src/group/support.rs", 4), ("src/worker.rs", 4)]);
    std::fs::create_dir(root.path().join("src/group/support")).unwrap();
    std::fs::write(root.path().join("src/group/support/mod.rs"), RUST_RESULT_HELPERS).unwrap();
    let ambiguous = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/worker.rs".into()], &[]);
    assert!(ambiguous.coverage.iter().any(|issue| issue.reason == "unresolved_rust_module_import"), "{:?}", ambiguous.coverage);
    assert!(!ambiguous.findings.iter().any(|finding| finding.source.rule_id.as_deref() == Some("rust-discarded-remove-file-result")), "{:#?}", ambiguous.findings);
    std::fs::write(root.path().join("src/lib.rs"), "#[path = \"alternate.rs\"]\nmod support;\nfn run() { support::remove(\"x\"); }\n").unwrap();
    let custom = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/lib.rs".into()], &[]);
    assert!(custom.coverage.iter().any(|issue| issue.reason == "unresolved_rust_module_import" && issue.path.as_deref() == Some("src/lib.rs")), "{:?}", custom.coverage);
    assert!(!custom.findings.iter().any(|finding| finding.source.rule_id.as_deref() == Some("rust-discarded-remove-file-result")), "{:#?}", custom.findings);
}

#[test]
fn rust_reexported_helpers_preserve_result_usage() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/lib.rs"), "mod support; mod api; mod worker;\n").unwrap();
    std::fs::write(root.path().join("src/support.rs"), RUST_RESULT_HELPERS).unwrap();
    std::fs::write(root.path().join("src/api.rs"), "pub(crate) use crate::support::{discard as ignore, forward, remove};\n").unwrap();
    let source = RUST_WORKER.replace("crate::support", "crate::api").replace("tasks::discard", "tasks::ignore");
    std::fs::write(root.path().join("src/worker.rs"), source).unwrap();
    let expected = &[("src/support.rs", 4), ("src/worker.rs", 4)];
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", expected);
    let worker = std::fs::read_to_string(root.path().join("src/worker.rs")).unwrap();
    let imports = "use crate::api::{ignore, forward, remove};";
    let direct = worker.replace("use crate::api as tasks;", imports).replace("tasks::", "");
    std::fs::write(root.path().join("src/worker.rs"), direct).unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", expected);
    std::fs::write(root.path().join("src/worker.rs"), worker.replace("tasks::ignore", "tasks::discard")).unwrap();
    std::fs::write(root.path().join("src/api.rs"), "pub(crate) use crate::support::*;\n").unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", expected);
    std::fs::write(root.path().join("src/worker.rs"), &worker).unwrap();
    std::fs::write(root.path().join("src/lib.rs"), "mod support; mod layer; mod api; mod worker;\n").unwrap();
    std::fs::write(root.path().join("src/layer.rs"), "pub(crate) use crate::support::{discard as ignore, forward, remove};\n").unwrap();
    std::fs::write(root.path().join("src/api.rs"), "pub(crate) use crate::layer::{ignore, forward, remove};\n").unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", expected);
    std::fs::create_dir(root.path().join("src/api")).unwrap();
    std::fs::write(root.path().join("src/api/inner.rs"), RUST_RESULT_HELPERS).unwrap();
    std::fs::write(root.path().join("src/api.rs"), "mod inner; pub(crate) use self::inner::{discard as ignore, forward, remove};\n").unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", &[("src/api/inner.rs", 4), ("src/worker.rs", 4)]);
    std::fs::write(root.path().join("src/support2.rs"), RUST_RESULT_HELPERS).unwrap();
    std::fs::write(root.path().join("src/lib.rs"), "mod support; mod support2; mod layer; mod api; mod worker;\n").unwrap();
    std::fs::write(root.path().join("src/api.rs"), "pub(crate) use crate::support::*; pub(crate) use crate::support2::*;\n").unwrap();
    std::fs::write(root.path().join("src/worker.rs"), worker.replace("tasks::ignore", "tasks::discard")).unwrap();
    let ambiguous = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/worker.rs".into()], &[]);
    assert!(ambiguous.coverage.iter().any(|issue| issue.reason == "unresolved_helper_target"), "{:?}", ambiguous.coverage);
    assert!(!ambiguous.findings.iter().any(|finding| finding.source.rule_id.as_deref() == Some("rust-discarded-remove-file-result")), "{:#?}", ambiguous.findings);
    std::fs::write(root.path().join("src/worker.rs"), &worker).unwrap();
    std::fs::write(root.path().join("src/api.rs"), "pub(crate) use crate::support::{discard as ignore, forward, remove}; pub(crate) use crate::support2::*;\n").unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", expected);
    for reexport in ["pub(crate) use crate::missing::*;\n", "#[cfg(feature = \"x\")] pub(crate) use crate::support::*;\n", "pub(super) use crate::support::*;\n"] {
        std::fs::write(root.path().join("src/api.rs"), reexport).unwrap();
        let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/worker.rs".into()], &[]);
        assert!(result.coverage.iter().any(|issue| issue.reason == "unresolved_helper_target"), "{reexport}: {:?}", result.coverage);
        assert!(!result.findings.iter().any(|finding| finding.source.rule_id.as_deref() == Some("rust-discarded-remove-file-result")), "{reexport}: {:#?}", result.findings);
    }
}

#[test]
fn rust_super_module_paths_preserve_helper_result_usage() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/lib.rs"), "mod support; mod api; mod worker;\n").unwrap();
    std::fs::write(root.path().join("src/support.rs"), RUST_RESULT_HELPERS).unwrap();
    let worker = RUST_WORKER.replace("crate::support", "super::support");
    std::fs::write(root.path().join("src/worker.rs"), &worker).unwrap();
    let expected = &[("src/support.rs", 4), ("src/worker.rs", 4)];
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", expected);
    let direct = worker.replace("use super::support as tasks;", "use super::support::{discard as ignore, forward, remove};")
        .replace("tasks::discard", "ignore").replace("tasks::forward", "forward").replace("tasks::remove", "remove");
    std::fs::write(root.path().join("src/worker.rs"), direct).unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", expected);
    std::fs::write(root.path().join("src/api.rs"), "pub(crate) use super::support::{discard as ignore, forward, remove};\n").unwrap();
    let reexported = RUST_WORKER.replace("crate::support", "crate::api").replace("tasks::discard", "tasks::ignore");
    std::fs::write(root.path().join("src/worker.rs"), reexported).unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", expected);
}

#[test]
fn rust_nested_relative_modules_and_reexports_follow_verified_parents() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("src");
    std::fs::create_dir_all(src.join("group/sub")).unwrap();
    std::fs::write(src.join("lib.rs"), "mod group;\n").unwrap();
    std::fs::write(src.join("group.rs"), "mod support; mod sub;\n").unwrap();
    std::fs::write(src.join("group/support.rs"), RUST_RESULT_HELPERS).unwrap();
    std::fs::write(src.join("group/sub.rs"), "mod api; mod worker;\n").unwrap();
    let worker = RUST_WORKER.replace("crate::support", "super::super::support");
    std::fs::write(src.join("group/sub/worker.rs"), &worker).unwrap();
    let expected = &[("src/group/sub/worker.rs", 4), ("src/group/support.rs", 4)];
    assert_helper_locations(root.path(), "src/group/sub/worker.rs", "rust-discarded-remove-file-result", expected);
    std::fs::write(src.join("group/sub/api.rs"), "pub(crate) use super::super::support::{discard as ignore, forward, remove};\n").unwrap();
    let via_api = RUST_WORKER.replace("crate::support", "super::api").replace("tasks::discard", "tasks::ignore");
    std::fs::write(src.join("group/sub/worker.rs"), via_api).unwrap();
    assert_helper_locations(root.path(), "src/group/sub/worker.rs", "rust-discarded-remove-file-result", expected);
}

#[test]
fn rust_self_alias_and_ambiguous_parents_are_handled_conservatively() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("src");
    std::fs::create_dir_all(src.join("worker")).unwrap();
    std::fs::write(src.join("lib.rs"), "mod worker;\n").unwrap();
    std::fs::write(src.join("worker/support.rs"), RUST_RESULT_HELPERS).unwrap();
    let worker = format!("mod support;\n{}", RUST_WORKER.replace("crate::support", "self::support"));
    std::fs::write(src.join("worker.rs"), &worker).unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", &[("src/worker.rs", 5), ("src/worker/support.rs", 4)]);
    std::fs::write(src.join("lib.rs"), "mod support; mod worker;\n").unwrap();
    std::fs::write(src.join("main.rs"), "mod support; mod worker;\n").unwrap();
    std::fs::write(src.join("support.rs"), RUST_RESULT_HELPERS).unwrap();
    std::fs::write(src.join("worker.rs"), RUST_WORKER.replace("crate::support", "super::support")).unwrap();
    let ambiguous = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/worker.rs".into()], &[]);
    assert!(ambiguous.coverage.iter().any(|issue| issue.reason == "unresolved_rust_module_import"), "{:?}", ambiguous.coverage);
    assert!(!ambiguous.findings.iter().any(|finding| finding.source.rule_id.as_deref() == Some("rust-discarded-remove-file-result")), "{:#?}", ambiguous.findings);
    std::fs::write(src.join("main.rs"), "#[path = \"worker.rs\"] mod renamed;\n").unwrap();
    let alternate = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/worker.rs".into()], &[]);
    assert!(alternate.coverage.iter().any(|issue| issue.reason == "unresolved_rust_module_import"), "{:?}", alternate.coverage);
    assert!(!alternate.findings.iter().any(|finding| finding.source.rule_id.as_deref() == Some("rust-discarded-remove-file-result")), "{:#?}", alternate.findings);
    let direct = RUST_WORKER.replace("use crate::support as tasks;", "use super::support::{discard as ignore, forward, remove};")
        .replace("tasks::discard", "ignore").replace("tasks::forward", "forward").replace("tasks::remove", "remove");
    std::fs::write(src.join("worker.rs"), direct).unwrap();
    let unavailable = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/worker.rs".into()], &[]);
    assert!(unavailable.coverage.iter().any(|issue| issue.reason == "unresolved_rust_module_import"), "{:?}", unavailable.coverage);
}

#[test]
fn rust_super_reexport_can_target_parent_module_function() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("src");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "use std::fs::remove_file;\npub(crate) fn remove(path: &str) -> std::io::Result<()> { remove_file(path) }\nmod api; mod worker;\n").unwrap();
    std::fs::write(src.join("api.rs"), "pub(crate) use super::remove as cleanup;\n").unwrap();
    std::fs::write(src.join("worker.rs"), "use super::api as tasks;\nfn run(path: &str) { tasks::cleanup(path); }\n").unwrap();
    assert_helper_locations(root.path(), "src/worker.rs", "rust-discarded-remove-file-result", &[("src/lib.rs", 2)]);
}

#[test]
fn rust_relative_modules_follow_mod_rs_layout() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("src");
    std::fs::create_dir_all(src.join("group")).unwrap();
    std::fs::write(src.join("lib.rs"), "mod group;\n").unwrap();
    std::fs::write(src.join("group/mod.rs"), "mod support; mod worker;\n").unwrap();
    std::fs::write(src.join("group/support.rs"), RUST_RESULT_HELPERS).unwrap();
    std::fs::write(src.join("group/worker.rs"), RUST_WORKER.replace("crate::support", "super::support")).unwrap();
    assert_helper_locations(root.path(), "src/group/worker.rs", "rust-discarded-remove-file-result", &[("src/group/support.rs", 4), ("src/group/worker.rs", 4)]);
}

#[test]
fn rust_unreferenced_modules_do_not_exhaust_helper_discovery() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("src");
    std::fs::create_dir(&src).unwrap();
    let mut source = String::new();
    for index in 0..80 {
        let name = format!("unused_{index:03}");
        source.push_str(&format!("mod {name};\n"));
        std::fs::write(src.join(format!("{name}.rs")), format!("pub fn spare_{index}() {{}}\n")).unwrap();
    }
    source.push_str("mod zhelper;\nfn run() { zhelper::remove(\"file\"); }\n");
    std::fs::write(src.join("lib.rs"), source).unwrap();
    std::fs::write(src.join("zhelper.rs"), "use std::fs::remove_file;\npub fn remove(path: &str) -> std::io::Result<()> { remove_file(path) }\n").unwrap();
    let result = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/lib.rs".into()], &[]);
    assert!(result.findings.iter().any(|finding| finding.source.rule_id.as_deref() == Some("rust-discarded-remove-file-result")
        && finding.location.path == "src/zhelper.rs" && finding.location.range.start_line == 2), "{:#?}", result.findings);
    assert!(!result.coverage.iter().any(|issue| issue.reason == "dependency_limit"), "{:?}", result.coverage);
    let declarations = (0..80).map(|index| format!("mod unused_{index:03};\n")).collect::<String>();
    let calls = (0..80).map(|index| format!("unused_{index:03}::spare_{index}();\n")).collect::<String>();
    std::fs::write(src.join("lib.rs"), format!("{declarations}fn run() {{\n{calls}}}\n")).unwrap();
    let exceeded = run_dataflow_check_with_coverage(root.path(), root.path(), &["src/lib.rs".into()], &[]);
    assert!(exceeded.coverage.iter().any(|issue| issue.reason == "dependency_limit"), "{:?}", exceeded.coverage);
}

#[test]
fn recursive_helpers_propagate_argument_use_and_returned_results() {
    let source = r#"package storage
import "os"
func discardAtEnd(err error, depth int) {
    if depth > 0 { discardAtEnd(err, depth - 1) }
}
func checkAtEnd(err error, depth int) {
    if depth > 0 { checkAtEnd(err, depth - 1); return }
    if err != nil { panic(err) }
}
func moveAtEnd(a, b string, depth int) error {
    if depth > 0 { return moveAtEnd(a, b, depth - 1) }
    return os.Rename(a, b)
}
func unused(a, b string) {
    err := os.Rename(a, b)
    discardAtEnd(err, 3)
}
func used(a, b string) {
    err := os.Rename(a, b)
    checkAtEnd(err, 3)
}
func discardedReturn(a, b string) {
    moveAtEnd(a, b, 3)
}
func handledReturn(a, b string) error {
    return moveAtEnd(a, b, 3)
}
"#;
    let root = write_single_file("storage.go", source);
    let found = run_dataflow_check(root.path(), &["storage.go".into()], &[]);
    let mut lines: Vec<_> = found.iter()
        .filter(|finding| finding.source.rule_id.as_deref() == Some("go-os-rename-discarded-error"))
        .map(|finding| finding.location.range.start_line).collect();
    lines.sort_unstable();
    assert_eq!(lines, [12, 15], "{found:#?}");
}

const ASSIGNED_RESULT_CASES: [(&str, &str, &str); 4] = [
    ("move.go", "go-os-rename-discarded-error", r#"package storage
import "os"
func overwritten(from, to string) error {
    var err, alias = os.Rename(from, to), error(nil) // unused result
    err, alias = nil, err
    alias = err
    return alias
}
func swapped(from, to string) error {
    var err, alias = os.Rename(from, to), error(nil)
    err, alias = nil, err
    return alias
}
func checked(from, to string) error {
    if err := os.Rename(from, to); err != nil { return err }
    return nil
}
func named(from, to string) (err error) {
    err = os.Rename(from, to)
    return
}
func deferred(from, to string) {
    err := os.Rename(from, to)
    defer inspect(err)
}
func blank(from, to string) {
    err := os.Rename(from, to) // unused result
    _ = err
}
"#),
    ("Main.kt", "kotlin-discarded-run-catching-result", r#"import kotlin.runCatching
fun parse(s: String, active: Boolean): Int {
    val result = runCatching { s.toInt() } // unused result on inactive path
    if (active) return result.getOrThrow()
    return 0
}
fun show(s: String) {
    val parsed = runCatching { s.toInt() }
    println("Read: $parsed")
}
fun overwrite(s: String): Int {
    var parsed = runCatching { s.toInt() } // unused result
    parsed = runCatching { 0 }
    return parsed.getOrThrow()
}
"#),
    ("lib.rs", "rust-discarded-remove-file-result", r#"use std::fs::remove_file;
fn overwrite(path: &str, spare: &str, refresh: bool) -> std::io::Result<()> {
    let mut removed = remove_file(path); // unused result
    if refresh {
        removed = remove_file(spare);
    } else {
        removed = Ok(());
    }
    removed
}
fn log(path: &str) {
    let removed = remove_file(path);
    println!("{removed:?}");
}
fn propagate(path: &str) -> std::io::Result<()> {
    let _removed = remove_file(path)?;
    Ok(())
}
fn shadow(path: &str) -> std::io::Result<()> {
    let removed = remove_file(path); // unused result
    let removed = Ok(());
    removed
}
fn forward(path: &str) -> std::io::Result<()> {
    let outcome = remove_file(path);
    let outcome = outcome;
    outcome
}
"#),
    ("Page.tsx", "typescript-node-fs-discarded-promise", r#"import { rename } from 'node:fs/promises';
export async function Page(from: string, to: string) {
    const result = rename(from, to); // unused result
    return <main><result /></main>;
    await result;
}
export async function Render(from: string, to: string) {
    const pending = rename(from, to);
    try { return <main />; } finally { await pending; }
}
export function Schedule(from: string, to: string) {
    let pending = rename(from, to);
    schedule(() => pending.catch(report));
}
const preview = rename => {
    const pending = rename('fake', 'never');
};
export const Tile = (from: string, to: string) => {
    const pending = rename(from, to); // unused result
    return <aside />;
};
"#),
];

#[test]
fn assigned_results_are_used_only_by_reachable_reads_of_the_original_value() {
    for (path, rule, source) in ASSIGNED_RESULT_CASES {
        let repo = write_single_file(path, source);
        let findings = run_dataflow_check(repo.path(), &[path.into()], &[]);
        let mut matches: Vec<_> = findings.iter().filter(|finding| finding.source.rule_id.as_deref() == Some(rule))
            .map(|finding| finding.location.range.start_line).collect();
        matches.sort_unstable();
        let expected: Vec<_> = source.lines().enumerate().filter(|(_, text)| text.contains("// unused result"))
            .map(|(index, _)| index as u32 + 1).collect();
        assert_eq!(matches, expected, "{path}: {findings:#?}");
    }
}

#[test]
fn applies_pack_call_sequences_to_go_and_web_languages() {
    use autoreview_core::ResolvedRulePack;
    use autoreview_schema::RulePackTrust;

    let dir = tempfile::tempdir().unwrap();
    let pack = tempfile::tempdir().unwrap();
    for language in ["Go", "JavaScript", "TypeScript"] {
        std::fs::write(
            pack.path().join(format!("{language}.yml")),
            format!(
                "id: example-{language}\nkind: call-sequence\nlanguage: {language}\ncategory: correctness\nseverity: warning\nmessage: Verify before sending\nafter: [{{call: begin}}]\nunless: [{{call: verify}}]\nbefore: [{{call: send}}]\ncheckBeforeReturn: true\n"
            ),
        ).unwrap();
    }
    let go = "package main\nfunc bad() {\nbegin()\nsend()\n}\nfunc explicit() {\nbegin()\nreturn\n}\nfunc deferred() {\nbegin()\ndefer verify()\nreturn\n}\nfunc earlySend() {\nbegin()\ndefer verify()\nsend()\nreturn\n}\n";
    let web = "function bad() {\nbegin();\nsend();\n}\nfunction clean() {\nbegin();\nverify();\nsend();\n}\n";
    let tsx = "function bad() {\nbegin();\nsend();\nreturn <main />;\n}\n";
    let cases = [
        ("worker.go", "Go", go, &[4, 8, 18][..]),
        ("worker.mjs", "JavaScript", web, &[3][..]),
        ("worker.cjs", "JavaScript", web, &[3][..]),
        ("worker.mts", "TypeScript", web, &[3][..]),
        ("worker.cts", "TypeScript", web, &[3][..]),
        ("view.tsx", "TypeScript", tsx, &[3, 4][..]),
    ];
    let files: Vec<_> = cases.iter().map(|(path, _, source, _)| {
        std::fs::write(dir.path().join(path), source).unwrap();
        (*path).to_owned()
    }).collect();
    let findings = run_dataflow_check(dir.path(), &files, &[ResolvedRulePack {
        id: "call-sequence-parity".into(), local_path: pack.path().to_owned(), trust: RulePackTrust::Full,
    }]);
    for (path, language, _, expected) in cases {
        let id = format!("example-{language}");
        let lines: Vec<_> = findings.iter().filter(|finding| finding.location.path == path && finding.source.rule_id.as_deref() == Some(&id))
            .map(|finding| finding.location.range.start_line).collect();
        assert_eq!(lines, expected, "{path}: {findings:#?}");
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
fn applies_rust_call_sequences_to_branch_and_early_return_paths() {
    use autoreview_core::ResolvedRulePack;
    use autoreview_schema::RulePackTrust;

    let repo = tempfile::tempdir().unwrap();
    let pack = tempfile::tempdir().unwrap();
    std::fs::write(pack.path().join("rust.yml"), r#"
id: rust-example-finish
kind: call-sequence
language: Rust
category: correctness
severity: warning
message: Finish before leaving
after: [{call: begin}]
unless: [{call: finish}]
checkBeforeReturn: true
"#).unwrap();
    std::fs::write(repo.path().join("lib.rs"), r#"impl Worker {
    async fn partial(&self, ready: bool) -> Result<(), Error> {
        self.begin();
        if ready {
            self.finish();
        } else {
            check().await?;
        }
        Ok(())
    }
    fn closed(&self) {
        self.begin();
        self.finish();
    }
    fn unreachable(&self) {
        return;
        self.begin();
    }
}"#).unwrap();
    let found = run_dataflow_check(repo.path(), &["lib.rs".into()], &[ResolvedRulePack {
        id: "call-sequence-rust".into(), local_path: pack.path().to_owned(), trust: RulePackTrust::Full,
    }]);
    let lines: Vec<_> = found.iter().filter(|finding| finding.source.rule_id.as_deref() == Some("rust-example-finish"))
        .map(|finding| finding.location.range.start_line).collect();
    assert_eq!(lines, [7, 10], "{found:#?}");
}

#[test]
fn a_call_order_rules_declared_cwe_reaches_the_finding() {
    let rule_id = "java-xxe-unconfigured-parser";
    let rule = discover_call_order_rules().into_iter().find(|r| r.id == rule_id).expect("java-xxe-unconfigured-parser is a builtin call-sequence rule");
    let filename = scan_filename_for_language(&rule.language);

    let positive = std::fs::read_to_string(fixtures_dir().join(rule_id).join(format!("positive.{}", extension_for_language(&rule.language)))).expect("positive fixture exists");
    let dir = write_single_file(&filename, &positive);
    let findings = run_dataflow_check(dir.path(), std::slice::from_ref(&filename), &[]);

    let finding = findings.iter().find(|f| f.source.rule_id.as_deref() == Some(rule_id)).expect("the positive fixture must fire this rule");
    let meta = finding.meta.as_ref().unwrap_or_else(|| panic!("{rule_id} declares a metadata block, so its finding must carry meta"));
    let cwe = meta.get("cwe").unwrap_or_else(|| panic!("{rule_id} declares cwe metadata; got meta keys {:?}", meta.keys().collect::<Vec<_>>()));
    assert_eq!(cwe, &serde_json::json!(["CWE-611"]), "the rule's declared CWE must reach the finding unchanged");
}

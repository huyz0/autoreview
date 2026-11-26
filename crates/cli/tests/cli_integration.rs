//! Integration tests against the actual compiled `autoreview` binary (not
//! library calls) — these are what would catch a bug in argument parsing,
//! process spawning, or the CLI's own glue code that unit tests on the
//! underlying functions can't see.

use std::path::PathBuf;
use std::process::Command as StdCommand;

use assert_cmd::Command;
use autoreview_test_support::{init_repo, init_repo_with_diff};
use predicates::prelude::*;

fn ast_grep_available() -> bool {
    StdCommand::new("ast-grep")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn current_commit(repo: &std::path::Path) -> String {
    let output = StdCommand::new("git").args(["rev-parse", "HEAD"]).current_dir(repo).output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn failed_replay_report(repo: &std::path::Path) -> serde_json::Value {
    let result = Command::cargo_bin("autoreview").unwrap().current_dir(repo)
        .args(["rules", "replay", "--suite", "suite.json"]).assert().failure();
    serde_json::from_slice(&result.get_output().stdout).unwrap()
}

#[test]
fn replay_reports_analysis_failures_without_losing_successful_cases() {
    let bad = "package main\nimport \"os\"\nfunc move(a, b string) { os.Rename(a, b) }\n";
    let fixed = "package main\nimport \"os\"\nfunc move(a, b string) error { return os.Rename(a, b) }\n";
    let missing_helper = "import { run } from './missing';\nexport function main() { run(); }\n";
    let repo = init_repo(&[("move.go", bad), ("main.js", missing_helper)]);
    let before = current_commit(repo.path());
    repo.commit(&[("move.go", fixed)], "check rename error");
    let after = current_commit(repo.path());
    let rule = serde_json::json!({"tool": "autoreview-dataflow", "id": "go-os-rename-discarded-error"});
    let suite = serde_json::json!({"cases": [
        {"name": "valid", "rule": rule, "evidence": {"kind": "synthetic", "source": "test fixture", "license": "MIT", "rationale": "The return value is discarded."},
         "before": {"commit": before, "path": "move.go", "lines": [3]},
         "after": {"commit": after, "path": "move.go", "lines": []}},
        {"name": "missing revision", "rule": rule, "before": {"commit": "missing-revision", "path": "move.go", "lines": [3]},
         "after": {"commit": after, "path": "move.go", "lines": []}},
        {"name": "incomplete helper", "rule": {"tool": "autoreview-dataflow", "id": "javascript-node-fs-discarded-promise"},
         "before": {"commit": before, "path": "main.js", "lines": []},
         "after": {"commit": after, "path": "main.js", "lines": []}}
    ]});
    std::fs::write(repo.path().join("suite.json"), suite.to_string()).unwrap();
    let report = failed_replay_report(repo.path());
    assert_eq!(report["rules"][0]["truePositives"], 1);
    assert_eq!(report["languages"][0]["language"], "Go");
    assert_eq!(report["cases"][0]["evidence"]["kind"], "synthetic");
    assert_eq!(report["evidenceKinds"][0]["kind"], "synthetic");
    assert_eq!(report["failures"][0]["name"], "missing revision");
    assert_eq!(report["failures"][0]["language"], "Go");
    assert_eq!(report["failures"][0]["kind"], "error");
    assert_eq!(report["coverage"][0]["language"], "Go");
    assert_eq!(report["coverage"][0]["rule"]["id"], "go-os-rename-discarded-error");
    assert_eq!(report["coverage"][0]["attempted"], 2);
    assert_eq!(report["coverage"][0]["completed"], 1);
    assert_eq!(report["coverage"][0]["incomplete"], 0);
    assert_eq!(report["coverage"][0]["failed"], 1);
    assert_eq!(report["failures"][1]["kind"], "incomplete-analysis");
    assert_eq!(report["coverage"][1]["language"], "JavaScript");
    assert_eq!(report["coverage"][1]["attempted"], 1);
    assert_eq!(report["coverage"][1]["completed"], 0);
    assert_eq!(report["coverage"][1]["incomplete"], 1);
    assert_eq!(report["coverage"][1]["failed"], 0);
    assert_eq!(report["cases"].as_array().unwrap().len(), 1);
}

#[test]
fn replay_excludes_ambiguous_helpers_even_without_a_known_result_source() {
    let entry = "import { move } from './index';\nexport function run(a, b) { move(a, b); }\n";
    let repo = init_repo(&[
        ("main.js", entry),
        ("a.js", "export function move(a, b) { return a; }\n"),
        ("b.js", "export function move(a, b) { return b; }\n"),
        ("index.js", "export * from './a';\n"),
    ]);
    let clean = current_commit(repo.path());
    repo.commit(&[("main.js", &format!("{entry}// unchanged behavior\n"))], "second clean revision");
    let clean_after = current_commit(repo.path());
    repo.commit(&[("index.js", "export * from './a'; export * from './b';\n")], "introduce ambiguous export");
    let ambiguous = current_commit(repo.path());
    let rule = serde_json::json!({"tool":"autoreview-dataflow", "id":"javascript-node-fs-discarded-promise"});
    let suite = serde_json::json!({"cases": [
        {"name":"resolved", "rule":rule, "before":{"commit":clean,"path":"main.js","lines":[]}, "after":{"commit":clean_after,"path":"main.js","lines":[]}},
        {"name":"ambiguous", "rule":rule, "before":{"commit":ambiguous,"path":"main.js","lines":[]}, "after":{"commit":clean_after,"path":"main.js","lines":[]}}
    ]});
    std::fs::write(repo.path().join("suite.json"), suite.to_string()).unwrap();
    let report = failed_replay_report(repo.path());
    assert_eq!(report["cases"].as_array().unwrap().len(), 1, "{report}");
    assert_eq!(report["cases"][0]["name"], "resolved");
    assert_eq!(report["failures"][0]["name"], "ambiguous");
    assert_eq!(report["failures"][0]["kind"], "incomplete-analysis");
    assert_eq!(report["coverage"][0]["attempted"], 2);
    assert_eq!(report["coverage"][0]["completed"], 1);
    assert_eq!(report["coverage"][0]["incomplete"], 1);
}

#[test]
fn replay_evaluates_a_registered_custom_call_sequence_rule() {
    let bad = "package main\nimport \"os\"\nfunc move(a, b string) { os.Rename(a, b) }\n";
    let fixed = "package main\nimport \"os\"\nfunc move(a, b string) error { return os.Rename(a, b) }\n";
    let repo = init_repo(&[("move.go", bad), ("main.ts", "declare function task(): Promise<void>;\nexport function run() { task(); }\n")]);
    let before = current_commit(repo.path());
    repo.commit(&[("move.go", fixed)], "check rename error");
    let after = current_commit(repo.path());
    let pack = tempfile::tempdir().unwrap();
    std::fs::write(pack.path().join("rulepack.yaml"), "id: custom\nversion: '1'\n").unwrap();
    std::fs::write(pack.path().join("rule.yml"), "id: custom-rename\nlanguage: Go\ncategory: correctness\nseverity: warning\nkind: call-sequence\nmessage: Check the error.\ncontract:\n  mode: must-use\n  import: os\n  methods: [Rename]\n").unwrap();
    std::fs::create_dir_all(repo.path().join(".autoreview")).unwrap();
    let path = serde_json::to_string(&pack.path().to_string_lossy()).unwrap();
    std::fs::write(repo.path().join(".autoreview/rulepacks.yaml"), format!("packs:\n  - id: custom\n    trust: shadow\n    source:\n      kind: local\n      path: {path}\n")).unwrap();
    let suite = serde_json::json!({"cases": [{"name":"custom", "rule":{"tool":"autoreview-dataflow","id":"custom-rename"}, "before":{"commit":before,"path":"move.go","lines":[3]}, "after":{"commit":after,"path":"move.go","lines":[]}}]});
    std::fs::write(repo.path().join("suite.json"), suite.to_string()).unwrap();
    let result = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path()).args(["rules", "replay", "--suite", "suite.json"]).assert().success();
    let report: serde_json::Value = serde_json::from_slice(&result.get_output().stdout).unwrap();
    assert_eq!(report["rules"][0]["truePositives"], 1);
    assert_eq!(report["coverage"][0]["completed"], 1);
    assert_eq!(report["customRuleSnapshots"][0]["packId"], "custom");
    assert_eq!(report["customRuleSnapshots"][0]["sha256"].as_str().unwrap().len(), 64);

    let mut wrong_language = suite.clone();
    wrong_language["cases"][0]["before"]["path"] = "main.ts".into();
    wrong_language["cases"][0]["before"]["lines"] = serde_json::json!([]);
    wrong_language["cases"][0]["after"]["path"] = "main.ts".into();
    std::fs::write(repo.path().join("suite.json"), wrong_language.to_string()).unwrap();
    let result = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path()).args(["rules", "replay", "--suite", "suite.json"]).assert().failure();
    let report: serde_json::Value = serde_json::from_slice(&result.get_output().stdout).unwrap();
    assert!(report["cases"].as_array().unwrap().is_empty());
    assert!(report["failures"][0]["error"].as_str().unwrap().contains("does not apply"));

    std::fs::write(pack.path().join("promise.yml"), "id: custom-promise\nlanguage: TypeScript\ncategory: correctness\nseverity: warning\nkind: call-sequence\nmessage: Observe the promise.\ncontract:\n  mode: must-use\n  source: typed-promise\n").unwrap();
    let mut incomplete = suite.clone();
    let mut promise = incomplete["cases"][0].clone();
    promise["name"] = "promise".into();
    promise["rule"]["id"] = "custom-promise".into();
    promise["before"]["path"] = "main.ts".into();
    promise["before"]["lines"] = serde_json::json!([]);
    promise["after"]["path"] = "main.ts".into();
    incomplete["cases"].as_array_mut().unwrap().push(promise);
    std::fs::write(repo.path().join("suite.json"), incomplete.to_string()).unwrap();
    let result = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path()).env_remove("AUTOREVIEW_TYPESCRIPT_MODULE").args(["rules", "replay", "--suite", "suite.json"]).assert().failure();
    let report: serde_json::Value = serde_json::from_slice(&result.get_output().stdout).unwrap();
    assert_eq!(report["cases"][0]["name"], "custom");
    assert_eq!(report["failures"][0]["kind"], "incomplete-analysis");

    let duplicate = tempfile::tempdir().unwrap();
    std::fs::write(duplicate.path().join("rulepack.yaml"), "id: duplicate\nversion: '1'\n").unwrap();
    std::fs::copy(pack.path().join("rule.yml"), duplicate.path().join("rule.yml")).unwrap();
    let second = serde_json::to_string(&duplicate.path().to_string_lossy()).unwrap();
    std::fs::write(repo.path().join(".autoreview/rulepacks.yaml"), format!("packs:\n  - id: custom\n    source: {{ kind: local, path: {path} }}\n  - id: duplicate\n    source: {{ kind: local, path: {second} }}\n")).unwrap();
    let mut mixed = suite.clone();
    let mut builtin = mixed["cases"][0].clone();
    builtin["name"] = "builtin".into();
    builtin["rule"]["id"] = "go-os-rename-discarded-error".into();
    mixed["cases"].as_array_mut().unwrap().push(builtin);
    std::fs::write(repo.path().join("suite.json"), mixed.to_string()).unwrap();
    let result = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path()).args(["rules", "replay", "--suite", "suite.json"]).assert().failure();
    let report: serde_json::Value = serde_json::from_slice(&result.get_output().stdout).unwrap();
    assert_eq!(report["cases"][0]["name"], "builtin");
    assert!(report["failures"][0]["error"].as_str().unwrap().contains("ambiguous"));
}

#[test]
fn nested_typescript_config_follows_replay_snapshots() {
    let Ok(compiler) = std::env::var("AUTOREVIEW_TEST_TYPESCRIPT") else { return };
    let before = "declare function pending(): Promise<void>;\nexport function run() { pending(); }\n";
    let after = "declare function pending(): Promise<void>;\nexport async function run() { await pending(); }\n";
    let repo = init_repo(&[("web/main.ts", before), ("web/tsconfig.json", r#"{"compilerOptions":{"target":"ES2022","strict":true},"files":["main.ts"]}"#)]);
    let old = current_commit(repo.path());
    repo.commit(&[("web/main.ts", after)], "observe promise");
    let fixed = current_commit(repo.path());
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let pack = root.join("benchmarks/packs/checkout-promise");
    std::fs::create_dir_all(repo.path().join(".autoreview")).unwrap();
    let path = serde_json::to_string(&pack.to_string_lossy()).unwrap();
    std::fs::write(repo.path().join(".autoreview/rulepacks.yaml"), format!("packs:\n  - id: checkout-promise-benchmark\n    source: {{ kind: local, path: {path} }}\n")).unwrap();
    let suite = serde_json::json!({"cases":[{"name":"nested config","rule":{"tool":"autoreview-dataflow","id":"benchmark-typescript-unobserved-promise"},"before":{"commit":old,"path":"web/main.ts","lines":[2]},"after":{"commit":fixed,"path":"web/main.ts","lines":[]}}]});
    std::fs::write(repo.path().join("suite.json"), suite.to_string()).unwrap();
    let result = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .env("AUTOREVIEW_TYPESCRIPT_MODULE", &compiler).env("AUTOREVIEW_TYPESCRIPT_CONFIG", "web/tsconfig.json")
        .args(["rules", "replay", "--suite", "suite.json"]).assert().success();
    let report: serde_json::Value = serde_json::from_slice(&result.get_output().stdout).unwrap();
    assert_eq!(report["rules"][0]["truePositives"], 1, "{report}");
    assert_eq!(report["coverage"][0]["completed"], 1, "{report}");
    assert_eq!(report["coverage"][0]["incomplete"], 0, "{report}");
    let unsupported = repo.path().join("unsupported.cjs");
    std::fs::write(&unsupported, "module.exports = {};\n").unwrap();
    let result = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .env("AUTOREVIEW_TYPESCRIPT_MODULE", &unsupported).env("AUTOREVIEW_TYPESCRIPT_CONFIG", "web/tsconfig.json")
        .args(["rules", "replay", "--suite", "suite.json"]).assert().failure();
    let report: serde_json::Value = serde_json::from_slice(&result.get_output().stdout).unwrap();
    assert_eq!(report["coverage"][0]["incomplete"], 1, "{report}");
    assert!(report["failures"][0]["error"].as_str().unwrap().contains("compiler does not provide the API"), "{report}");
    repo.commit(&[("web/tsconfig.json", r#"{"compilerOptions":{"target":"ES2022"},"files":[]}"#)], "remove source from config");
    let changed_config = current_commit(repo.path());
    let mut changed_suite = suite;
    changed_suite["cases"][0]["after"]["commit"] = changed_config.into();
    std::fs::write(repo.path().join("suite.json"), changed_suite.to_string()).unwrap();
    let result = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .env("AUTOREVIEW_TYPESCRIPT_MODULE", &compiler).env("AUTOREVIEW_TYPESCRIPT_CONFIG", "web/tsconfig.json")
        .args(["rules", "replay", "--suite", "suite.json"]).assert().failure();
    let report: serde_json::Value = serde_json::from_slice(&result.get_output().stdout).unwrap();
    assert_eq!(report["coverage"][0]["completed"], 0, "{report}");
    assert_eq!(report["coverage"][0]["incomplete"], 1, "{report}");
    repo.commit(&[("web/main.ts", before), ("web/tsconfig.json", r#"{"compilerOptions":{"target":"ES2022","strict":true},"files":["main.ts"]}"#)], "discard promise again");
    let run = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .env("AUTOREVIEW_TYPESCRIPT_MODULE", &compiler).env("AUTOREVIEW_TYPESCRIPT_CONFIG", "web/tsconfig.json")
        .args(["diff", "--base", &fixed, "--head", "HEAD", "--static-only"]).assert().success();
    let path = report_path_from_stdout(&String::from_utf8_lossy(&run.get_output().stdout));
    let review: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(review["findings"].as_array().unwrap().iter().any(|finding|
        finding["source"]["ruleId"] == "benchmark-typescript-unobserved-promise" && finding["location"]["path"] == "web/main.ts"), "{review}");
}

#[test]
fn replay_keeps_typed_benchmark_incomplete_with_compatibility_adapter() {
    let Ok(compiler) = std::env::var("AUTOREVIEW_TEST_TYPESCRIPT") else { return };
    let source = "declare function task(): Promise<void>;\ndeclare function mystery(): unknown;\nexport function run() { try { throw new Error(); } catch (error) { error.message; } task(); mystery(); }\n";
    let config = r#"{"compilerOptions":{"target":"ES2022","strict":true},"files":["main.ts"]}"#;
    let repo = init_repo(&[("main.ts", source), ("tsconfig.json", config)]);
    let before = current_commit(repo.path());
    repo.commit(&[("main.ts", &format!("{source}// fixed snapshot\n"))], "second revision");
    let after = current_commit(repo.path());
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let pack = root.join("benchmarks/packs/checkout-promise");
    let adapter = root.join("benchmarks/typescript-legacy-catch.cjs");
    std::fs::create_dir_all(repo.path().join(".autoreview")).unwrap();
    let path = serde_json::to_string(&pack.to_string_lossy()).unwrap();
    std::fs::write(repo.path().join(".autoreview/rulepacks.yaml"), format!("packs:\n  - id: checkout-promise-benchmark\n    source: {{ kind: local, path: {path} }}\n")).unwrap();
    let suite = serde_json::json!({"cases":[{"name":"typed","rule":{"tool":"autoreview-dataflow","id":"benchmark-typescript-unobserved-promise"},"before":{"commit":before,"path":"main.ts","lines":[]},"after":{"commit":after,"path":"main.ts","lines":[]}}]});
    std::fs::write(repo.path().join("suite.json"), suite.to_string()).unwrap();
    let result = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .env("AUTOREVIEW_TYPESCRIPT_MODULE", adapter).env("AUTOREVIEW_BENCHMARK_TYPESCRIPT_MODULE", compiler)
        .args(["rules", "replay", "--suite", "suite.json"]).assert().failure();
    let report: serde_json::Value = serde_json::from_slice(&result.get_output().stdout).unwrap();
    assert_eq!(report["coverage"][0]["incomplete"], 1);
    assert_eq!(report["failures"][0]["kind"], "incomplete-analysis");
    assert!(report["failures"][0]["error"].as_str().unwrap().contains("promise type could not be verified"));
    assert!(report["rules"].as_array().unwrap().is_empty());
}

#[test]
fn commit_replay_measures_rule_precision_and_gates_only_qualified_changed_lines() {
    if !ast_grep_available() {
        return;
    }
    let policy_path = ".autoreview/replay.yaml";
    let repo = init_repo(&[("old.go", "package fixture\nfunc Old(x int) bool { return x == x }\n")]);
    let rule = serde_json::json!({"tool": "ast-grep", "id": "go-no-self-comparison"});
    let mut cases = Vec::new();
    let mut insufficient_base = String::new();
    for index in 1..=3 {
        let path = format!("case{index}.go");
        let before = format!("package fixture\nfunc Compare{index}(x, y int) bool {{ return x == x }}\n");
        let after = before.replace("x == x", "x == y");
        repo.commit(&[(&path, &before)], "add known-bad comparison");
        let bad = current_commit(repo.path());
        repo.commit(&[(&path, &after)], "fix comparison");
        let fixed = current_commit(repo.path());
        cases.push(serde_json::json!({
            "name": format!("comparison-{index}"), "rule": rule,
            "before": {"commit": bad, "path": path, "lines": [2]},
            "after": {"commit": fixed, "path": path, "lines": []}
        }));
        if index == 1 {
            let policy = serde_json::json!({"gate": [rule], "cases": cases}).to_string();
            repo.commit(&[(policy_path, &policy)], "policy with one example");
            insufficient_base = current_commit(repo.path());
        }
    }
    let policy = serde_json::json!({"gate": [rule], "cases": cases}).to_string();
    repo.commit(&[(policy_path, &policy)], "policy with three independent cases");
    let qualified_base = current_commit(repo.path());

    let result = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .args(["rules", "replay"]).assert().success();
    let report: serde_json::Value = serde_json::from_slice(&result.get_output().stdout).unwrap();
    assert_eq!(report["rules"][0]["truePositives"], 3);
    assert_eq!(report["rules"][0]["precision"], 1.0);
    assert_eq!(report["rules"][0]["recall"], 1.0);

    Command::cargo_bin("autoreview").unwrap().current_dir(repo.path()).args(["rules", "gate", "--base", &insufficient_base])
        .assert().failure().stderr(predicate::str::contains("needs 3 distinct"));

    repo.commit(&[("old.go", "package fixture\nfunc Old(x int) bool { return x == x }\n\n// Unrelated documentation.\n")], "touch a file that already has a finding");
    Command::cargo_bin("autoreview").unwrap().current_dir(repo.path()).args(["rules", "gate", "--base", &qualified_base])
        .assert().success().stdout(predicate::str::contains("\"passed\": true"));

    repo.commit(&[
        (policy_path, "gate: []\ncases: []\n"),
        ("new.go", "package fixture\nfunc New(x int) bool { return x == x }\n"),
    ], "introduce a newly added defect and change the head policy");
    let result = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path()).args(["rules", "gate", "--base", &qualified_base])
        .assert().failure().stderr(predicate::str::contains("merge gate found 1 issue"));
    let report: serde_json::Value = serde_json::from_slice(&result.get_output().stdout).unwrap();
    assert_eq!(report["findings"][0]["location"]["path"], "new.go");
    assert_eq!(report["passed"], false);

    cases[0]["before"]["lines"] = serde_json::json!([]);
    cases[0]["after"]["lines"] = serde_json::json!([2]);
    std::fs::write(repo.path().join(policy_path), serde_json::json!({"cases": cases}).to_string()).unwrap();
    Command::cargo_bin("autoreview").unwrap().current_dir(repo.path()).args(["rules", "replay"])
        .assert().failure().stdout(predicate::str::contains("\"falseNegatives\": 1").and(predicate::str::contains("\"falsePositives\": 1")));

    let advisory = "gate:\n  - tool: autoreview-complexity\n    id: long-method\ncases: []\n";
    repo.commit(&[(policy_path, advisory)], "try to make a design threshold block merges");
    Command::cargo_bin("autoreview").unwrap().current_dir(repo.path()).args(["rules", "gate", "--base", "HEAD"])
        .assert().failure().stderr(predicate::str::contains("must be a non-heuristic built-in correctness or security rule"));
}

/// Extracts the report path from the CLI's own "report written: <path>"
/// line, then reads and parses it — exercising the real on-disk artifact
/// the binary produced, not a value handed back in-process.
fn report_path_from_stdout(stdout: &str) -> PathBuf {
    let line = stdout
        .lines()
        .find(|l| l.contains("report written:"))
        .expect("expected a 'report written:' line in stdout");
    let path_str = line
        .split("report written:")
        .nth(1)
        .expect("malformed report-written line")
        .trim();
    PathBuf::from(path_str)
}

#[test]
fn diff_reports_dataflow_syntax_limitations_without_warning_on_clean_files() {
    let repo = init_repo(&[("main.go", "package main\nfunc main() {}\n")]);
    let base = current_commit(repo.path());
    repo.commit(&[("main.go", "package main\nfunc main( {\n")], "break syntax");
    let bad = current_commit(repo.path());
    let run = |head: &str| {
        let result = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
            .args(["diff", "--base", &base, "--head", head, "--static-only"])
            .assert().success();
        let report = report_path_from_stdout(&String::from_utf8_lossy(&result.get_output().stdout));
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(report).unwrap()).unwrap()
    };
    let report = run(&bad);
    assert!(report["analysisCoverage"].as_array().unwrap().iter().any(|issue|
        issue["analyzer"] == "dataflow" && issue["reason"] == "syntax_errors" && issue["path"] == "main.go"));
    assert!(report["analyzerWarnings"].as_array().unwrap().iter().any(|warning|
        warning.as_str().is_some_and(|text| text.contains("main.go") && text.contains("incomplete"))));
    repo.commit(&[("main.go", "package main\nfunc main() { println(1) }\n")], "fix syntax");
    let clean = run(&current_commit(repo.path()));
    assert!(!clean.get("analysisCoverage").and_then(|items| items.as_array()).is_some_and(|items|
        items.iter().any(|issue| issue["analyzer"] == "dataflow" && issue["reason"] == "syntax_errors")), "{clean}");
}

#[test]
fn review_reports_missing_external_tools_as_structured_coverage() {
    let git_name = if cfg!(windows) { "git.exe" } else { "git" };
    let git_dir = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .find(|dir| dir.join(git_name).is_file()).expect("git is required by integration tests");
    let mut dirs = vec![git_dir];
    if cfg!(windows) {
        dirs.push(PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32"));
    }
    let restricted = std::env::join_paths(dirs).unwrap();
    for tool in ["ast-grep", "golangci-lint"] {
        if StdCommand::new(tool).env("PATH", &restricted).arg("--version").output().is_ok() {
            // Windows may resolve executables using the parent process PATH.
            return;
        }
    }
    let repo = init_repo(&[("main.go", "package main\nfunc run() {}\n")]);
    let base = current_commit(repo.path());
    repo.commit(&[("main.go", "package main\nfunc run() { println(1) }\n")], "change Go code");
    let run = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path()).env("PATH", &restricted)
        .args(["diff", "--base", &base, "--head", "HEAD", "--static-only"])
        .assert().success();
    let report = report_path_from_stdout(&String::from_utf8_lossy(&run.get_output().stdout));
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
    let coverage = report["analysisCoverage"].as_array().expect("structured coverage");
    for tool in ["ast-grep", "golangci-lint"] {
        assert!(coverage.iter().any(|issue| issue["analyzer"] == tool && issue["reason"] == "tool_unavailable" && issue["scope"] == "analyzer"), "{report}");
        assert!(report["analyzerWarnings"].as_array().unwrap().iter().any(|warning| warning.as_str().unwrap().contains(tool)), "{report}");
    }
    let base = current_commit(repo.path());
    repo.commit(&[("Cargo.lock", "version = 3\n")], "add lockfile");
    let run = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path()).env("PATH", restricted)
        .args(["diff", "--base", &base, "--head", "HEAD", "--static-only", "--audit-rust-deps"])
        .assert().success();
    let path = report_path_from_stdout(&String::from_utf8_lossy(&run.get_output().stdout));
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let coverage = report["analysisCoverage"].as_array().expect("structured coverage");
    assert!(coverage.iter().any(|issue| issue["analyzer"] == "cargo-audit" && issue["reason"] == "tool_failed"
        && issue["scope"] == "file" && issue["path"] == "Cargo.lock"), "{report}");
    assert!(!coverage.iter().any(|issue| issue["analyzer"] == "ast-grep" || issue["analyzer"] == "golangci-lint"), "{report}");
}

#[test]
fn review_keeps_clippy_findings_when_another_target_fails() {
    if !StdCommand::new("cargo").args(["clippy", "--version"]).output().is_ok_and(|output| output.status.success()) { return; }
    let manifest = "[package]\nname = \"coverage_clippy_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
    let before = "pub fn f() -> Result<(), ()> { Ok(()) }\n#[cfg(test)] mod tests {}\n";
    let after = "pub fn f() -> Result<(), ()> { Ok(()) }\npub fn run() { f(); }\n#[cfg(test)] mod tests {}\n";
    let repo = init_repo(&[("Cargo.toml", manifest), ("src/lib.rs", before), ("tests/broken.rs", "compile_error!(\"failing test target\");\n")]);
    let base = current_commit(repo.path());
    repo.commit(&[("src/lib.rs", after)], "discard a result");
    let report = static_review_report(&repo, &base, Some(&repo.path().join("target")));
    assert!(report["findings"].as_array().unwrap().iter().any(|finding|
        finding["source"]["tool"] == "clippy" && finding["source"]["ruleId"] == "unused_must_use"), "{report}");
    assert!(report["analysisCoverage"].as_array().unwrap().iter().any(|issue|
        issue["analyzer"] == "clippy" && issue["reason"] == "tool_failed"
            && issue["detail"].as_str().unwrap().contains("additional targets failed")), "{report}");
}

fn static_review_report(repo: &autoreview_test_support::TestRepo, base: &str, target_dir: Option<&std::path::Path>) -> serde_json::Value {
    let mut command = Command::cargo_bin("autoreview").unwrap();
    command.current_dir(repo.path()).args(["diff", "--base", base, "--head", "HEAD", "--static-only"]);
    if let Some(target) = target_dir { command.env("CARGO_TARGET_DIR", target); }
    let run = command.assert().success();
    let path = report_path_from_stdout(&String::from_utf8_lossy(&run.get_output().stdout));
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn unresolved_assigned_result_crosses_helpers_in_review_and_replay() {
    for helper in ["export function send(value) { unknown(value); }\n", "export function send(value) { unknown([value]); }\n"] {
        let before = "import { rename } from 'node:fs/promises';\nimport { send } from './helper';\nexport function run(a, b) { const pending = rename(a, b); send(pending); }\n";
        let after = "import { rename } from 'node:fs/promises';\nexport async function run(a, b) { await rename(a, b); }\n";
        let repo = init_repo(&[("main.js", before), ("helper.js", helper)]);
        let old = current_commit(repo.path());
        repo.commit(&[("main.js", after)], "observe rename promise");
        let fixed = current_commit(repo.path());
        let suite = serde_json::json!({"cases":[{"name":"unknown helper receiver","rule":{"tool":"autoreview-dataflow","id":"javascript-node-fs-discarded-promise"},"before":{"commit":old,"path":"main.js","lines":[]},"after":{"commit":fixed,"path":"main.js","lines":[]}}]});
        std::fs::write(repo.path().join("suite.json"), suite.to_string()).unwrap();
        let report = failed_replay_report(repo.path());
        assert_eq!(report["coverage"][0]["incomplete"], 1, "{report}");
        assert!(report["failures"][0]["error"].as_str().unwrap().contains("unresolved helper"), "{report}");
        repo.commit(&[("main.js", before)], "pass promise through helper");
        let review = static_review_report(&repo, &fixed, None);
        assert!(review["analysisCoverage"].as_array().unwrap().iter().any(|issue|
            issue["reason"] == "unresolved_result_receiver" && issue["rule"] == "javascript-node-fs-discarded-promise"
                && issue["path"] == "main.js" && issue["line"] == 3), "{review}");
    }
}

#[test]
fn array_wrapped_result_uncertainty_reaches_review_and_replay() {
    let before = "import { rename } from 'node:fs/promises';\nexport function run(a, b) { unknown([rename(a, b)]); }\n";
    let after = "import { rename } from 'node:fs/promises';\nexport async function run(a, b) { unknown([await rename(a, b)]); }\n";
    let repo = init_repo(&[("main.js", before)]);
    let old = current_commit(repo.path());
    repo.commit(&[("main.js", after)], "Observe the result before wrapping it");
    let fixed = current_commit(repo.path());
    let suite = serde_json::json!({"cases":[{"name":"wrapped unknown receiver", "rule":{"tool":"autoreview-dataflow","id":"javascript-node-fs-discarded-promise"},
        "before":{"commit":old,"path":"main.js","lines":[]},"after":{"commit":fixed,"path":"main.js","lines":[]}}]});
    std::fs::write(repo.path().join("suite.json"), suite.to_string()).unwrap();
    let replay = failed_replay_report(repo.path());
    assert_eq!(replay["coverage"][0]["incomplete"], 1, "{replay}");
    assert!(replay["failures"][0]["error"].as_str().unwrap().contains("array or spread argument"), "{replay}");
    repo.commit(&[("main.js", before)], "Pass wrapped result to unknown receiver");
    let review = static_review_report(&repo, &fixed, None);
    assert!(review["analysisCoverage"].as_array().unwrap().iter().any(|issue|
        issue["reason"] == "wrapped_result_receiver" && issue["rule"] == "javascript-node-fs-discarded-promise"
            && issue["path"] == "main.js" && issue["line"] == 2), "{review}");
}

#[test]
fn imported_helper_arguments_reach_review_and_replay_across_languages() {
    let fixtures = [
        ("go-os-rename-discarded-error", "worker.go", "package main\nimport (\"os\"; \"example.com/project/helper\")\nfunc run(a, b string) { helper.Ignore(os.Rename(a, b)) }\n", "package main\nimport (\"os\"; \"example.com/project/helper\")\nfunc run(a, b string) error { return helper.Forward(os.Rename(a, b)) }\n", 3),
        ("java-discarded-objects-comparison", "app/Worker.java", "package app;\nimport java.util.Objects;\nimport support.Checks;\nclass Worker { void run(Object a, Object b) { Checks.discard(Objects.equals(a, b)); } }\n", "package app;\nimport java.util.Objects;\nimport support.Checks;\nclass Worker { boolean run(Object a, Object b) { return Checks.forward(Objects.equals(a, b)); } }\n", 4),
        ("kotlin-discarded-run-catching-result", "app/Worker.kt", "package app\nimport kotlin.runCatching\nimport support.discard\nfun run(value: String) { discard(runCatching { value.toInt() }) }\n", "package app\nimport kotlin.runCatching\nimport support.forward\nfun run(value: String): Result<Int> { return forward(runCatching { value.toInt() }) }\n", 4),
        ("rust-discarded-remove-file-result", "src/worker.rs", "use crate::support::discard;\nuse std::fs::remove_file;\nfn run(path: &str) { discard(remove_file(path)); }\n", "use crate::support::forward;\nuse std::fs::remove_file;\nfn run(path: &str) -> std::io::Result<()> { forward(remove_file(path)) }\n", 3),
        ("javascript-node-fs-discarded-promise", "web/js/worker.js", "import { rename } from 'node:fs/promises';\nimport { discard } from './helper';\nexport function run(a, b) { discard(rename(a, b)); }\n", "import { rename } from 'node:fs/promises';\nimport { forward } from './helper';\nexport function run(a, b) { return forward(rename(a, b)); }\n", 3),
        ("typescript-node-fs-discarded-promise", "web/ts/worker.ts", "import { rename } from 'node:fs/promises';\nimport { discard } from './helper';\nexport function run(a, b) { discard(rename(a, b)); }\n", "import { rename } from 'node:fs/promises';\nimport { forward } from './helper';\nexport function run(a, b) { return forward(rename(a, b)); }\n", 3),
        ("typescript-node-fs-discarded-promise", "web/tsx/Worker.tsx", "import { rename } from 'node:fs/promises';\nimport { discard } from './helper';\nexport function run(a, b) { discard(rename(a, b)); }\n", "import { rename } from 'node:fs/promises';\nimport { forward } from './helper';\nexport function run(a, b) { return forward(rename(a, b)); }\n", 3),
    ];
    let extras = [
        ("go.mod", "module example.com/project\n\ngo 1.22\n"),
        ("helper/helper.go", "package helper\nfunc Ignore(err error) {}\nfunc Forward(err error) error { return err }\n"),
        ("support/Checks.java", "package support;\npublic class Checks { public static void discard(boolean value) {} public static boolean forward(boolean value) { return value; } }\n"),
        ("support/Checks.kt", "package support\nfun discard(value: Result<Int>) {}\nfun forward(value: Result<Int>): Result<Int> = value\n"),
        ("src/lib.rs", "mod support; mod worker;\n"),
        ("src/support.rs", "pub(crate) fn discard(value: std::io::Result<()>) {}\npub(crate) fn forward(value: std::io::Result<()>) -> std::io::Result<()> { value }\n"),
        ("web/js/helper.js", "export function discard(value) {}\nexport function forward(value) { return value; }\n"),
        ("web/ts/helper.ts", "export function discard(value) {}\nexport function forward(value) { return value; }\n"),
        ("web/tsx/helper.tsx", "export function discard(value) {}\nexport function forward(value) { return value; }\n"),
    ];
    let files: Vec<_> = extras.into_iter().chain(fixtures.iter().map(|(_, path, bad, _, _)| (*path, *bad))).collect();
    let repo = init_repo(&files);
    let before = current_commit(repo.path());
    repo.commit(&fixtures.map(|(_, path, _, fixed, _)| (path, fixed)), "Use results through imported helpers");
    let after = current_commit(repo.path());
    let cases = fixtures.map(|(id, path, _, _, line)| contract_replay_case(id, path, &[line], &before, &after));
    assert_contract_replay(repo.path(), &cases);
    repo.commit(&fixtures.map(|(_, path, bad, _, _)| (path, bad)), "Discard results through imported helpers");
    let review = static_review_report(&repo, &after, None);
    for (rule, path, _, _, line) in fixtures {
        assert!(review["findings"].as_array().unwrap().iter().any(|finding|
            finding["source"]["ruleId"] == rule && finding["location"]["path"] == path
                && finding["location"]["range"]["startLine"] == line), "{rule}, {path}: {review}");
    }
}

#[test]
fn replay_scores_a_source_in_an_imported_helper_from_its_caller() {
    let helper = "import { rename } from 'node:fs/promises';\nexport function move(a, b) { return rename(a, b); }\n";
    let bad = "import { move } from './helper';\nexport function run(a, b) { move(a, b); }\n";
    let fixed = "import { move } from './helper';\nexport function run(a, b) { return move(a, b); }\n";
    let repo = init_repo(&[("worker.js", bad), ("helper.js", helper)]);
    let before = current_commit(repo.path());
    repo.commit(&[("worker.js", fixed)], "Forward helper result");
    let after = current_commit(repo.path());
    let suite = serde_json::json!({"cases": [{"name": "helper result", "rule": {"tool": "autoreview-dataflow", "id": "javascript-node-fs-discarded-promise"},
        "before": {"commit": before, "entry": "worker.js", "path": "helper.js", "lines": [2]},
        "after": {"commit": after, "entry": "worker.js", "path": "helper.js", "lines": []}}]});
    std::fs::write(repo.path().join("contracts.yaml"), suite.to_string()).unwrap();
    let replay = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .args(["rules", "replay", "--suite", "contracts.yaml"]).assert().success();
    let replay: serde_json::Value = serde_json::from_slice(&replay.get_output().stdout).unwrap();
    assert_eq!(replay["cases"][0]["before"]["entry"], "worker.js");
    repo.commit(&[("worker.js", bad)], "Discard helper result");
    let review = static_review_report(&repo, &after, None);
    assert!(review["findings"].as_array().unwrap().iter().any(|finding|
        finding["source"]["ruleId"] == "javascript-node-fs-discarded-promise"
            && finding["location"]["path"] == "helper.js" && finding["location"]["range"]["startLine"] == 2
            && finding["relatedLocations"].as_array().is_some_and(|related| related.iter().any(|location|
                location["path"] == "worker.js" && location["range"]["startLine"] == 2))), "{review}");
}

#[test]
fn replay_rejects_invalid_cross_file_entry_paths() {
    let source = "export function run() {}\n";
    let repo = init_repo(&[("source.js", source), ("broken.js", "export function (\n")]);
    let commit = current_commit(repo.path());
    for entry in ["../outside.js", "missing.js", "broken.js"] {
        let suite = serde_json::json!({"cases": [{"name": entry, "rule": {"tool": "autoreview-dataflow", "id": "javascript-node-fs-discarded-promise"},
            "before": {"commit": commit, "entry": entry, "path": "source.js", "lines": []},
            "after": {"commit": commit, "entry": entry, "path": "source.js", "lines": []}}]});
        std::fs::write(repo.path().join("suite.json"), suite.to_string()).unwrap();
        let report = failed_replay_report(repo.path());
        assert_eq!(report["failures"][0]["kind"], "error", "{entry}: {report}");
        assert_eq!(report["coverage"][0]["completed"], 0, "{entry}: {report}");
    }
}

#[test]
fn imported_helper_returns_replay_across_languages() {
    let web_helper = "import { rename } from 'node:fs/promises';\nexport function move(a, b) { return rename(a, b); }\n";
    let web_bad = "import { move } from './helper';\nexport function run(a, b) { move(a, b); }\n";
    let web_fixed = "import { move } from './helper';\nexport function run(a, b) { return move(a, b); }\n";
    let cases = [
        ("go-os-rename-discarded-error", "worker.go", "package main\nimport \"example.com/project/helper\"\nfunc run(a, b string) { helper.Move(a, b) }\n", "package main\nimport \"example.com/project/helper\"\nfunc run(a, b string) error { return helper.Move(a, b) }\n", "helper/helper.go", "package helper\nimport \"os\"\nfunc Move(a, b string) error { return os.Rename(a, b) }\n", 3),
        ("java-discarded-objects-comparison", "app/Worker.java", "package app;\nimport support.Checks;\nclass Worker { void run(Object a, Object b) { Checks.compare(a, b); } }\n", "package app;\nimport support.Checks;\nclass Worker { boolean run(Object a, Object b) { return Checks.compare(a, b); } }\n", "support/Checks.java", "package support;\nimport java.util.Objects;\npublic class Checks { public static boolean compare(Object a, Object b) { return Objects.equals(a, b); } }\n", 3),
        ("kotlin-discarded-run-catching-result", "app/Worker.kt", "package app\nimport support.parse\nfun run(text: String) { parse(text) }\n", "package app\nimport support.parse\nfun run(text: String): Result<Int> { return parse(text) }\n", "support/Checks.kt", "package support\nimport kotlin.runCatching\nfun parse(text: String): Result<Int> = runCatching { text.toInt() }\n", 3),
        ("rust-discarded-remove-file-result", "src/worker.rs", "use crate::support::remove;\nfn run(path: &str) { remove(path); }\n", "use crate::support::remove;\nfn run(path: &str) -> std::io::Result<()> { remove(path) }\n", "src/support.rs", "use std::fs::remove_file;\npub(crate) fn remove(path: &str) -> std::io::Result<()> { remove_file(path) }\n", 2),
        ("javascript-node-fs-discarded-promise", "web/js/worker.js", web_bad, web_fixed, "web/js/helper.js", web_helper, 2),
        ("typescript-node-fs-discarded-promise", "web/ts/worker.ts", web_bad, web_fixed, "web/ts/helper.ts", web_helper, 2),
        ("typescript-node-fs-discarded-promise", "web/tsx/Worker.tsx", web_bad, web_fixed, "web/tsx/helper.tsx", web_helper, 2),
    ];
    let files: Vec<_> = [("go.mod", "module example.com/project\n\ngo 1.22\n"), ("src/lib.rs", "mod support; mod worker;\n")]
        .into_iter().chain(cases.iter().flat_map(|(_, caller, bad, _, helper, source, _)| [(*caller, *bad), (*helper, *source)])).collect();
    let repo = init_repo(&files);
    let before = current_commit(repo.path());
    repo.commit(&cases.map(|(_, caller, _, fixed, _, _, _)| (caller, fixed)), "Observe returned helper results");
    let after = current_commit(repo.path());
    let replay_cases: Vec<_> = cases.iter().map(|(rule, caller, _, _, helper, _, line)| serde_json::json!({
        "name": caller, "rule": {"tool": "autoreview-dataflow", "id": rule},
        "before": {"commit": before, "entry": caller, "path": helper, "lines": [line]},
        "after": {"commit": after, "entry": caller, "path": helper, "lines": []}
    })).collect();
    assert_contract_replay(repo.path(), &replay_cases);
    repo.commit(&cases.map(|(_, caller, bad, _, _, _, _)| (caller, bad)), "Discard returned helper results");
    let review = static_review_report(&repo, &after, None);
    for (rule, caller, _, _, helper, _, line) in cases {
        assert!(review["findings"].as_array().unwrap().iter().any(|finding|
            finding["source"]["ruleId"] == rule && finding["location"]["path"] == helper
                && finding["location"]["range"]["startLine"] == line
                && finding["relatedLocations"].as_array().is_some_and(|related| related.iter().any(|item| item["path"] == caller))),
            "{rule}, {caller}: {review}");
    }
}

#[test]
fn unresolved_result_receivers_reach_review_and_block_replay_scoring() {
    let before = "import { rename } from 'node:fs/promises';\nexport function run(a, b) { unknown(rename(a, b)); }\n";
    let after = "import { rename } from 'node:fs/promises';\nexport async function run(a, b) { await rename(a, b); }\n";
    let repo = init_repo(&[("main.js", before)]);
    let old = current_commit(repo.path());
    repo.commit(&[("main.js", after)], "observe rename promise");
    let fixed = current_commit(repo.path());
    let suite = serde_json::json!({"cases":[{"name":"unknown receiver","rule":{"tool":"autoreview-dataflow","id":"javascript-node-fs-discarded-promise"},"before":{"commit":old,"path":"main.js","lines":[]},"after":{"commit":fixed,"path":"main.js","lines":[]}}]});
    std::fs::write(repo.path().join("suite.json"), suite.to_string()).unwrap();
    let report = failed_replay_report(repo.path());
    assert_eq!(report["coverage"][0]["incomplete"], 1, "{report}");
    assert_eq!(report["failures"][0]["kind"], "incomplete-analysis");
    assert!(report["failures"][0]["error"].as_str().unwrap().contains("unresolved helper"), "{report}");
    repo.commit(&[("main.js", before)], "pass promise to unknown receiver");
    let review = static_review_report(&repo, &fixed, None);
    assert!(review["analysisCoverage"].as_array().unwrap().iter().any(|issue|
        issue["reason"] == "unresolved_result_receiver" && issue["rule"] == "javascript-node-fs-discarded-promise"
            && issue["path"] == "main.js" && issue["line"] == 2), "{review}");
}

#[test]
fn review_reports_ambiguous_helpers_without_known_result_sources() {
    let repo = init_repo(&[
        ("main.js", "import { move } from './index';\nexport function run(a, b) { return a; }\n"),
        ("a.js", "export function move(a, b) { return a; }\n"),
        ("b.js", "export function move(a, b) { return b; }\n"),
        ("index.js", "export * from './a'; export * from './b';\n"),
    ]);
    let base = current_commit(repo.path());
    repo.commit(&[("main.js", "import { move } from './index';\nexport function run(a, b) { move(a, b); }\n")], "call ambiguous helper");
    let report = static_review_report(&repo, &base, None);
    assert!(report["analysisCoverage"].as_array().unwrap().iter().any(|issue|
        issue["reason"] == "unresolved_helper_target" && issue["rule"] == "javascript-node-fs-discarded-promise"
            && issue["path"] == "main.js" && issue["line"] == 2), "{report}");
}

fn assert_rust_helper_review(repo: &autoreview_test_support::TestRepo, base: &str, incomplete_reason: &str) {
    let report = static_review_report(repo, base, None);
    for (file, line) in [("src/worker.rs", 3), ("src/support.rs", 3)] {
        assert!(report["findings"].as_array().unwrap().iter().any(|finding|
            finding["source"]["ruleId"] == "rust-discarded-remove-file-result"
                && finding["location"]["path"] == file && finding["location"]["range"]["startLine"] == line), "{report}");
    }
    assert!(!report.get("analysisCoverage").and_then(|items| items.as_array()).is_some_and(|items|
        items.iter().any(|issue| issue["reason"] == incomplete_reason)), "{report}");
}

#[test]
fn rust_local_associated_result_replays_and_reaches_review() {
    let prefix = "use std::fs::remove_file;\nstruct Files;\nimpl Files {\n    fn remove(path: &str) -> std::io::Result<()> { remove_file(path) }\n}\n";
    let before = format!("{prefix}fn run(path: &str) {{ Files::remove(path); }}\n");
    let after = format!("{prefix}fn run(path: &str) -> std::io::Result<()> {{ Files::remove(path) }}\n");
    let repo = init_repo(&[("lib.rs", &before)]);
    let old = current_commit(repo.path());
    repo.commit(&[("lib.rs", &after)], "use associated result");
    let fixed = current_commit(repo.path());
    let suite = serde_json::json!({"cases": [{"name":"local associated helper", "rule":{"tool":"autoreview-dataflow","id":"rust-discarded-remove-file-result"},
        "before":{"commit":old,"path":"lib.rs","lines":[4]}, "after":{"commit":fixed,"path":"lib.rs","lines":[]}}]});
    std::fs::write(repo.path().join("suite.json"), suite.to_string()).unwrap();
    let replay = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path()).args(["rules", "replay", "--suite", "suite.json"]).assert().success();
    let report: serde_json::Value = serde_json::from_slice(&replay.get_output().stdout).unwrap();
    assert_eq!(report["rules"][0]["truePositives"], 1, "{report}");
    assert_eq!(report["rules"][0]["falsePositives"], 0, "{report}");
    repo.commit(&[("lib.rs", &before)], "discard associated result");
    let review = static_review_report(&repo, &fixed, None);
    assert!(review["findings"].as_array().unwrap().iter().any(|finding|
        finding["source"]["ruleId"] == "rust-discarded-remove-file-result"
            && finding["location"]["path"] == "lib.rs" && finding["location"]["range"]["startLine"] == 4), "{review}");
}

#[test]
fn rust_imported_associated_helper_reaches_review_report() {
    let support = "use std::fs::remove_file;\npub(crate) struct Files;\nimpl Files {\n    pub(crate) fn remove(path: &str) -> std::io::Result<()> { remove_file(path) }\n}\n";
    let before = "use crate::support::Files as Store;\nfn run(path: &str) -> std::io::Result<()> { Store::remove(path) }\n";
    let after = "use crate::support::Files as Store;\nfn run(path: &str) { Store::remove(path); }\n";
    let repo = init_repo(&[("src/lib.rs", "mod support; mod worker;\n"), ("src/support.rs", support), ("src/worker.rs", before)]);
    let base = current_commit(repo.path());
    repo.commit(&[("src/worker.rs", after)], "discard associated helper result");
    let report = static_review_report(&repo, &base, None);
    assert!(report["findings"].as_array().unwrap().iter().any(|finding|
        finding["source"]["ruleId"] == "rust-discarded-remove-file-result"
            && finding["location"]["path"] == "src/support.rs" && finding["location"]["range"]["startLine"] == 4), "{report}");
    assert!(report["analysisCoverage"].as_array().unwrap().iter().any(|issue|
        issue["reason"] == "unresolved_rust_module_import" && issue["path"] == "src/worker.rs"), "{report}");
}

#[test]
fn rust_module_alias_results_reach_the_review_report() {
    let root = "mod support; mod worker;\n";
    let support = "use std::fs::remove_file;\npub(crate) fn discard(value: std::io::Result<()>) {}\npub(crate) fn remove(path: &str) -> std::io::Result<()> { remove_file(path) }\n";
    let before = "use crate::support as tasks;\nfn run(path: &str) { let _ = path; }\n";
    let after = "use crate::support as tasks;\nuse std::fs::remove_file;\nfn run(path: &str) { let result = remove_file(path); tasks::discard(result); }\nfn other(path: &str) { tasks::remove(path); }\n";
    let repo = init_repo(&[("src/lib.rs", root), ("src/support.rs", support), ("src/worker.rs", before)]);
    let base = current_commit(repo.path());
    repo.commit(&[("src/worker.rs", after)], "discard helper results");
    assert_rust_helper_review(&repo, &base, "unresolved_rust_module_import");
}

#[test]
fn rust_review_finds_helper_after_many_unreferenced_modules() {
    let mut files = Vec::new();
    let mut modules = String::new();
    for index in 0..80 {
        let name = format!("unused_{index:03}");
        modules.push_str(&format!("mod {name};\n"));
        files.push((format!("src/{name}.rs"), format!("pub fn spare_{index}() {{}}\n")));
    }
    modules.push_str("mod zhelper;\n");
    files.push(("src/lib.rs".into(), modules.clone()));
    files.push(("src/zhelper.rs".into(), "use std::fs::remove_file;\npub fn remove(path: &str) -> std::io::Result<()> { remove_file(path) }\n".into()));
    let refs: Vec<_> = files.iter().map(|(path, source)| (path.as_str(), source.as_str())).collect();
    let repo = init_repo(&refs);
    let base = current_commit(repo.path());
    let after = format!("{modules}fn run() {{ zhelper::remove(\"file\"); }}\n");
    repo.commit(&[("src/lib.rs", &after)], "call helper");
    let report = static_review_report(&repo, &base, None);
    assert!(report["findings"].as_array().unwrap().iter().any(|finding|
        finding["source"]["ruleId"] == "rust-discarded-remove-file-result"
            && finding["location"]["path"] == "src/zhelper.rs"), "{report}");
    assert!(!report.get("analysisCoverage").and_then(|items| items.as_array()).is_some_and(|items|
        items.iter().any(|issue| issue["reason"] == "dependency_limit")), "{report}");
}

#[test]
fn rust_relative_module_results_reach_the_review_report() {
    let root = "mod support; mod worker;\n";
    let support = "use std::fs::remove_file;\npub(crate) fn discard(value: std::io::Result<()>) {}\npub(crate) fn remove(path: &str) -> std::io::Result<()> { remove_file(path) }\n";
    let before = "use super::support as tasks;\nfn run(path: &str) { let _ = path; }\n";
    let after = "use super::support as tasks;\nuse std::fs::remove_file;\nfn run(path: &str) { let result = remove_file(path); tasks::discard(result); }\nfn other(path: &str) { tasks::remove(path); }\n";
    let repo = init_repo(&[("src/lib.rs", root), ("src/support.rs", support), ("src/worker.rs", before)]);
    let base = current_commit(repo.path());
    repo.commit(&[("src/worker.rs", after)], "discard relative helper results");
    assert_rust_helper_review(&repo, &base, "unresolved_rust_module_import");
}

#[test]
fn rust_reexported_results_reach_the_review_report() {
    let root = "mod support; mod api; mod worker;\n";
    let support = "use std::fs::remove_file;\npub(crate) fn discard(value: std::io::Result<()>) {}\npub(crate) fn remove(path: &str) -> std::io::Result<()> { remove_file(path) }\n";
    let api = "pub(crate) use crate::support::{discard as ignore, remove};\n";
    let before = "use crate::api as tasks;\nfn run(path: &str) { let _ = path; }\n";
    let after = "use crate::api as tasks;\nuse std::fs::remove_file;\nfn run(path: &str) { let result = remove_file(path); tasks::ignore(result); }\nfn other(path: &str) { tasks::remove(path); }\n";
    let repo = init_repo(&[("src/lib.rs", root), ("src/support.rs", support), ("src/api.rs", api), ("src/worker.rs", before)]);
    let base = current_commit(repo.path());
    repo.commit(&[("src/worker.rs", after)], "discard reexported results");
    assert_rust_helper_review(&repo, &base, "unresolved_helper_target");
}

#[test]
fn changed_lines_include_verified_project_error_results() {
    let (Ok(probe), Ok(go)) = (std::env::var("AUTOREVIEW_TEST_GO_PROBE"), std::env::var("AUTOREVIEW_TEST_GO_BINARY")) else { return };
    let helper = "package helper\nfunc Work() (int, error) { return 1, nil }\nfunc Ignore(n int, err error) {}\nfunc Check(n int, err error) { if err != nil { return } }\n";
    let before = "package main\nimport \"example.test/review/helper\"\nfunc run() error { _, err := helper.Work(); return err }\n";
    let after = "package main\nimport \"example.test/review/helper\"\nfunc run() { value, _ := helper.Work(); _ = value }\nfunc nested() { helper.Ignore(helper.Work()) }\nfunc checked() { helper.Check(helper.Work()) }\n";
    let repo = init_repo(&[("go.mod", "module example.test/review\n\ngo 1.22\n"), ("helper/helper.go", helper), ("main.go", before)]);
    let base = current_commit(repo.path());
    repo.commit(&[("main.go", after)], "discard project error");
    let run = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .env("AUTOREVIEW_GO_ERROR_PROBE", probe).env("AUTOREVIEW_GO_BINARY", go)
        .args(["diff", "--base", &base, "--head", "HEAD", "--static-only", "--changed-lines"])
        .assert().success();
    let report = report_path_from_stdout(&String::from_utf8_lossy(&run.get_output().stdout));
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
    assert!(report["findings"].as_array().unwrap().iter().any(|finding|
        finding["source"]["ruleId"] == "go-project-error-result-unobserved" && finding["location"]["range"]["startLine"] == 3), "{report}");
    assert!(report["findings"].as_array().unwrap().iter().any(|finding|
        finding["source"]["ruleId"] == "go-project-error-result-unobserved" && finding["location"]["range"]["startLine"] == 4), "{report}");
    assert!(!report["findings"].as_array().unwrap().iter().any(|finding|
        finding["source"]["ruleId"] == "go-project-error-result-unobserved" && finding["location"]["range"]["startLine"] == 5), "{report}");
    assert!(!report.get("analysisCoverage").and_then(|items| items.as_array()).is_some_and(|items|
        items.iter().any(|issue| issue["rule"] == "go-project-error-result-unobserved")), "{report}");
}

#[test]
fn changed_lines_include_results_discarded_by_a_caller_in_another_file() {
    let helper = "package helper\nimport \"os\"\nfunc Move(a, b string) error { return os.Rename(a, b) }\n";
    let before = "package main\nimport \"example.com/demo/helper\"\nfunc move(a, b string) error { return helper.Move(a, b) }\n";
    let after = "package main\nimport \"example.com/demo/helper\"\nfunc move(a, b string) { helper.Move(a, b) }\n";
    let repo = init_repo(&[("go.mod", "module example.com/demo\n\ngo 1.22\n"), ("helper/helper.go", helper), ("main.go", before)]);
    let base = current_commit(repo.path());
    repo.commit(&[("main.go", after)], "discard helper result");
    let review = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .args(["diff", "--base", &base, "--head", "HEAD", "--static-only", "--changed-lines", "--aspects", "correctness"])
        .assert().success();
    let path = report_path_from_stdout(&String::from_utf8_lossy(&review.get_output().stdout));
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(report["findings"].as_array().unwrap().iter().any(|finding|
        finding["source"]["ruleId"] == "go-os-rename-discarded-error"
            && finding["location"]["path"] == "helper/helper.go"), "{report}");
}

#[test]
fn rust_structural_rules_surface_and_replay_at_exact_commits() {
    let path = "src/checkout.rs";
    let clean = "struct Cart { base: i32, tax: i32, fee: i32 }\nstruct Checkout;\n";
    let repo = init_repo(&[(path, clean)]);
    let base = current_commit(repo.path());
    let mut oversized = String::from(clean);
    oversized.push_str("impl Checkout {\n    fn total(&self, cart: &Cart) -> i32 {\n        cart.base + cart.tax + cart.fee\n    }\n}\n");
    for index in 0..21 {
        oversized.push_str(&format!("fn step_{index}() {{\n"));
        for value in 0..14 {
            oversized.push_str(&format!("    let _ = {index} + {value};\n"));
        }
        oversized.push_str("}\n");
    }
    repo.commit(&[(path, &oversized)], "add a large checkout module");
    let bad = current_commit(repo.path());
    let result = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .args(["diff", "--base", &base, "--head", &bad, "--static-only", "--changed-lines", "--aspects", "design"])
        .assert().success();
    let report_path = report_path_from_stdout(&String::from_utf8_lossy(&result.get_output().stdout));
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(report_path).unwrap()).unwrap();
    repo.commit(&[(path, clean)], "restore small checkout module");
    let fixed = current_commit(repo.path());
    let mut cases = Vec::new();
    for (id, tool) in [("rust-god-module", "autoreview-complexity"), ("rust-feature-envy", "autoreview-rust")] {
        let finding = report["findings"].as_array().unwrap().iter()
            .find(|finding| finding["source"]["ruleId"] == id).expect(id);
        cases.push(serde_json::json!({
            "name": id, "rule": {"id": id, "tool": tool},
            "before": {"commit": bad, "path": path, "lines": [finding["location"]["range"]["startLine"]]},
            "after": {"commit": fixed, "path": path, "lines": []},
        }));
        Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
            .args(["explain", finding["id"].as_str().unwrap()])
            .assert().success().stdout(predicate::str::contains(id));
    }
    let suite = repo.path().join("replay.yml");
    std::fs::write(&suite, serde_json::json!({"cases": cases}).to_string()).unwrap();
    Command::cargo_bin("autoreview").unwrap().current_dir(repo.path()).args(["rules", "replay", "--suite"])
        .arg(suite).assert().success().stdout(predicate::str::contains("\"falsePositives\": 0"));
}

#[test]
fn finding_draft_replays_commits_and_only_persists_a_matching_rule() {
    if !ast_grep_available() {
        return;
    }
    let clean = "package fixture\nfunc Equal(a, b int) bool { return a == b }\n";
    let bad = "package fixture\nfunc Equal(a, b int) bool { return a == a }\n";
    let repo = init_repo(&[("compare.go", clean)]);
    let base = current_commit(repo.path());
    repo.commit(&[("compare.go", bad)], "add defect");
    let broken = current_commit(repo.path());
    let review = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .args(["diff", "--base", &base, "--head", &broken, "--static-only", "--changed-lines"])
        .assert().success();
    let report_file = report_path_from_stdout(&String::from_utf8_lossy(&review.get_output().stdout));
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(report_file).unwrap()).unwrap();
    let finding = report["findings"].as_array().unwrap().iter()
        .find(|finding| finding["source"]["ruleId"] == "go-no-self-comparison").unwrap();
    let id = finding["id"].as_str().unwrap();
    repo.commit(&[("compare.go", clean)], "fix defect");
    let fixed = current_commit(repo.path());
    std::fs::write(repo.path().join("compare.go"), bad).unwrap();
    let proposal = repo.path().join("proposal.yml");
    let yaml = "id: demo-repeated-return-operand\nlanguage: Go\ncategory: correctness\nseverity: warning\nmessage: Compare the distinct arguments\nrule:\n  pattern: return $A == $A\n";
    std::fs::write(&proposal, yaml).unwrap();

    Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .args(["rules", "draft", id, "--proposal", "proposal.yml", "--fixed", &broken])
        .assert().failure().stderr(predicate::str::contains("still matches"));
    assert!(!repo.path().join(".autoreview/rules/drafts").exists());

    Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .args(["rules", "draft", id, "--proposal", "proposal.yml", "--fixed", &fixed])
        .assert().success().stdout(predicate::str::contains("Unregistered draft pack"));
    let draft = std::fs::read_dir(repo.path().join(".autoreview/rules/drafts"))
        .unwrap().next().unwrap().unwrap().path();
    Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .args(["rules", "packs", "validate"]).arg(draft).assert().success();
    assert!(!repo.path().join(".autoreview/rulepacks.yaml").exists());
    assert_eq!(std::fs::read_to_string(repo.path().join("compare.go")).unwrap(), bad);
}

#[test]
fn doctor_reports_on_all_required_tools_and_the_cost_model_caveat() {
    let mut cmd = Command::cargo_bin("autoreview").unwrap();
    cmd.arg("doctor");
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Exit code isn't asserted here — it legitimately depends on whether
    // `claude`/git are on PATH in whatever environment runs this suite.
    // What must always be true is that doctor actually checked and reported
    // on each required tool, and printed the cost-model transparency note.
    for expected in [
        "autoreview doctor",
        "git",
        "claude",
        "ast-grep",
        "golangci-lint",
        "Cost model assumption",
    ] {
        assert!(
            stdout.contains(expected),
            "doctor output missing '{expected}':\n{stdout}"
        );
    }
}

#[test]
fn diff_against_a_seeded_bug_produces_a_valid_report_with_the_expected_finding() {
    if !ast_grep_available() {
        eprintln!("skipping: ast-grep not on PATH");
        return;
    }

    let repo = init_repo_with_diff(
        &[("main.go", "package main\n\nfunc main() {\n\tprintln(\"hello\")\n}\n")],
        &[("main.go", "package main\n\nfunc main() {\n\tx := 1\n\tif x == x {\n\t\tprintln(\"bug\")\n\t}\n}\n")],
        "seed a self-comparison bug",
    );

    let mut cmd = Command::cargo_bin("autoreview").unwrap();
    cmd.current_dir(repo.path())
        .args(["diff", "--base", "main~1", "--head", "main"]);
    let assert = cmd.assert().success();
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("files changed:  1"),
        "stdout was:\n{stdout}"
    );

    let report_path = report_path_from_stdout(&stdout);
    let report_text = std::fs::read_to_string(&report_path)
        .unwrap_or_else(|e| panic!("could not read report at {}: {e}", report_path.display()));
    let report: serde_json::Value = serde_json::from_str(&report_text).unwrap();

    assert_eq!(report["schemaVersion"], "1");
    assert_eq!(report["target"]["baseRef"], "main~1");
    assert_eq!(report["target"]["headRef"], "main");

    let findings = report["findings"]
        .as_array()
        .expect("findings should be an array");
    assert!(
        findings
            .iter()
            .any(|f| f["source"]["ruleId"] == "go-no-self-comparison"),
        "expected the seeded bug in the report's findings, got: {findings:#?}"
    );
}

#[test]
fn rust_design_and_performance_findings_follow_aspect_filters() {
    if !ast_grep_available() {
        return;
    }
    let decisions = "    if flag { total += 1; }\n".repeat(11);
    let source = format!(
        "pub async fn work(flag: bool) -> i32 {{\n    std::thread::sleep(std::time::Duration::from_millis(1));\n    let mut total = 0;\n{decisions}    total\n}}\n"
    );
    let repo = init_repo_with_diff(
        &[("app.rs", "pub async fn work(_: bool) -> i32 { 0 }\n")],
        &[("app.rs", &source)],
        "seed Rust performance and design findings",
    );
    for (category, expected) in [
        ("performance", "rust-blocking-call-in-async"),
        ("design", "cyclomatic-complexity"),
    ] {
        let result = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
            .args(["diff", "--base", "main~1", "--head", "main", "--static-only", "--changed-lines", "--aspects", category])
            .assert().success();
        let stdout = String::from_utf8_lossy(&result.get_output().stdout);
        let report: serde_json::Value = serde_json::from_slice(&std::fs::read(report_path_from_stdout(&stdout)).unwrap()).unwrap();
        let findings = report["findings"].as_array().unwrap();
        assert!(findings.iter().any(|f| f["source"]["ruleId"] == expected && f["location"]["path"] == "app.rs"));
        assert!(findings.iter().all(|f| f["category"] == category));
    }
}

#[test]
fn scoped_rust_rules_report_explain_and_replay_historical_commits() {
    let repo = init_repo(&[("lib.rs", "fn now() {}\n")]);
    let base = current_commit(repo.path());
    let before_source = "use std::time::SystemTime as Clock;\nfn elapsed() {\n    let start = Clock::now();\n    start.elapsed().unwrap();\n}\n";
    repo.commit(&[("lib.rs", before_source)], "use wall clock for elapsed time");
    let before = current_commit(repo.path());
    let after_source = "use std::time::Instant as Clock;\nfn elapsed() {\n    let start = Clock::now();\n    start.elapsed();\n}\n";
    repo.commit(&[("lib.rs", after_source)], "use monotonic clock");
    let after = current_commit(repo.path());
    let id = "rust-wall-clock-elapsed-unwrap";

    let diff = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .args(["diff", "--base", &base, "--head", &before, "--static-only", "--changed-lines", "--aspects", "correctness"])
        .assert().success();
    let stdout = String::from_utf8_lossy(&diff.get_output().stdout);
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(report_path_from_stdout(&stdout)).unwrap()).unwrap();
    let findings = report["findings"].as_array().unwrap();
    let finding = findings.iter().find(|finding| finding["source"]["ruleId"] == id).unwrap();
    assert_eq!(finding["source"]["tool"], "autoreview-rust");
    assert_eq!(finding["location"]["range"]["startLine"], 4);
    Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .args(["explain", finding["id"].as_str().unwrap()])
        .assert().success().stdout(predicate::str::contains("Built-in Rust analyzer rule"));

    let suite = serde_json::json!({"cases": [{
        "name": "wall-clock-timer",
        "rule": {"tool": "autoreview-rust", "id": id},
        "before": {"commit": before, "path": "lib.rs", "lines": [4]},
        "after": {"commit": after, "path": "lib.rs", "lines": []},
    }]}).to_string();
    std::fs::write(repo.path().join("suite.yaml"), suite).unwrap();
    let replay = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .args(["rules", "replay", "--suite", "suite.yaml"]).assert().success();
    let results: serde_json::Value = serde_json::from_slice(&replay.get_output().stdout).unwrap();
    assert_eq!(results["rules"][0]["precision"], 1.0);
    assert_eq!(results["rules"][0]["recall"], 1.0);
}

#[test]
fn cargo_review_uses_confirmed_build_paths_and_reports_manifest_layer_edges() {
    let workspace = "[workspace]\nmembers = [\"domain\", \"infra\"]\nresolver = \"2\"\n";
    let domain = "[package]\nname = \"domain\"\nversion = \"0.1.0\"\nedition = \"2021\"\nbuild = \"generate\"\n";
    let infra = "[package]\nname = \"infra\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
    let policy = "architecture:\n  layers:\n    - name: domain\n      match: ['domain/**']\n    - name: infra\n      match: ['infra/**']\n  rules:\n    - forbid: {from: domain, to: [infra]}\n";
    let domain_with_edge = format!("{domain}\n[dependencies]\nimplementation = {{ package = \"infra\", path = \"../infra\" }}\n");
    let repo = init_repo_with_diff(&[
        ("Cargo.toml", workspace),
        ("domain/Cargo.toml", domain),
        ("domain/src/lib.rs", ""),
        ("domain/generate", "fn main() { println!(\"cargo:rerun-if-env-changed=CC\"); }\n"),
        ("domain/build.rs", "fn main() { println!(\"cargo:rerun-if-env-changed=TARGET\"); }\n"),
        ("infra/Cargo.toml", infra),
        ("infra/src/lib.rs", ""),
        (".autoreview/architecture.yaml", policy),
    ], &[
        ("domain/Cargo.toml", &domain_with_edge),
        ("domain/generate", "fn main() { println!(\"cargo:rerun-if-env-changed=TARGET\"); }\n"),
    ], "add a forbidden workspace dependency and ineffective build trigger");

    let diff = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .args(["diff", "--base", "main~1", "--head", "main", "--static-only", "--changed-lines"])
        .assert().success();
    let stdout = String::from_utf8_lossy(&diff.get_output().stdout);
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(report_path_from_stdout(&stdout)).unwrap()).unwrap();
    let findings = report["findings"].as_array().unwrap();
    for (rule, path) in [
        ("rust-cargo-generated-env-trigger", "domain/generate"),
        ("rust-cargo-layer-violation", "domain/Cargo.toml"),
    ] {
        assert!(findings.iter().any(|finding| {
            finding["source"]["ruleId"] == rule && finding["location"]["path"] == path
        }), "{findings:#?}");
    }
}

#[test]
fn a_shadow_trust_rule_packs_finding_is_suppressed_not_surfaced() {
    if !ast_grep_available() {
        eprintln!("skipping: ast-grep not on PATH");
        return;
    }

    let pack_dir = tempfile::tempdir().unwrap();
    std::fs::write(pack_dir.path().join("rulepack.yaml"), "id: acme-shadow\nversion: \"1.0.0\"\n").unwrap();
    std::fs::write(
        pack_dir.path().join("no-println.yml"),
        "id: acme-shadow-no-println\nlanguage: Go\ncategory: security\nseverity: error\nmessage: no println\nrule:\n  pattern: println($$$ARGS)\n",
    )
    .unwrap();

    let repo = init_repo_with_diff(
        &[("main.go", "package main\n\nfunc main() {}\n")],
        &[("main.go", "package main\n\nfunc main() {\n\tprintln(\"debug leftover\")\n}\n")],
        "seed a println debug leftover",
    );
    std::fs::create_dir_all(repo.path().join(".autoreview")).unwrap();
    std::fs::write(
        repo.path().join(".autoreview/rulepacks.yaml"),
        format!("packs:\n  - id: acme-shadow\n    source:\n      kind: local\n      path: {}\n    trust: shadow\n", pack_dir.path().display()),
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("autoreview").unwrap();
    cmd.current_dir(repo.path()).args(["diff", "--base", "main~1", "--head", "main", "--tier", "quick"]);
    let assert = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);

    let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(report_path_from_stdout(&stdout)).unwrap()).unwrap();
    let findings = report["findings"].as_array().unwrap();
    assert!(
        !findings.iter().any(|f| f["source"]["ruleId"] == "acme-shadow-no-println"),
        "a shadow-trust pack rule's finding must not surface, got: {findings:#?}"
    );
    let suppressed = report["suppressed"].as_array().unwrap();
    assert!(
        suppressed.iter().any(|s| s["finding"]["source"]["ruleId"] == "acme-shadow-no-println" && s["reason"] == "shadow-rule-pack"),
        "expected the finding in suppressed with reason shadow-rule-pack, got: {suppressed:#?}"
    );
}

#[test]
fn rules_packs_add_registers_a_local_source_pack() {
    let pack_dir = tempfile::tempdir().unwrap();
    std::fs::write(pack_dir.path().join("rulepack.yaml"), "id: acme-security\nversion: \"1.0.0\"\n").unwrap();

    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);

    let mut cmd = Command::cargo_bin("autoreview").unwrap();
    cmd.current_dir(repo.path()).args(["rules", "packs", "add", &pack_dir.path().to_string_lossy()]);
    cmd.assert().success().stdout(predicate::str::contains("Registered rule pack 'acme-security'"));

    let config_contents = std::fs::read_to_string(repo.path().join(".autoreview/rulepacks.yaml")).unwrap();
    assert!(config_contents.contains("acme-security"), "got: {config_contents}");
    assert!(config_contents.contains(&pack_dir.path().to_string_lossy().to_string()), "got: {config_contents}");
}

#[test]
fn rules_packs_add_fails_clearly_for_an_already_registered_id() {
    let pack_dir = tempfile::tempdir().unwrap();
    std::fs::write(pack_dir.path().join("rulepack.yaml"), "id: acme-security\nversion: \"1.0.0\"\n").unwrap();

    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
    std::fs::create_dir_all(repo.path().join(".autoreview")).unwrap();
    std::fs::write(repo.path().join(".autoreview/rulepacks.yaml"), format!("packs:\n  - id: acme-security\n    source:\n      kind: local\n      path: {}\n", pack_dir.path().display())).unwrap();

    let mut cmd = Command::cargo_bin("autoreview").unwrap();
    cmd.current_dir(repo.path()).args(["rules", "packs", "add", &pack_dir.path().to_string_lossy()]);
    cmd.assert().failure().stderr(predicate::str::contains("already registered"));
}

#[test]
fn rules_packs_validate_succeeds_for_a_well_formed_pack() {
    let pack_dir = tempfile::tempdir().unwrap();
    std::fs::write(pack_dir.path().join("rulepack.yaml"), "id: acme-security\nversion: \"1.0.0\"\n").unwrap();
    std::fs::write(pack_dir.path().join("no-println.yml"), "id: acme-no-println\nlanguage: Go\ncategory: security\nseverity: error\nmessage: m\nrule:\n  pattern: println($$$ARGS)\n").unwrap();

    let mut cmd = Command::cargo_bin("autoreview").unwrap();
    cmd.args(["rules", "packs", "validate", &pack_dir.path().to_string_lossy()]);
    cmd.assert().success().stdout(predicate::str::contains("no problems found"));
}

#[test]
fn rules_packs_validate_fails_clearly_on_a_broken_pattern_rule() {
    let pack_dir = tempfile::tempdir().unwrap();
    std::fs::write(pack_dir.path().join("rulepack.yaml"), "id: acme-security\nversion: \"1.0.0\"\n").unwrap();
    std::fs::write(pack_dir.path().join("broken.yml"), "id: acme-broken\nlanguage: Go\ncategory: security\nseverity: error\nmessage: m\n").unwrap();

    let mut cmd = Command::cargo_bin("autoreview").unwrap();
    cmd.args(["rules", "packs", "validate", &pack_dir.path().to_string_lossy()]);
    cmd.assert().failure().stderr(predicate::str::contains("validation problem"));
}

fn run_git(cwd: &std::path::Path, args: &[&str]) {
    let status = StdCommand::new("git").args(args).current_dir(cwd).status().unwrap();
    assert!(status.success(), "git {args:?} failed in {}", cwd.display());
}

#[test]
fn static_review_uses_the_requested_commit_and_handles_renames_from_a_subdirectory() {
    if !ast_grep_available() {
        return;
    }
    let baseline = "package sample\n\n// Compare two values supplied by the caller.\nfunc compare(a, b int) bool {\n    return a == b\n}\n";
    let bug = baseline.replace("a == b", "a == a");
    let legacy = "package sample\n\nfunc legacy(a int) bool { return a == a }\nfunc current(a int) int { return a + 1 }\n";
    let repo = autoreview_test_support::init_repo(&[("src/before.go", baseline), ("src/legacy.go", legacy)]);
    run_git(repo.path(), &["mv", "src/before.go", "src/café renamed.go"]);
    let changed_legacy = legacy.replace("a + 1", "a + 2");
    repo.commit(&[("src/café renamed.go", &bug), ("src/legacy.go", &changed_legacy)], "introduce wrong operand");
    repo.commit(&[("src/café renamed.go", baseline)], "fix wrong operand at the current head");
    std::fs::write(repo.path().join("src/legacy.go"), changed_legacy.replace("a + 2", "a + 3")).unwrap();

    let output = assert_cmd::Command::cargo_bin("autoreview").unwrap()
        .current_dir(repo.path().join("src"))
        .args(["diff", "--base", "HEAD~2", "--head", "HEAD~1", "--tier", "deep", "--static-only", "--changed-lines"])
        .assert().success().get_output().stdout.clone();
    let report_path = report_path_from_stdout(&String::from_utf8(output).unwrap());
    let report: autoreview_schema::ReviewReport = serde_json::from_slice(&std::fs::read(report_path).unwrap()).unwrap();
    assert!(report.plan.specialists.is_empty());
    assert!(report.costs.per_stage.is_empty());
    let findings: Vec<_> = report.findings.iter()
        .filter(|finding| finding.source.rule_id.as_deref() == Some("go-no-self-comparison"))
        .map(|finding| finding.location.path.as_str())
        .collect();
    assert_eq!(findings, ["src/café renamed.go"]);
    assert_eq!(std::fs::read_to_string(repo.path().join("src/café renamed.go")).unwrap(), baseline);
}


#[test]
fn java_nullability_regressions_are_reviewed_and_replayed_against_the_base() {
    const RULE: &str = "java-new-nullable-return-breaks-caller";
    let helper = "src/service/Names.java";
    let caller = "src/client/Caller.java";
    let original = nullability_helper_source();
    let caller_source = nullability_caller_source();
    let repo = init_repo(&[
        (helper, original),
        (caller, caller_source),
        ("src/other/Names.java", "package other; public class Names { public static String label(boolean b) { return \"other\"; } }"),
    ]);
    let base = current_commit(repo.path());
    let regression = original
        .replacen("        return \"ready\";", "        if (missing) return null;\n        return \"ready\";", 1)
        .replace("return System.getProperty(\"name\");", "return null;")
        .replace("public static String unreachable() {", "public static String unreachable() {\n        if (false) return null;")
        .replace("public static String existing(boolean missing) {", "public static String existing(boolean missing) { // still nullable");
    repo.commit(&[(helper, &regression)], "Introduce a nullable return");
    let bad = current_commit(repo.path());
    repo.commit(&[(helper, original)], "Restore the helper contract");
    let fixed = current_commit(repo.path());

    let output = Command::cargo_bin("autoreview").unwrap()
        .current_dir(repo.path())
        .args(["diff", "--base", &base, "--head", &bad, "--static-only", "--changed-lines",
            "--subcategories", "correctness.nullability"])
        .assert().success().get_output().stdout.clone();
    let report = report_path_from_stdout(&String::from_utf8(output).unwrap());
    let review: autoreview_schema::ReviewReport = serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
    let mut lines: Vec<_> = review.findings.iter()
        .filter(|item| item.source.rule_id.as_deref() == Some(RULE))
        .map(|item| {
            assert_eq!(item.location.path, caller);
            let related = item.related_locations.as_ref().unwrap();
            assert!(related.iter().any(|location| location.path == helper && location.range.start_line == 4));
            assert!(related.iter().any(|location| location.path == caller && location.snippet.contains("Names.label")));
            item.location.range.start_line
        }).collect();
    lines.sort_unstable();
    assert_eq!(lines, [6, 9], "{:?}", review.findings);

    let suite = serde_json::json!({
        "cases": [{
            "name": "new nullable return",
            "rule": {"tool": "autoreview-frameworks", "id": RULE},
            "base": base,
            "before": {"commit": bad, "path": caller, "lines": lines},
            "after": {"commit": fixed, "path": caller, "lines": []}
        }]
    });
    std::fs::write(repo.path().join("replay.yaml"), suite.to_string()).unwrap();
    let output = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .args(["rules", "replay", "--suite", "replay.yaml"]).assert().success().get_output().stdout.clone();
    let replay: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(replay["rules"][0]["truePositives"], 2);
    assert_eq!(replay["rules"][0]["falsePositives"], 0);
    assert_eq!(replay["rules"][0]["falseNegatives"], 0);
}

fn nullability_helper_source() -> &'static str {
    r#"package service;
public class Names {
    public static String label(boolean missing) {
        return "ready";
    }
    public static String existing(boolean missing) {
        if (missing) return null;
        return "ready";
    }
    public static String unknown() {
        return System.getProperty("name");
    }
    public static String unreachable() {
        return "ready";
    }
}
"#
}

fn nullability_caller_source() -> &'static str {
    r#"package client;
import service.Names;
class Caller {
    int length() {
        String value = Names.label(true);
        return value.length();
    }
    int chained() {
        return service.Names.label(true).length();
    }
    int guarded() {
        String value = Names.label(true);
        if (/* checked here */ value == null) return 0;
        return value.length();
    }
    int guardedAlias() {
        String value = Names.label(true);
        String alias = value;
        if (alias == null) throw new IllegalStateException();
        return value.length();
    }
    int reassigned() {
        String value = Names.label(true);
        value = "ready";
        return value.length();
    }
    String staticCall() {
        String value = Names.label(true);
        return value.valueOf(1);
    }
    int unrelated() {
        String value = other.Names.label(true);
        return value.length();
    }
    int existing() {
        return Names.existing(true).length();
    }
    int unknownBaseline() {
        return Names.unknown().length();
    }
    int unreachableNull() {
        return Names.unreachable().length();
    }
    int nonNullArgument() {
        return Names.label(false).length();
    }
}
"#
}

#[test]
fn api_contract_rules_replay_on_bad_and_fixed_commits() {
    const FIXTURES: [(&str, &str, &str, &str, u32); 3] = [
        (
            "go-os-rename-discarded-error", "rename/move.go",
            include_str!("../../core/tests/fixtures/rules/go-os-rename-discarded-error/positive.go"),
            include_str!("../../core/tests/fixtures/rules/go-os-rename-discarded-error/negative.go"),
            6,
        ),
        (
            "go-gzip-writer-unclosed", "gzip/archive.go",
            include_str!("../../core/tests/fixtures/rules/go-gzip-writer-unclosed/positive.go"),
            include_str!("../../core/tests/fixtures/rules/go-gzip-writer-unclosed/negative.go"),
            15,
        ),
        (
            "java-completable-future-unobserved", "Main.java",
            include_str!("../../core/tests/fixtures/rules/java-completable-future-unobserved/positive.java"),
            include_str!("../../core/tests/fixtures/rules/java-completable-future-unobserved/negative.java"),
            7,
        ),
    ];
    let bad_files = FIXTURES.map(|(_, path, bad, _, _)| (path, bad));
    let repo = init_repo(&bad_files);
    let before = current_commit(repo.path());
    let fix_files = FIXTURES.map(|(_, path, _, fixed, _)| (path, fixed));
    repo.commit(&fix_files, "Satisfy API contracts");
    let after = current_commit(repo.path());
    let cases = FIXTURES.map(|(id, path, _, _, line)| contract_replay_case(id, path, &[line], &before, &after));
    assert_contract_replay(repo.path(), &cases);
}

fn contract_replay_case(id: &str, path: &str, lines: &[u32], before: &str, after: &str) -> serde_json::Value {
    serde_json::json!({
        "name": format!("{id} ({path})"),
        "rule": { "tool": "autoreview-dataflow", "id": id },
        "before": { "commit": before, "path": path, "lines": lines },
        "after": { "commit": after, "path": path, "lines": [] }
    })
}

fn assert_contract_replay(repo: &std::path::Path, cases: &[serde_json::Value]) {
    std::fs::write(repo.join("contracts.yaml"), serde_json::json!({ "cases": cases }).to_string()).unwrap();
    let replay = Command::cargo_bin("autoreview").unwrap()
        .current_dir(repo)
        .args(["rules", "replay", "--suite", "contracts.yaml"])
        .output().unwrap();
    assert!(
        replay.status.success(), "{}\n{}",
        String::from_utf8_lossy(&replay.stderr), String::from_utf8_lossy(&replay.stdout),
    );
}

#[test]
fn assigned_api_results_replay_in_every_supported_language() {
    let web = "import { rename } from 'node:fs/promises';\n\
        async function publish(from, to, ready) {\n\
            let result = rename(from, to);\n\
            if (ready) { result = Promise.resolve(); }\n\
            else { result = Promise.resolve(); }\n\
            await result;\n";
    let go = "package publish\nimport \"os\"\n\
        func Promote(from, to string, ready bool) error {\n\
            err := os.Rename(from, to)\n\
            if ready { err = fallback() } else { err = nil }\n\
            return err\n}\n";
    let rust = "use std::fs::remove_file;\n\
        fn purge(path: &str) {\n\
            let result = remove_file(path);\n\
            let alias = result;\n\
        }\n";
    let kotlin = "import kotlin.runCatching\nfun parse(value: String): Int {\n\
        val result = runCatching { value.toInt() }\n\
        return 0\n}\n";
    let java = include_str!("../../core/tests/fixtures/rules/java-discarded-objects-comparison/positive.java");
    let tsx = format!("{web}return <main />;\n}}\n");
    let javascript = format!("{web}}}\n");
    let typescript = javascript.replace("publish(from, to, ready)", "publish(from: string, to: string, ready: boolean)");
    let web_fix = |text: &str| text.replace("let result = rename", "let result = await rename");
    let fixtures = [
        ("go-os-rename-discarded-error", "publish/move.go", go, go.replace("if ready { err = fallback() } else { err = nil }", "if ready && err == nil { err = fallback() }"), 4),
        ("java-discarded-objects-comparison", "Label.java", java, include_str!("../../core/tests/fixtures/rules/java-discarded-objects-comparison/negative.java").into(), 5),
        ("kotlin-discarded-run-catching-result", "Parse.kt", kotlin, kotlin.replace("return 0", "return result.getOrThrow()"), 3),
        ("rust-discarded-remove-file-result", "lib.rs", rust, rust.replace("let alias = result;", "let alias = result;\n    alias.unwrap();"), 3),
        ("javascript-node-fs-discarded-promise", "publish.js", &javascript, web_fix(&javascript), 3),
        ("typescript-node-fs-discarded-promise", "publish.ts", &typescript, web_fix(&typescript), 3),
        ("typescript-node-fs-discarded-promise", "Publish.tsx", &tsx, web_fix(&tsx), 3),
    ];
    let repo = init_repo(&fixtures.each_ref().map(|(_, path, bad, _, _)| (*path, *bad)));
    let before = current_commit(repo.path());
    repo.commit(&fixtures.each_ref().map(|(_, path, _, fixed, _)| (*path, fixed.as_str())), "Use the original API results");
    let after = current_commit(repo.path());
    let cases = fixtures.map(|(id, path, _, _, line)| contract_replay_case(id, path, &[line], &before, &after));
    assert_contract_replay(repo.path(), &cases);
}

#[test]
fn node_contracts_replay_in_tsx_with_the_typescript_rule_ids() {
    let path = "Jobs.tsx";
    let bad = r#"import { open, rename } from 'node:fs/promises';
import { Worker } from 'node:worker_threads';

export function Relabel(name: string) {
  rename(name, 'published');
  return <div />;
}
export async function Read(path: string) {
  const file = await open(path);
  file.close();
  return <article />;
}
export function Launch(path: string) {
  const job = new Worker(path);
  return <aside />;
}
"#;
    let fixed = bad
        .replace("function Relabel(", "async function Relabel(")
        .replace("  rename(", "  await rename(")
        .replace("  file.close();", "  await file.close();")
        .replace("function Launch(", "async function Launch(")
        .replace("  return <aside", "  await job.terminate();\n  return <aside");
    let repo = init_repo(&[(path, bad)]);
    let before = current_commit(repo.path());
    repo.commit(&[(path, &fixed)], "Observe promises and finish owned handles");
    let after = current_commit(repo.path());
    let cases = [
        ("typescript-node-fs-discarded-promise", 5),
        ("typescript-node-file-handle-unclosed", 11),
        ("typescript-node-worker-outlives-owner", 15),
    ].map(|(id, line)| contract_replay_case(id, path, &[line], &before, &after));
    assert_contract_replay(repo.path(), &cases);
}

#[test]
fn closure_and_field_result_contracts_replay_across_all_languages() {
    let web = include_str!("../../core/tests/fixtures/result-use/publish.js").replace("\r\n", "\n");
    let tsx = web.replace("    await slot.pending;\n}", "    await slot.pending;\n    return <aside />;\n}");
    let fixtures = [
        ("go-os-rename-discarded-error", "storage/move.go", include_str!("../../core/tests/fixtures/result-use/storage.go")),
        ("java-discarded-objects-comparison", "Comparison.java", include_str!("../../core/tests/fixtures/result-use/Comparison.java")),
        ("kotlin-discarded-run-catching-result", "Parse.kt", include_str!("../../core/tests/fixtures/result-use/Parse.kt")),
        ("rust-discarded-remove-file-result", "lib.rs", include_str!("../../core/tests/fixtures/result-use/removal.rs")),
        ("javascript-node-fs-discarded-promise", "publish.js", &web),
        ("typescript-node-fs-discarded-promise", "publish.ts", &web),
        ("typescript-node-fs-discarded-promise", "PublishView.tsx", &tsx),
    ];
    let repo = init_repo(&fixtures.map(|(_, path, source)| (path, source)));
    let before = current_commit(repo.path());
    let fixed = fixtures.map(|(_, _, source)| fixture_result_edits(source));
    let changes: Vec<_> = fixtures.iter().zip(&fixed).map(|((_, path, _), (_, source))| (*path, source.as_str())).collect();
    repo.commit(&changes, "Observe the original closure and field results");
    let after = current_commit(repo.path());
    let cases: Vec<_> = fixtures.iter().zip(&fixed).map(|((id, path, _), (lines, _))| {
        assert!(!lines.is_empty(), "{path}");
        contract_replay_case(id, path, lines, &before, &after)
    }).collect();
    assert_contract_replay(repo.path(), &cases);
}

fn fixture_result_edits(source: &str) -> (Vec<u32>, String) {
    let mut lines = Vec::new();
    let mut fixed = String::new();
    for (index, line) in source.lines().enumerate() {
        if let Some((call, fix)) = line.split_once("// unused result; then ") {
            lines.push(index as u32 + 1);
            let indent = &line[..line.len() - line.trim_start().len()];
            fixed.push_str(call.trim_end());
            fixed.push('\n');
            fixed.push_str(indent);
            fixed.push_str(fix);
        } else {
            fixed.push_str(line);
        }
        fixed.push('\n');
    }
    (lines, fixed)
}

#[test]
fn diff_subcategories_intersect_with_aspects() {
    if !ast_grep_available() {
        return;
    }
    let repo = init_repo(&[("property.js", "import * as fc from 'fast-check';\n")]);
    repo.commit(
        &[("property.js", "import * as fc from 'fast-check';\nfc.check(fc.property(fc.nat(), n => n < 100));\n")],
        "add discarded property check",
    );
    for (aspect, expected) in [("correctness", true), ("performance", false)] {
        let output = Command::cargo_bin("autoreview").unwrap()
            .current_dir(repo.path())
            .args(["diff", "--base", "HEAD~1", "--static-only", "--aspects", aspect,
                "--subcategories", "frontend,testing"])
            .assert().success().get_output().stdout.clone();
        let path = report_path_from_stdout(&String::from_utf8(output).unwrap());
        let report: autoreview_schema::ReviewReport = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(report.findings.iter().any(|finding| {
            finding.source.rule_id.as_deref() == Some("javascript-discarded-property-check")
        }), expected, "{aspect}");
    }
}

#[test]
fn rules_list_filters_builtin_rule_kinds_by_subcategory() {
    let repo = init_repo(&[("README.md", "rules\n")]);
    let output = Command::cargo_bin("autoreview").unwrap()
        .current_dir(repo.path())
        .args(["rules", "list", "--subcategories", "reliability,build,persistence.data-integrity,observability.metric-cardinality", "--json"])
        .assert().success().get_output().stdout.clone();
    let rules: Vec<serde_json::Value> = serde_json::from_slice(&output).unwrap();
    for (id, kind) in [
        ("go-discarded-context-cancel", "pattern"),
        ("go-context-cancel-on-all-paths", "framework"),
        ("github-actions-mutable-reference", "artifact"),
        ("java-spring-transaction-self-invocation", "framework"),
        ("rust-sqlx-pool-write-during-transaction", "rust"),
        ("ts-prom-client-high-cardinality-label", "pattern"),
    ] {
        assert!(rules.iter().any(|rule| rule["id"] == id && rule["kind"] == kind), "{id}");
    }
    assert!(rules.iter().all(|rule| rule["subcategory"].is_null()
        && rule["subcategories"].as_array().unwrap().iter().any(|label| {
            label.as_str().is_some_and(|label| label.starts_with("reliability.")
                || label.starts_with("build.")
                || matches!(label, "persistence.data-integrity" | "observability.metric-cardinality"))
        })));
}

#[test]
fn fetch_body_lifecycle_uses_the_same_rule_id_in_tsx_reports() {
    let path = "src/Widget.tsx";
    let before = r#"
export async function Widget() {
    const response = await fetch('/widget');
    await response.text();
    return <div />;
}
"#;
    let after = before.replace("    return <div />;", "    const alias = response;\n    await alias.json();\n    return <div />;");
    let repo = init_repo(&[(path, before)]);
    repo.commit(&[(path, &after)], "read the same body through an alias");
    let report = scoped_static_diff(repo.path(), "protocol.response-lifecycle");
    assert_eq!(report.findings.len(), 1);
    let found = &report.findings[0];
    assert_eq!(found.source.rule_id.as_deref(), Some("typescript-fetch-response-body-read-twice"));
    assert_eq!(found.location.snippet, "alias.json()");
    assert_eq!(found.related_locations.as_ref().unwrap()[0].snippet, "response.text()");
}

#[test]
fn outbox_policy_pairs_only_configured_operations_on_a_reachable_path() {
    let source_path = "src/orders.tsx";
    let source = r#"
import * as storage from './storage';
import { send as publish } from './broker';
import { enqueue } from './outbox';

export async function direct(values: Order) {
    const result = await storage.db.orders.update(values);
    if (result) await publish('orders.updated', result);
    return <section />;
}
export const reversed = async (values: Order) => {
    await publish('orders.updated', values);
    let result;
    result = await storage.db.orders.update(values);
    return result;
};
export async function atomic(values: Order) {
    await storage.db.orders.update(values);
    await enqueue('orders.updated', values);
}
export async function unrelated(values: Order) {
    await storage.db.orders.update(values);
    await publish('metrics.updated', values);
}
export async function either(shouldWrite: boolean, values: Order) {
    if (shouldWrite) {
        await storage.db.orders.update(values);
    } else {
        await publish('orders.updated', values);
    }
}
export async function stopped(values: Order) {
    await storage.db.orders.update(values);
    return values;
    await publish('orders.updated', values);
}
export async function shadowed(publish: Sender, values: Order) {
    await storage.db.orders.update(values);
    await publish('orders.updated', values);
}
"#;
    let repo = init_repo(&[(source_path, "// orders operation\n"), ("examples/orders.tsx", "")]);
    repo.commit(&[(source_path, source), ("examples/orders.tsx", source)], "add order operations");
    assert!(scoped_static_diff(repo.path(), "messaging.atomic-publication").findings.is_empty());

    write_artifact_config(repo.path(), r#"
artifactChecks:
  outboxOperations:
    - paths: ['src/*.tsx']
      databaseWrite:
        module: ./storage
        export: db
        members: [orders, update]
      brokerPublish:
        module: ./broker
        export: send
        firstArgument: orders.updated
"#);
    let report = scoped_static_diff(repo.path(), "messaging.atomic-publication");
    let expected = [(8, 7), (12, 14)];
    assert_eq!(report.findings.len(), expected.len(), "{:#?}", report.findings);
    for (finding, (publish, write)) in report.findings.iter().zip(expected) {
        assert_eq!(finding.source.rule_id.as_deref(), Some("outbox-direct-database-and-broker-write"));
        assert_eq!(finding.location.path, source_path);
        assert_eq!(finding.location.range.start_line, publish);
        let evidence = &finding.related_locations.as_ref().unwrap()[0];
        assert_eq!(evidence.path, source_path);
        assert_eq!(evidence.range.start_line, write);
    }
}

#[test]
fn tsx_metric_units_report_one_finding_for_each_distinct_mismatch() {
    if !ast_grep_available() {
        return;
    }
    let path = "instrumentation.tsx";
    let source = r#"import { metrics } from '@opentelemetry/api';
const meter = metrics.getMeter('service');
const duration = meter.createHistogram('http.server.request.duration', { unit: 's' });
const legacy = meter.createHistogram('http.client.request.duration', { unit: 'ms' });
const started = performance.now();
duration.record(performance.now() - started);
duration.record((performance.now() - started) / 1000);
legacy.record(performance.now() - started);
"#;
    let repo = init_repo(&[(path, "// instrumentation\n")]);
    repo.commit(&[(path, source)], "add metric recordings");
    let report = scoped_static_diff(repo.path(), "observability.metric-units");
    let mut actual: Vec<_> = report.findings.iter().map(|finding| (
        finding.location.range.start_line,
        finding.source.rule_id.as_deref().unwrap(),
    )).collect();
    actual.sort_unstable();
    assert_eq!(actual, [
        (4, "typescript-otel-http-duration-unit"),
        (6, "typescript-otel-http-duration-recorded-milliseconds"),
    ]);
}

fn scoped_static_diff(repo: &std::path::Path, subcategory: &str) -> autoreview_schema::ReviewReport {
    let output = Command::cargo_bin("autoreview").unwrap()
        .current_dir(repo)
        .args(["diff", "--base", "HEAD~1", "--static-only", "--changed-lines", "--subcategories", subcategory])
        .assert().success().get_output().stdout.clone();
    let report_path = report_path_from_stdout(&String::from_utf8(output).unwrap());
    serde_json::from_slice(&std::fs::read(report_path).unwrap()).unwrap()
}

fn write_artifact_config(root: &std::path::Path, yaml: &str) {
    let directory = root.join(".autoreview");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("config.yaml"), yaml).unwrap();
}

#[test]
fn icu_catalog_policy_reports_missing_arguments_and_nested_select_other() {
    let base = "locales/en.json";
    let translation = "locales/fr.json";
    let source = r#"{
  "inbox": {"message": "{count, plural, one{{user} sent one} other{{user} sent #}}"},
  "usage": "Type '{example}' for {name}"
}"#;
    let good_translation = r#"{
  "inbox": {"message": "{count, plural, one{{user} a envoyé un message} many{{user} en a envoyé #} other{{user} en a envoyé #}}"},
  "usage": "Saisir '{exemple}' pour {name}"
}"#;
    let lost = r#"{
  "inbox": {"message": "{count, plural, one{{sender, select, team{Équipe}}} other{Livré #}}"},
  "usage": "Saisir '{exemple}' pour {name}"
}"#;
    let repo = init_repo(&[(base, source), (translation, good_translation)]);
    repo.commit(&[(translation, lost)], "drop placeholders from translation");
    write_artifact_config(repo.path(),
        "artifactChecks:\n  localeCatalogs:\n    - reference: locales/en.json\n      translations: [locales/fr.json]\n");
    let report = scoped_static_diff(repo.path(), "internationalization");
    assert_eq!(report.findings.len(), 2);
    for finding in report.findings {
        assert_eq!(finding.location.path, translation);
        assert_eq!(finding.location.range.start_line, 2);
        match finding.source.rule_id.as_deref() {
            Some("icu-translation-missing-other-branch") => assert!(finding.message.contains("sender")),
            Some("icu-translation-drops-placeholder") => {
                assert!(finding.message.contains("user"));
                assert_eq!(finding.related_locations.unwrap()[0].path, base);
            }
            other => panic!("unexpected rule {other:?}"),
        }
    }
    let updated_reference = source.replace("{user} sent #", "{user} sent # to {team}");
    repo.commit(&[(base, &updated_reference)], "add a reference placeholder");
    let report = scoped_static_diff(repo.path(), "internationalization");
    assert_eq!(report.findings.len(), 1);
    let finding = &report.findings[0];
    assert_eq!(finding.source.rule_id.as_deref(), Some("icu-translation-drops-placeholder"));
    assert!(finding.message.contains("team"));
    assert_eq!(finding.location.path, translation);
    assert_eq!(finding.related_locations.as_ref().unwrap()[0].path, base);
}

#[test]
fn postgres_policy_tracks_changed_sql_and_transaction_sidecars() {
    let sql = "migration/V001.sql";
    let sidecar = "migration/V001.sql.conf";
    let statement = "/* keep */\ncreate UNIQUE index\nconcurrently idx on orders (created_at);\n";
    let repo = init_repo(&[
        (sql, statement),
        (sidecar, "executeInTransaction=false\n"),
        ("migration/V003.sql.conf", "executeInTransaction=false\n"),
        ("other/V002.sql", "SELECT 1;\n"),
    ]);
    repo.commit(&[
        (sidecar, "# overrides\nexecuteInTransaction=true\n"),
        ("other/V002.sql", "CREATE INDEX CONCURRENTLY idx ON table_name (column_name);\n"),
        ("migration/V003.sql", "CREATE INDEX CONCURRENTLY idx3 ON orders (created_at);\n"),
    ], "change the migration wrapping policy");
    for enabled in [false, true] {
        if enabled {
            write_artifact_config(repo.path(),
                "artifactChecks:\n  transactionalPostgresMigrations: ['migration/*.sql']\n");
        }
        let report = scoped_static_diff(repo.path(), "deployment.migrations");
        let findings: Vec<_> = report.findings.iter()
            .filter(|finding| finding.source.rule_id.as_deref() == Some("postgres-concurrent-index-in-transaction"))
            .collect();
        if enabled {
            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].location.path, sql);
            assert_eq!(findings[0].location.range.start_line, 2);
            assert_eq!(findings[0].related_locations.as_ref().unwrap()[0].path, sidecar);
        } else {
            assert!(findings.is_empty());
        }
    }
}

#[test]
fn protobuf_policy_tracks_schema_compatibility_in_the_git_diff() {
    use autoreview_schema::Side;

    let path = "schemas/message.proto";
    let previous = r#"
syntax = "proto3";
package example;
message Envelope {
  message Item {
    string legacy = 1;
    string remove_me = 2;
    repeated int32 values = 3;
    string safely_removed = 4;
    reserved 40 to 49;
  }
}
"#;
    let current = r#"
syntax = "proto3";
package example;
message Envelope {
  message Item {
    string replacement = 1;
    int32 values = 3;
    reserved 4, 41 to 49;
    string added = 5 [(example.hint) = "{; }"];
    string reused_reservation = 40;
  }
}
"#;
    let repo = init_repo(&[(path, previous), ("unconfigured.proto", previous)]);
    repo.commit(&[(path, current), ("unconfigured.proto", current)], "change message fields");

    for enabled in [false, true] {
        if enabled {
            write_artifact_config(repo.path(),
                "artifactChecks:\n  protobufCompatibility: ['schemas/*.proto']\n");
        }
        let report = scoped_static_diff(repo.path(), "compatibility");
        let expected = [
            ("protobuf-field-tag-reused", 6, Side::New),
            ("protobuf-incompatible-field-change", 7, Side::New),
            ("protobuf-field-tag-reused", 10, Side::New),
            ("protobuf-removed-field-tag-not-reserved", 7, Side::Old),
        ];
        assert_eq!(report.findings.len(), if enabled { expected.len() } else { 0 });
        for finding in report.findings {
            assert_eq!(finding.location.path, path);
            assert!(finding.message.contains("example.Envelope.Item"));
            assert!(expected.iter().any(|(rule, line, side)| {
                finding.source.rule_id.as_deref() == Some(rule)
                    && finding.location.range.start_line == *line
                    && finding.location.side == *side
            }), "{finding:?}");
        }
    }
}

#[test]
fn workflow_pinning_policy_is_opt_in_and_points_to_the_changed_steps() {
    let workflow = ".github/workflows/checks.yml";
    let baseline = "jobs:\n  test:\n    steps: []\n";
    let changed = "jobs:\n  test:\n    steps:\n      - uses: vendor/action@v2\n      - uses: other/task@0123456789abcdef0123456789abcdef01234567\n      - uses: './.github/actions/local'\n      - run: echo vendor/action@v2\n  reusable:\n    uses: vendor/project/.github/workflows/run.yml@main\n";
    let repo = init_repo(&[(workflow, baseline)]);
    repo.commit(&[(workflow, changed)], "add external actions");
    for enabled in [false, true] {
        if enabled {
            write_artifact_config(repo.path(),
                "artifactChecks:\n  immutableGithubActionRefs: true\n");
        }
        let report = scoped_static_diff(repo.path(), "build.dependency-integrity");
        let lines: Vec<_> = report.findings.iter()
            .filter(|finding| finding.source.rule_id.as_deref() == Some("github-actions-mutable-reference"))
            .map(|finding| finding.location.range.start_line).collect();
        if enabled {
            assert_eq!(lines, [4, 9]);
        } else {
            assert!(lines.is_empty());
        }
    }
}

#[test]
fn rules_packs_refresh_reports_a_freshly_cloned_git_pack() {
    let remote = tempfile::tempdir().unwrap();
    run_git(remote.path(), &["init", "-q"]);
    run_git(remote.path(), &["config", "user.email", "t@t.com"]);
    run_git(remote.path(), &["config", "user.name", "T"]);
    std::fs::write(remote.path().join("rulepack.yaml"), "id: acme-git-pack\nversion: \"1.0.0\"\n").unwrap();
    run_git(remote.path(), &["add", "-A"]);
    run_git(remote.path(), &["commit", "-q", "-m", "init"]);

    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
    std::fs::create_dir_all(repo.path().join(".autoreview")).unwrap();
    std::fs::write(
        repo.path().join(".autoreview/rulepacks.yaml"),
        format!("packs:\n  - id: acme-git-pack\n    source:\n      kind: git\n      url: {}\n", remote.path().display()),
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("autoreview").unwrap();
    cmd.current_dir(repo.path()).args(["rules", "packs", "refresh"]);
    cmd.assert().success().stdout(predicate::str::contains("acme-git-pack: cloned fresh at"));
}

#[test]
fn rules_packs_refresh_reports_no_git_packs_registered_when_none_configured() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
    let mut cmd = Command::cargo_bin("autoreview").unwrap();
    cmd.current_dir(repo.path()).args(["rules", "packs", "refresh"]);
    cmd.assert().success().stdout(predicate::str::contains("No git-source rule packs registered"));
}

#[test]
fn aspects_filter_scopes_out_complexity_findings_outside_the_requested_category() {
    let repo = init_repo_with_diff(
        &[("main.go", "package main\n\nfunc main() {}\n")],
        &[("main.go", "package main\n\nfunc doIt(a int, b int, c int, d int, e int, f int) {\n\t_ = a\n}\n\nfunc main() {\n\tdoIt(1, 2, 3, 4, 5, 6)\n}\n")],
        "add a function with 6 parameters",
    );

    let unrestricted = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path()).args(["diff", "--base", "main~1", "--head", "main", "--tier", "quick"]).assert().success();
    let unrestricted_stdout = String::from_utf8_lossy(&unrestricted.get_output().stdout);
    assert!(unrestricted_stdout.contains("Long parameter list"), "expected the design finding with no --aspects restriction, got:\n{unrestricted_stdout}");

    let restricted = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path()).args(["diff", "--base", "main~1", "--head", "main", "--tier", "quick", "--aspects", "security"]).assert().success();
    let restricted_stdout = String::from_utf8_lossy(&restricted.get_output().stdout);
    assert!(!restricted_stdout.contains("Long parameter list"), "--aspects security should scope out a design-only finding, got:\n{restricted_stdout}");
}

#[test]
fn diff_incremental_suppresses_findings_already_reported_in_the_previous_run() {
    if !ast_grep_available() {
        eprintln!("skipping: ast-grep not on PATH");
        return;
    }

    let repo = init_repo_with_diff(
        &[("main.go", "package main\n\nfunc main() {\n\tprintln(\"hello\")\n}\n")],
        &[("main.go", "package main\n\nfunc main() {\n\tx := 1\n\tif x == x {\n\t\tprintln(\"bug\")\n\t}\n}\n")],
        "seed a self-comparison bug",
    );

    // First run establishes the baseline in history — same fingerprint each
    // time since it's content-anchored, not run-id-anchored.
    let first = Command::cargo_bin("autoreview").unwrap().current_dir(repo.path()).args(["diff", "--base", "main~1", "--head", "main"]).assert().success();
    let first_stdout = String::from_utf8_lossy(&first.get_output().stdout);
    assert!(first_stdout.contains("go-no-self-comparison") || first_stdout.contains("Self Comparison"), "first run should report the seeded bug, got:\n{first_stdout}");

    let second = Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["diff", "--base", "main~1", "--head", "main", "--incremental"])
        .assert()
        .success();
    let second_stdout = String::from_utf8_lossy(&second.get_output().stdout);
    assert!(second_stdout.contains("[incremental] suppressed"), "second run should report the incremental suppression, got:\n{second_stdout}");

    let report_path = report_path_from_stdout(&second_stdout);
    let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    assert_eq!(report["findings"].as_array().unwrap().len(), 0, "the repeat finding should be suppressed, not re-reported");
    assert!(
        report["suppressed"].as_array().unwrap().iter().any(|s| s["reason"] == "baseline"),
        "suppressed list should record the baseline reason, got: {:#?}",
        report["suppressed"]
    );
}

#[test]
fn diff_tier_override_is_honored_regardless_of_computed_score() {
    if !ast_grep_available() {
        eprintln!("skipping: ast-grep not on PATH");
        return;
    }

    // This diff would score well above the quick-tier threshold on its own
    // (sensitive path + a real analyzer finding) — forcing --tier quick must
    // still take precedence over the heuristic.
    let repo = init_repo_with_diff(
        &[(
            "auth/login.go",
            "package auth\n\nfunc Login() bool {\n\treturn true\n}\n",
        )],
        &[(
            "auth/login.go",
            "package auth\n\nfunc Login() bool {\n\tx := true\n\treturn x == x\n}\n",
        )],
        "seed a bug in a sensitive path",
    );

    let mut cmd = Command::cargo_bin("autoreview").unwrap();
    cmd.current_dir(repo.path()).args([
        "diff", "--base", "main~1", "--head", "main", "--tier", "quick",
    ]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("-> tier: quick"))
        .stdout(predicate::str::contains("--tier=quick"));
}

#[test]
fn diff_aspects_override_limits_the_report_plan_to_the_requested_aspect() {
    let repo = init_repo_with_diff(
        &[(
            "auth/login.go",
            "package auth\n\nfunc Login() bool {\n\treturn true\n}\n",
        )],
        &[(
            "auth/login.go",
            "package auth\n\nfunc Login() bool {\n\treturn false\n}\n",
        )],
        "touch a sensitive path",
    );

    let mut cmd = Command::cargo_bin("autoreview").unwrap();
    cmd.current_dir(repo.path()).args([
        "diff",
        "--base",
        "main~1",
        "--head",
        "main",
        "--aspects",
        "security",
    ]);
    let assert = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);

    let report_path = report_path_from_stdout(&stdout);
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    let specialists = report["plan"]["specialists"].as_array().unwrap();
    assert!(
        specialists.iter().all(|s| s["aspect"] == "security"),
        "expected only 'security' in specialists, got: {specialists:#?}"
    );
}

#[test]
fn diff_on_a_repo_with_no_changes_produces_an_empty_but_valid_report() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);

    let mut cmd = Command::cargo_bin("autoreview").unwrap();
    cmd.current_dir(repo.path())
        .args(["diff", "--base", "main", "--head", "main"]);
    let assert = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);

    assert!(stdout.contains("files changed:  0"));
    assert!(
        !stdout.contains("-0.0"),
        "empty diff must not print a negative-zero triage score:\n{stdout}"
    );

    let report_path = report_path_from_stdout(&stdout);
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    assert_eq!(report["findings"].as_array().unwrap().len(), 0);
    assert_eq!(report["plan"]["tier"], "quick");
}

#[test]
fn diff_also_writes_a_markdown_report_alongside_the_json_one() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);

    let mut cmd = Command::cargo_bin("autoreview").unwrap();
    cmd.current_dir(repo.path())
        .args(["diff", "--base", "main", "--head", "main"]);
    let assert = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);

    let json_path = report_path_from_stdout(&stdout);
    let markdown_path = json_path.with_extension("md");
    assert!(markdown_path.exists(), "expected report.md alongside report.json at {}", markdown_path.display());
    let markdown = std::fs::read_to_string(&markdown_path).unwrap();
    assert!(markdown.contains("# Code Review Report"));
    assert!(markdown.contains("No findings"));

    let sarif_path = json_path.with_extension("sarif");
    assert!(sarif_path.exists(), "expected report.sarif alongside report.json at {}", sarif_path.display());
    let sarif: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&sarif_path).unwrap()).unwrap();
    assert_eq!(sarif["version"], "2.1.0");
    assert_eq!(sarif["runs"][0]["tool"]["driver"]["name"], "autoreview");

    let index_path = json_path.parent().unwrap().join("index.md");
    assert!(index_path.exists(), "expected index.md alongside report.json at {}", index_path.display());
    let index = std::fs::read_to_string(&index_path).unwrap();
    assert!(index.contains("# Review Index"));
    assert!(index.contains("No findings."));
}

#[test]
fn skills_list_reports_all_builtin_skills() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
    let mut cmd = Command::cargo_bin("autoreview").unwrap();
    cmd.current_dir(repo.path()).args(["skills", "list"]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("correctness"))
        .stdout(predicate::str::contains("security"))
        .stdout(predicate::str::contains("design"))
        .stdout(predicate::str::contains("performance"));
}

#[test]
fn rules_rollback_of_an_untracked_rule_fails_clearly_not_silently() {
    // The full promote/demote/reject state-machine behavior is unit-tested
    // directly in commands::rules (no subprocess round-trip needed there);
    // this just confirms the CLI wiring surfaces a real error instead of
    // succeeding silently or falling back to stub text.
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);

    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["rules", "rollback", "some-rule"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not tracked"));
}

#[test]
fn rules_review_reports_no_candidates_on_a_fresh_repo() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);

    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["rules", "review"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No candidates found"));
}

#[test]
fn rules_review_approve_fails_clearly_for_an_unknown_cluster() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);

    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["rules", "review", "--approve", "no-such-cluster"])
        .assert()
        .failure();
}

#[test]
fn rules_review_refuses_unbenched_candidates() {
    let repo = autoreview_test_support::init_repo(&[("README.md", "fixture\n")]);
    let candidate = repo.path().join(".autoreview/rules/candidates/candidate");
    std::fs::create_dir_all(&candidate).unwrap();
    std::fs::write(
        candidate.join("rule.yaml"),
        "id: example\nlanguage: Go\ncategory: correctness\nseverity: warning\nmessage: Example\nrule:\n  pattern: $A == $A\n",
    ).unwrap();
    Command::cargo_bin("autoreview").unwrap().current_dir(repo.path())
        .args(["rules", "review", "--approve", "candidate"])
        .assert().failure().stderr(predicates::str::contains("NeedsFixtures"));
    assert!(!repo.path().join(".autoreview/rules/shadow/example.yaml").exists());
}

#[test]
fn rules_mine_reports_no_findings_on_a_fresh_repo() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);

    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["rules", "mine"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to mine"));
}

#[test]
fn skills_mine_reports_nothing_to_mine_on_a_fresh_repo() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);

    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["skills", "mine"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to mine"));
}

#[test]
fn skills_bench_fails_clearly_when_no_proposal_exists() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);

    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["skills", "bench", "correctness", "no-such-proposal"])
        .assert()
        .failure();
}

#[test]
fn skills_review_reports_no_proposals_on_a_fresh_repo() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);

    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["skills", "review"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No skill-evolution proposals found"));
}

#[test]
fn skills_review_approve_fails_clearly_for_an_unknown_proposal() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);

    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["skills", "review", "--approve", "correctness:no-such-proposal"])
        .assert()
        .failure();
}

#[test]
fn history_sync_is_a_no_op_when_sync_mode_is_not_git() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);

    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["history", "sync"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to sync"));
}

#[test]
fn rules_bench_fails_clearly_when_no_rule_has_been_drafted() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);

    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["rules", "bench", "no-such-cluster"])
        .assert()
        .failure();
}

#[test]
fn rules_bench_reports_needs_fixtures_for_a_drafted_rule_with_no_test_files() {
    // `rules bench` runs the drafted rule through the real ast-grep
    // binary, so without it the command exits with a bare ENOENT instead
    // of reporting a verdict. Every other tool-dependent test here guards
    // the same way; this one did not, and it was the single failure that
    // took the macOS CI job red on its first ever run.
    if !ast_grep_available() {
        eprintln!("skipping: ast-grep not on PATH");
        return;
    }
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
    let candidate_dir = repo.path().join(".autoreview/rules/candidates/c1");
    std::fs::create_dir_all(&candidate_dir).unwrap();
    std::fs::write(candidate_dir.join("rule.yaml"), "id: go-example\nlanguage: Go\ncategory: correctness\nseverity: warning\nmessage: m\nrule:\n  pattern: $A == $A\n").unwrap();

    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["rules", "bench", "c1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("needs-fixtures"));
}

#[test]
fn apply_on_an_unknown_id_fails_with_a_clear_message() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["apply", "f-this-id-was-never-seen"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("No finding with id"));
}

#[test]
fn apply_on_a_real_finding_with_no_suggestion_says_so_and_does_not_error() {
    if !ast_grep_available() {
        eprintln!("skipping: ast-grep not on PATH");
        return;
    }

    let repo = init_repo_with_diff(
        &[("main.go", "package main\n\nfunc main() {\n\tprintln(\"hello\")\n}\n")],
        &[("main.go", "package main\n\nfunc main() {\n\tx := 1\n\tif x == x {\n\t\tprintln(\"bug\")\n\t}\n}\n")],
        "seed a self-comparison bug",
    );

    let diff_assert = Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["diff", "--base", "main~1", "--head", "main"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&diff_assert.get_output().stdout);
    let report_path = report_path_from_stdout(&stdout);
    let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    let finding_id = report["findings"][0]["id"].as_str().expect("expected at least one finding with an id").to_string();

    // ast-grep findings don't currently carry a suggestion, so this exercises
    // the "found the finding, nothing to apply" path rather than a real patch.
    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["apply", &finding_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("has no suggested fix"));
}

#[test]
fn feedback_without_fp_tp_or_missed_is_a_usage_error() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["feedback", "f-abc123"])
        .assert()
        .failure();
}

#[test]
fn feedback_on_an_unknown_id_fails_with_a_clear_message() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["feedback", "f-this-id-was-never-seen", "--fp"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("No finding with id"));
}

#[test]
fn feedback_missed_is_recorded_even_without_a_matching_finding() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["feedback", "deadbeef", "--missed", "should have flagged the missing nil check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Recorded missed-finding report"));
}

#[test]
fn feedback_fp_on_a_real_finding_from_a_prior_diff_run_succeeds() {
    if !ast_grep_available() {
        eprintln!("skipping: ast-grep not on PATH");
        return;
    }

    let repo = init_repo_with_diff(
        &[("main.go", "package main\n\nfunc main() {\n\tprintln(\"hello\")\n}\n")],
        &[("main.go", "package main\n\nfunc main() {\n\tx := 1\n\tif x == x {\n\t\tprintln(\"bug\")\n\t}\n}\n")],
        "seed a self-comparison bug",
    );

    let diff_assert = Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["diff", "--base", "main~1", "--head", "main"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&diff_assert.get_output().stdout);
    let report_path = report_path_from_stdout(&stdout);
    let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    let finding_id = report["findings"][0]["id"].as_str().expect("expected at least one finding with an id").to_string();

    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["feedback", &finding_id, "--fp", "--note", "acceptable pattern here"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Recorded 'false positive' feedback"));
}

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
}

#[test]
fn skills_list_reports_the_three_builtin_skills() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
    let mut cmd = Command::cargo_bin("autoreview").unwrap();
    cmd.current_dir(repo.path()).args(["skills", "list"]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("correctness"))
        .stdout(predicate::str::contains("security"))
        .stdout(predicate::str::contains("design"));
}

#[test]
fn rules_is_an_explicit_stub_not_silent_or_erroring() {
    let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);

    Command::cargo_bin("autoreview")
        .unwrap()
        .current_dir(repo.path())
        .args(["rules", "mine"])
        .assert()
        .success()
        .stdout(predicate::str::contains("not implemented yet"));
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

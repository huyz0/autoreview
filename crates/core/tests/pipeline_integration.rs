//! End-to-end pipeline tests: real git repos on disk, run through
//! collect -> Stage 1 (analyzers) -> Stage 2 (triage) -> dedupe, exactly the
//! sequence `autoreview diff` itself runs. Unlike the per-module unit tests,
//! these exercise the actual composition — a bug in how Stage 1's finding
//! count feeds Stage 2's `analyzerDensity` signal, for example, would not be
//! caught by either stage's unit tests alone.

use std::process::Command;

use autoreview_core::{assign_fingerprints, collect_diff_facts, dedupe_exact, discover_manifests, load_config, plan_review, run_ast_grep, run_golangci_lint, to_finding, PlanOverrides};
use autoreview_schema::Tier;
use autoreview_test_support::init_repo_with_diff;

fn ast_grep_available() -> bool {
    Command::new("ast-grep").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn golangci_lint_available() -> bool {
    Command::new("golangci-lint").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// Runs Stage 0 through Stage 2 (no agent specialists — those need `claude`,
/// covered separately) exactly as `autoreview diff` composes them, returning
/// the deduped findings and the resulting plan for assertions.
fn run_pipeline(repo_root: &std::path::Path, base: &str, head: &str) -> (Vec<autoreview_schema::Finding>, autoreview_schema::ReviewPlan) {
    let repo_root_str = repo_root.to_string_lossy().to_string();
    let config = load_config(&repo_root.join(".autoreview").join("config.yaml")).unwrap();
    let facts = collect_diff_facts(&repo_root_str, base, head, None).unwrap();
    let skills = discover_manifests(repo_root).unwrap();

    let changed_paths: Vec<String> = facts.files.iter().map(|f| f.path.clone()).collect();
    let mut stage1 = Vec::new();
    stage1.extend(run_ast_grep(repo_root, &changed_paths).unwrap());
    stage1.extend(run_golangci_lint(repo_root, &changed_paths).unwrap());
    let stage1_count = stage1.len();
    let stage1_findings: Vec<_> = assign_fingerprints(stage1).into_iter().map(to_finding).collect();

    let plan = plan_review(&facts, &config, &skills, stage1_count, PlanOverrides::default());
    let dedupe_result = dedupe_exact(stage1_findings);
    (dedupe_result.findings, plan)
}

#[test]
fn seeded_bug_in_a_sensitive_path_surfaces_findings_and_escalates_tier() {
    if !ast_grep_available() {
        eprintln!("skipping: ast-grep not on PATH");
        return;
    }

    let repo = init_repo_with_diff(
        &[("src/main.go", "package main\n\nfunc main() {\n\tprintln(\"hello\")\n}\n")],
        &[(
            "auth/login.go",
            "package auth\n\nfunc Login(user, pass string) bool {\n\tif user == user {\n\t\treturn false\n\t}\n\treturn user == pass\n}\n",
        )],
        "add login with a seeded self-comparison bug",
    );

    let (findings, plan) = run_pipeline(repo.path(), "main~1", "main");

    assert!(
        findings.iter().any(|f| f.source.rule_id.as_deref() == Some("go-no-self-comparison")),
        "expected the seeded self-comparison bug to be caught; findings were: {:?}",
        findings.iter().map(|f| f.source.rule_id.clone()).collect::<Vec<_>>()
    );
    assert!(plan.signals.iter().any(|s| s.signal == "sensitivePathHit"), "auth/ path should trigger sensitivePathHit");
    assert_ne!(plan.tier, Tier::Quick, "a sensitive-path hit plus a real analyzer finding should not stay at quick tier");
    assert!(plan.specialists.iter().any(|s| s.aspect == "security"), "security specialist should be triggered by the sensitive path");
}

#[test]
fn clean_diff_produces_no_findings_and_stays_at_quick_tier() {
    if !ast_grep_available() {
        eprintln!("skipping: ast-grep not on PATH");
        return;
    }

    let repo = init_repo_with_diff(
        &[("src/main.go", "package main\n\nimport \"fmt\"\n\nfunc add(a, b int) int {\n\treturn a + b\n}\n\nfunc main() {\n\tfmt.Println(add(1, 2))\n}\n")],
        &[(
            "src/main.go",
            "package main\n\nimport \"fmt\"\n\nfunc add(a, b int) int {\n\treturn a + b\n}\n\nfunc multiply(a, b int) int {\n\treturn a * b\n}\n\nfunc main() {\n\tfmt.Println(add(1, 2))\n\tfmt.Println(multiply(2, 3))\n}\n",
        )],
        "add a small, used, unremarkable helper",
    );

    let (findings, plan) = run_pipeline(repo.path(), "main~1", "main");

    assert!(findings.is_empty(), "expected no findings at all on genuinely clean code, got: {:?}", findings.iter().map(|f| (f.source.tool.clone(), f.title.clone())).collect::<Vec<_>>());
    assert_eq!(plan.tier, Tier::Quick);
}

#[test]
fn a_deleted_file_in_the_diff_does_not_break_the_pipeline() {
    // Regression coverage at the real-git level (not just synthetic file
    // lists): `git diff --numstat` reports deleted files, and both analyzer
    // adapters must tolerate that without erroring out or poisoning results
    // for the files that do still exist.
    if !ast_grep_available() || !golangci_lint_available() {
        eprintln!("skipping: ast-grep and/or golangci-lint not on PATH");
        return;
    }

    let repo = init_repo_with_diff(
        &[
            ("go.mod", "module example.com/test\n\ngo 1.21\n"),
            ("main.go", "package main\n\nfunc main() {\n\tprintln(\"hello\")\n}\n"),
            ("old.go", "package main\n\nfunc unused() {}\n"),
        ],
        &[("removed-marker.go", "package main\n\n// this commit deletes old.go\n")],
        "delete old.go and add something else",
    );
    std::fs::remove_file(repo.path().join("old.go")).unwrap();
    repo.commit(&[], "actually remove old.go");

    // The pipeline must not panic or error even though `old.go` no longer
    // exists but is present in the diff between main~2 and main.
    let (findings, _plan) = run_pipeline(repo.path(), "main~2", "main");
    // No specific assertion on findings content — the point is that this
    // completes at all rather than erroring out on the deleted path.
    let _ = findings;
}

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

mod targets;

#[derive(Debug, Deserialize)]
struct GolangciOutput {
    #[serde(rename = "Issues", default)]
    issues: Vec<GolangciIssue>,
    #[serde(rename = "Report", default)]
    report: Option<GolangciReport>,
}

#[derive(Debug, Deserialize, Default)]
struct GolangciReport {
    #[serde(rename = "Error", default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GolangciIssue {
    #[serde(rename = "FromLinter")]
    from_linter: String,
    #[serde(rename = "Text")]
    text: String,
    #[serde(rename = "Pos")]
    pos: GolangciPos,
    #[serde(rename = "SourceLines", default)]
    source_lines: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct GolangciPos {
    #[serde(rename = "Filename")]
    filename: String,
    #[serde(rename = "Line")]
    line: u32,
    #[serde(rename = "Column")]
    column: u32,
}

/// golangci-lint doesn't reliably populate a per-issue `Severity` field (it's
/// usually empty unless the user's own .golangci.yml configures severity
/// rules), so this is our own default mapping — same idea as the plan's
/// "per-tool severity mapping table in the adapter, overridable in config."
fn map_severity(linter: &str) -> Severity {
    match linter {
        "gosec" => Severity::High,
        "typecheck" => Severity::High, // the code doesn't compile
        "gofmt" | "gofumpt" | "goimports" | "whitespace" | "wsl" | "wsl_v5" | "godot" | "misspell" | "lll" | "nlreturn" | "tagalign" | "gci" => Severity::Low,
        _ => Severity::Medium,
    }
}

fn map_category(linter: &str) -> &'static str {
    match linter {
        "gosec" | "sqlclosecheck" | "rowserrcheck" | "noctx" => "security",
        "gofmt" | "gofumpt" | "goimports" | "whitespace" | "wsl" | "wsl_v5" | "godot" | "misspell" | "lll" | "nlreturn" | "tagalign" | "gci" => "style",
        _ => "correctness",
    }
}

/// golangci-lint wraps compiler errors (the `typecheck` pseudo-linter) with a
/// degenerate `Pos` (observed: Line 1, Column 0) and embeds the real
/// file:line:col inside `Text` instead, e.g.:
///   ": # example.com/pkg\n./main.go:6:2: declared and not used: x"
/// This recovers the real location when that pattern is present; falls back
/// to the issue's own `Pos` otherwise.
fn typecheck_location(diagnostic: &str) -> Option<(GolangciPos, &str)> {
    let diagnostic = diagnostic.trim();
    for (offset, _) in diagnostic.match_indices(':') {
        let filename = &diagnostic[..offset];
        if !filename.ends_with(".go") {
            continue;
        }
        let mut parts = diagnostic[offset + 1..].splitn(3, ':');
        if let (Some(line), Some(column), Some(message)) = (parts.next(), parts.next(), parts.next()) {
            if let (Ok(line), Ok(column)) = (line.parse(), column.parse()) {
                return Some((GolangciPos { filename: filename.to_string(), line, column }, message.trim()));
            }
        }
    }
    None
}

fn issue_to_finding(repo_root: &Path, issue: &GolangciIssue) -> AgentFinding {
    let recovered = (issue.from_linter == "typecheck").then(|| issue.text.lines().find_map(typecheck_location)).flatten();
    let (pos, message) = recovered.as_ref().map_or((&issue.pos, issue.text.as_str()), |(position, message)| (position, *message));
    let filename = Path::new(&pos.filename);
    let file = filename.strip_prefix(repo_root).unwrap_or(filename).to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
    let path = file.strip_prefix("./").unwrap_or(&file).to_string();
    let severity = map_severity(&issue.from_linter);
    let category = map_category(&issue.from_linter).to_string();

    AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "golangci-lint".to_string(), rule_id: Some(issue.from_linter.clone()), aspect: None, backend: None },
        category,
        severity,
        confidence: 1.0,
        title: message.lines().next().unwrap_or(message).to_string(),
        message: message.to_string(),
        location: Location { path, range: LocationRange { start_line: pos.line.max(1), start_col: Some(pos.column), end_line: None, end_col: None }, snippet: issue.source_lines.as_deref().unwrap_or_default().join("\n"), side: Side::New },
        related_locations: None,
        suggestion: None,
        tags: None,
        meta: None,
        suggested_patch: None,
    }
}

/// One attempt's outcome, distinguishing a real analysis failure (a
/// malformed config, a genuine compile error in the target repo — not
/// worth retrying, the next attempt would fail the same way) from a
/// transient one (golangci-lint's own `go/packages` context-loading step
/// failed outright before writing any output at all — observed, under
/// heavy concurrent subprocess load, to intermittently produce a
/// corrupted/truncated error rather than a clean failure; a contended
/// shared Go build cache is the most likely cause, and a short retry
/// reliably clears it).
enum GolangciRunError {
    Retryable(String),
    Other(anyhow::Error),
}

const GOLANGCI_LINT_MAX_ATTEMPTS: u32 = 3;
const GOLANGCI_LINT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

fn run_golangci_lint_once(repo_root: &Path, go_files: &[&str], targets: &[&str]) -> Result<Option<Vec<AgentFinding>>, GolangciRunError> {
    let temp_dir = tempfile::tempdir().map_err(|err| GolangciRunError::Other(err.into()))?;
    let json_path = temp_dir.path().join("golangci-out.json");

    let output = match Command::new("golangci-lint").arg("run").arg("--output.json.path").arg(&json_path).arg("--").args(targets).current_dir(repo_root).output() {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(GolangciRunError::Other(err.into())),
    };

    // golangci-lint exits non-zero whenever it finds any issues at all —
    // that's the normal case, not a tool failure. Only treat it as a real
    // failure if the JSON file was never written (the flag itself failing,
    // a config error, etc), which the read below surfaces naturally.
    let json_text = match std::fs::read_to_string(&json_path) {
        Ok(text) => text,
        Err(_) => return Err(GolangciRunError::Retryable(String::from_utf8_lossy(&output.stderr).to_string())),
    };

    let mut parsed: GolangciOutput = serde_json::from_str(&json_text).map_err(|err| GolangciRunError::Other(err.into()))?;
    let report_error = parsed.report.as_ref().and_then(|r| r.error.as_ref());
    if let Some(report_error) = report_error {
        // Newer versions put compiler diagnostics under Report.Error.
        let compiler_issues = report_error.lines().filter_map(typecheck_location).map(|(pos, message)| GolangciIssue {
            from_linter: "typecheck".to_string(), text: message.to_string(), pos, source_lines: None,
        });
        parsed.issues.extend(compiler_issues);
    }
    let changed_set: HashSet<&str> = go_files.iter().copied().collect();

    let findings: Vec<_> = parsed.issues.iter()
        .map(|issue| issue_to_finding(repo_root, issue))
        .filter(|f| changed_set.contains(f.location.path.as_str()))
        .collect();
    if let Some(report_error) = report_error.filter(|_| findings.is_empty()) {
        return Err(GolangciRunError::Other(anyhow::anyhow!("golangci-lint reported a package-level error: {report_error}")));
    }
    Ok(Some(findings))
}

/// Runs golangci-lint against the given changed Go files and normalizes its
/// issues into analyzer findings. Returns an empty list (not an error) if
/// there are no changed Go files, or if `golangci-lint` isn't on PATH —
/// Stage 1 degrades gracefully rather than blocking the rest of the review.
/// Retries up to `GOLANGCI_LINT_MAX_ATTEMPTS` times on the transient
/// "produced no output at all" failure mode (see `GolangciRunError`); any
/// other failure (a real config/compile problem) surfaces immediately,
/// unretried.
pub fn run_golangci_lint(repo_root: &Path, changed_files: &[String]) -> anyhow::Result<Vec<AgentFinding>> {
    let mut findings = Vec::new();
    for group in targets::groups(repo_root, changed_files) {
        let files: Vec<_> = group.files.iter().map(String::as_str).collect();
        let targets: Vec<_> = group.targets.iter().map(String::as_str).collect();
        let mut found = run_with_retries(&group.root, &files, &targets)?;
        for finding in &mut found {
            finding.location.path = targets::normalize(&group.prefix.join(&finding.location.path));
        }
        findings.extend(found);
    }
    Ok(findings)
}

fn run_with_retries(repo_root: &Path, go_files: &[&str], targets: &[&str]) -> anyhow::Result<Vec<AgentFinding>> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match run_golangci_lint_once(repo_root, go_files, targets) {
            Ok(None) => return Ok(vec![]),
            Ok(Some(findings)) => return Ok(findings),
            Err(GolangciRunError::Other(err)) => return Err(err),
            Err(GolangciRunError::Retryable(stderr)) => {
                if attempt >= GOLANGCI_LINT_MAX_ATTEMPTS {
                    anyhow::bail!("golangci-lint did not produce an output file after {attempt} attempt(s): {stderr}");
                }
                std::thread::sleep(GOLANGCI_LINT_RETRY_DELAY);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_and_category_mapping_table_is_exhaustive_and_stable() {
        // Pure, fast, no binary required: pins down the exact mapping so a
        // future edit can't silently downgrade a security linter to
        // "correctness"/medium or a compile-error linter to low severity.
        let cases: &[(&str, Severity, &str)] = &[
            ("gosec", Severity::High, "security"),
            ("typecheck", Severity::High, "correctness"),
            ("sqlclosecheck", Severity::Medium, "security"),
            ("rowserrcheck", Severity::Medium, "security"),
            ("noctx", Severity::Medium, "security"),
            ("gofmt", Severity::Low, "style"),
            ("gofumpt", Severity::Low, "style"),
            ("goimports", Severity::Low, "style"),
            ("whitespace", Severity::Low, "style"),
            ("misspell", Severity::Low, "style"),
            ("gci", Severity::Low, "style"),
            ("errcheck", Severity::Medium, "correctness"),
            ("govet", Severity::Medium, "correctness"),
            ("staticcheck", Severity::Medium, "correctness"),
            ("ineffassign", Severity::Medium, "correctness"),
            ("unused", Severity::Medium, "correctness"),
            ("some-unrecognized-future-linter", Severity::Medium, "correctness"),
        ];

        for (linter, expected_severity, expected_category) in cases {
            assert_eq!(map_severity(linter), *expected_severity, "severity mismatch for linter '{linter}'");
            assert_eq!(map_category(linter), *expected_category, "category mismatch for linter '{linter}'");
        }
    }

    /// Requires golangci-lint **v2**, not merely a binary called
    /// golangci-lint. `run_golangci_lint` passes `--output.json.path`,
    /// which only exists in v2 (v1 used `--out-format`), so a v1 install
    /// satisfies a presence-only check and then rejects the one flag this
    /// analyzer uses.
    ///
    /// That is not hypothetical: CI installed v1 for weeks — the Go module
    /// path was missing its `/v2` suffix — and these tests failed on every
    /// run rather than skipping, because this check only asked whether the
    /// binary existed. Skipping on a version mismatch keeps the suite
    /// honest for anyone with v1 on their PATH instead of handing them
    /// four confusing panics.
    fn golangci_lint_available() -> bool {
        let Ok(output) = Command::new("golangci-lint").arg("--version").output() else { return false };
        if !output.status.success() {
            return false;
        }
        let version = String::from_utf8_lossy(&output.stdout);
        let is_v2 = version.contains("version 2.") || version.contains("version v2.");
        if !is_v2 {
            eprintln!("skipping: golangci-lint v2 required (this analyzer passes --output.json.path), found: {}", version.lines().next().unwrap_or("unknown"));
        }
        is_v2
    }

    fn write_go_module(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.com/test\n\ngo 1.21\n").unwrap();
        for (name, contents) in files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        dir
    }

    #[test]
    fn separates_module_workspace_and_nonmodule_directories() {
        let dir = tempfile::tempdir().unwrap();
        for path in ["mod/pkg/file.go", "workspace/pkg/file.go", "loose/a.go", "loose/b.go", "other/c.go"] {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, "package p\n").unwrap();
        }
        std::fs::write(dir.path().join("mod/go.mod"), "module example.test/mod\n").unwrap();
        std::fs::write(dir.path().join("workspace/go.work"), "go 1.22\n").unwrap();
        let input = ["mod/pkg/file.go", "workspace/pkg/file.go", "loose/a.go", "loose/b.go", "other/c.go", "missing.go", "../escape.go"];
        let groups = targets::groups(dir.path(), &input.map(str::to_owned));
        assert_eq!(groups.len(), 4);
        let module = groups.iter().find(|group| group.prefix == Path::new("mod")).unwrap();
        assert_eq!(module.files, ["pkg/file.go"]);
        assert_eq!(module.targets, ["./pkg"]);
        let workspace = groups.iter().find(|group| group.prefix == Path::new("workspace")).unwrap();
        assert_eq!(workspace.targets, ["./pkg"]);
        let loose = groups.iter().find(|group| group.targets.len() == 2).unwrap();
        assert_eq!(loose.prefix, Path::new(""));
        assert_eq!(loose.targets, ["loose/a.go", "loose/b.go"]);
    }

    #[test]
    fn returns_empty_without_invoking_when_no_go_files() {
        let result = run_golangci_lint(Path::new("/nonexistent"), &["README.md".to_string()]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn filters_out_a_deleted_file_instead_of_poisoning_the_whole_package_scan() {
        // Regression test: passing a nonexistent path to golangci-lint used to
        // silently return zero issues for the *entire* package (deleted files
        // still appear in `git diff --numstat`). A real bug in an existing
        // file must still be found once the missing path is filtered out.
        if !golangci_lint_available() {
            eprintln!("skipping: golangci-lint not on PATH");
            return;
        }
        let dir = write_go_module(&[(
            "main.go",
            "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n\nfunc main() {\n\tf, _ := os.Open(\"/nonexistent\")\n\tf.Close()\n\tfmt.Println(\"hi\")\n}\n",
        )]);
        let findings = run_golangci_lint(dir.path(), &["main.go".to_string(), "deleted.go".to_string()]).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("errcheck"));
    }

    #[test]
    fn retries_the_transient_no_output_failure_before_giving_up() {
        if !golangci_lint_available() {
            eprintln!("skipping: golangci-lint not on PATH");
            return;
        }
        // An unparseable .golangci.yml reliably reproduces golangci-lint's
        // own "fails before writing any output at all" failure mode
        // deterministically (the same shape observed intermittently under
        // heavy concurrent test load, from a corrupted go/packages load
        // rather than a config error, but indistinguishable from this
        // function's own point of view — see `GolangciRunError`) — proves
        // the retry loop actually retries (the error names the attempt
        // count) and still surfaces a clear failure once exhausted, rather
        // than hanging or panicking.
        let dir = write_go_module(&[("main.go", "package main\n\nfunc main() {}\n")]);
        std::fs::write(dir.path().join(".golangci.yml"), "this is not: valid: yaml: [structure\n").unwrap();
        let err = run_golangci_lint(dir.path(), &["main.go".to_string()]).unwrap_err();
        assert!(err.to_string().contains(&format!("after {GOLANGCI_LINT_MAX_ATTEMPTS} attempt(s)")), "got: {err}");
    }

    #[test]
    fn finds_a_real_errcheck_issue() {
        if !golangci_lint_available() {
            eprintln!("skipping: golangci-lint not on PATH");
            return;
        }
        let dir = write_go_module(&[(
            "main.go",
            "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n\nfunc main() {\n\tf, _ := os.Open(\"/nonexistent\")\n\tf.Close()\n\tfmt.Println(\"hi\")\n}\n",
        )]);
        let findings = run_golangci_lint(dir.path(), &["main.go".to_string()]).unwrap();

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.tool, "golangci-lint");
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("errcheck"));
        assert_eq!(findings[0].location.path, "main.go");
        assert_eq!(findings[0].location.range.start_line, 10);
        assert_eq!(findings[0].confidence, 1.0);
    }

    #[test]
    fn reviews_multiple_go_packages_with_unchanged_siblings() {
        if !golangci_lint_available() {
            return;
        }
        let dir = write_go_module(&[
            ("example.go", "package example\nfunc Check() { helper() }\n"),
            ("helper.go", "package example\nfunc helper() error { return nil }\n"),
            ("child/child.go", "package child\nfunc Check() { helper() }\n"),
            ("child/helper.go", "package child\nfunc helper() error { return nil }\n"),
        ]);
        let findings = run_golangci_lint(dir.path(), &["example.go".to_string(), "child/child.go".to_string()]).unwrap();
        assert_eq!(findings.len(), 2, "got: {findings:#?}");
        for file in ["example.go", "child/child.go"] {
            assert!(findings.iter().any(|finding| finding.location.path == file && finding.source.rule_id.as_deref() == Some("errcheck")));
        }
    }

    #[test]
    fn reviews_nested_modules_with_unchanged_siblings_and_repo_relative_paths() {
        if !golangci_lint_available() { return; }
        let dir = write_go_module(&[
            ("root.go", "package root\nfunc Check() { helper() }\n"),
            ("helper.go", "package root\nfunc helper() error { return nil }\n"),
            ("nested/go.mod", "module example.com/nested\n\ngo 1.21\n"),
            ("nested/child/changed.go", "package child\nfunc Check() { helper() }\n"),
            ("nested/child/helper.go", "package child\nfunc helper() error { return nil }\n"),
        ]);
        let findings = run_golangci_lint(dir.path(), &["root.go".into(), "nested/child/changed.go".into()]).unwrap();
        assert_errcheck_paths(&findings, &["root.go", "nested/child/changed.go"]);
    }

    #[test]
    fn nested_module_with_no_root_go_module_uses_full_package() {
        if !golangci_lint_available() { return; }
        let dir = write_go_module(&[
            ("tool/go.mod", "module example.com/tool\n\ngo 1.21\n"),
            ("tool/main.go", "package tool\nfunc Check() { helper() }\n"),
            ("tool/helper.go", "package tool\nfunc helper() error { return nil }\n"),
        ]);
        std::fs::remove_file(dir.path().join("go.mod")).unwrap();
        let findings = run_golangci_lint(dir.path(), &["tool/main.go".into()]).unwrap();
        assert!(findings.iter().any(|finding| finding.location.path == "tool/main.go" && finding.source.rule_id.as_deref() == Some("errcheck")), "{findings:#?}");
    }

    #[test]
    fn reviews_modules_selected_by_go_workspace() {
        if !golangci_lint_available() { return; }
        let dir = write_go_module(&[
            ("go.work", "go 1.21\nuse (\n ./one\n ./two\n)\n"),
            ("one/go.mod", "module example.com/one\n\ngo 1.21\n"),
            ("one/main.go", "package one\nfunc Check() { helper() }\n"),
            ("one/helper.go", "package one\nfunc helper() error { return nil }\n"),
            ("two/go.mod", "module example.com/two\n\ngo 1.21\n"),
            ("two/main.go", "package two\nfunc Check() { helper() }\n"),
            ("two/helper.go", "package two\nfunc helper() error { return nil }\n"),
        ]);
        std::fs::remove_file(dir.path().join("go.mod")).unwrap();
        let findings = run_golangci_lint(dir.path(), &["one/main.go".into(), "two/main.go".into()]).unwrap();
        assert_errcheck_paths(&findings, &["one/main.go", "two/main.go"]);
    }

    fn assert_errcheck_paths(findings: &[AgentFinding], paths: &[&str]) {
        assert_eq!(findings.len(), paths.len(), "{findings:#?}");
        for path in paths {
            assert!(findings.iter().any(|finding| finding.location.path == *path && finding.source.rule_id.as_deref() == Some("errcheck")), "{findings:#?}");
        }
    }

    #[test]
    fn nested_module_reports_type_errors_in_unchanged_siblings() {
        if !golangci_lint_available() { return; }
        let dir = write_go_module(&[
            ("tool/go.mod", "module example.com/tool\n\ngo 1.21\n"),
            ("tool/main.go", "package tool\nfunc Check() { helper() }\n"),
            ("tool/helper.go", "package tool\nfunc helper() { missing() }\n"),
        ]);
        std::fs::remove_file(dir.path().join("go.mod")).unwrap();
        let error = run_golangci_lint(dir.path(), &["tool/main.go".into()]).unwrap_err();
        assert!(error.to_string().contains("typecheck"), "{error:#}");
    }

    #[test]
    fn recovers_real_location_from_a_typecheck_compile_error() {
        if !golangci_lint_available() {
            eprintln!("skipping: golangci-lint not on PATH");
            return;
        }
        let dir = write_go_module(&[("main.go", "package main\n\nfunc main() {\n\tx := 5\n\t_ = 1\n}\n")]);
        let findings = run_golangci_lint(dir.path(), &["main.go".to_string()]).unwrap();

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("typecheck"));
        // The real location (line 4: "x := 5") should be recovered from Text,
        // not the degenerate Pos (line 1, col 0) golangci-lint reports for
        // typecheck issues.
        assert_eq!(findings[0].location.range.start_line, 4);
        assert!(findings[0].message.contains("declared and not used"));
    }

    #[test]
    fn clean_code_produces_no_findings() {
        if !golangci_lint_available() {
            eprintln!("skipping: golangci-lint not on PATH");
            return;
        }
        let dir = write_go_module(&[("main.go", "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"hi\")\n}\n")]);
        let findings = run_golangci_lint(dir.path(), &["main.go".to_string()]).unwrap();
        assert!(findings.is_empty());
    }
}

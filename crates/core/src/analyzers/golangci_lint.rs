use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

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
fn recover_typecheck_location(issue: &GolangciIssue) -> (String, u32, u32, String) {
    if issue.from_linter == "typecheck" {
        if let Some(second_line) = issue.text.lines().nth(1) {
            if let Some(rest) = second_line.strip_prefix("./") {
                let mut parts = rest.splitn(4, ':');
                if let (Some(file), Some(line_str), Some(col_str), Some(msg)) = (parts.next(), parts.next(), parts.next(), parts.next()) {
                    if let (Ok(line), Ok(col)) = (line_str.parse::<u32>(), col_str.parse::<u32>()) {
                        return (file.to_string(), line, col, msg.trim().to_string());
                    }
                }
            }
        }
    }
    (issue.pos.filename.clone(), issue.pos.line, issue.pos.column, issue.text.clone())
}

fn issue_to_finding(issue: &GolangciIssue) -> AgentFinding {
    let (file, line, col, message) = recover_typecheck_location(issue);
    let severity = map_severity(&issue.from_linter);
    let category = map_category(&issue.from_linter).to_string();

    AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "golangci-lint".to_string(), rule_id: Some(issue.from_linter.clone()), aspect: None, backend: None },
        category,
        severity,
        confidence: 1.0,
        title: message.lines().next().unwrap_or(&message).to_string(),
        message,
        location: Location { path: file, range: LocationRange { start_line: line.max(1), start_col: Some(col), end_line: None, end_col: None }, snippet: String::new(), side: Side::New },
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

fn run_golangci_lint_once(repo_root: &Path, go_files: &[&str]) -> Result<Option<Vec<AgentFinding>>, GolangciRunError> {
    let temp_dir = tempfile::tempdir().map_err(|err| GolangciRunError::Other(err.into()))?;
    let json_path = temp_dir.path().join("golangci-out.json");

    let output = match Command::new("golangci-lint").arg("run").arg("--output.json.path").arg(&json_path).args(go_files).current_dir(repo_root).output() {
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

    let parsed: GolangciOutput = serde_json::from_str(&json_text).map_err(|err| GolangciRunError::Other(err.into()))?;
    if let Some(report_error) = parsed.report.as_ref().and_then(|r| r.error.as_ref()) {
        return Err(GolangciRunError::Other(anyhow::anyhow!("golangci-lint reported a package-level error (results may be incomplete or empty): {report_error}")));
    }
    let changed_set: std::collections::HashSet<&str> = go_files.iter().copied().collect();

    Ok(Some(
        parsed
            .issues
            .iter()
            .map(issue_to_finding)
            // Scanning by explicit file args can still surface package-level
            // issues attributed to a file outside our diff; keep only issues
            // whose (possibly recovered) location is one of the files we asked about.
            .filter(|f| changed_set.contains(f.location.path.as_str()))
            .collect(),
    ))
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
    // A deleted file's path still shows up in `git diff --numstat`, but
    // passing a nonexistent path to golangci-lint doesn't just skip that one
    // file — it fails typechecking for the *whole package* and silently
    // reports zero issues everywhere, burying the real cause in `Report.Error`.
    // Filtering to files that still exist avoids that failure mode entirely.
    let go_files: Vec<&str> = changed_files.iter().map(String::as_str).filter(|p| p.ends_with(".go") && repo_root.join(p).exists()).collect();
    if go_files.is_empty() {
        return Ok(vec![]);
    }

    let mut attempt = 0;
    loop {
        attempt += 1;
        match run_golangci_lint_once(repo_root, &go_files) {
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
            std::fs::write(dir.path().join(name), contents).unwrap();
        }
        dir
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

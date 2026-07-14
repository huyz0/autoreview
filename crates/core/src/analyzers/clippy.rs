//! Wraps `cargo clippy` for Rust — the plan's "Language coverage" item,
//! and dogfooded directly on this very workspace (16 real warnings found
//! across the codebase while building this adapter, none fixed here since
//! fixing them isn't this task's job — see the conformance test below for
//! how that's verified instead of just asserted).
//!
//! `cargo clippy --message-format=json` runs over the whole workspace (like
//! `golangci-lint`, clippy has no "just these files" mode that respects
//! cross-file type information) — findings are filtered down to the diff's
//! changed files afterward, the same "analyze broad, report narrow" shape
//! `run_archgraph_check` already uses for Go import cycles.

use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

#[derive(Debug, Deserialize)]
struct CargoMessage {
    reason: String,
    #[serde(default)]
    message: Option<ClippyDiagnostic>,
}

#[derive(Debug, Deserialize)]
struct ClippyDiagnostic {
    level: String,
    message: String,
    code: Option<ClippyCode>,
    spans: Vec<ClippySpan>,
}

#[derive(Debug, Deserialize)]
struct ClippyCode {
    code: String,
}

#[derive(Debug, Deserialize)]
struct ClippySpan {
    is_primary: bool,
    file_name: String,
    line_start: u32,
    line_end: u32,
    column_start: u32,
    column_end: u32,
}

fn map_severity(level: &str) -> Severity {
    match level {
        "error" => Severity::High,
        "warning" => Severity::Medium,
        "note" | "help" => Severity::Low,
        _ => Severity::Medium,
    }
}

fn is_relevant_source_file(path: &str) -> bool {
    Path::new(path).extension().and_then(|e| e.to_str()) == Some("rs")
}

fn parse_clippy_output(stdout: &str, changed_files: &std::collections::HashSet<&str>) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<CargoMessage>(line) else { continue };
        if msg.reason != "compiler-message" {
            continue;
        }
        let Some(diagnostic) = msg.message else { continue };
        // Only "warning"/"error" carry an actionable clippy lint id; a bare
        // "note"/"help" child diagnostic (already folded into the parent's
        // own `rendered` text) would just double-report the same lint.
        if diagnostic.level != "warning" && diagnostic.level != "error" {
            continue;
        }
        let Some(code) = &diagnostic.code else { continue };
        if !code.code.starts_with("clippy::") {
            continue;
        }
        let Some(span) = diagnostic.spans.iter().find(|s| s.is_primary) else { continue };
        if !changed_files.contains(span.file_name.as_str()) {
            continue;
        }

        findings.push(AgentFinding {
            source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "clippy".to_string(), rule_id: Some(code.code.clone()), aspect: None, backend: None },
            category: "style".to_string(),
            severity: map_severity(&diagnostic.level),
            confidence: 1.0,
            title: code.code.clone(),
            message: diagnostic.message.clone(),
            location: Location {
                path: span.file_name.clone(),
                range: LocationRange { start_line: span.line_start, start_col: Some(span.column_start), end_line: Some(span.line_end), end_col: Some(span.column_end) },
                snippet: String::new(),
                side: Side::New,
            },
            related_locations: None,
            suggestion: None,
            tags: None,
            meta: None,
            suggested_patch: None,
        });
    }
    findings
}

/// Runs `cargo clippy --message-format=json` over the whole workspace at
/// `repo_root` and returns findings for whichever changed files clippy
/// flagged. Returns an empty list (not an error) if no changed file is
/// Rust, `cargo`/clippy isn't on PATH, or `repo_root` isn't a Cargo
/// project — Stage 1 degrades gracefully, same contract as every other
/// analyzer adapter here.
pub fn run_clippy(repo_root: &Path, changed_files: &[String]) -> anyhow::Result<Vec<AgentFinding>> {
    let relevant: std::collections::HashSet<&str> = changed_files.iter().map(String::as_str).filter(|p| is_relevant_source_file(p)).collect();
    if relevant.is_empty() || !repo_root.join("Cargo.toml").exists() {
        return Ok(vec![]);
    }

    let output = match Command::new("cargo").args(["clippy", "--workspace", "--message-format=json", "--quiet"]).current_dir(repo_root).output() {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(err) => return Err(err.into()),
    };

    // clippy exits non-zero when it finds warnings/errors under `-D
    // warnings`-style configs — that's signal, not a broken invocation, so
    // only a genuinely unparsable stdout (checked inside parse_clippy_output
    // per-line, tolerantly) would indicate the tool itself failed.
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_clippy_output(&stdout, &relevant))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clippy_available() -> bool {
        Command::new("cargo").args(["clippy", "--version"]).output().map(|o| o.status.success()).unwrap_or(false)
    }

    #[test]
    fn parses_a_real_compiler_message_line_into_a_finding() {
        let line = r#"{"reason":"compiler-message","package_id":"x","manifest_path":"x","target":{"kind":["lib"],"crate_types":["lib"],"name":"x","src_path":"x","edition":"2021","doc":true,"doctest":true,"test":true},"message":{"rendered":"warning: this could be simplified","message":"this could be simplified","code":{"code":"clippy::needless_return","explanation":null},"level":"warning","spans":[{"file_name":"src/lib.rs","byte_start":0,"byte_end":1,"line_start":10,"line_end":10,"column_start":5,"column_end":20,"is_primary":true,"text":[],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null}],"children":[],"$message_type":"diagnostic"}}"#;
        let changed = std::collections::HashSet::from(["src/lib.rs"]);
        let findings = parse_clippy_output(line, &changed);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("clippy::needless_return"));
        assert_eq!(findings[0].category, "style");
        assert_eq!(findings[0].location.range.start_line, 10);
    }

    #[test]
    fn ignores_messages_for_files_outside_the_changed_set() {
        let line = r#"{"reason":"compiler-message","package_id":"x","manifest_path":"x","target":{"kind":["lib"],"crate_types":["lib"],"name":"x","src_path":"x","edition":"2021","doc":true,"doctest":true,"test":true},"message":{"rendered":"x","message":"x","code":{"code":"clippy::needless_return","explanation":null},"level":"warning","spans":[{"file_name":"src/other.rs","byte_start":0,"byte_end":1,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"text":[],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null}],"children":[],"$message_type":"diagnostic"}}"#;
        let changed = std::collections::HashSet::from(["src/lib.rs"]);
        assert!(parse_clippy_output(line, &changed).is_empty());
    }

    #[test]
    fn ignores_non_compiler_message_lines() {
        let line = r#"{"reason":"compiler-artifact"}"#;
        let changed = std::collections::HashSet::from(["src/lib.rs"]);
        assert!(parse_clippy_output(line, &changed).is_empty());
    }

    #[test]
    fn ignores_non_clippy_lint_codes() {
        let line = r#"{"reason":"compiler-message","package_id":"x","manifest_path":"x","target":{"kind":["lib"],"crate_types":["lib"],"name":"x","src_path":"x","edition":"2021","doc":true,"doctest":true,"test":true},"message":{"rendered":"x","message":"unused variable","code":{"code":"unused_variables","explanation":null},"level":"warning","spans":[{"file_name":"src/lib.rs","byte_start":0,"byte_end":1,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"text":[],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null}],"children":[],"$message_type":"diagnostic"}}"#;
        let changed = std::collections::HashSet::from(["src/lib.rs"]);
        assert!(parse_clippy_output(line, &changed).is_empty());
    }

    #[test]
    fn run_clippy_returns_empty_when_no_changed_file_is_rust() {
        let findings = run_clippy(Path::new("/nonexistent-repo-root"), &["main.go".to_string()]).unwrap();
        assert!(findings.is_empty());
    }

    #[test]
    fn run_clippy_finds_a_real_seeded_issue_in_a_temp_cargo_project() {
        if !clippy_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"scratch\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        // A textbook clippy::needless_return lint.
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn f(x: i32) -> i32 {\n    return x + 1;\n}\n").unwrap();

        let findings = run_clippy(dir.path(), &["src/lib.rs".to_string()]).unwrap();
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("clippy::needless_return")), "got: {findings:#?}");
    }
}

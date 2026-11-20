//! Runs Clippy on workspace packages that contain changed Rust files, then
//! keeps diagnostics whose primary span points to one of those files.

use std::collections::{BTreeSet, HashSet};
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
    #[serde(default)]
    text: Vec<ClippySourceLine>,
}

#[derive(Debug, Deserialize)]
struct ClippySourceLine {
    text: String,
}

#[derive(Deserialize)]
struct CargoMetadata {
    workspace_members: HashSet<String>,
    packages: Vec<CargoPackage>,
}

#[derive(Deserialize)]
struct CargoPackage {
    id: String,
    name: String,
    manifest_path: std::path::PathBuf,
}

fn map_severity(level: &str) -> Severity {
    match level {
        "error" => Severity::High,
        "warning" => Severity::Medium,
        "note" | "help" => Severity::Low,
        _ => Severity::Medium,
    }
}

fn diagnostic_category(code: Option<&str>) -> &'static str {
    match code.and_then(|code| code.strip_prefix("clippy::")) {
        Some("await_holding_lock" | "await_holding_refcell_ref" | "infinite_iter" | "invalid_regex"
            | "out_of_bounds_indexing" | "suspicious_open_options" | "unused_io_amount" | "zombie_processes") => "correctness",
        Some("box_collection" | "iter_overeager_cloned" | "large_enum_variant" | "manual_memcpy"
            | "readonly_write_lock" | "redundant_allocation" | "regex_creation_in_loops"
            | "result_large_err" | "slow_vector_initialization") => "performance",
        Some("cognitive_complexity" | "too_many_arguments" | "type_complexity") => "design",
        Some(_) => "style",
        None => "correctness",
    }
}

fn is_relevant_source_file(path: &str) -> bool {
    Path::new(path).extension().and_then(|e| e.to_str()) == Some("rs")
}

fn parse_clippy_output(stdout: &str, changed_files: &HashSet<&str>) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    let mut seen = HashSet::new();
    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<CargoMessage>(line) else { continue };
        if msg.reason != "compiler-message" {
            continue;
        }
        let Some(diagnostic) = msg.message else { continue };
        let code = diagnostic.code.as_ref().map(|code| code.code.as_str());
        let is_clippy = code.is_some_and(|code| code.starts_with("clippy::"));
        let is_error = diagnostic.level == "error";
        if !(is_error || diagnostic.level == "warning" && is_clippy) {
            continue;
        }
        let Some((span, path)) = diagnostic.spans.iter().filter(|span| span.is_primary).find_map(|span| {
            let path = span.file_name.replace(std::path::MAIN_SEPARATOR, "/");
            changed_files.contains(path.as_str()).then_some((span, path))
        }) else { continue };
        if !seen.insert((path.clone(), span.line_start, span.column_start, code.map(str::to_owned), diagnostic.message.clone())) {
            continue;
        }

        findings.push(AgentFinding {
            source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "clippy".to_string(), rule_id: code.map(str::to_owned), aspect: None, backend: None },
            category: diagnostic_category(code).to_string(),
            severity: map_severity(&diagnostic.level),
            confidence: 1.0,
            title: code.unwrap_or("Rust compiler error").to_string(),
            message: diagnostic.message.clone(),
            location: Location {
                path,
                range: LocationRange { start_line: span.line_start, start_col: Some(span.column_start), end_line: Some(span.line_end), end_col: Some(span.column_end) },
                snippet: span.text.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join("\n"),
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

fn changed_packages(repo_root: &Path, changed_files: &HashSet<&str>) -> Option<BTreeSet<String>> {
    let output = Command::new("cargo").args(["metadata", "--no-deps", "--format-version=1"]).current_dir(repo_root).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let metadata: CargoMetadata = serde_json::from_slice(&output.stdout).ok()?;
    let members: Vec<_> = metadata.packages.iter()
        .filter(|package| metadata.workspace_members.contains(&package.id))
        .filter_map(|package| package.manifest_path.parent().map(|directory| (directory, &package.name)))
        .collect();
    let mut packages = BTreeSet::new();
    for file in changed_files {
        let absolute = repo_root.join(file);
        let (_, package) = members.iter().filter(|(directory, _)| absolute.starts_with(directory))
            .max_by_key(|(directory, _)| directory.components().count())?;
        packages.insert((*package).clone());
    }
    Some(packages)
}

pub fn run_clippy(repo_root: &Path, changed_files: &[String]) -> anyhow::Result<Vec<AgentFinding>> {
    let relevant: HashSet<&str> = changed_files.iter().map(String::as_str)
        .filter(|p| is_relevant_source_file(p) && repo_root.join(p).is_file())
        .collect();
    if relevant.is_empty() || !repo_root.join("Cargo.toml").exists() {
        return Ok(vec![]);
    }

    let mut command = Command::new("cargo");
    command.args(["clippy", "--message-format=json", "--quiet"]).current_dir(repo_root);
    // Standalone test, example and benchmark files need their Cargo
    // targets. Ordinary source changes can use Cargo's default targets.
    if relevant.iter().any(|path| path.split('/').any(|part| matches!(part, "tests" | "examples" | "benches"))) {
        command.arg("--all-targets");
    }
    match changed_packages(repo_root, &relevant) {
        Some(packages) => {
            for package in packages {
                command.arg("--package").arg(package);
            }
        }
        None => { command.arg("--workspace"); }
    }
    let output = command.output()?;

    // A nonzero exit can carry a valid diagnostic, including errors in
    // the changed code. Surface failed invocations that yielded none.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let findings = parse_clippy_output(&stdout, &relevant);
    if !output.status.success() && findings.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let excerpt = stderr.lines().take(20).collect::<Vec<_>>().join("\n");
        anyhow::bail!("cargo clippy failed ({}): {}", output.status, excerpt.trim());
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clippy_available() -> bool {
        Command::new("cargo").args(["clippy", "--version"]).output().map(|o| o.status.success()).unwrap_or(false)
    }

    #[test]
    fn maps_known_clippy_categories_without_guessing_unknown_ids() {
        assert_eq!(diagnostic_category(Some("clippy::regex_creation_in_loops")), "performance");
        assert_eq!(diagnostic_category(Some("clippy::await_holding_lock")), "correctness");
        assert_eq!(diagnostic_category(Some("clippy::type_complexity")), "design");
        assert_eq!(diagnostic_category(Some("clippy::needless_return")), "style");
        assert_eq!(diagnostic_category(Some("E0308")), "correctness");
        assert_eq!(diagnostic_category(None), "correctness");
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

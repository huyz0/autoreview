use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use autoreview_core::{run_ast_grep_with_coverage, run_golangci_lint, FileChange, ResolvedRulePack};
use autoreview_langsupport::Language;
use autoreview_schema::{AgentFinding, AnalysisCoverageIssue, ArtifactChecksConfig};

pub(super) struct ReviewInput<'a> {
    pub repo_root: &'a Path,
    pub source_root: &'a Path,
    pub files: &'a [FileChange],
    pub aspects: Option<&'a [String]>,
    pub audit_rust_deps: bool,
    pub tier4_go: bool,
    pub artifact_checks: &'a ArtifactChecksConfig,
}

impl ReviewInput<'_> {
    fn wants(&self, category: &str) -> bool {
        self.aspects
            .is_none_or(|aspects| aspects.iter().any(|aspect| aspect == category))
    }
}

pub(super) fn collect(
    input: ReviewInput<'_>,
    changed_files: &[String],
    packs: &[ResolvedRulePack],
) -> (Vec<AgentFinding>, Vec<String>, Vec<AnalysisCoverageIssue>) {
    let languages =
        autoreview_langsupport::languages_present(changed_files.iter().map(String::as_str));
    let mut warnings = Vec::new();
    let mut coverage = Vec::new();
    let mut findings = lint(
        input.source_root,
        changed_files,
        packs,
        &mut warnings,
        &mut coverage,
    );
    if input.audit_rust_deps && input.wants("security") {
        findings.extend(audit_dependencies(
            input.source_root,
            changed_files,
            &mut warnings,
            &mut coverage,
        ));
    }
    if let Some(snapshot) = input.artifact_checks.npm_advisory_snapshot.as_deref()
        .filter(|_| input.wants("security"))
    {
        findings.extend(audit_npm(&input, changed_files, snapshot, &mut warnings, &mut coverage));
    }
    findings.extend(artifact_findings(&input, changed_files, &mut warnings));
    findings.extend(structural(
        input.source_root,
        changed_files,
        packs,
        &languages,
        input.wants("design"),
    ));
    findings.extend(check_layers(
        input.repo_root,
        input.source_root,
        changed_files,
        &mut warnings,
    ));
    findings.extend(cross_file(&input, changed_files, packs, &mut warnings, &mut coverage));
    (findings, warnings, coverage)
}

fn record_warning(warnings: &mut Vec<String>, message: String) {
    println!("  [warn] {message}");
    warnings.push(message);
}


fn record_tool_issue(
    warnings: &mut Vec<String>, coverage: &mut Vec<AnalysisCoverageIssue>,
    tool: &str, reason: &str, message: String,
) {
    record_warning(warnings, message.clone());
    coverage.push(AnalysisCoverageIssue {
        analyzer: tool.into(), reason: reason.into(), scope: "analyzer".into(),
        path: None, line: None, rule: None, detail: message,
    });
}

fn artifact_findings(
    input: &ReviewInput<'_>,
    paths: &[String],
    warnings: &mut Vec<String>,
) -> Vec<AgentFinding> {
    let checks = input.artifact_checks;
    let root = input.source_root;
    let mut findings = Vec::new();
    if input.wants("security") {
        findings.extend(autoreview_core::run_spring_session_policy(root, paths));
        if checks.immutable_github_action_refs {
            findings.extend(autoreview_core::run_workflow_policy(root, paths));
        }
        findings.extend(autoreview_core::run_workflow_security(root, paths, checks));
    }
    if !input.wants("correctness") {
        return findings;
    }
    findings.extend(autoreview_core::run_locale_catalog_policy(
        root, paths, &checks.locale_catalogs,
    ));
    for (name, result) in [
        ("PostgreSQL migration", autoreview_core::run_postgres_migration_policy(
            root, paths, &checks.transactional_postgres_migrations,
        )),
        ("Outbox", autoreview_core::run_outbox_policy(root, paths, &checks.outbox_operations)),
    ] {
        match result {
            Ok(result) => findings.extend(result),
            Err(error) => record_warning(warnings, format!("{name} policy failed: {error}")),
        }
    }
    findings
}

fn binary_available(binary: &str) -> bool {
    Command::new(binary)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn lint(
    root: &Path,
    files: &[String],
    packs: &[ResolvedRulePack],
    warnings: &mut Vec<String>,
    coverage: &mut Vec<AnalysisCoverageIssue>,
) -> Vec<AgentFinding> {
    let languages = autoreview_langsupport::languages_present(files.iter()
        .filter(|path| root.join(path).is_file()).map(String::as_str));
    if !languages.is_empty() && !binary_available("ast-grep") {
        record_tool_issue(warnings, coverage, "ast-grep", "tool_unavailable",
            "ast-grep is unavailable. Pattern rules were skipped.".into());
    }
    if languages.contains(&Language::Go) && !binary_available("golangci-lint") {
        record_tool_issue(warnings, coverage, "golangci-lint", "tool_unavailable",
            "golangci-lint is unavailable. Go linter checks were skipped.".into());
    }
    let mut findings = Vec::new();
    match run_ast_grep_with_coverage(root, files, packs) {
        Ok(result) => {
            findings.extend(result.findings);
            for issue in result.coverage {
                record_warning(warnings, format!("{} [{}]: {}", issue.path.as_deref().unwrap_or("ast-grep"), issue.rule.as_deref().unwrap_or("pattern"), issue.detail));
                coverage.push(issue);
            }
        }
        Err(err) => record_tool_issue(warnings, coverage, "ast-grep", "tool_failed", format!("ast-grep run failed: {err}")),
    }
    match run_golangci_lint(root, files) {
        Ok(items) => findings.extend(items),
        Err(err) => record_tool_issue(warnings, coverage, "golangci-lint", "tool_failed", format!("golangci-lint run failed: {err}")),
    }
    match autoreview_core::run_clippy_with_status(root, files) {
        Ok(result) => {
            findings.extend(result.findings);
            for failure in result.failures {
                record_tool_issue(warnings, coverage, "clippy", "tool_failed", failure);
            }
        }
        Err(error) => record_tool_issue(warnings, coverage, "clippy", "tool_failed", format!("clippy run failed: {error}")),
    }
    findings.extend(autoreview_core::run_rust_review(root, files));
    findings
}

fn audit_dependencies(
    root: &Path,
    paths: &[String],
    warnings: &mut Vec<String>,
    coverage: &mut Vec<AnalysisCoverageIssue>,
) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    for path in paths.iter().filter(|path| {
        Path::new(path)
            .file_name()
            .is_some_and(|name| name == "Cargo.lock")
    }) {
        match autoreview_core::run_cargo_audit(root, path) {
            Ok(items) => findings.extend(items),
            Err(err) => {
                record_tool_issue(warnings, coverage, "cargo-audit", "tool_failed", format!("cargo audit failed for {path}: {err:#}"));
                if let Some(issue) = coverage.last_mut() {
                    issue.scope = "file".into();
                    issue.path = Some(path.clone());
                }
            }
        }
    }
    findings
}

fn audit_npm(
    input: &ReviewInput<'_>,
    paths: &[String],
    snapshot: &str,
    warnings: &mut Vec<String>,
    coverage: &mut Vec<AnalysisCoverageIssue>,
) -> Vec<AgentFinding> {
    let tool = "autoreview-npm-osv";
    let paths: Vec<_> = paths.iter().filter(|path| {
        Path::new(path).file_name().is_some_and(|name| name == "package-lock.json")
    }).collect();
    if paths.is_empty() {
        return Vec::new();
    }
    let advisories = match autoreview_core::NpmAdvisories::load(&input.repo_root.join(snapshot)) {
        Ok(advisories) => advisories,
        Err(error) => {
            record_tool_issue(warnings, coverage, tool, "advisory_snapshot_unavailable",
                format!("npm advisory coverage unavailable: {error:#}"));
            return Vec::new();
        }
    };
    let mut findings = Vec::new();
    for path in paths {
        match autoreview_core::audit_npm_lock(input.source_root, path, &advisories) {
            Ok(items) => findings.extend(items),
            Err(error) => {
                record_tool_issue(warnings, coverage, tool, "lockfile_unavailable",
                    format!("npm advisory coverage unavailable for {path}: {error:#}"));
                if let Some(issue) = coverage.last_mut() {
                    issue.scope = "file".into();
                    issue.path = Some(path.clone());
                }
            }
        }
    }
    findings
}

fn structural(
    source_root: &Path,
    changed_files: &[String],
    registered_packs: &[ResolvedRulePack],
    languages_present: &HashSet<Language>,
    design: bool,
) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    // Duplication detection is deliberately NOT gated by language here: its
    // sliding-window line-hash algorithm is language-agnostic and already
    // fires on non-source text files (.sql/.proto/.md/etc) — gating it to
    // the tracked source-language set would silently drop that coverage,
    // a behavior change, not a speed win.
    findings.extend(autoreview_core::run_duplication_check(
        source_root,
        changed_files,
    ));
    findings.extend(autoreview_core::run_cross_file_duplication_check(
        source_root,
        changed_files,
    ));
    const STRUCTURAL: &[Language] = &[
        Language::Go,
        Language::Java,
        Language::Kotlin,
        Language::TypeScript,
        Language::Tsx,
        Language::JavaScript,
    ];
    let has_structural_language = STRUCTURAL
        .iter()
        .any(|language| languages_present.contains(language));
    if (has_structural_language || languages_present.contains(&Language::Rust)) && design {
        findings.extend(autoreview_core::run_complexity_check(
            source_root,
            changed_files,
            registered_packs,
        ));
    }
    if has_structural_language {
        findings.extend(autoreview_core::run_practices_check(
            source_root,
            changed_files,
        ));
    }
    findings
}

fn check_layers(
    repo_root: &Path,
    source_root: &Path,
    changed_files: &[String],
    warnings: &mut Vec<String>,
) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    let architecture = match autoreview_core::load_architecture_config(
        &repo_root.join(".autoreview").join("architecture.yaml"),
    ) {
        Ok(config) => config,
        Err(err) => {
            record_warning(
                warnings,
                format!("failed to parse .autoreview/architecture.yaml: {err}"),
            );
            None
        }
    };
    if let Some(architecture) = architecture.as_ref() {
        findings.extend(autoreview_core::run_architecture_check(
            source_root,
            changed_files,
            architecture,
        ));
    }
    let cargo_review =
        autoreview_core::run_cargo_review(source_root, changed_files, architecture.as_ref());
    findings.extend(cargo_review.findings);
    for warning in cargo_review.warnings {
        record_warning(warnings, format!("Cargo review: {warning}"));
    }
    findings
}

fn cross_file(
    input: &ReviewInput<'_>,
    changed_files: &[String],
    registered_packs: &[ResolvedRulePack],
    warnings: &mut Vec<String>,
    coverage: &mut Vec<AnalysisCoverageIssue>,
) -> Vec<AgentFinding> {
    let ReviewInput {
        repo_root,
        source_root,
        files,
        tier4_go,
        ..
    } = *input;
    let mut findings = Vec::new();
    // archgraph (Tier 2): whole-repo import-cycle detection (Go and
    // Java/Kotlin, each its own graph), a no-op for a repo/diff with
    // neither a go.mod nor any .go/.java/.kt files touched.
    findings.extend(autoreview_core::run_archgraph_check(
        source_root,
        changed_files,
    ));
    findings.extend(autoreview_core::run_symindex_check_with_tier4(
        source_root,
        changed_files,
        tier4_go,
    ));
    let dataflow_files: Vec<_> = changed_files.iter().filter(|path| !deleted_in_snapshot(source_root, path)).cloned().collect();
    let dataflow = autoreview_core::run_dataflow_check_with_coverage(
        repo_root,
        source_root,
        &dataflow_files,
        registered_packs,
    );
    findings.extend(dataflow.findings);
    for issue in dataflow.coverage {
        let location = issue.path.as_deref().unwrap_or("dataflow");
        let rule = issue.rule.as_deref().map_or(String::new(), |rule| format!(" [{rule}]"));
        record_warning(warnings, format!("{location}{rule}: {}", issue.detail));
        coverage.push(issue);
    }
    findings.extend(autoreview_core::run_framework_checks(source_root, changed_files));
    findings.extend(autoreview_core::detect_shotgun_surgery(files));
    findings.extend(autoreview_core::run_divergent_change_check(
        source_root,
        changed_files,
    ));
    findings.extend(autoreview_core::run_bidi_control_character_check(
        source_root,
        changed_files,
    ));
    findings
}

// `changed_files` comes from a Git diff, which includes deleted paths. Only
// exclude a missing path when the review snapshot's own HEAD confirms that
// it is absent; an unavailable Git state must not silently hide a skip.
fn deleted_in_snapshot(root: &Path, path: &str) -> bool {
    if root.join(path).symlink_metadata().is_ok() || !root.join(".git").exists() { return false; }
    Command::new("git").current_dir(root)
        .args(["--literal-pathspecs", "ls-tree", "-z", "HEAD", "--", path])
        .output().is_ok_and(|result| result.status.success() && result.stdout.is_empty())
}

#[cfg(test)]
mod tests {
    use super::deleted_in_snapshot;

    #[test]
    fn deleted_snapshot_paths_are_distinguished_from_missing_tracked_sources() {
        let repo = autoreview_test_support::init_repo(&[("tracked.go", "package example\n"), ("present.go", "package example\n")]);
        assert!(!deleted_in_snapshot(repo.path(), "present.go"));
        std::fs::remove_file(repo.path().join("tracked.go")).unwrap();
        assert!(!deleted_in_snapshot(repo.path(), "tracked.go"));
        assert!(deleted_in_snapshot(repo.path(), "deleted.go"));
        let invalid = tempfile::tempdir().unwrap();
        std::fs::create_dir(invalid.path().join(".git")).unwrap();
        assert!(!deleted_in_snapshot(invalid.path(), "missing.go"));
    }
}

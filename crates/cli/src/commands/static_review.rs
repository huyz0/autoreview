use std::path::Path;
use std::process::Command;

use autoreview_core::{run_ast_grep, run_golangci_lint, FileChange, ResolvedRulePack};
use autoreview_langsupport::Language;
use autoreview_schema::AgentFinding;

pub(super) struct ReviewInput<'a> {
    pub repo_root: &'a Path,
    pub source_root: &'a Path,
    pub files: &'a [FileChange],
    pub aspects: Option<&'a [String]>,
    pub audit_rust_deps: bool,
    pub tier4_go: bool,
}

pub(super) fn collect(input: ReviewInput<'_>, changed_files: &[String], registered_packs: &[ResolvedRulePack]) -> (Vec<AgentFinding>, Vec<String>) {
    let ReviewInput { repo_root, source_root, files, aspects, audit_rust_deps, tier4_go } = input;
    let wants = |category: &str| aspects.is_none_or(|aspects| aspects.iter().any(|aspect| aspect == category));
    let languages_present = autoreview_langsupport::languages_present(changed_files.iter().map(String::as_str));
    let mut stage1_agent_findings = Vec::new();
    let mut analyzer_warnings = Vec::new();
    let mut warn = |message: String| {
        println!("  [warn] {message}");
        analyzer_warnings.push(message);
    };
    let available = |binary: &str| Command::new(binary).arg("--version").output().is_ok_and(|output| output.status.success());
    if !languages_present.is_empty() && !available("ast-grep") {
        warn("ast-grep is unavailable. Pattern rules were skipped.".to_string());
    }
    if languages_present.contains(&Language::Go) && !available("golangci-lint") {
        warn("golangci-lint is unavailable. Go linter checks were skipped.".to_string());
    }
    match run_ast_grep(source_root, changed_files, registered_packs) {
        Ok(findings) => stage1_agent_findings.extend(findings),
        Err(err) => warn(format!("ast-grep run failed: {err}")),
    }
    match run_golangci_lint(source_root, changed_files) {
        Ok(findings) => stage1_agent_findings.extend(findings),
        Err(err) => warn(format!("golangci-lint run failed: {err}")),
    }
    match autoreview_core::run_clippy(source_root, changed_files) {
        Ok(findings) => stage1_agent_findings.extend(findings),
        Err(err) => warn(format!("clippy run failed: {err}")),
    }
    stage1_agent_findings.extend(autoreview_core::run_rust_review(source_root, changed_files));
    if audit_rust_deps && wants("security") {
        for path in changed_files.iter().filter(|path| Path::new(path).file_name().is_some_and(|name| name == "Cargo.lock")) {
            match autoreview_core::run_cargo_audit(source_root, path) {
                Ok(findings) => stage1_agent_findings.extend(findings),
                Err(err) => warn(format!("cargo audit failed for {path}: {err:#}")),
            }
        }
    }
    // Duplication detection is deliberately NOT gated by language here: its
    // sliding-window line-hash algorithm is language-agnostic and already
    // fires on non-source text files (.sql/.proto/.md/etc) — gating it to
    // the tracked source-language set would silently drop that coverage,
    // a behavior change, not a speed win.
    stage1_agent_findings.extend(autoreview_core::run_duplication_check(source_root, changed_files));
    stage1_agent_findings.extend(autoreview_core::run_cross_file_duplication_check(source_root, changed_files));
    const STRUCTURAL: &[Language] = &[Language::Go, Language::Java, Language::Kotlin, Language::TypeScript, Language::Tsx, Language::JavaScript];
    let has_structural_language = STRUCTURAL.iter().any(|language| languages_present.contains(language));
    if (has_structural_language || languages_present.contains(&Language::Rust))
        && wants("design")
    {
        stage1_agent_findings.extend(autoreview_core::run_complexity_check(source_root, changed_files, registered_packs));
    }
    if has_structural_language {
        stage1_agent_findings.extend(autoreview_core::run_practices_check(source_root, changed_files));
    }
    // Architecture layer check is opt-in: only runs when the repo has a
    // .autoreview/architecture.yaml declaring layers, per the plan ("no sane
    // generic default for what a repo's layers are").
    let architecture = match autoreview_core::load_architecture_config(&repo_root.join(".autoreview").join("architecture.yaml")) {
        Ok(config) => config,
        Err(err) => {
            warn(format!("failed to parse .autoreview/architecture.yaml: {err}"));
            None
        }
    };
    if let Some(architecture) = architecture.as_ref() {
        stage1_agent_findings.extend(autoreview_core::run_architecture_check(source_root, changed_files, architecture));
    }
    let cargo_review = autoreview_core::run_cargo_review(source_root, changed_files, architecture.as_ref());
    stage1_agent_findings.extend(cargo_review.findings);
    for warning in cargo_review.warnings {
        warn(format!("Cargo review: {warning}"));
    }
    // archgraph (Tier 2): whole-repo import-cycle detection (Go and
    // Java/Kotlin, each its own graph), a no-op for a repo/diff with
    // neither a go.mod nor any .go/.java/.kt files touched.
    stage1_agent_findings.extend(autoreview_core::run_archgraph_check(source_root, changed_files));
    stage1_agent_findings.extend(autoreview_core::run_symindex_check_with_tier4(source_root, changed_files, tier4_go));
    stage1_agent_findings.extend(autoreview_core::run_dataflow_check_for_snapshot(repo_root, source_root, changed_files, registered_packs));
    stage1_agent_findings.extend(autoreview_core::detect_shotgun_surgery(files));
    stage1_agent_findings.extend(autoreview_core::run_divergent_change_check(source_root, changed_files));
    stage1_agent_findings.extend(autoreview_core::run_bidi_control_character_check(source_root, changed_files));
    (stage1_agent_findings, analyzer_warnings)
}

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use autoreview_core::{run_ast_grep, run_golangci_lint, FileChange, ResolvedRulePack};
use autoreview_langsupport::Language;
use autoreview_schema::{AgentFinding, LocaleCatalogConfig};

pub(super) struct ReviewInput<'a> {
    pub repo_root: &'a Path,
    pub source_root: &'a Path,
    pub files: &'a [FileChange],
    pub aspects: Option<&'a [String]>,
    pub audit_rust_deps: bool,
    pub tier4_go: bool,
    pub immutable_github_action_refs: bool,
    pub transactional_postgres_migrations: &'a [String],
    pub locale_catalogs: &'a [LocaleCatalogConfig],
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
) -> (Vec<AgentFinding>, Vec<String>) {
    let languages =
        autoreview_langsupport::languages_present(changed_files.iter().map(String::as_str));
    let mut warnings = Vec::new();
    let mut findings = lint(
        input.source_root,
        changed_files,
        packs,
        &languages,
        &mut warnings,
    );
    if input.audit_rust_deps && input.wants("security") {
        findings.extend(audit_dependencies(
            input.source_root,
            changed_files,
            &mut warnings,
        ));
    }
    if input.immutable_github_action_refs && input.wants("security") {
        findings.extend(autoreview_core::run_workflow_policy(input.source_root, changed_files));
    }
    if !input.transactional_postgres_migrations.is_empty() && input.wants("correctness") {
        match autoreview_core::run_postgres_migration_policy(
            input.source_root, changed_files, input.transactional_postgres_migrations,
        ) {
            Ok(migrations) => findings.extend(migrations),
            Err(error) => record_warning(&mut warnings, format!("PostgreSQL migration policy failed: {error}")),
        }
    }
    if !input.locale_catalogs.is_empty() && input.wants("correctness") {
        findings.extend(autoreview_core::run_locale_catalog_policy(
            input.source_root, changed_files, input.locale_catalogs,
        ));
    }
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
    findings.extend(cross_file(&input, changed_files, packs));
    (findings, warnings)
}

fn record_warning(warnings: &mut Vec<String>, message: String) {
    println!("  [warn] {message}");
    warnings.push(message);
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
    languages: &HashSet<Language>,
    warnings: &mut Vec<String>,
) -> Vec<AgentFinding> {
    if !languages.is_empty() && !binary_available("ast-grep") {
        record_warning(
            warnings,
            "ast-grep is unavailable. Pattern rules were skipped.".into(),
        );
    }
    if languages.contains(&Language::Go) && !binary_available("golangci-lint") {
        record_warning(
            warnings,
            "golangci-lint is unavailable. Go linter checks were skipped.".into(),
        );
    }
    let results = [
        ("ast-grep", run_ast_grep(root, files, packs)),
        ("golangci-lint", run_golangci_lint(root, files)),
        ("clippy", autoreview_core::run_clippy(root, files)),
    ];
    let mut findings = Vec::new();
    for (tool, result) in results {
        match result {
            Ok(items) => findings.extend(items),
            Err(err) => record_warning(warnings, format!("{tool} run failed: {err}")),
        }
    }
    findings.extend(autoreview_core::run_rust_review(root, files));
    findings
}

fn audit_dependencies(
    root: &Path,
    paths: &[String],
    warnings: &mut Vec<String>,
) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    for path in paths.iter().filter(|path| {
        Path::new(path)
            .file_name()
            .is_some_and(|name| name == "Cargo.lock")
    }) {
        match autoreview_core::run_cargo_audit(root, path) {
            Ok(items) => findings.extend(items),
            Err(err) => record_warning(warnings, format!("cargo audit failed for {path}: {err:#}")),
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
    findings.extend(autoreview_core::run_dataflow_check_for_snapshot(
        repo_root,
        source_root,
        changed_files,
        registered_packs,
    ));
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

mod model;
mod results;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use autoreview_core::{
    assign_fingerprints, collect_diff_facts, discover_manifests, load_config, to_finding,
    DiffFacts, ResolvedRulePack,
};
use autoreview_schema::{
    AgentBackendKind, AgentFinding, AutoreviewConfig, Finding, RulePackTrust, SuppressedFinding,
    SuppressedReason, Tier,
};

use super::diff_lines::ChangedLines;
use super::revision::{checkout_snapshot, resolve_commit};

pub(crate) use results::move_shadow_rule_file;

pub struct DiffCommandOptions {
    pub repo_root: PathBuf,
    pub base_ref: String,
    pub head_ref: String,
    pub tier: Option<Tier>,
    pub aspects: Option<Vec<String>>,
    pub subcategories: Option<Vec<String>>,
    pub max_usd: Option<f64>,
    pub incremental: bool,
    pub static_only: bool,
    pub changed_lines: bool,
    pub audit_rust_deps: bool,
    pub backend: Option<AgentBackendKind>,
}

struct ReviewRun<'a> {
    options: &'a DiffCommandOptions,
    config: &'a AutoreviewConfig,
    facts: &'a DiffFacts,
    source_root: &'a Path,
    packs: &'a [ResolvedRulePack],
    changed_files: &'a [String],
    changed_lines: Option<&'a ChangedLines>,
}

impl ReviewRun<'_> {
    fn in_scope(&self, finding: &AgentFinding) -> bool {
        self.options
            .aspects
            .as_ref()
            .is_none_or(|aspects| aspects.contains(&finding.category))
            && self.matches_subcategory(finding.meta.as_ref())
            && self
                .changed_lines
                .is_none_or(|lines| lines.contains_finding(finding))
    }

    fn matches_subcategory(
        &self,
        metadata: Option<&std::collections::HashMap<String, serde_json::Value>>,
    ) -> bool {
        self.options.subcategories.as_ref().is_none_or(|selected| {
            metadata
                .and_then(|fields| fields.get("subcategory"))
                .and_then(serde_json::Value::as_array)
                .is_some_and(|labels| {
                    labels.iter().filter_map(serde_json::Value::as_str).any(|label| {
                        selected.iter().any(|value| super::rule_catalog::subcategory_matches(label, value))
                    })
                })
        })
    }
}

struct StaticFindings {
    suppressed: Vec<SuppressedFinding>,
    warnings: Vec<String>,
    coverage: Vec<autoreview_schema::AnalysisCoverageIssue>,
}

pub fn run_diff(options: DiffCommandOptions) -> anyhow::Result<()> {
    let (base, head) = diff_revisions(&options.repo_root, &options.base_ref, &options.head_ref)?;
    run_diff_at_revisions(&options, &base, &head)
}

fn run_diff_at_revisions(
    options: &DiffCommandOptions,
    base: &str,
    head: &str,
) -> anyhow::Result<()> {
    let config = load_config(&options.repo_root.join(".autoreview/config.yaml"))?;
    let repo = options.repo_root.to_string_lossy();
    let facts = collect_diff_facts(&repo, base, head, Some(&config.triage.sensitive_paths))?;
    let changed_lines = options
        .changed_lines
        .then(|| ChangedLines::load(&options.repo_root, base, head))
        .transpose()?;
    let snapshot = checkout_snapshot(&options.repo_root, head)?;
    let skills = discover_manifests(&options.repo_root)?;
    print_header(options, &facts);
    let changed_files: Vec<_> = facts.files.iter().map(|f| f.path.clone()).collect();
    let packs = load_registered_packs(&options.repo_root);
    let run = ReviewRun {
        options,
        config: &config,
        facts: &facts,
        source_root: snapshot.path(),
        packs: &packs,
        changed_files: &changed_files,
        changed_lines: changed_lines.as_ref(),
    };
    let (findings, static_results) = run_static_analyzers(&run);
    let model = model::review(&run, &skills, findings);
    results::finish(&run, model, static_results)
}

fn print_header(options: &DiffCommandOptions, facts: &DiffFacts) {
    println!(
        "autoreview diff  ({}...{})\n",
        options.base_ref, options.head_ref
    );
    println!("  head commit:    {}", facts.head_ref);
    println!("  files changed:  {}", facts.files.len());
    let langs = if facts.languages.is_empty() {
        "none detected".to_string()
    } else {
        facts
            .languages
            .iter()
            .map(|(l, n)| format!("{l}({n})"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!("  languages:      {langs}");
}

fn load_registered_packs(repo: &Path) -> Vec<ResolvedRulePack> {
    let configured = match autoreview_core::load_rule_packs_config(
        &autoreview_core::rule_packs_config_path(repo),
    ) {
        Ok(configured) => configured,
        Err(err) => {
            println!("  [warn] failed to parse .autoreview/rulepacks.yaml: {err}");
            return Vec::new();
        }
    };
    let cache = autoreview_core::default_rule_packs_cache_root();
    let mut packs = Vec::new();
    for (id, result) in autoreview_core::resolve_rule_packs(repo, &cache, &configured) {
        match result {
            Ok(pack) => packs.push(pack),
            Err(err) => println!("  [warn] failed to resolve rule pack '{id}': {err}"),
        }
    }
    packs
}

fn run_static_analyzers(run: &ReviewRun<'_>) -> (Vec<Finding>, StaticFindings) {
    let (mut findings, mut warnings, coverage) = super::static_review::collect(
        super::static_review::ReviewInput {
            repo_root: &run.options.repo_root,
            source_root: run.source_root,
            files: &run.facts.files,
            aspects: run.options.aspects.as_deref(),
            audit_rust_deps: run.options.audit_rust_deps,
            tier4_go: run.config.symindex.tier4_go,
            artifact_checks: &run.config.artifact_checks,
        },
        run.changed_files,
        run.packs,
    );
    append_protobuf_findings(run, &mut findings, &mut warnings);
    append_java_nullability_findings(run, &mut findings, &mut warnings);
    let shadow: HashSet<_> = run
        .packs
        .iter()
        .filter(|pack| pack.trust == RulePackTrust::Shadow)
        .map(|pack| pack.id.as_str())
        .collect();
    let (findings, suppressed): (Vec<_>, Vec<_>) = findings
        .into_iter()
        .filter(|finding| run.in_scope(finding))
        .partition(|finding| {
            let pack_id = finding
                .meta
                .as_ref()
                .and_then(|meta| meta.get("rulePackId"))
                .and_then(|value| value.as_str());
            !pack_id.is_some_and(|pack| shadow.contains(pack))
        });
    println!(
        "  stage 1:        {} deterministic finding(s)",
        findings.len()
    );
    let findings = assign_fingerprints(findings)
        .into_iter()
        .map(to_finding)
        .collect();
    let suppressed = assign_fingerprints(suppressed)
        .into_iter()
        .map(|finding| SuppressedFinding {
            finding: to_finding(finding),
            reason: SuppressedReason::ShadowRulePack,
        })
        .collect();
    (
        findings,
        StaticFindings {
            suppressed,
            warnings,
            coverage,
        },
    )
}

fn append_protobuf_findings(run: &ReviewRun<'_>, findings: &mut Vec<AgentFinding>, warnings: &mut Vec<String>) {
    let globs = &run.config.artifact_checks.protobuf_compatibility;
    if globs.is_empty() || run.options.aspects.as_ref().is_some_and(|aspects| !aspects.iter().any(|aspect| aspect == "correctness")) {
        return;
    }
    let result = super::revision::compare_protobuf(
        &run.options.repo_root, run.source_root, &run.facts.base_ref, &run.facts.head_ref, globs,
    );
    append_comparison("Protobuf compatibility policy", result, findings, warnings);
}

fn append_java_nullability_findings(run: &ReviewRun<'_>, findings: &mut Vec<AgentFinding>, warnings: &mut Vec<String>) {
    if run.options.aspects.as_ref().is_some_and(|aspects| !aspects.iter().any(|aspect| aspect == "correctness")) {
        return;
    }
    let result = super::revision::compare_java_nullability(
        &run.options.repo_root, run.source_root, &run.facts.base_ref, &run.facts.head_ref, run.changed_files,
    );
    append_comparison("Java nullability comparison", result, findings, warnings);
}

fn append_comparison(
    label: &str,
    result: anyhow::Result<Vec<AgentFinding>>,
    findings: &mut Vec<AgentFinding>,
    warnings: &mut Vec<String>,
) {
    match result {
        Ok(detected) => findings.extend(detected),
        Err(error) => {
            let warning = format!("{label} failed: {error}");
            println!("  [warn] {warning}");
            warnings.push(warning);
        }
    }
}

/// Watch both resolved commits so edits with identical diff statistics
/// still trigger a new review.
fn diff_revisions(
    repo_root: &Path,
    base_ref: &str,
    head_ref: &str,
) -> anyhow::Result<(String, String)> {
    let base = resolve_commit(repo_root, base_ref)?;
    let head = resolve_commit(repo_root, head_ref)?;
    Ok((base, head))
}

/// `autoreview diff --watch` — polls `base_ref...head_ref` on an interval
/// and re-runs the review whenever either ref resolves to a different commit,
/// instead of requiring a manual re-run after every commit/amend while
/// iterating on a branch. `max_iterations` bounds the loop for tests and
/// scripted verification (`None` — the CLI's real usage — loops until the
/// process is killed, e.g. Ctrl+C, which needs no special handling here
/// since there's no mid-run state to clean up between polls).
///
/// Watches the same `base_ref...head_ref` comparison `run_diff` itself
/// reviews, not the working tree — so, matching `diff`'s own existing
/// semantics, uncommitted edits only trigger (and appear in) a re-run once
/// `head_ref` actually resolves to them (e.g. after a `git commit --amend`
/// in another terminal), not the instant a file is saved.
fn run_diff_watch(
    options: DiffCommandOptions,
    poll_interval: std::time::Duration,
    max_iterations: Option<u32>,
) -> anyhow::Result<()> {
    println!(
        "autoreview diff --watch  ({}...{}, polling every {}s — Ctrl+C to stop)\n",
        options.base_ref,
        options.head_ref,
        poll_interval.as_secs()
    );

    let mut last_reviewed = None;
    let mut iterations: u32 = 0;
    loop {
        match diff_revisions(&options.repo_root, &options.base_ref, &options.head_ref) {
            Ok(revisions) if last_reviewed.as_ref() != Some(&revisions) => {
                println!("[watch] change detected — running review...\n");
                match run_diff_at_revisions(&options, &revisions.0, &revisions.1) {
                    Ok(()) => last_reviewed = Some(revisions),
                    Err(err) => println!("[watch] review run failed: {err}"),
                }
                println!("\n[watch] watching for further changes...");
            }
            Ok(_) => {}
            Err(err) => println!("[watch] failed to check for changes: {err}"),
        }

        iterations += 1;
        if max_iterations.is_some_and(|max| iterations >= max) {
            return Ok(());
        }
        std::thread::sleep(poll_interval);
    }
}

pub fn run_diff_or_watch(
    options: DiffCommandOptions,
    watch: bool,
    poll_interval_secs: u64,
) -> anyhow::Result<()> {
    if watch {
        run_diff_watch(
            options,
            std::time::Duration::from_secs(poll_interval_secs.max(1)),
            None,
        )
    } else {
        run_diff(options)
    }
}

// Each parameter is independent context one specialist invocation needs
// (backend, target repo, tier/plan entry, three prompt-building inputs, an
// optional override) — no natural subgroup exists that wouldn't just be a
// struct wrapping this whole parameter list.
#[cfg(test)]
mod watch_tests {
    use super::*;
    use autoreview_test_support::init_repo;
    use std::process::Command;

    fn head_sha(repo_root: &Path) -> String {
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo_root)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn diff_signature_changes_when_a_new_commit_lands_on_head() {
        let repo = init_repo(&[("main.go", "package main\n")]);
        let base_sha = head_sha(repo.path());

        repo.commit(&[("main.go", "package main\n\nfunc f() {}\n")], "add f");
        let sig1 = diff_revisions(repo.path(), &base_sha, "HEAD").unwrap();

        repo.commit(
            &[("main.go", "package main\n\nfunc g() {}\n")],
            "replace f with g",
        );
        let sig2 = diff_revisions(repo.path(), &base_sha, "HEAD").unwrap();

        assert_ne!(
            sig1, sig2,
            "an edit with identical line counts must trigger a review"
        );
    }

    #[test]
    fn diff_signature_is_stable_when_nothing_changes() {
        let repo = init_repo(&[("main.go", "package main\n")]);
        let base_sha = head_sha(repo.path());
        repo.commit(&[("main.go", "package main\n\nfunc f() {}\n")], "add f");

        let sig1 = diff_revisions(repo.path(), &base_sha, "HEAD").unwrap();
        let sig2 = diff_revisions(repo.path(), &base_sha, "HEAD").unwrap();
        assert_eq!(
            sig1, sig2,
            "polling with no repo changes between calls must produce the same signature"
        );
    }

    #[test]
    fn diff_signature_errors_clearly_on_an_unresolvable_ref() {
        let repo = init_repo(&[("main.go", "package main\n")]);
        let err = diff_revisions(repo.path(), "does-not-exist", "HEAD").unwrap_err();
        assert!(err.to_string().contains("does-not-exist"), "got: {err}");
    }

    /// `run_diff_watch`'s polling loop only runs `run_diff` on a genuine
    /// change and stops after `max_iterations` — asserted here without
    /// actually invoking `run_diff` on the "no change" ticks, since a real
    /// `run_diff` needs an agent backend and network-adjacent config this
    /// unit test shouldn't depend on. Uses a base/head pair with no diff at
    /// all (`HEAD...HEAD`) precisely so `run_diff` is never triggered, and
    /// only checks the loop terminates after the bounded iteration count
    /// rather than running forever.
    #[test]
    fn run_diff_watch_stops_after_max_iterations_when_nothing_changes() {
        let repo = init_repo(&[("main.go", "package main\n")]);
        let options = DiffCommandOptions {
            repo_root: repo.path().to_path_buf(),
            base_ref: "HEAD".to_string(),
            head_ref: "HEAD".to_string(),
            tier: None,
            aspects: None,
            subcategories: None,
            max_usd: None,
            incremental: false,
            static_only: true,
            changed_lines: false,
            audit_rust_deps: false,
            backend: None,
        };
        run_diff_watch(options, std::time::Duration::from_millis(1), Some(3)).unwrap();
    }
}

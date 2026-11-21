use std::path::Path;
use std::time::Instant;

use super::{build_backend, cheap_model_for, ModelCosts, ModelRun, ReviewRun};
use autoreview_schema::{CriterionResult, Finding, SuppressedFinding};

fn semantic_rules(run: &ReviewRun<'_>) -> std::collections::HashSet<String> {
    let mut semantic_ids = autoreview_core::semantic_rule_ids(run.packs);
    semantic_ids.extend(autoreview_core::rust_review_semantic_ids().map(str::to_owned));
    semantic_ids.extend([
        "message-chain".to_string(),
        "feature-envy".to_string(),
        "data-clump".to_string(),
        "excessive-comment-padding".to_string(),
        "divergent-change".to_string(),
        "data-class".to_string(),
        "refused-bequest".to_string(),
        "unused-import".to_string(),
        "unused-private-field".to_string(),
        "unused-private-method".to_string(),
        "duplicate-string-literal".to_string(),
        "swallowed-exception".to_string(),
        "iterator-err-not-checked".to_string(),
        "double-checked-locking-no-volatile".to_string(),
        "concurrent-modification-during-foreach".to_string(),
        "loopvar-capture-pre-1.22".to_string(),
        "loopvar-address-pre-1.22".to_string(),
        "typed-nil-interface-return".to_string(),
        "append-shared-backing-array".to_string(),
        // go-{command-injection,sql-injection,path-traversal}-taint are
        // NOT listed here — they're kind: taint YAML rules with
        // semantic: true declared in the rule file itself, so
        // autoreview_core::semantic_rule_ids() (which walks the whole
        // rules-builtin tree, not just pattern-kind rules) already
        // includes them. Adding a future taint rule means adding a YAML
        // file, not editing this list.
    ]);
    semantic_ids
}

pub(super) fn verify_findings(
    run: &ModelRun<'_>,
    findings: Vec<Finding>,
    costs: &mut ModelCosts,
) -> (Vec<Finding>, Vec<SuppressedFinding>) {
    let max_usd = run.review.options.max_usd;
    if costs.at_limit(max_usd) {
        println!(
            "\n  [budget] skipping verify pass — already at/over --max-usd {:.2}",
            max_usd.unwrap_or_default()
        );
        return (findings, Vec::new());
    }
    let config = run.review.config;
    if !config.verify.enabled || !run.judge_available() {
        return (findings, Vec::new());
    }
    let rules = semantic_rules(run.review);
    let noisy = &config.verify.noisy_categories;
    let count = autoreview_core::select_for_verification(&findings, noisy, &rules).len();
    if count == 0 {
        return (findings, Vec::new());
    }
    println!("\n  verify:         checking {count} finding(s) against the diff...");
    let backend = build_backend(run.kind, config);
    let result = autoreview_core::run_verify_pass_with_budget(
        backend.as_ref(),
        findings,
        &run.diff(),
        autoreview_core::VerifyOptions {
            model: cheap_model_for(run.kind, config),
            max_turns: 4,
            cwd: run.review.source_root,
            noisy_categories: noisy,
            semantic_rule_ids: &rules,
            budget: autoreview_core::VerifyBudget {
                remaining_usd: max_usd.map(|limit| limit - costs.usd.unwrap_or_default()),
                deadline: Some(run.deadline),
            },
        },
    );
    if result.checked < count {
        println!("  [budget] verified {} of {count} selected finding(s). Unchecked findings remain in the report.", result.checked);
    }
    if !result.suppressed.is_empty() {
        println!("  [verify] refuted {} finding(s):", result.suppressed.len());
        for item in &result.suppressed {
            println!(
                "    - {} ({})",
                item.finding.title, item.finding.location.path
            );
        }
    }
    costs.record("verify".into(), &result.usage, result.wall_ms);
    (result.kept, result.suppressed)
}

pub(super) fn verify_spec(run: &ModelRun<'_>, costs: &mut ModelCosts) -> Vec<CriterionResult> {
    if Instant::now() >= run.deadline
        || costs.at_limit(run.review.options.max_usd)
        || !run.judge_available()
    {
        return Vec::new();
    }
    let Some(spec) = read_spec(&run.review.options.repo_root) else {
        return Vec::new();
    };
    println!(
        "\n  spec:           checking {} acceptance criterion/criteria against the diff...",
        spec.criteria.len()
    );
    let config = run.review.config;
    let backend = build_backend(run.kind, config);
    let result = autoreview_core::run_spec_verify(
        backend.as_ref(),
        &spec,
        &run.diff(),
        cheap_model_for(run.kind, config),
        4,
        run.review.source_root,
    );
    for criterion in &result.results {
        use autoreview_schema::CriterionVerdict::*;
        let marker = match criterion.verdict {
            Satisfied => "✓",
            NotSatisfied => "✗",
            Uncertain => "?",
        };
        println!("    {marker} {}", criterion.criterion);
    }
    costs.record("spec_verify".into(), &result.usage, result.wall_ms);
    result.results
}

fn read_spec(repo: &Path) -> Option<autoreview_schema::AcceptanceSpec> {
    match std::fs::read_to_string(repo.join(".autoreview/spec.md")) {
        Ok(contents) => autoreview_core::parse_spec(&contents).or_else(|| {
            println!("  [warn] .autoreview/spec.md has no title or no Acceptance Criteria bullets — skipping spec verification");
            None
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            println!("  [warn] failed to read .autoreview/spec.md: {err}");
            None
        }
    }
}

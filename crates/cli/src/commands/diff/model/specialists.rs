use std::time::Instant;

use autoreview_core::{
    assign_fingerprints, collect_context, compile_skill, render_context_block, run_specialist,
    to_finding, AgentBackend, InvokeRequest, SpecialistResult, SpecialistStatus,
};
use autoreview_schema::{
    AgentBackendKind, AgentFinding, AutoreviewConfig, Finding, FindingSource, FindingSourceKind,
    PatchSafety, SpecialistPlanEntry, Suggestion, Tier,
};

use super::super::super::backend::{backend_available, backend_label, build_backend};
use super::{ModelCosts, ModelRun, ReviewRun};

pub(super) fn run_specialists(
    run: &ModelRun<'_>,
    static_findings: &[Finding],
    costs: &mut ModelCosts,
) -> Vec<Finding> {
    if run.review.options.static_only {
        println!("  [info] Static-only review. Model stages are disabled.");
        return Vec::new();
    }
    if run.plan.specialists.is_empty() {
        println!(
            "  [info] No specialists triggered for this diff at tier '{}'.",
            run.plan.tier
        );
        return Vec::new();
    }
    let context = specialist_context(run.review);
    if !backend_available(run.kind, run.review.config) {
        let aspects = run
            .plan
            .specialists
            .iter()
            .map(|s| s.aspect.as_str())
            .collect::<Vec<_>>();
        println!("  [warn] backend '{}' was not found/reachable — skipping {} planned specialist(s): {}. Run `autoreview doctor` to check required tools.",
            backend_label(run.kind), aspects.len(), aspects.join(", "));
        return Vec::new();
    }
    println!(
        "\n  running {} specialist(s) at tier '{}' via backend '{}':",
        run.plan.specialists.len(),
        run.plan.tier,
        backend_label(run.kind)
    );
    let backend = build_backend(run.kind, run.review.config);
    let inputs = SpecialistInputs {
        run,
        static_findings,
        diff: run.diff(),
        context,
    };
    collect_specialist_batches(&inputs, backend.as_ref(), costs)
}

fn specialist_context(run: &ReviewRun<'_>) -> String {
    let items = collect_context(run.source_root, run.facts, &run.config.context.providers);
    let labels = items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    let names = if labels.is_empty() {
        String::new()
    } else {
        format!(" ({})", labels.join(", "))
    };
    println!("  context:        {} item(s){names}", items.len());
    render_context_block(&items)
}

struct SpecialistInputs<'a> {
    run: &'a ModelRun<'a>,
    static_findings: &'a [Finding],
    diff: String,
    context: String,
}

fn collect_specialist_batches(
    inputs: &SpecialistInputs<'_>,
    backend: &(dyn AgentBackend + Sync),
    costs: &mut ModelCosts,
) -> Vec<Finding> {
    let run = inputs.run;
    let tiers = &run.review.config.budgets.tiers;
    let max_concurrency = match run.plan.tier {
        Tier::Quick => tiers.quick.max_concurrency,
        Tier::Standard => tiers.standard.max_concurrency,
        Tier::Deep => tiers.deep.max_concurrency,
    } as usize;
    let mut findings = Vec::new();
    for (index, chunk) in run
        .plan
        .specialists
        .chunks(max_concurrency.max(1))
        .enumerate()
    {
        if let Some(reason) = stop_reason(run, costs) {
            let skipped = run
                .plan
                .specialists
                .iter()
                .skip(index * max_concurrency.max(1))
                .map(|s| s.aspect.as_str())
                .collect::<Vec<_>>();
            println!(
                "\n  [budget] stopped — {reason}; skipped {} remaining specialist(s): {}",
                skipped.len(),
                skipped.join(", ")
            );
            break;
        }
        for (aspect, result) in dispatch_batch(inputs, backend, chunk) {
            record_specialist(&mut findings, costs, &aspect, result, run.kind);
        }
    }
    findings
}

fn stop_reason(run: &ModelRun<'_>, costs: &ModelCosts) -> Option<String> {
    let limit = run.review.options.max_usd;
    if costs.at_limit(limit) {
        return Some(format!(
            "spent ${:.4} of the --max-usd {:.2} cap",
            costs.usd.unwrap_or_default(),
            limit.unwrap_or_default()
        ));
    }
    (Instant::now() >= run.deadline).then(|| {
        format!(
            "hit the {}s wallClockSec budget for this tier",
            run.plan.budgets.wall_clock_sec
        )
    })
}

type SpecialistOutcome = (String, anyhow::Result<SpecialistResult>);

fn dispatch_batch(
    inputs: &SpecialistInputs<'_>,
    backend: &(dyn AgentBackend + Sync),
    batch: &[SpecialistPlanEntry],
) -> Vec<SpecialistOutcome> {
    std::thread::scope(|scope| {
        let handles = batch
            .iter()
            .map(|specialist| {
                scope.spawn(move || {
                    (
                        specialist.aspect.clone(),
                        run_one_specialist(backend, inputs, specialist),
                    )
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("specialist thread panicked"))
            .collect()
    })
}

fn record_specialist(
    findings: &mut Vec<Finding>,
    costs: &mut ModelCosts,
    aspect: &str,
    result: anyhow::Result<SpecialistResult>,
    kind: AgentBackendKind,
) {
    let mut specialist = match result {
        Ok(specialist) => specialist,
        Err(err) => {
            println!("    ✗ {aspect:<12} error: {err}");
            return;
        }
    };
    costs.record(
        format!("agent:{aspect}"),
        &specialist.usage,
        specialist.wall_ms,
    );
    if let SpecialistStatus::Failed { reason } = &specialist.status {
        println!("    ✗ {aspect:<12} failed: {reason}");
        return;
    }
    for finding in &mut specialist.findings {
        normalize_specialist_finding(finding, aspect, kind);
    }
    println!(
        "    ✓ {aspect:<12} {} finding(s)",
        specialist.findings.len()
    );
    findings.extend(
        assign_fingerprints(specialist.findings)
            .into_iter()
            .map(to_finding),
    );
}

/// The model to invoke a specialist with. For the local-LLM and
/// OpenAI-compatible backends this is always the one configured model,
/// overriding whatever tier-based alias `plan_review` resolved (Claude
/// Code model aliases like "haiku"/"sonnet" mean nothing to either of
/// them).
fn specialist_model_for(
    kind: AgentBackendKind,
    config: &AutoreviewConfig,
    planned_model: &str,
) -> String {
    match kind {
        AgentBackendKind::LocalLlm => config.agents.local_llm.model.clone(),
        AgentBackendKind::OpenAiCompatible => config.agents.open_ai_compatible.model.clone(),
        AgentBackendKind::ClaudeCode | AgentBackendKind::Pi => planned_model.to_string(),
    }
}

fn stage1_summary_for_category(stage1_findings: &[Finding], category: &str) -> String {
    let matching: Vec<&Finding> = stage1_findings
        .iter()
        .filter(|f| f.category == category)
        .collect();
    if matching.is_empty() {
        return "(none)".to_string();
    }
    matching
        .iter()
        .map(|f| {
            format!(
                "- [{}] {}:{} — {}",
                f.source
                    .rule_id
                    .as_deref()
                    .unwrap_or(f.source.tool.as_str()),
                f.location.path,
                f.location.range.start_line,
                f.title
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_specialist_finding(
    finding: &mut AgentFinding,
    aspect: &str,
    backend: AgentBackendKind,
) {
    let tool = match backend {
        AgentBackendKind::ClaudeCode => "claude-code",
        other => backend_label(other),
    };
    finding.source = FindingSource {
        kind: FindingSourceKind::Agent,
        tool: tool.into(),
        rule_id: None,
        aspect: Some(aspect.into()),
        backend: Some(tool.into()),
    };
    if let Some(meta) = &mut finding.meta {
        meta.remove("rulePackId");
    }
    finding.confidence = finding.confidence.clamp(0.0, 1.0);
    let patch = finding
        .suggested_patch
        .take()
        .filter(|patch| !patch.trim().is_empty());
    if let Some(suggestion) = &mut finding.suggestion {
        suggestion.safety = PatchSafety::NeedsReview;
        if suggestion.patch.is_none() {
            suggestion.patch = patch;
        }
    } else if patch.is_some() {
        finding.suggestion = Some(Suggestion {
            description: "Suggested patch from the reviewer".into(),
            patch,
            safety: PatchSafety::NeedsReview,
        });
    }
}

fn run_one_specialist(
    backend: &dyn AgentBackend,
    inputs: &SpecialistInputs<'_>,
    specialist: &SpecialistPlanEntry,
) -> anyhow::Result<SpecialistResult> {
    let run = inputs.run;
    let compiled = compile_skill(
        &run.review.options.repo_root,
        &specialist.aspect,
        run.plan.tier,
    )?;
    let mut allowed_tools = Vec::new();
    if compiled.manifest.tools.read {
        allowed_tools.push("Read".to_string());
    }
    if compiled.manifest.tools.grep {
        allowed_tools.push("Grep".to_string());
        allowed_tools.push("Glob".to_string());
    }
    for bash_cmd in &compiled.manifest.tools.bash {
        allowed_tools.push(format!("Bash({bash_cmd}:*)"));
    }

    let task_prompt = format!(
        "Review the following diff for {} issues.\n\nProject context:\n{}\n\nStage-1 deterministic findings already reported for this aspect (do not re-report these):\n{}\n\n```diff\n{}\n```",
        specialist.aspect, inputs.context, stage1_summary_for_category(inputs.static_findings, &specialist.aspect), inputs.diff
    );

    let base_request = InvokeRequest {
        prompt: String::new(),
        system_prompt: compiled.system_prompt,
        allowed_tools,
        max_turns: specialist.max_turns,
        model: specialist_model_for(run.kind, run.review.config, &specialist.model),
        cwd: run.review.source_root.to_path_buf(),
    };

    Ok(run_specialist(
        backend,
        &specialist.aspect,
        base_request,
        &task_prompt,
    ))
}

#[cfg(test)]
mod specialist_tests {
    use super::*;

    #[test]
    fn records_reviewer_provenance_and_preserves_patch_for_explicit_review() {
        let mut finding: AgentFinding = serde_json::from_value(serde_json::json!({
            "source": {"kind":"analyzer", "tool":"clippy", "ruleId":"unrelated"},
            "category": "correctness", "severity": "high", "confidence": 2.0,
            "title": "Report", "message": "Fix the failing comparison",
            "location": {"path": "main.rs", "range":{"startLine": 1}, "side": "new", "snippet": "fn main() {}"},
            "suggestedPatch": "test patch",
            "meta": {"rulePackId": "incorrect"}
        })).unwrap();
        normalize_specialist_finding(&mut finding, "correctness", AgentBackendKind::Pi);
        assert!(matches!(finding.source.kind, FindingSourceKind::Agent));
        assert_eq!(finding.source.tool, "pi");
        assert_eq!(finding.source.aspect.as_deref(), Some("correctness"));
        assert!(finding.source.rule_id.is_none());
        assert!(finding.meta.unwrap().is_empty());
        assert_eq!(finding.confidence, 1.0);
        let suggestion = finding.suggestion.unwrap();
        assert_eq!(suggestion.patch.as_deref(), Some("test patch"));
        assert_eq!(suggestion.safety, PatchSafety::NeedsReview);
    }
}

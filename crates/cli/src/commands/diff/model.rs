mod judges;
mod specialists;

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use autoreview_core::{plan_review, PlanOverrides, Usage};
use autoreview_schema::{
    AgentBackendKind, CostEntry, CriterionResult, Finding, ReviewPlan, RunCosts, SkillManifest,
    SuppressedFinding, Tier,
};

use super::super::backend::{backend_available, build_backend};
use super::super::rules::cheap_model_for;
use super::ReviewRun;

pub(super) struct ModelReview {
    pub plan: ReviewPlan,
    pub findings: Vec<Finding>,
    pub suppressed: Vec<SuppressedFinding>,
    pub criteria: Vec<CriterionResult>,
    pub costs: RunCosts,
}

#[derive(Default)]
struct ModelCosts {
    stages: HashMap<String, CostEntry>,
    input_tokens: u64,
    output_tokens: u64,
    wall_ms: u64,
    usd: Option<f64>,
}

impl ModelCosts {
    fn record(&mut self, stage: String, usage: &Usage, wall_ms: u64) {
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.wall_ms += wall_ms;
        if let Some(cost) = usage.usd {
            self.usd = Some(self.usd.unwrap_or_default() + cost);
        }
        self.stages.insert(
            stage,
            CostEntry {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                usd: usage.usd,
                wall_ms,
            },
        );
    }

    fn at_limit(&self, limit: Option<f64>) -> bool {
        autoreview_core::should_stop_for_budget(
            self.usd.unwrap_or_default(),
            self.usd.is_some(),
            limit,
        )
    }

    fn finish(self) -> RunCosts {
        RunCosts {
            total: CostEntry {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                usd: self.usd,
                wall_ms: self.wall_ms,
            },
            per_stage: self.stages,
        }
    }
}

struct ModelRun<'a> {
    review: &'a ReviewRun<'a>,
    kind: AgentBackendKind,
    plan: &'a ReviewPlan,
    deadline: Instant,
}

impl ModelRun<'_> {
    fn diff(&self) -> String {
        let review = self.review;
        diff_context(
            &review.options.repo_root,
            &review.facts.base_ref,
            &review.facts.head_ref,
            &review.facts.files,
        )
    }

    fn judge_available(&self) -> bool {
        !self.review.options.static_only
            && self.plan.tier != Tier::Quick
            && backend_available(self.kind, self.review.config)
    }
}

pub(super) fn review(
    run: &ReviewRun<'_>,
    skills: &[SkillManifest],
    static_findings: Vec<Finding>,
) -> ModelReview {
    let started = Instant::now();
    let kind = run.options.backend.unwrap_or(run.config.agents.backend);
    let mut costs = ModelCosts::default();
    let classified_tier = classify(run, kind, static_findings.len(), &mut costs);
    let mut plan = plan_review(
        run.facts,
        run.config,
        skills,
        static_findings.len(),
        PlanOverrides {
            tier: run.options.tier,
            classified_tier,
            aspects: run.options.aspects.clone(),
            max_usd: run.options.max_usd,
        },
    );
    configure_scope(&mut plan, run);
    print_plan(&plan);
    let deadline = started + Duration::from_secs(u64::from(plan.budgets.wall_clock_sec));
    let model = ModelRun {
        review: run,
        kind,
        plan: &plan,
        deadline,
    };
    let agent_findings = specialists::run_specialists(&model, &static_findings, &mut costs);
    let mut findings = static_findings;
    findings.extend(agent_findings);
    let (findings, suppressed) = judges::verify_findings(&model, findings, &mut costs);
    let criteria = judges::verify_spec(&model, &mut costs);
    ModelReview {
        plan,
        findings,
        suppressed,
        criteria,
        costs: costs.finish(),
    }
}

fn classify(
    run: &ReviewRun<'_>,
    kind: AgentBackendKind,
    finding_count: usize,
    costs: &mut ModelCosts,
) -> Option<Tier> {
    if run.options.static_only || run.options.tier.is_some() || !backend_available(kind, run.config)
    {
        return None;
    }
    let (score, _) = autoreview_core::score_diff_facts(run.facts, run.config, finding_count);
    let bounds = autoreview_core::ambiguous_tier_boundary(score, run.config)?;
    let backend = build_backend(kind, run.config);
    let (tier, usage, wall_ms) = autoreview_core::classify_ambiguous_tier_with_usage(
        backend.as_ref(),
        run.facts,
        (score, bounds),
        cheap_model_for(kind, run.config),
        run.source_root,
    );
    costs.record("triage".into(), &usage, wall_ms);
    if let Some(tier) = tier {
        let (lower, upper) = bounds;
        println!("  [triage] score {score:.1} is ambiguous between {lower} and {upper} — classifier picked '{tier}'");
    }
    tier
}

fn configure_scope(plan: &mut ReviewPlan, run: &ReviewRun<'_>) {
    if run.options.static_only {
        plan.specialists.clear();
        plan.overrides.push("--static-only".into());
    }
    if run.options.changed_lines {
        plan.overrides.push("--changed-lines".into());
    }
}

fn print_plan(plan: &ReviewPlan) {
    println!(
        "  triage score:   {:.1}  -> tier: {}",
        plan.score, plan.tier
    );
    for signal in &plan.signals {
        let detail = signal
            .detail
            .as_ref()
            .map(|d| format!("  ({d})"))
            .unwrap_or_default();
        println!("    + {:<20} {:.1}{}", signal.signal, signal.points, detail);
    }
    println!(
        "  budget:         maxAgents={} totalTokenCap={} wallClockSec={}",
        plan.budgets.max_agents, plan.budgets.total_token_cap, plan.budgets.wall_clock_sec
    );
    if !plan.overrides.is_empty() {
        println!("  overrides:      {}", plan.overrides.join(" "));
    }
}

const MAX_INLINE_DIFF_CHARS: usize = 20_000;

/// The diff text handed to specialists: inline if small enough, otherwise
/// just the changed-file list (the specialist can still Read/Grep the repo
/// itself for anything it needs — this only bounds what's pushed into the
/// prompt up front, per the plan's "inline up to a threshold, paths beyond it").
fn diff_context(
    repo_root: &Path,
    base_ref: &str,
    head_ref: &str,
    files: &[autoreview_core::FileChange],
) -> String {
    let output = Command::new("git")
        .args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            &format!("{base_ref}...{head_ref}"),
            "--",
        ])
        .current_dir(repo_root)
        .output();
    if let Ok(output) = output {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout).to_string();
            if text.chars().count() <= MAX_INLINE_DIFF_CHARS {
                return text;
            }
        }
    }
    let file_list: String = files
        .iter()
        .map(|f| format!("- {} (+{} -{})", f.path, f.additions, f.deletions))
        .collect::<Vec<_>>()
        .join("\n");
    format!("The diff is too large to inline here. Changed files:\n{file_list}\n\nUse Read/Grep to inspect the files you need.")
}

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use autoreview_core::{
    append_event_log, assign_fingerprints, collect_context, collect_diff_facts, compile_skill,
    dedupe_exact, dedupe_fuzzy, discover_manifests, events_from_report, load_config, plan_review,
    render_context_block, run_ast_grep, run_golangci_lint, run_specialist, to_finding,
    AgentBackend, ClaudeCodeBackend, HistoryStore, InvokeRequest, PlanOverrides, SpecialistStatus,
};
use autoreview_schema::{
    CostEntry, DiffStats, Finding, ReviewReport, ReviewSummary, ReviewTarget, RunCosts, Tier,
};

use super::history::{history_dir_for, hostname};

pub struct DiffCommandOptions {
    pub repo_root: PathBuf,
    pub base_ref: String,
    pub head_ref: String,
    pub tier: Option<Tier>,
    pub aspects: Option<Vec<String>>,
    pub max_usd: Option<f64>,
    pub incremental: bool,
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
        .args(["diff", &format!("{base_ref}...{head_ref}")])
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

fn claude_available() -> bool {
    Command::new("claude")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A short, model-readable summary of Stage-1 findings for a given category,
/// so specialists know what's already been flagged deterministically and
/// don't waste turns re-reporting it (per the plan: "don't re-report lint").
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

pub fn run_diff(options: DiffCommandOptions) -> anyhow::Result<()> {
    let repo_root_str = options.repo_root.to_string_lossy().to_string();
    let config = load_config(&options.repo_root.join(".autoreview").join("config.yaml"))?;
    let facts = collect_diff_facts(&repo_root_str, &options.base_ref, &options.head_ref, None)?;
    let skills = discover_manifests(&options.repo_root)?;

    println!(
        "autoreview diff  ({}...{})\n",
        options.base_ref, options.head_ref
    );
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

    // Stage 1: deterministic analyzers, always run (near-free), before Stage 2
    // triage so analyzer finding density can factor into the tier decision.
    let changed_file_paths: Vec<String> = facts.files.iter().map(|f| f.path.clone()).collect();
    let mut stage1_agent_findings = Vec::new();
    match run_ast_grep(&options.repo_root, &changed_file_paths) {
        Ok(findings) => stage1_agent_findings.extend(findings),
        Err(err) => println!("  [warn] ast-grep run failed: {err}"),
    }
    match run_golangci_lint(&options.repo_root, &changed_file_paths) {
        Ok(findings) => stage1_agent_findings.extend(findings),
        Err(err) => println!("  [warn] golangci-lint run failed: {err}"),
    }
    stage1_agent_findings.extend(autoreview_core::run_duplication_check(&options.repo_root, &changed_file_paths));
    let stage1_finding_count = stage1_agent_findings.len();
    let stage1_findings: Vec<Finding> = assign_fingerprints(stage1_agent_findings)
        .into_iter()
        .map(to_finding)
        .collect();
    println!("  stage 1:        {stage1_finding_count} deterministic finding(s) (ast-grep + golangci-lint + duplication)");

    let overrides = PlanOverrides {
        tier: options.tier,
        aspects: options.aspects.clone(),
        max_usd: options.max_usd,
    };
    let plan = plan_review(&facts, &config, &skills, stage1_finding_count, overrides);

    println!(
        "  triage score:   {:.1}  -> tier: {}",
        plan.score, plan.tier
    );
    for s in &plan.signals {
        let detail = s
            .detail
            .as_ref()
            .map(|d| format!("  ({d})"))
            .unwrap_or_default();
        println!("    + {:<20} {:.1}{}", s.signal, s.points, detail);
    }
    println!(
        "  budget:         maxAgents={} totalTokenCap={} wallClockSec={}",
        plan.budgets.max_agents, plan.budgets.total_token_cap, plan.budgets.wall_clock_sec
    );
    if !plan.overrides.is_empty() {
        println!("  overrides:      {}", plan.overrides.join(" "));
    }

    let mut findings = stage1_findings.clone();
    let mut per_stage_costs: HashMap<String, CostEntry> = HashMap::new();
    let mut total_input_tokens = 0u64;
    let mut total_output_tokens = 0u64;
    let mut total_wall_ms = 0u64;

    if plan.specialists.is_empty() {
        println!(
            "  [info] No specialists triggered for this diff at tier '{}'.",
            plan.tier
        );
    } else {
        // Collected up front (and reported) regardless of whether `claude`
        // ends up being available, so `autoreview diff` is still useful as a
        // diagnostic of what context *would* be sent even when specialists
        // can't run.
        let context_items = collect_context(&options.repo_root, &facts, &config.context.providers);
        println!(
            "  context:        {} item(s){}",
            context_items.len(),
            if context_items.is_empty() {
                String::new()
            } else {
                format!(
                    " ({})",
                    context_items
                        .iter()
                        .map(|i| i.label.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        );
        let context_block = render_context_block(&context_items);

        if !claude_available() {
            println!(
                "  [warn] `claude` was not found on PATH — skipping {} planned specialist(s): {}. Run `autoreview doctor` to check required tools.",
                plan.specialists.len(),
                plan.specialists.iter().map(|s| s.aspect.as_str()).collect::<Vec<_>>().join(", ")
            );
        } else {
            println!(
                "\n  running {} specialist(s) at tier '{}':",
                plan.specialists.len(),
                plan.tier
            );
            let diff_text = diff_context(
                &options.repo_root,
                &options.base_ref,
                &options.head_ref,
                &facts.files,
            );
            let backend = ClaudeCodeBackend::default();
            let max_concurrency = match plan.tier {
                Tier::Quick => config.budgets.tiers.quick.max_concurrency,
                Tier::Standard => config.budgets.tiers.standard.max_concurrency,
                Tier::Deep => config.budgets.tiers.deep.max_concurrency,
            } as usize;

            for chunk in plan.specialists.chunks(max_concurrency.max(1)) {
                let results: Vec<_> = std::thread::scope(|scope| {
                    let handles: Vec<_> = chunk
                        .iter()
                        .map(|specialist| {
                            let repo_root = options.repo_root.clone();
                            let diff_text = diff_text.clone();
                            let context_block = context_block.clone();
                            let stage1_summary =
                                stage1_summary_for_category(&stage1_findings, &specialist.aspect);
                            let backend_ref: &(dyn AgentBackend + Sync) = &backend;
                            scope.spawn(move || {
                                run_one_specialist(
                                    backend_ref,
                                    &repo_root,
                                    plan.tier,
                                    specialist,
                                    &diff_text,
                                    &stage1_summary,
                                    &context_block,
                                )
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .map(|h| h.join().expect("specialist thread panicked"))
                        .collect()
                });

                for (aspect, result) in results {
                    match result {
                        Ok(specialist_result) => {
                            total_input_tokens += specialist_result.usage.input_tokens;
                            total_output_tokens += specialist_result.usage.output_tokens;
                            total_wall_ms += specialist_result.wall_ms;
                            per_stage_costs.insert(
                                format!("agent:{aspect}"),
                                CostEntry {
                                    input_tokens: specialist_result.usage.input_tokens,
                                    output_tokens: specialist_result.usage.output_tokens,
                                    usd: None,
                                    wall_ms: specialist_result.wall_ms,
                                },
                            );
                            match &specialist_result.status {
                                SpecialistStatus::Ok => {
                                    println!(
                                        "    ✓ {aspect:<12} {} finding(s)",
                                        specialist_result.findings.len()
                                    );
                                    let fingerprinted =
                                        assign_fingerprints(specialist_result.findings);
                                    findings.extend(fingerprinted.into_iter().map(to_finding));
                                }
                                SpecialistStatus::Failed { reason } => {
                                    println!("    ✗ {aspect:<12} failed: {reason}");
                                }
                            }
                        }
                        Err(err) => println!("    ✗ {aspect:<12} error: {err}"),
                    }
                }
            }
        }
    }

    // Stage 3.5: verify pass. Budget-gated (skipped entirely in quick tier,
    // per the plan) and skipped when there's nothing this pass would even
    // look at, so a diff with no high/blocker or noisy-category findings
    // never pays for a judge call it doesn't need.
    let mut verify_suppressed = Vec::new();
    if plan.tier != Tier::Quick && config.verify.enabled && claude_available() {
        let to_check = autoreview_core::select_for_verification(&findings, &config.verify.noisy_categories).len();
        if to_check > 0 {
            println!("\n  verify:         checking {to_check} finding(s) against the diff...");
            let diff_text = diff_context(&options.repo_root, &options.base_ref, &options.head_ref, &facts.files);
            let backend = ClaudeCodeBackend::default();
            let verify_result = autoreview_core::run_verify_pass(
                &backend,
                findings,
                &diff_text,
                &config.budgets.models.cheap,
                4,
                &options.repo_root,
                &config.verify.noisy_categories,
            );
            findings = verify_result.kept;
            if !verify_result.suppressed.is_empty() {
                println!("  [verify] refuted {} finding(s):", verify_result.suppressed.len());
                for s in &verify_result.suppressed {
                    println!("    - {} ({})", s.finding.title, s.finding.location.path);
                }
            }
            total_input_tokens += verify_result.usage.input_tokens;
            total_output_tokens += verify_result.usage.output_tokens;
            total_wall_ms += verify_result.wall_ms;
            per_stage_costs.insert(
                "verify".to_string(),
                CostEntry { input_tokens: verify_result.usage.input_tokens, output_tokens: verify_result.usage.output_tokens, usd: None, wall_ms: verify_result.wall_ms },
            );
            verify_suppressed = verify_result.suppressed;
        }
    }

    let exact_result = dedupe_exact(findings);
    // Fuzzy dedupe runs second, over what exact dedupe left behind — it's
    // the one that catches a Stage-1 analyzer and a Stage-3 specialist
    // independently flagging the same underlying issue with different rule
    // keys/wording, which exact fingerprint matching structurally can't see.
    let mut dedupe_result = dedupe_fuzzy(exact_result.findings, 3, 0.55);
    dedupe_result.suppressed.extend(exact_result.suppressed);
    dedupe_result.suppressed.extend(verify_suppressed);
    if !dedupe_result.suppressed.is_empty() {
        println!(
            "\n  [dedupe] suppressed {} duplicate/refuted finding(s)",
            dedupe_result.suppressed.len()
        );
    }

    let history_dir = history_dir_for(&options.repo_root);

    // Incremental mode: suppress findings already reported in the most
    // recent prior run on this repo (queried *before* this run's own
    // findings are recorded, further down, so it never sees itself). This is
    // opt-in — it exists to cut repeat noise across successive `diff` runs
    // against the same evolving branch, not to hide anything by default.
    if options.incremental {
        let baseline = HistoryStore::open(&history_dir)
            .and_then(|store| match store.most_recent_run_id()? {
                Some(run_id) => store.fingerprints_for_run(&run_id),
                None => Ok(std::collections::HashSet::new()),
            })
            .unwrap_or_else(|err| {
                println!("  [warn] --incremental: failed to read baseline from history: {err}");
                std::collections::HashSet::new()
            });
        if !baseline.is_empty() {
            let (new_findings, already_known): (Vec<_>, Vec<_>) = dedupe_result.findings.into_iter().partition(|f| !baseline.contains(&f.fingerprints.primary));
            if !already_known.is_empty() {
                println!("\n  [incremental] suppressed {} finding(s) already reported in the previous run", already_known.len());
            }
            dedupe_result.findings = new_findings;
            dedupe_result.suppressed.extend(already_known.into_iter().map(|finding| autoreview_schema::SuppressedFinding { finding, reason: autoreview_schema::SuppressedReason::Baseline }));
        }
    }

    for finding in &dedupe_result.findings {
        println!(
            "\n  [{:?}] {} ({}) — {}",
            finding.severity, finding.title, finding.id, finding.location.path
        );
        println!("    {}", finding.message);
    }
    if !dedupe_result.findings.is_empty() {
        println!("\n  (use `autoreview feedback <id> --fp|--tp` to record feedback on a finding above)");
    }

    let run_id = uuid::Uuid::new_v4().to_string();
    let run_dir = history_dir.join("runs").join(&run_id);
    std::fs::create_dir_all(&run_dir)?;

    let additions: u32 = facts.files.iter().map(|f| f.additions).sum();
    let deletions: u32 = facts.files.iter().map(|f| f.deletions).sum();

    let mut by_severity: HashMap<String, u32> = HashMap::new();
    let mut by_category: HashMap<String, u32> = HashMap::new();
    for finding in &dedupe_result.findings {
        *by_severity
            .entry(format!("{:?}", finding.severity).to_lowercase())
            .or_insert(0) += 1;
        *by_category.entry(finding.category.clone()).or_insert(0) += 1;
    }

    let report = ReviewReport {
        schema_version: "1".to_string(),
        run_id: run_id.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        target: ReviewTarget {
            repo_root: repo_root_str,
            base_ref: options.base_ref.clone(),
            head_ref: options.head_ref.clone(),
            diff_stats: DiffStats {
                files: facts.files.len() as u32,
                additions,
                deletions,
                languages: facts.languages.clone(),
            },
        },
        plan,
        findings: dedupe_result.findings,
        suppressed: dedupe_result.suppressed,
        costs: RunCosts {
            total: CostEntry {
                input_tokens: total_input_tokens,
                output_tokens: total_output_tokens,
                usd: None,
                wall_ms: total_wall_ms,
            },
            per_stage: per_stage_costs,
        },
        summary: ReviewSummary {
            by_severity,
            by_category,
            gate: None,
        },
    };

    let report_path = run_dir.join("report.json");
    std::fs::write(&report_path, serde_json::to_string_pretty(&report)?)?;
    println!("\n  report written: {}", report_path.display());

    let markdown_path = run_dir.join("report.md");
    std::fs::write(&markdown_path, crate::render::render_markdown(&report))?;
    println!("  markdown written: {}", markdown_path.display());

    let sarif_path = run_dir.join("report.sarif");
    std::fs::write(&sarif_path, crate::render::render_sarif(&report))?;
    println!("  sarif written:  {}", sarif_path.display());

    let index_path = run_dir.join("index.md");
    std::fs::write(&index_path, crate::render::render_index(&report))?;
    println!("  index written:  {}", index_path.display());

    // Storage write path (M1 scope, per the plan): append this run's findings
    // to the local event log and index them in SQLite. Both are best-effort —
    // a failure here must never take down an otherwise-successful review.
    let host = hostname();
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let events = events_from_report(&report, &host);
    match append_event_log(&history_dir, &date, &host, &events) {
        Ok(path) => println!("  event log:      {} ({} event(s))", path.display(), events.len()),
        Err(err) => println!("  [warn] failed to append event log: {err}"),
    }
    match HistoryStore::open(&history_dir).and_then(|store| store.record_run(&report)) {
        Ok(()) => println!("  history index:  {}", history_dir.join("index.db").display()),
        Err(err) => println!("  [warn] failed to update history index: {err}"),
    }

    Ok(())
}

fn run_one_specialist(
    backend: &dyn AgentBackend,
    repo_root: &Path,
    tier: Tier,
    specialist: &autoreview_schema::SpecialistPlanEntry,
    diff_text: &str,
    stage1_summary: &str,
    context_block: &str,
) -> (String, anyhow::Result<autoreview_core::SpecialistResult>) {
    let aspect = specialist.aspect.clone();
    let result = (|| -> anyhow::Result<autoreview_core::SpecialistResult> {
        let compiled = compile_skill(repo_root, &specialist.aspect, tier)?;
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
            specialist.aspect, context_block, stage1_summary, diff_text
        );

        let base_request = InvokeRequest {
            prompt: String::new(),
            system_prompt: compiled.system_prompt,
            allowed_tools,
            max_turns: specialist.max_turns,
            model: specialist.model.clone(),
            cwd: repo_root.to_path_buf(),
        };

        Ok(run_specialist(
            backend,
            &specialist.aspect,
            base_request,
            &task_prompt,
        ))
    })();
    (aspect, result)
}

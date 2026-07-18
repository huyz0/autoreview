use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use autoreview_core::{
    append_event_log, assign_fingerprints, collect_context, collect_diff_facts, compile_skill,
    dedupe_exact, dedupe_fuzzy, discover_manifests, events_from_report, load_config, plan_review,
    render_context_block, run_ast_grep, run_golangci_lint, run_specialist, to_finding,
    AgentBackend, ClaudeCodeBackend, HistoryStore, InvokeRequest, LocalLlmBackend, PiBackend,
    PlanOverrides, SpecialistStatus,
};
use autoreview_schema::{
    AgentBackendKind, AutoreviewConfig, CostEntry, DiffStats, Finding, ReviewReport, ReviewSummary, ReviewTarget, RunCosts, Tier,
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
    pub backend: Option<AgentBackendKind>,
}

fn backend_label(kind: AgentBackendKind) -> &'static str {
    match kind {
        AgentBackendKind::ClaudeCode => "claude",
        AgentBackendKind::Pi => "pi",
        AgentBackendKind::LocalLlm => "local-llm",
    }
}

/// Availability check per backend, mirroring `claude --version` for the
/// other two: `pi --version` for pi, an HTTP reachability probe for the
/// local-LLM server (there's no CLI binary to version-check).
fn backend_available(kind: AgentBackendKind, config: &AutoreviewConfig) -> bool {
    match kind {
        AgentBackendKind::ClaudeCode => Command::new("claude").arg("--version").output().map(|o| o.status.success()).unwrap_or(false),
        AgentBackendKind::Pi => Command::new("pi").arg("--version").output().map(|o| o.status.success()).unwrap_or(false),
        AgentBackendKind::LocalLlm => autoreview_core::local_llm_available(&config.agents.local_llm.base_url, "curl"),
    }
}

fn build_backend(kind: AgentBackendKind, config: &AutoreviewConfig) -> Box<dyn AgentBackend + Sync> {
    match kind {
        AgentBackendKind::ClaudeCode => Box::new(ClaudeCodeBackend::default()),
        AgentBackendKind::Pi => Box::new(PiBackend { binary: "pi".to_string(), provider: config.agents.pi_provider.clone() }),
        AgentBackendKind::LocalLlm => Box::new(LocalLlmBackend { base_url: config.agents.local_llm.base_url.clone(), curl_binary: "curl".to_string() }),
    }
}

/// The cheap-tier model name to use for a given backend. The local-LLM
/// backend has exactly one served model (`agents.localLlm.model`) rather
/// than a cheap/standard/deep alias set — there's no "cheaper" model to fall
/// back to on a single local server, so it's used for every call regardless
/// of tier.
fn cheap_model_for(kind: AgentBackendKind, config: &AutoreviewConfig) -> &str {
    match kind {
        AgentBackendKind::LocalLlm => &config.agents.local_llm.model,
        AgentBackendKind::ClaudeCode | AgentBackendKind::Pi => &config.budgets.models.cheap,
    }
}

/// The model to invoke a specialist with. For the local-LLM backend this is
/// always the one served model, overriding whatever tier-based alias
/// `plan_review` resolved (Claude Code model aliases like "haiku"/"sonnet"
/// mean nothing to a local server).
fn specialist_model_for(kind: AgentBackendKind, config: &AutoreviewConfig, planned_model: &str) -> String {
    match kind {
        AgentBackendKind::LocalLlm => config.agents.local_llm.model.clone(),
        AgentBackendKind::ClaudeCode | AgentBackendKind::Pi => planned_model.to_string(),
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


/// A short, model-readable summary of Stage-1 findings for a given category,
/// so specialists know what's already been flagged deterministically and
/// don't waste turns re-reporting it (per the plan: "don't re-report lint").
/// Physically relocates a rule file between `.autoreview/rules/{from,to}/`
/// on promotion/demotion — best-effort (a failure here doesn't block the
/// status transition already recorded in the history store, it just means
/// the on-disk file needs a manual move next time someone looks).
fn move_shadow_rule_file(repo_root: &Path, rule_id: &str, from: &str, to: &str) {
    let rule_files = autoreview_core::discover_shadow_rule_files(repo_root);
    for rule_file in rule_files {
        if rule_file.status != from {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&rule_file.path) else { continue };
        if !contents.contains(&format!("id: {rule_id}")) {
            continue;
        }
        let dest_dir = repo_root.join(".autoreview").join("rules").join(to);
        if std::fs::create_dir_all(&dest_dir).is_err() {
            continue;
        }
        let dest = dest_dir.join(rule_file.path.file_name().unwrap());
        let _ = std::fs::rename(&rule_file.path, &dest);
        break;
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

pub fn run_diff(options: DiffCommandOptions) -> anyhow::Result<()> {
    let repo_root_str = options.repo_root.to_string_lossy().to_string();
    let config = load_config(&options.repo_root.join(".autoreview").join("config.yaml"))?;
    let facts = collect_diff_facts(&repo_root_str, &options.base_ref, &options.head_ref, None)?;
    let skills = discover_manifests(&options.repo_root)?;
    let backend_kind = options.backend.unwrap_or(config.agents.backend);

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
    match autoreview_core::run_clippy(&options.repo_root, &changed_file_paths) {
        Ok(findings) => stage1_agent_findings.extend(findings),
        Err(err) => println!("  [warn] clippy run failed: {err}"),
    }
    stage1_agent_findings.extend(autoreview_core::run_duplication_check(&options.repo_root, &changed_file_paths));
    stage1_agent_findings.extend(autoreview_core::run_cross_file_duplication_check(&options.repo_root, &changed_file_paths));
    stage1_agent_findings.extend(autoreview_core::run_complexity_check(&options.repo_root, &changed_file_paths));
    stage1_agent_findings.extend(autoreview_core::run_practices_check(&options.repo_root, &changed_file_paths));
    // Architecture layer check is opt-in: only runs when the repo has a
    // .autoreview/architecture.yaml declaring layers, per the plan ("no sane
    // generic default for what a repo's layers are").
    match autoreview_core::load_architecture_config(&options.repo_root.join(".autoreview").join("architecture.yaml")) {
        Ok(Some(arch_config)) => stage1_agent_findings.extend(autoreview_core::run_architecture_check(&options.repo_root, &changed_file_paths, &arch_config)),
        Ok(None) => {}
        Err(err) => println!("  [warn] failed to parse .autoreview/architecture.yaml: {err}"),
    }
    // archgraph (Tier 2): Go-only whole-repo import-cycle detection, a
    // no-op for non-Go-module repos or diffs that touch no .go files.
    stage1_agent_findings.extend(autoreview_core::run_archgraph_check(&options.repo_root, &changed_file_paths));
    stage1_agent_findings.extend(autoreview_core::run_symindex_check_with_tier4(&options.repo_root, &changed_file_paths, config.symindex.tier4_go));
    stage1_agent_findings.extend(autoreview_core::detect_shotgun_surgery(&facts.files));
    stage1_agent_findings.extend(autoreview_core::run_divergent_change_check(&options.repo_root, &changed_file_paths));
    let stage1_finding_count = stage1_agent_findings.len();
    let stage1_findings: Vec<Finding> = assign_fingerprints(stage1_agent_findings)
        .into_iter()
        .map(to_finding)
        .collect();
    println!("  stage 1:        {stage1_finding_count} deterministic finding(s) (ast-grep + golangci-lint + clippy + duplication + cross-file-duplication + complexity + practices + architecture + archgraph + symindex)");

    // LLM triage classifier (M2): only consulted when no explicit --tier was
    // given and the heuristic score itself landed within the ambiguity band
    // of a tier boundary — the common case never pays for this call.
    let mut classified_tier = None;
    if options.tier.is_none() && backend_available(backend_kind, &config) {
        let (heuristic_score, _) = autoreview_core::score_diff_facts(&facts, &config, stage1_finding_count);
        if let Some((lower, upper)) = autoreview_core::ambiguous_tier_boundary(heuristic_score, &config) {
            let backend = build_backend(backend_kind, &config);
            classified_tier = autoreview_core::classify_ambiguous_tier(backend.as_ref(), &facts, heuristic_score, lower, upper, cheap_model_for(backend_kind, &config), &options.repo_root);
            if let Some(tier) = classified_tier {
                println!("  [triage] score {heuristic_score:.1} is ambiguous between {lower} and {upper} — classifier picked '{tier}'");
            }
        }
    }

    let overrides = PlanOverrides {
        tier: options.tier,
        classified_tier,
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
    let mut total_usd = 0.0f64;
    let mut any_usd_reported = false;

    if plan.specialists.is_empty() {
        println!(
            "  [info] No specialists triggered for this diff at tier '{}'.",
            plan.tier
        );
    } else {
        // Collected up front (and reported) regardless of whether the
        // selected backend ends up being available, so `autoreview diff` is
        // still useful as a diagnostic of what context *would* be sent even
        // when specialists can't run.
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

        if !backend_available(backend_kind, &config) {
            println!(
                "  [warn] backend '{}' was not found/reachable — skipping {} planned specialist(s): {}. Run `autoreview doctor` to check required tools.",
                backend_label(backend_kind),
                plan.specialists.len(),
                plan.specialists.iter().map(|s| s.aspect.as_str()).collect::<Vec<_>>().join(", ")
            );
        } else {
            println!(
                "\n  running {} specialist(s) at tier '{}' via backend '{}':",
                plan.specialists.len(),
                plan.tier,
                backend_label(backend_kind)
            );
            let diff_text = diff_context(
                &options.repo_root,
                &options.base_ref,
                &options.head_ref,
                &facts.files,
            );
            let backend = build_backend(backend_kind, &config);
            let max_concurrency = match plan.tier {
                Tier::Quick => config.budgets.tiers.quick.max_concurrency,
                Tier::Standard => config.budgets.tiers.standard.max_concurrency,
                Tier::Deep => config.budgets.tiers.deep.max_concurrency,
            } as usize;

            let mut launched_aspects: std::collections::HashSet<&str> = std::collections::HashSet::new();
            let mut budget_stopped = false;
            for chunk in plan.specialists.chunks(max_concurrency.max(1)) {
                if autoreview_core::should_stop_for_budget(total_usd, any_usd_reported, options.max_usd) {
                    budget_stopped = true;
                    break;
                }
                for specialist in chunk {
                    launched_aspects.insert(specialist.aspect.as_str());
                }
                let results: Vec<_> = std::thread::scope(|scope| {
                    let handles: Vec<_> = chunk
                        .iter()
                        .map(|specialist| {
                            let repo_root = options.repo_root.clone();
                            let diff_text = diff_text.clone();
                            let context_block = context_block.clone();
                            let stage1_summary =
                                stage1_summary_for_category(&stage1_findings, &specialist.aspect);
                            let backend_ref: &(dyn AgentBackend + Sync) = backend.as_ref();
                            let model_override = (backend_kind == AgentBackendKind::LocalLlm).then(|| specialist_model_for(backend_kind, &config, &specialist.model));
                            scope.spawn(move || {
                                run_one_specialist(
                                    backend_ref,
                                    &repo_root,
                                    plan.tier,
                                    specialist,
                                    &diff_text,
                                    &stage1_summary,
                                    &context_block,
                                    model_override.as_deref(),
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
                            if let Some(usd) = specialist_result.usage.usd {
                                total_usd += usd;
                                any_usd_reported = true;
                            }
                            per_stage_costs.insert(
                                format!("agent:{aspect}"),
                                CostEntry {
                                    input_tokens: specialist_result.usage.input_tokens,
                                    output_tokens: specialist_result.usage.output_tokens,
                                    usd: specialist_result.usage.usd,
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

            if budget_stopped {
                let skipped: Vec<&str> = plan.specialists.iter().map(|s| s.aspect.as_str()).filter(|a| !launched_aspects.contains(a)).collect();
                if !skipped.is_empty() {
                    println!(
                        "\n  [budget] stopped after ${total_usd:.4} spent (--max-usd {:.2}) — skipped {} remaining specialist(s): {}",
                        options.max_usd.unwrap_or(0.0),
                        skipped.len(),
                        skipped.join(", ")
                    );
                }
            }
        }
    }

    // Cost-ceiling enforcement also gates the verify pass below: it's a
    // real cost, and a diff that's already over --max-usd from specialists
    // alone shouldn't spend more on a judge pass.
    let over_budget = autoreview_core::should_stop_for_budget(total_usd, any_usd_reported, options.max_usd);

    // Stage 3.5: verify pass. Budget-gated (skipped entirely in quick tier,
    // per the plan) and skipped when there's nothing this pass would even
    // look at, so a diff with no high/blocker or noisy-category findings
    // never pays for a judge call it doesn't need.
    let mut verify_suppressed = Vec::new();
    if over_budget {
        println!("\n  [budget] skipping verify pass — already at/over --max-usd {:.2}", options.max_usd.unwrap_or(0.0));
    } else if plan.tier != Tier::Quick && config.verify.enabled && backend_available(backend_kind, &config) {
        // "Semantic rules": syntactically precise but semantically
        // approximate (no type resolution) rules — ast-grep rules declaring
        // `semantic: true`, plus the symindex heuristic rules (message-chain/
        // feature-envy/data-clump aren't YAML-declared, so they're unioned
        // in directly), plus the hand-rolled excessive-comment-padding check
        // (also not YAML-declared — it's a practices.rs line scan) — always
        // get a Stage 3.5 confirmation regardless of their own severity/
        // category, on top of the noisy-category selection. Padding-comment
        // in particular is a syntactic proxy (comment volume vs. body
        // volume) for a semantic claim ("this comment is stale/no longer
        // useful") it can't verify on its own, so it needs the LLM check.
        let mut semantic_ids = autoreview_core::semantic_rule_ids();
        semantic_ids.extend([
            "message-chain".to_string(),
            "feature-envy".to_string(),
            "data-clump".to_string(),
            "excessive-comment-padding".to_string(),
            "divergent-change".to_string(),
            "data-class".to_string(),
        ]);

        let to_check = autoreview_core::select_for_verification(&findings, &config.verify.noisy_categories, &semantic_ids).len();
        if to_check > 0 {
            println!("\n  verify:         checking {to_check} finding(s) against the diff...");
            let diff_text = diff_context(&options.repo_root, &options.base_ref, &options.head_ref, &facts.files);
            let backend = build_backend(backend_kind, &config);
            let verify_result = autoreview_core::run_verify_pass(
                backend.as_ref(),
                findings,
                &diff_text,
                cheap_model_for(backend_kind, &config),
                4,
                &options.repo_root,
                &config.verify.noisy_categories,
                &semantic_ids,
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
            if let Some(usd) = verify_result.usage.usd {
                total_usd += usd;
                any_usd_reported = true;
            }
            per_stage_costs.insert(
                "verify".to_string(),
                CostEntry { input_tokens: verify_result.usage.input_tokens, output_tokens: verify_result.usage.output_tokens, usd: verify_result.usage.usd, wall_ms: verify_result.wall_ms },
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
    let run_id = uuid::Uuid::new_v4().to_string();
    let run_timestamp = chrono::Utc::now().to_rfc3339();

    // Shadow-mode rules: `.autoreview/rules/{shadow,promoted}/*.yaml`, a
    // human-curated interim path onto a bench-passed candidate (`rules
    // review`'s human-approval gate is still a stub). Shadow firings are
    // suppressed (reason: ShadowRule); promoted ones surface as normal
    // findings. Every firing is checked for agreement against this run's
    // own agent findings and recorded, then promotion/demotion gates are
    // evaluated per rule that fired.
    match autoreview_core::run_shadow_rules(&options.repo_root, &changed_file_paths) {
        Ok(shadow_findings) if !shadow_findings.is_empty() => match HistoryStore::open(&history_dir) {
            Ok(store) => {
                let agent_locations: Vec<(String, String, u32)> = dedupe_result
                    .findings
                    .iter()
                    .filter(|f| matches!(f.source.kind, autoreview_schema::FindingSourceKind::Agent))
                    .map(|f| (f.category.clone(), f.location.path.clone(), f.location.range.start_line))
                    .collect();
                let agent_refs: Vec<autoreview_core::AgentFindingRef> = agent_locations.iter().map(|(category, path, line)| autoreview_core::AgentFindingRef { category, path, line: *line }).collect();

                let mut fired_rule_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
                let (raw_findings, statuses): (Vec<_>, Vec<_>) = shadow_findings.into_iter().map(|sf| (sf.finding, (sf.rule_id, sf.status))).unzip();
                let fingerprinted = assign_fingerprints(raw_findings);

                for (ff, (rule_id, status)) in fingerprinted.into_iter().zip(statuses) {
                    let finding = to_finding(ff);
                    fired_rule_ids.insert(rule_id.clone());
                    store.ensure_rule_tracked(&rule_id, status, &run_timestamp).ok();
                    let agreement = autoreview_core::classify_agreement(&finding.category, &finding.location.path, finding.location.range.start_line, &agent_refs);
                    store.record_shadow_firing(&rule_id, &finding.fingerprints.primary, &run_id, &finding.location.path, finding.location.range.start_line, agreement.as_str(), &run_timestamp).ok();

                    if status == "shadow" {
                        dedupe_result.suppressed.push(autoreview_schema::SuppressedFinding { finding, reason: autoreview_schema::SuppressedReason::ShadowRule });
                    } else {
                        dedupe_result.findings.push(finding);
                    }
                }

                for rule_id in fired_rule_ids {
                    let Ok(Some(state)) = store.rule_state(&rule_id) else { continue };
                    let distinct_runs = store.distinct_shadow_run_count(&rule_id).unwrap_or(0);
                    let user_fp_count = store.count_fp_feedback_for_rule(&rule_id).unwrap_or(0);
                    let days_since_valid_from = chrono::DateTime::parse_from_rfc3339(&state.valid_from).map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_days()).unwrap_or(0);

                    if state.status == "shadow" {
                        let inputs = autoreview_core::PromotionInputs { firings: state.firings, distinct_runs, agent_agreed: state.agent_agreed, agent_disagreed: state.agent_disagreed, user_fp_count, days_since_valid_from };
                        if autoreview_core::should_promote(&inputs) {
                            store.set_rule_status(&rule_id, "promoted").ok();
                            move_shadow_rule_file(&options.repo_root, &rule_id, "shadow", "promoted");
                            println!("\n  [shadow] rule '{rule_id}' promoted — {} firings across {distinct_runs} runs, {:.0}% agent agreement", state.firings, inputs.agent_agreed as f64 / (inputs.agent_agreed + inputs.agent_disagreed).max(1) as f64 * 100.0);
                        }
                    } else if state.status == "promoted" && autoreview_core::should_demote(user_fp_count) {
                        store.set_rule_status(&rule_id, "shadow").ok();
                        move_shadow_rule_file(&options.repo_root, &rule_id, "promoted", "shadow");
                        println!("\n  [shadow] rule '{rule_id}' demoted back to shadow — {user_fp_count} user false-positive report(s)");
                    }
                }
            }
            Err(err) => println!("  [warn] shadow rules: failed to open history store: {err}"),
        },
        Ok(_) => {}
        Err(err) => println!("  [warn] shadow rules run failed: {err}"),
    }

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

    // Embedding-similarity noise filter: opt-in (needs a configured embedding
    // server), and best-effort — an unreachable server just skips the filter
    // rather than failing the run. Reuses the plan's Greptile-derived rule:
    // suppress a finding cosine-similar to >= fpBlockThreshold distinct past
    // `--fp` findings, unless it's also similar to >= tpOverrideThreshold
    // distinct past `--tp` findings (a recurring real issue shouldn't be
    // silenced just because it also resembles some noise).
    if config.agents.embedding.enabled {
        match HistoryStore::open(&history_dir) {
            Ok(store) => {
                let (mut kept, mut suppressed_by_embedding) = (Vec::new(), Vec::new());
                for finding in dedupe_result.findings.into_iter() {
                    let text = format!("{} {}", finding.title, finding.message);
                    let embedding = autoreview_core::fetch_embedding(&config.agents.embedding.base_url, &config.agents.embedding.model, &text, &config.agents.embedding.curl_binary);
                    let suppress = match embedding {
                        Ok(embedding) => {
                            let fp_count = store.count_similar_embeddings(&embedding, "fp", 0.9).unwrap_or(0);
                            let tp_count = store.count_similar_embeddings(&embedding, "tp", 0.9).unwrap_or(0);
                            fp_count >= config.storage.fp_block_threshold && tp_count < config.storage.tp_override_threshold
                        }
                        Err(_) => false,
                    };
                    if suppress {
                        suppressed_by_embedding.push(finding);
                    } else {
                        kept.push(finding);
                    }
                }
                if !suppressed_by_embedding.is_empty() {
                    println!("\n  [embedding] suppressed {} finding(s) similar to past false positives", suppressed_by_embedding.len());
                }
                dedupe_result.findings = kept;
                dedupe_result.suppressed.extend(suppressed_by_embedding.into_iter().map(|finding| autoreview_schema::SuppressedFinding { finding, reason: autoreview_schema::SuppressedReason::EmbeddingFpMatch }));
            }
            Err(err) => println!("  [warn] embedding filter: failed to open history store: {err}"),
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
                usd: any_usd_reported.then_some(total_usd),
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

    // Team sync (opt-in, storage.sync.mode: git): push this run's event log
    // to the team's sync branch — best-effort by design (see sync_push's own
    // docs), so a flaky/offline network never fails an otherwise-successful
    // review.
    autoreview_core::sync_push(&options.repo_root, &history_dir, &config.storage.sync);

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
    model_override: Option<&str>,
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
            model: model_override.map(str::to_string).unwrap_or_else(|| specialist.model.clone()),
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

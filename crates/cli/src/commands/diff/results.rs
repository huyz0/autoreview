use std::collections::{HashMap, HashSet};
use std::path::Path;

use autoreview_core::{
    append_event_log, assign_fingerprints, dedupe_exact, dedupe_fuzzy, events_from_report,
    to_finding, AgentFindingRef, DedupeResult, HistoryStore, ShadowFinding,
};
use autoreview_schema::{
    DiffStats, Finding, FindingSourceKind, ReviewReport, ReviewSummary, ReviewTarget,
    SuppressedFinding, SuppressedReason,
};

use super::super::history::{history_dir_for, hostname};
use super::{model::ModelReview, ReviewRun, StaticFindings};

struct RunIdentity {
    id: String,
    timestamp: String,
}

pub(super) fn finish(
    run: &ReviewRun<'_>,
    mut model: ModelReview,
    static_results: StaticFindings,
) -> anyhow::Result<()> {
    let history = history_dir_for(&run.options.repo_root);
    let identity = RunIdentity {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    };
    let mut findings = reconcile(&mut model, static_results.suppressed);
    let mut analyzer_warnings = static_results.warnings;
    if let Err(err) = collect_shadow_rules(run, &history, &identity, &mut findings) {
        let warning = format!("shadow rules run failed: {err}");
        println!("  [warn] {warning}");
        analyzer_warnings.push(warning);
    }
    if run.options.incremental {
        suppress_baseline(&history, &mut findings);
    }
    if !run.options.static_only && run.config.agents.embedding.enabled {
        suppress_similar_false_positives(run, &history, &mut findings);
    }
    print_findings(&findings.findings);
    let report = build_report(run, &identity, model, findings, analyzer_warnings);
    persist(run, &history, &report)
}

fn reconcile(model: &mut ModelReview, shadow: Vec<SuppressedFinding>) -> DedupeResult {
    let exact = dedupe_exact(std::mem::take(&mut model.findings));
    let mut result = dedupe_fuzzy(exact.findings, 3, 0.55);
    result.suppressed.extend(exact.suppressed);
    result.suppressed.append(&mut model.suppressed);
    result.suppressed.extend(shadow);
    if !result.suppressed.is_empty() {
        println!(
            "\n  [dedupe] suppressed {} duplicate/refuted finding(s)",
            result.suppressed.len()
        );
    }
    result
}

fn collect_shadow_rules(
    run: &ReviewRun<'_>,
    history: &Path,
    identity: &RunIdentity,
    result: &mut DedupeResult,
) -> anyhow::Result<()> {
    let shadow = autoreview_core::run_shadow_rules_for_snapshot(
        &run.options.repo_root,
        run.source_root,
        run.changed_files,
    )?;
    if shadow.is_empty() {
        return Ok(());
    }
    let store = match HistoryStore::open(history) {
        Ok(store) => store,
        Err(err) => {
            println!("  [warn] shadow rules: failed to open history store: {err}");
            return Ok(());
        }
    };
    let fired = record_shadow_matches(run, &store, identity, shadow, result);
    for rule in fired {
        update_rule_state(&store, &run.options.repo_root, &rule);
    }
    Ok(())
}

fn record_shadow_matches(
    run: &ReviewRun<'_>,
    store: &HistoryStore,
    identity: &RunIdentity,
    shadow: Vec<ShadowFinding>,
    result: &mut DedupeResult,
) -> HashSet<String> {
    let originals = result
        .findings
        .iter()
        .filter(|f| matches!(f.source.kind, FindingSourceKind::Agent))
        .map(|f| {
            (
                f.category.clone(),
                f.location.path.clone(),
                f.location.range.start_line,
            )
        })
        .collect::<Vec<_>>();
    let refs = originals
        .iter()
        .map(|(category, path, line)| AgentFindingRef {
            category,
            path,
            line: *line,
        })
        .collect::<Vec<_>>();
    let mut statuses = HashMap::new();
    let raw = shadow
        .into_iter()
        .filter(|match_| run.in_scope(&match_.finding))
        .map(|match_| {
            statuses.insert(match_.rule_id, match_.status);
            match_.finding
        })
        .collect();

    let mut fired = HashSet::new();
    for finding in assign_fingerprints(raw).into_iter().map(to_finding) {
        let Some(rule_id) = finding.source.rule_id.as_deref() else {
            continue;
        };
        let Some(&status) = statuses.get(rule_id) else {
            continue;
        };
        fired.insert(rule_id.to_owned());
        store
            .ensure_rule_tracked(rule_id, status, &identity.timestamp)
            .ok();
        let location = &finding.location;
        let agreement = autoreview_core::classify_agreement(
            &finding.category,
            &location.path,
            location.range.start_line,
            &refs,
        );
        store
            .record_shadow_firing(
                rule_id,
                &finding.fingerprints.primary,
                &identity.id,
                &location.path,
                location.range.start_line,
                agreement.as_str(),
                &identity.timestamp,
            )
            .ok();
        if status == "shadow" {
            result.suppressed.push(SuppressedFinding {
                finding,
                reason: SuppressedReason::ShadowRule,
            });
        } else {
            result.findings.push(finding);
        }
    }
    fired
}

fn update_rule_state(store: &HistoryStore, repo: &Path, rule: &str) {
    let Ok(Some(state)) = store.rule_state(rule) else {
        return;
    };
    let user_fp_count = store.count_fp_feedback_for_rule(rule).unwrap_or(0);
    if state.status == "promoted" && autoreview_core::should_demote(user_fp_count) {
        store.set_rule_status(rule, "shadow").ok();
        move_shadow_rule_file(repo, rule, "promoted", "shadow");
        println!("\n  [shadow] rule '{rule}' demoted back to shadow — {user_fp_count} user false-positive report(s)");
    }
    if state.status != "shadow" {
        return;
    }
    let distinct_runs = store.distinct_shadow_run_count(rule).unwrap_or(0);
    let days_since_valid_from = chrono::DateTime::parse_from_rfc3339(&state.valid_from)
        .map(|time| (chrono::Utc::now() - time.with_timezone(&chrono::Utc)).num_days())
        .unwrap_or(0);
    let inputs = autoreview_core::PromotionInputs {
        firings: state.firings,
        distinct_runs,
        agent_agreed: state.agent_agreed,
        agent_disagreed: state.agent_disagreed,
        user_fp_count,
        days_since_valid_from,
    };
    if autoreview_core::should_promote(&inputs) {
        store.set_rule_status(rule, "promoted").ok();
        move_shadow_rule_file(repo, rule, "shadow", "promoted");
        let agreement = inputs.agent_agreed as f64
            / (inputs.agent_agreed + inputs.agent_disagreed).max(1) as f64
            * 100.0;
        println!("\n  [shadow] rule '{rule}' promoted — {} firings across {distinct_runs} runs, {agreement:.0}% agent agreement", state.firings);
    }
}

fn suppress_baseline(history: &Path, result: &mut DedupeResult) {
    let baseline = HistoryStore::open(history)
        .and_then(|store| match store.most_recent_run_id()? {
            Some(run_id) => store.fingerprints_for_run(&run_id),
            None => Ok(HashSet::new()),
        })
        .unwrap_or_else(|err| {
            println!("  [warn] --incremental: failed to read baseline from history: {err}");
            HashSet::new()
        });
    if !baseline.is_empty() {
        let (new, old): (Vec<_>, Vec<_>) = std::mem::take(&mut result.findings)
            .into_iter()
            .partition(|f| !baseline.contains(&f.fingerprints.primary));
        if !old.is_empty() {
            println!(
                "\n  [incremental] suppressed {} finding(s) already reported in the previous run",
                old.len()
            );
        }
        result.findings = new;
        result
            .suppressed
            .extend(old.into_iter().map(|finding| SuppressedFinding {
                finding,
                reason: SuppressedReason::Baseline,
            }));
    }
}

fn suppress_similar_false_positives(
    run: &ReviewRun<'_>,
    history: &Path,
    result: &mut DedupeResult,
) {
    let store = match HistoryStore::open(history) {
        Ok(store) => store,
        Err(err) => {
            println!("  [warn] embedding filter: failed to open history store: {err}");
            return;
        }
    };
    let (kept, suppressed): (Vec<_>, Vec<_>) = std::mem::take(&mut result.findings)
        .into_iter()
        .partition(|finding| !matches_false_positive(run, &store, finding));
    if !suppressed.is_empty() {
        println!(
            "\n  [embedding] suppressed {} finding(s) similar to past false positives",
            suppressed.len()
        );
    }
    result.findings = kept;
    result
        .suppressed
        .extend(suppressed.into_iter().map(|finding| SuppressedFinding {
            finding,
            reason: SuppressedReason::EmbeddingFpMatch,
        }));
}

fn matches_false_positive(run: &ReviewRun<'_>, store: &HistoryStore, finding: &Finding) -> bool {
    use autoreview_schema::FeedbackVerdict::*;
    let settings = &run.config.agents.embedding;
    let text = format!("{} {}", finding.title, finding.message);
    let Ok(embedding) = autoreview_core::fetch_embedding(
        &settings.base_url,
        &settings.model,
        &text,
        &settings.curl_binary,
    ) else {
        return false;
    };
    let fp = store
        .count_similar_embeddings(&embedding, &[FalsePositive], 0.9)
        .unwrap_or(0);
    let tp = store
        .count_similar_embeddings(
            &embedding,
            &[TruePositive, AcceptedRisk, FixInFollowup],
            0.9,
        )
        .unwrap_or(0);
    fp >= run.config.storage.fp_block_threshold && tp < run.config.storage.tp_override_threshold
}

fn print_findings(findings: &[Finding]) {
    for finding in findings {
        println!(
            "\n  [{:?}] {} ({}) — {}:{}",
            finding.severity,
            finding.title,
            finding.id,
            finding.location.path,
            finding.location.range.start_line
        );
        println!("    {}", finding.message);
    }
    if !findings.is_empty() {
        println!("\n  (use `autoreview feedback <id> --fp|--tp` to record feedback on a finding above — or --doesnt-apply/--accepted-risk/--fix-in-followup for a more specific reason)");
    }
}

fn build_report(
    run: &ReviewRun<'_>,
    identity: &RunIdentity,
    model: ModelReview,
    result: DedupeResult,
    analyzer_warnings: Vec<String>,
) -> ReviewReport {
    let facts = run.facts;
    let summary = summarize(&result.findings);
    ReviewReport {
        schema_version: "1".into(),
        run_id: identity.id.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        target: ReviewTarget {
            repo_root: run.options.repo_root.to_string_lossy().into_owned(),
            base_ref: run.options.base_ref.clone(),
            head_ref: run.options.head_ref.clone(),
            commits: Some(autoreview_schema::ComparedCommits {
                base: facts.base_ref.clone(),
                head: facts.head_ref.clone(),
            }),
            diff_stats: DiffStats {
                files: facts.files.len() as u32,
                additions: facts.files.iter().map(|f| f.additions).sum(),
                deletions: facts.files.iter().map(|f| f.deletions).sum(),
                languages: facts.languages.clone(),
            },
        },
        plan: model.plan,
        findings: result.findings,
        suppressed: result.suppressed,
        analyzer_warnings,
        costs: model.costs,
        summary,
        spec_verdicts: model.criteria,
    }
}

fn summarize(findings: &[Finding]) -> ReviewSummary {
    let mut by_severity = HashMap::new();
    let mut by_category = HashMap::new();
    for finding in findings {
        *by_severity
            .entry(format!("{:?}", finding.severity).to_lowercase())
            .or_insert(0) += 1;
        *by_category.entry(finding.category.clone()).or_insert(0) += 1;
    }
    ReviewSummary {
        by_severity,
        by_category,
        gate: None,
    }
}

fn persist(run: &ReviewRun<'_>, history_dir: &Path, report: &ReviewReport) -> anyhow::Result<()> {
    let run_dir = history_dir.join("runs").join(&report.run_id);
    std::fs::create_dir_all(&run_dir)?;
    let report_path = run_dir.join("report.json");
    std::fs::write(&report_path, serde_json::to_string_pretty(report)?)?;
    println!("\n  report written: {}", report_path.display());

    let markdown_path = run_dir.join("report.md");
    std::fs::write(&markdown_path, crate::render::render_markdown(report))?;
    println!("  markdown written: {}", markdown_path.display());

    let sarif_path = run_dir.join("report.sarif");
    std::fs::write(&sarif_path, crate::render::render_sarif(report))?;
    println!("  sarif written:  {}", sarif_path.display());

    let index_path = run_dir.join("index.md");
    std::fs::write(&index_path, crate::render::render_index(report))?;
    println!("  index written:  {}", index_path.display());

    // Storage write path (M1 scope, per the plan): append this run's findings
    // to the local event log and index them in SQLite. Both are best-effort —
    // a failure here must never take down an otherwise-successful review.
    let host = hostname();
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let events = events_from_report(report, &host);
    match append_event_log(history_dir, &date, &host, &events) {
        Ok(path) => println!(
            "  event log:      {} ({} event(s))",
            path.display(),
            events.len()
        ),
        Err(err) => println!("  [warn] failed to append event log: {err}"),
    }
    match HistoryStore::open(history_dir).and_then(|store| store.record_run(report)) {
        Ok(()) => println!(
            "  history index:  {}",
            history_dir.join("index.db").display()
        ),
        Err(err) => println!("  [warn] failed to update history index: {err}"),
    }

    // Team sync (opt-in, storage.sync.mode: git): push this run's event log
    // to the team's sync branch — best-effort by design (see sync_push's own
    // docs), so a flaky/offline network never fails an otherwise-successful
    // review.
    autoreview_core::sync_push(
        &run.options.repo_root,
        history_dir,
        &run.config.storage.sync,
    );
    Ok(())
}

/// Physically relocates a rule file between `.autoreview/rules/{from,to}/`
/// on promotion/demotion — best-effort (a failure here doesn't block the
/// status transition already recorded in the history store, it just means
/// the on-disk file needs a manual move next time someone looks). Also
/// used by `commands::rules::run_rules_rollback` for a manual demotion/
/// rejection, same file-move semantics as the automatic gate.
pub(crate) fn move_shadow_rule_file(repo_root: &Path, rule_id: &str, from: &str, to: &str) {
    let rule_files = autoreview_core::discover_shadow_rule_files(repo_root);
    for rule_file in rule_files {
        if rule_file.status != from {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&rule_file.path) else {
            continue;
        };
        let Ok(rule) = serde_yaml::from_str::<serde_yaml::Value>(&contents) else {
            continue;
        };
        if rule.get("id").and_then(serde_yaml::Value::as_str) != Some(rule_id) {
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

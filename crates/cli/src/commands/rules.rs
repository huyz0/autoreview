//! `autoreview rules mine`/`rules bench`/`rules shadow-log`/`rules review`
//! — the first five real (non-stub) pieces of the M3 rule factory: clusters
//! recorded agent findings into candidate seeds
//! (`.autoreview/rules/candidates/<clusterId>/seed.json`), attempts to
//! draft an ast-grep rule for each seed via the configured agent backend,
//! 5x ensemble-agreement filtered (`rule_factory::draft`), benches a
//! drafted rule against human-supplied fixtures plus a current-repo FP
//! smoke test (`rule_factory::bench`), lists recent firings of a
//! shadow/promoted rule for spot-checking (`rule_factory::shadow`, wired
//! into every `diff` run — see `commands::diff`), and gives a human the
//! actual `--approve`/`--reject` gate the plan calls for before a candidate
//! ever reaches shadow mode. Rollback stays a stub (`run_rules_stub`).

use autoreview_core::{draft_candidate, mine_candidates, run_bench, write_seed_file, BenchVerdict, DraftOutcome, HistoryStore};
use autoreview_schema::AgentBackendKind;
use serde::Deserialize;

use super::backend::{backend_available, build_backend};
use super::history::history_dir_for;

fn cheap_model_for(kind: AgentBackendKind, config: &autoreview_schema::AutoreviewConfig) -> &str {
    match kind {
        AgentBackendKind::LocalLlm => &config.agents.local_llm.model,
        AgentBackendKind::ClaudeCode | AgentBackendKind::Pi => &config.budgets.models.cheap,
    }
}

pub fn run_rules_mine(repo_root: &std::path::Path) -> anyhow::Result<()> {
    let history_dir = history_dir_for(repo_root);
    let store = HistoryStore::open(&history_dir)?;
    let findings = store.agent_findings_for_mining()?;

    if findings.is_empty() {
        println!("No agent findings recorded yet on this machine — nothing to mine. Run `autoreview diff` a few times first.");
        return Ok(());
    }

    let seeds = mine_candidates(findings);
    if seeds.is_empty() {
        println!("No recurring clusters found (need >= 3 similar findings spanning >= 2 distinct runs).");
        return Ok(());
    }

    let config = autoreview_core::load_config(&repo_root.join(".autoreview").join("config.yaml"))?;
    let backend_kind = config.agents.backend;
    let can_draft = backend_available(backend_kind, &config);
    let backend = can_draft.then(|| build_backend(backend_kind, &config));
    let model = cheap_model_for(backend_kind, &config).to_string();

    println!("Found {} candidate cluster(s):", seeds.len());
    for seed in &seeds {
        let seed_path = write_seed_file(repo_root, seed)?;
        println!(
            "  {} ({}, {} member(s) across {} run(s)) -> {}",
            seed.cluster_id,
            seed.category,
            seed.member_fingerprints.len(),
            seed.distinct_run_count,
            seed_path.display()
        );

        let Some(backend) = &backend else {
            println!("    [draft] skipped — no agent backend available (needed to attempt a rule draft)");
            continue;
        };
        let (outcome, _usage) = draft_candidate(backend.as_ref(), seed, &model, 2, repo_root);
        match outcome {
            DraftOutcome::Drafted { rule_yaml, agreement_count } => {
                let dir = repo_root.join(".autoreview").join("rules").join("candidates").join(&seed.cluster_id);
                std::fs::create_dir_all(&dir)?;
                let rule_path = dir.join("rule.yaml");
                std::fs::write(&rule_path, &rule_yaml)?;
                println!("    [draft] {agreement_count}/5 attempts agreed -> {}", rule_path.display());
            }
            DraftOutcome::Inexpressible { rationale } => {
                println!("    [draft] inexpressible as a syntactic rule: {rationale}");
            }
        }
    }
    println!("\n(Review/shadow/promote are not yet implemented. Run `autoreview rules bench <clusterId>` on a drafted candidate once you've added tests/positive and tests/negative fixtures under its candidate directory.)");
    Ok(())
}

pub fn run_rules_bench(repo_root: &std::path::Path, cluster_id: &str) -> anyhow::Result<()> {
    let report = run_bench(repo_root, cluster_id)?;

    if let Some(self_test) = &report.self_test {
        println!(
            "  self-test: {}/{} positive matched, {}/{} negative matched ({})",
            self_test.positive_matched,
            self_test.positive_total,
            self_test.negative_matched,
            self_test.negative_total,
            if self_test.passed() { "passed" } else { "failed" }
        );
    } else {
        println!("  self-test: skipped — no tests/positive or tests/negative fixtures supplied yet");
    }

    if let Some(fp_smoke) = &report.fp_smoke {
        println!(
            "  fp-smoke:  {}/{} sampled repo file(s) matched ({})",
            fp_smoke.matched_files,
            fp_smoke.sampled_files,
            if fp_smoke.passed() { "passed" } else { "failed" }
        );
    } else {
        println!("  fp-smoke:  skipped — no sample files of this rule's language found in the current repo");
    }

    println!("  historical-precision: skipped — {}", report.historical_precision_skipped_reason);

    match report.verdict {
        BenchVerdict::Candidate => println!("\nverdict: candidate — ready for `autoreview rules review` (still a stub)"),
        BenchVerdict::NeedsFixtures => println!("\nverdict: needs-fixtures — add tests/positive/*, tests/negative/* under this candidate's directory, then re-run bench"),
        BenchVerdict::SelfTestFailed => println!("\nverdict: self-test-failed — the drafted rule doesn't cleanly match its own fixtures yet"),
        BenchVerdict::FailedFpSmoke => println!("\nverdict: failed-fp-smoke — the rule matches too many unrelated files in this repo"),
    }
    Ok(())
}

const SHADOW_LOG_LIMIT: u32 = 20;

pub fn run_rules_shadow_log(repo_root: &std::path::Path, rule_id: &str) -> anyhow::Result<()> {
    let history_dir = history_dir_for(repo_root);
    let store = HistoryStore::open(&history_dir)?;

    let Some(state) = store.rule_state(rule_id)? else {
        println!("Rule '{rule_id}' has never fired in shadow/promoted mode on this machine — nothing to show yet.");
        return Ok(());
    };

    let distinct_runs = store.distinct_shadow_run_count(rule_id)?;
    let user_fp_count = store.count_fp_feedback_for_rule(rule_id)?;
    println!(
        "rule '{rule_id}': status={}, firings={}, distinct_runs={distinct_runs}, agent_agreed={}, agent_disagreed={}, user_fp_reports={user_fp_count}, tracked_since={}",
        state.status, state.firings, state.agent_agreed, state.agent_disagreed, state.valid_from
    );

    let firings = store.recent_shadow_firings(rule_id, SHADOW_LOG_LIMIT)?;
    if firings.is_empty() {
        println!("(no firings recorded)");
        return Ok(());
    }
    println!("\nrecent firings (most recent first):");
    for firing in &firings {
        println!("  [{}] {}:{} (run {}, {}) — {}", firing.agreement, firing.location_path, firing.location_line, firing.run_id, firing.created_at, firing.fingerprint);
    }
    println!("\n(use `autoreview feedback <id> --fp|--tp` on a finding's own id to feed the promotion/demotion gates)");
    Ok(())
}

#[derive(Debug, Deserialize)]
struct RuleIdOnly {
    id: String,
}

fn candidates_dir(repo_root: &std::path::Path) -> std::path::PathBuf {
    repo_root.join(".autoreview").join("rules").join("candidates")
}

fn list_candidate_ids(repo_root: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(candidates_dir(repo_root)) else { return Vec::new() };
    entries.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).filter_map(|e| e.file_name().into_string().ok()).collect()
}

/// `autoreview rules review` — the human-approval gate the plan calls for
/// between bench and shadow mode. No args: lists every candidate with a
/// drafted rule, its bench verdict, and whether it's already been
/// approved/rejected. `--approve <clusterId>`: moves the candidate's
/// rule.yaml into `.autoreview/rules/shadow/`, registers it in the history
/// store's shadow lifecycle (status "shadow", so it starts accumulating
/// firings/agreement from the next `diff` run), and marks the candidate
/// dir approved. `--reject <clusterId> --reason <text>`: records why,
/// without deleting the candidate (kept for provenance, per the plan).
pub fn run_rules_review(repo_root: &std::path::Path, approve: Option<String>, reject: Option<String>, reason: Option<String>) -> anyhow::Result<()> {
    if let Some(cluster_id) = approve {
        return approve_candidate(repo_root, &cluster_id);
    }
    if let Some(cluster_id) = reject {
        let Some(reason) = reason else {
            anyhow::bail!("--reject requires --reason \"<why>\"");
        };
        return reject_candidate(repo_root, &cluster_id, &reason);
    }

    let ids = list_candidate_ids(repo_root);
    if ids.is_empty() {
        println!("No candidates found — run `autoreview rules mine` first.");
        return Ok(());
    }

    println!("Candidate rules pending review:\n");
    for cluster_id in ids {
        let dir = candidates_dir(repo_root).join(&cluster_id);
        if !dir.join("rule.yaml").exists() {
            continue;
        }
        let status = if dir.join("approved.json").exists() {
            "approved".to_string()
        } else if dir.join("rejected.json").exists() {
            "rejected".to_string()
        } else {
            "pending".to_string()
        };
        let bench_summary = match run_bench(repo_root, &cluster_id) {
            Ok(report) => format!("{:?}", report.verdict),
            Err(err) => format!("bench error: {err}"),
        };
        println!("  {cluster_id}  status={status}  bench={bench_summary}");
    }
    println!("\n(use --approve <clusterId> to move a candidate to shadow mode, or --reject <clusterId> --reason \"<why>\")");
    Ok(())
}

fn approve_candidate(repo_root: &std::path::Path, cluster_id: &str) -> anyhow::Result<()> {
    let dir = candidates_dir(repo_root).join(cluster_id);
    let rule_path = dir.join("rule.yaml");
    let contents = std::fs::read_to_string(&rule_path).map_err(|_| anyhow::anyhow!("no drafted rule found at {} — run `autoreview rules mine` first", rule_path.display()))?;
    let meta: RuleIdOnly = serde_yaml::from_str(&contents)?;

    let shadow_dir = repo_root.join(".autoreview").join("rules").join("shadow");
    std::fs::create_dir_all(&shadow_dir)?;
    std::fs::write(shadow_dir.join(format!("{}.yaml", meta.id)), &contents)?;

    let history_dir = history_dir_for(repo_root);
    let store = HistoryStore::open(&history_dir)?;
    store.ensure_rule_tracked(&meta.id, "shadow", &chrono::Utc::now().to_rfc3339())?;

    std::fs::write(dir.join("approved.json"), serde_json::json!({ "ruleId": meta.id, "approvedAt": chrono::Utc::now().to_rfc3339() }).to_string())?;
    println!("Approved '{cluster_id}' — rule '{}' is now in shadow mode (see `autoreview rules shadow-log {}`).", meta.id, meta.id);
    Ok(())
}

fn reject_candidate(repo_root: &std::path::Path, cluster_id: &str, reason: &str) -> anyhow::Result<()> {
    let dir = candidates_dir(repo_root).join(cluster_id);
    if !dir.exists() {
        anyhow::bail!("no candidate '{cluster_id}' found under {}", candidates_dir(repo_root).display());
    }
    std::fs::write(dir.join("rejected.json"), serde_json::json!({ "reason": reason, "rejectedAt": chrono::Utc::now().to_rfc3339() }).to_string())?;
    println!("Rejected '{cluster_id}': {reason}");
    Ok(())
}

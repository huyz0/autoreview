//! `autoreview rules mine`/`rules bench`/`rules shadow-log` — the first
//! four real (non-stub) pieces of the M3 rule factory: clusters recorded
//! agent findings into candidate seeds
//! (`.autoreview/rules/candidates/<clusterId>/seed.json`), attempts to
//! draft an ast-grep rule for each seed via the configured agent backend,
//! 5x ensemble-agreement filtered (`rule_factory::draft`), benches a
//! drafted rule against human-supplied fixtures plus a current-repo FP
//! smoke test (`rule_factory::bench`), and lists recent firings of a
//! shadow/promoted rule for spot-checking (`rule_factory::shadow`, wired
//! into every `diff` run — see `commands::diff`). Review/rollback stay
//! stubs (`run_rules_stub`) until their own infrastructure lands.

use std::process::Command;

use autoreview_core::{draft_candidate, mine_candidates, run_bench, write_seed_file, AgentBackend, BenchVerdict, ClaudeCodeBackend, DraftOutcome, HistoryStore, LocalLlmBackend, PiBackend};
use autoreview_schema::AgentBackendKind;

use super::history::history_dir_for;

fn backend_available(kind: AgentBackendKind, config: &autoreview_schema::AutoreviewConfig) -> bool {
    match kind {
        AgentBackendKind::ClaudeCode => Command::new("claude").arg("--version").output().map(|o| o.status.success()).unwrap_or(false),
        AgentBackendKind::Pi => Command::new("pi").arg("--version").output().map(|o| o.status.success()).unwrap_or(false),
        AgentBackendKind::LocalLlm => autoreview_core::local_llm_available(&config.agents.local_llm.base_url, "curl"),
    }
}

fn build_backend(kind: AgentBackendKind, config: &autoreview_schema::AutoreviewConfig) -> Box<dyn AgentBackend + Sync> {
    match kind {
        AgentBackendKind::ClaudeCode => Box::new(ClaudeCodeBackend::default()),
        AgentBackendKind::Pi => Box::new(PiBackend { binary: "pi".to_string(), provider: config.agents.pi_provider.clone() }),
        AgentBackendKind::LocalLlm => Box::new(LocalLlmBackend { base_url: config.agents.local_llm.base_url.clone(), curl_binary: "curl".to_string() }),
    }
}

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

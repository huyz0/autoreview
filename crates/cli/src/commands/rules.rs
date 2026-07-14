//! `autoreview rules mine` — the first two real (non-stub) pieces of the M3
//! rule factory: clusters recorded agent findings into candidate seeds
//! (`.autoreview/rules/candidates/<clusterId>/seed.json`), then attempts to
//! draft an ast-grep rule for each seed via the configured agent backend,
//! 5x ensemble-agreement filtered (`rule_factory::draft`). Bench/review/
//! shadow-log/rollback stay stubs (`run_rules_stub`) until their own
//! infrastructure lands — a drafted candidate here still needs a human to
//! supply real positive/negative fixtures before it could ever be benched.

use std::process::Command;

use autoreview_core::{draft_candidate, mine_candidates, write_seed_file, AgentBackend, ClaudeCodeBackend, DraftOutcome, HistoryStore, LocalLlmBackend, PiBackend};
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
    println!("\n(Bench/review/shadow/promote are not yet implemented — drafted candidates above still need human-supplied fixtures before they could be benched.)");
    Ok(())
}

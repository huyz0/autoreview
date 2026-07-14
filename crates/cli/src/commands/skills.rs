use std::path::Path;
use std::process::Command;

use autoreview_core::{discover_manifests, draft_negative_guidance, mine_negative_guidance, write_skill_proposal_file, AgentBackend, ClaudeCodeBackend, HistoryStore, LocalLlmBackend, PiBackend};
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

/// Lists every skill visible to this repo — embedded builtins plus any
/// repo-local overrides under `.autoreview/skills/` (which shadow a builtin
/// of the same id). Unlike the other M1 command stubs, this one is fully
/// real: `discover_manifests`/`compile_skill` already exist for Stage 3, so
/// surfacing them as a standalone command costs nothing extra.
pub fn run_skills_list(repo_root: &Path) -> anyhow::Result<()> {
    let manifests = discover_manifests(repo_root)?;
    println!("Skills available in this repo:\n");
    for manifest in &manifests {
        let triggers = if manifest.triggers.always {
            "always".to_string()
        } else {
            let mut parts = Vec::new();
            if !manifest.triggers.globs.is_empty() {
                parts.push(format!("globs: {}", manifest.triggers.globs.join(", ")));
            }
            if !manifest.triggers.signals.is_empty() {
                parts.push(format!("signals: {}", manifest.triggers.signals.join(", ")));
            }
            if parts.is_empty() {
                "(none)".to_string()
            } else {
                parts.join("; ")
            }
        };
        println!("  {} (v{})", manifest.id, manifest.version);
        println!("    title:      {}", manifest.title);
        println!("    categories: {}", manifest.categories.join(", "));
        println!("    cost class: {:?}", manifest.cost_class);
        println!("    triggers:   {triggers}");
        println!();
    }
    Ok(())
}

pub fn run_skills_stub(action: &str) {
    println!("`autoreview skills {action}` is not implemented yet — planned for M3 (feedback-driven skill evolution + replay eval), per the project plan.");
    println!("Available today: `autoreview skills list`, `autoreview skills mine`.");
}

/// `autoreview skills mine` — channel 2 of skill evolution: clusters
/// repeated `--fp` feedback (with a human-supplied `--note`) by category,
/// drafts one negative-guidance instruction line per cluster via the
/// configured agent backend, and writes each as a proposal document under
/// `.autoreview/skills/<category>/proposals/<clusterId>.md`. Channels 1
/// (rule-draft inexpressible verdicts) and 3 (`--missed` reports) aren't
/// wired in yet — see `skill_evolution` module docs.
pub fn run_skills_mine(repo_root: &Path) -> anyhow::Result<()> {
    let history_dir = history_dir_for(repo_root);
    let store = HistoryStore::open(&history_dir)?;
    let rows = store.fp_feedback_with_notes()?;

    if rows.is_empty() {
        println!("No noted `--fp` feedback recorded yet on this machine — nothing to mine. Use `autoreview feedback <id> --fp --note \"...\"` a few times first.");
        return Ok(());
    }

    let seeds = mine_negative_guidance(rows);
    if seeds.is_empty() {
        println!("No recurring false-positive clusters found (need >= 3 similar noted --fp reports in the same category).");
        return Ok(());
    }

    let config = autoreview_core::load_config(&repo_root.join(".autoreview").join("config.yaml"))?;
    let backend_kind = config.agents.backend;
    let backend = backend_available(backend_kind, &config).then(|| build_backend(backend_kind, &config));
    let model = cheap_model_for(backend_kind, &config).to_string();

    println!("Found {} negative-guidance cluster(s):", seeds.len());
    for seed in &seeds {
        let drafted = backend.as_ref().map(|backend| draft_negative_guidance(backend.as_ref(), seed, &model, 2, repo_root).0).unwrap_or(None);
        let path = write_skill_proposal_file(repo_root, seed, drafted.as_deref())?;
        println!("  {} ({}, {} note(s)) -> {}", seed.cluster_id, seed.category, seed.notes.len(), path.display());
        match &drafted {
            Some(line) => println!("    drafted: {line}"),
            None => println!("    drafted: (skipped — no agent backend available)"),
        }
    }
    println!("\n(`skills review` is not yet implemented — these proposals still need a human to review and apply them.)");
    Ok(())
}

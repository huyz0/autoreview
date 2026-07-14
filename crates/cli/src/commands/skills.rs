use std::path::Path;
use std::process::Command;

use autoreview_core::{discover_manifests, draft_negative_guidance, materialize_builtin_skill_to_disk, mine_negative_guidance, write_skill_proposal_file, AgentBackend, ClaudeCodeBackend, HistoryStore, LocalLlmBackend, PiBackend};
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
    println!("\n(use `autoreview skills review` to list proposals, or `skills bench <aspect> <proposalId>` to replay-bench one)");
    Ok(())
}

/// Extracts the drafted line from a `write_skill_proposal_file`-produced
/// document — everything after the "## Drafted instruction line" heading.
fn extract_drafted_line(proposal_text: &str) -> Option<String> {
    let marker = "## Drafted instruction line\n\n";
    let start = proposal_text.find(marker)? + marker.len();
    let line = proposal_text[start..].lines().next()?.trim();
    if line.is_empty() || line.starts_with('(') {
        None
    } else {
        Some(line.to_string())
    }
}

fn skill_proposals_dir(repo_root: &Path, aspect: &str) -> std::path::PathBuf {
    repo_root.join(".autoreview").join("skills").join(aspect).join("proposals")
}

/// Every `(aspect, proposalId)` pair with a proposal file on disk, across
/// every aspect directory under `.autoreview/skills/`.
fn list_all_proposals(repo_root: &Path) -> Vec<(String, String)> {
    let skills_dir = repo_root.join(".autoreview").join("skills");
    let Ok(aspect_entries) = std::fs::read_dir(&skills_dir) else { return Vec::new() };
    let mut result = Vec::new();
    for aspect_entry in aspect_entries.filter_map(|e| e.ok()) {
        let Some(aspect) = aspect_entry.file_name().into_string().ok() else { continue };
        let proposals_dir = skill_proposals_dir(repo_root, &aspect);
        let Ok(proposal_entries) = std::fs::read_dir(&proposals_dir) else { continue };
        for entry in proposal_entries.filter_map(|e| e.ok()) {
            if entry.path().extension().and_then(|e| e.to_str()) == Some("md") {
                if let Some(stem) = entry.path().file_stem().and_then(|s| s.to_str()) {
                    result.push((aspect.clone(), stem.to_string()));
                }
            }
        }
    }
    result
}

/// `autoreview skills review` — the human-approval gate for a skill-
/// evolution proposal. No args: lists every proposal found across all
/// aspects. `--approve <aspect>:<proposalId>`: materializes the aspect's
/// builtin skill to a repo-local override (if none exists yet) and appends
/// the proposal's drafted line to its `instructions.md` — the actual
/// prompt-edit landing, version-bumping is left for `skills rollback`
/// (still a stub) to reason about later. `--reject <aspect>:<proposalId>
/// --reason <text>`: records why, alongside the proposal file.
pub fn run_skills_review(repo_root: &Path, approve: Option<String>, reject: Option<String>, reason: Option<String>) -> anyhow::Result<()> {
    if let Some(spec) = approve {
        let (aspect, proposal_id) = spec.split_once(':').ok_or_else(|| anyhow::anyhow!("--approve expects <aspect>:<proposalId>"))?;
        return approve_proposal(repo_root, aspect, proposal_id);
    }
    if let Some(spec) = reject {
        let (aspect, proposal_id) = spec.split_once(':').ok_or_else(|| anyhow::anyhow!("--reject expects <aspect>:<proposalId>"))?;
        let Some(reason) = reason else {
            anyhow::bail!("--reject requires --reason \"<why>\"");
        };
        return reject_proposal(repo_root, aspect, proposal_id, &reason);
    }

    let proposals = list_all_proposals(repo_root);
    if proposals.is_empty() {
        println!("No skill-evolution proposals found — run `autoreview skills mine` first.");
        return Ok(());
    }
    println!("Skill-evolution proposals pending review:\n");
    for (aspect, proposal_id) in proposals {
        let dir = skill_proposals_dir(repo_root, &aspect);
        let status = if dir.join(format!("{proposal_id}.approved.json")).exists() {
            "approved"
        } else if dir.join(format!("{proposal_id}.rejected.json")).exists() {
            "rejected"
        } else {
            "pending"
        };
        println!("  {aspect}:{proposal_id}  status={status}");
    }
    println!("\n(use --approve <aspect>:<proposalId>, or --reject <aspect>:<proposalId> --reason \"<why>\")");
    Ok(())
}

fn approve_proposal(repo_root: &Path, aspect: &str, proposal_id: &str) -> anyhow::Result<()> {
    let proposal_path = skill_proposals_dir(repo_root, aspect).join(format!("{proposal_id}.md"));
    let proposal_text = std::fs::read_to_string(&proposal_path).map_err(|_| anyhow::anyhow!("no proposal found at {}", proposal_path.display()))?;
    let drafted_line = extract_drafted_line(&proposal_text).ok_or_else(|| anyhow::anyhow!("proposal {proposal_id} has no drafted instruction line to apply"))?;

    let disk_dir = materialize_builtin_skill_to_disk(repo_root, aspect)?;
    let instructions_path = disk_dir.join("instructions.md");
    let mut instructions = std::fs::read_to_string(&instructions_path).unwrap_or_default();
    instructions.push_str("\n\n");
    instructions.push_str(&drafted_line);
    instructions.push('\n');
    std::fs::write(&instructions_path, instructions)?;

    let marker_path = skill_proposals_dir(repo_root, aspect).join(format!("{proposal_id}.approved.json"));
    std::fs::write(&marker_path, serde_json::json!({ "approvedAt": chrono::Utc::now().to_rfc3339(), "appliedLine": drafted_line }).to_string())?;
    println!("Approved '{aspect}:{proposal_id}' — appended to {}", instructions_path.display());
    Ok(())
}

fn reject_proposal(repo_root: &Path, aspect: &str, proposal_id: &str, reason: &str) -> anyhow::Result<()> {
    let proposal_path = skill_proposals_dir(repo_root, aspect).join(format!("{proposal_id}.md"));
    if !proposal_path.exists() {
        anyhow::bail!("no proposal found at {}", proposal_path.display());
    }
    let marker_path = skill_proposals_dir(repo_root, aspect).join(format!("{proposal_id}.rejected.json"));
    std::fs::write(&marker_path, serde_json::json!({ "reason": reason, "rejectedAt": chrono::Utc::now().to_rfc3339() }).to_string())?;
    println!("Rejected '{aspect}:{proposal_id}': {reason}");
    Ok(())
}

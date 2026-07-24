use std::path::Path;

use autoreview_core::{discover_manifests, draft_negative_guidance, materialize_builtin_skill_to_disk, mine_negative_guidance, write_skill_proposal_file, HistoryStore};
use autoreview_schema::AgentBackendKind;

use super::backend::{backend_available, build_backend};
use super::history::history_dir_for;

fn cheap_model_for(kind: AgentBackendKind, config: &autoreview_schema::AutoreviewConfig) -> &str {
    match kind {
        AgentBackendKind::LocalLlm => &config.agents.local_llm.model,
        AgentBackendKind::OpenAiCompatible => &config.agents.open_ai_compatible.model,
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
/// builtin skill to a repo-local override (if none exists yet), appends
/// the proposal's drafted line to its `instructions.md`, and snapshots the
/// result as a new version in `skill_versions` (`skills rollback` reads
/// these back). `--reject <aspect>:<proposalId> --reason <text>`: records
/// why, alongside the proposal file.
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

    let history_dir = history_dir_for(repo_root);
    let store = HistoryStore::open(&history_dir)?;
    let now = chrono::Utc::now().to_rfc3339();
    // First approval for this aspect: snapshot the pre-edit content as
    // version "0" before touching it, so `skills rollback` can always get
    // back to the unmodified baseline, not just to the most recent edit.
    if store.latest_skill_version(aspect)?.is_none() {
        store.record_skill_version(aspect, "0", &instructions, &now)?;
    }

    instructions.push_str("\n\n");
    instructions.push_str(&drafted_line);
    instructions.push('\n');
    std::fs::write(&instructions_path, &instructions)?;

    let next_version = store.latest_skill_version(aspect)?.unwrap_or(0) + 1;
    store.record_skill_version(aspect, &next_version.to_string(), &instructions, &now)?;

    let marker_path = skill_proposals_dir(repo_root, aspect).join(format!("{proposal_id}.approved.json"));
    std::fs::write(&marker_path, serde_json::json!({ "approvedAt": &now, "appliedLine": drafted_line, "version": next_version }).to_string())?;
    println!("Approved '{aspect}:{proposal_id}' — appended to {} (version {next_version}, use `skills rollback {aspect} <version>` to undo)", instructions_path.display());
    Ok(())
}

/// Restores a skill aspect's `instructions.md` to a previously recorded
/// version — the manual override `skills review`'s own doc comment
/// deferred to here. Version `"0"` is the pre-evolution baseline, snapshotted
/// automatically on the aspect's first `--approve`; each later approval
/// adds one more version on top.
pub fn run_skills_rollback(repo_root: &Path, aspect: &str, version: &str) -> anyhow::Result<()> {
    let history_dir = history_dir_for(repo_root);
    let store = HistoryStore::open(&history_dir)?;

    let Some(source) = store.skill_version_source(aspect, version)? else {
        let available = store.list_skill_versions(aspect)?;
        if available.is_empty() {
            anyhow::bail!("no tracked versions for skill '{aspect}' — versions are recorded starting with that aspect's first `skills review --approve`");
        }
        let versions = available.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>().join(", ");
        anyhow::bail!("skill '{aspect}' has no version '{version}' — available versions: {versions}");
    };

    let disk_dir = materialize_builtin_skill_to_disk(repo_root, aspect)?;
    let instructions_path = disk_dir.join("instructions.md");
    std::fs::write(&instructions_path, &source)?;
    println!("Rolled back skill '{aspect}' to version {version} — wrote {}", instructions_path.display());
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

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_core::{write_skill_proposal_file, NegativeGuidanceSeed};
    use autoreview_test_support::init_repo;

    fn seed_proposal(repo_root: &Path, aspect: &str, cluster_id: &str, drafted_line: &str) {
        let seed = NegativeGuidanceSeed {
            cluster_id: cluster_id.to_string(),
            category: aspect.to_string(),
            representative_title: "t".to_string(),
            representative_message: "m".to_string(),
            notes: vec!["n".to_string()],
        };
        write_skill_proposal_file(repo_root, &seed, Some(drafted_line)).unwrap();
    }

    #[test]
    fn rollback_of_an_untracked_aspect_errors_clearly() {
        let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
        let err = run_skills_rollback(repo.path(), "correctness", "0").unwrap_err();
        assert!(err.to_string().contains("no tracked versions"), "got: {err}");
    }

    #[test]
    fn approving_a_proposal_snapshots_a_baseline_and_a_new_version() {
        let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
        seed_proposal(repo.path(), "correctness", "cluster-1", "Do not flag X.");

        approve_proposal(repo.path(), "correctness", "cluster-1").unwrap();

        let history_dir = history_dir_for(repo.path());
        let store = HistoryStore::open(&history_dir).unwrap();
        assert_eq!(store.latest_skill_version("correctness").unwrap(), Some(1));
        let baseline = store.skill_version_source("correctness", "0").unwrap().unwrap();
        let v1 = store.skill_version_source("correctness", "1").unwrap().unwrap();
        assert!(!baseline.contains("Do not flag X."), "version 0 must be the pre-edit baseline");
        assert!(v1.contains("Do not flag X."), "version 1 must include the newly appended line");
    }

    #[test]
    fn rollback_restores_a_prior_versions_content() {
        let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
        seed_proposal(repo.path(), "correctness", "cluster-1", "Do not flag X.");
        approve_proposal(repo.path(), "correctness", "cluster-1").unwrap();

        let instructions_path = repo.path().join(".autoreview/skills/correctness/instructions.md");
        assert!(std::fs::read_to_string(&instructions_path).unwrap().contains("Do not flag X."));

        run_skills_rollback(repo.path(), "correctness", "0").unwrap();
        let restored = std::fs::read_to_string(&instructions_path).unwrap();
        assert!(!restored.contains("Do not flag X."), "rollback to version 0 must remove what version 1 added");
    }

    #[test]
    fn rollback_to_a_nonexistent_version_lists_the_available_ones() {
        let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
        seed_proposal(repo.path(), "correctness", "cluster-1", "Do not flag X.");
        approve_proposal(repo.path(), "correctness", "cluster-1").unwrap();

        let err = run_skills_rollback(repo.path(), "correctness", "99").unwrap_err();
        assert!(err.to_string().contains("0") && err.to_string().contains("1"), "got: {err}");
    }
}

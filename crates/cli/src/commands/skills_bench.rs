//! `autoreview skills bench <aspect> <proposalId>` — the replay-bench
//! harness the plan calls for before a skill-evolution proposal can be
//! adopted: checks out each past run with known `--fp`/`--tp` feedback into
//! an isolated `git worktree` (never touches the user's working tree),
//! runs the candidate skill (builtin/current instructions + the proposed
//! line appended) once against that replayed diff, and compares its
//! findings against the known ground truth via `compare_replay`.
//!
//! Simplification versus the plan's fuller design, stated plainly: this
//! only runs the *candidate* per replay case, not old-vs-candidate side by
//! side — the "old" side is already known (it's exactly the historical
//! run's own recorded findings + their `--fp`/`--tp` verdicts, which is
//! what `known_verdicts_for_run` supplies), so re-running the old skill
//! would just be re-deriving data already on hand.

use std::path::Path;
use std::process::Command;

use autoreview_core::{compare_replay, compile_skill, run_specialist, AgentBackend, HistoryStore, ReplayComparison};
use autoreview_schema::{AgentBackendKind, ReviewReport, Tier};

use super::backend::{backend_available, build_backend};
use super::history::history_dir_for;

fn cheap_model_for(kind: AgentBackendKind, config: &autoreview_schema::AutoreviewConfig) -> &str {
    match kind {
        AgentBackendKind::LocalLlm => &config.agents.local_llm.model,
        AgentBackendKind::OpenAiCompatible => &config.agents.open_ai_compatible.model,
        AgentBackendKind::ClaudeCode | AgentBackendKind::Pi => &config.budgets.models.cheap,
    }
}

/// Extracts the drafted line from a `write_proposal_file`-produced
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

struct ReplayCase {
    run_id: String,
    repo_root: std::path::PathBuf,
    base_ref: String,
    head_ref: String,
}

fn load_replay_cases(repo_root: &Path, history_dir: &Path, run_ids: &[String]) -> Vec<ReplayCase> {
    run_ids
        .iter()
        .filter_map(|run_id| {
            let report_path = history_dir.join("runs").join(run_id).join("report.json");
            let contents = std::fs::read_to_string(&report_path).ok()?;
            let report: ReviewReport = serde_json::from_str(&contents).ok()?;
            if Path::new(&report.target.repo_root) != repo_root {
                return None;
            }
            Some(ReplayCase { run_id: run_id.clone(), repo_root: repo_root.to_path_buf(), base_ref: report.target.base_ref, head_ref: report.target.head_ref })
        })
        .collect()
}

/// Runs the candidate skill against one replayed diff inside an isolated
/// worktree. Returns `None` (not an error) on any step that fails —
/// missing refs, worktree add failure, invoke/parse failure — since one
/// bad replay case shouldn't abort the whole bench run.
fn run_candidate_for_case(case: &ReplayCase, aspect: &str, drafted_line: &str, backend: &dyn AgentBackend, model: &str) -> Option<Vec<autoreview_schema::AgentFinding>> {
    let worktree_dir = std::env::temp_dir().join(format!("autoreview-replay-{}", uuid::Uuid::new_v4()));
    let add_status = Command::new("git").args(["worktree", "add", "--detach"]).arg(&worktree_dir).arg(&case.head_ref).current_dir(&case.repo_root).status().ok()?;
    if !add_status.success() {
        return None;
    }

    let result = (|| -> Option<Vec<autoreview_schema::AgentFinding>> {
        let compiled = compile_skill(&worktree_dir, aspect, Tier::Standard).ok()?;
        let candidate_instructions = format!("{}\n\n{}", compiled.system_prompt, drafted_line);

        let diff_output = Command::new("git").args(["diff", &format!("{}...{}", case.base_ref, case.head_ref)]).current_dir(&worktree_dir).output().ok()?;
        let diff_text = String::from_utf8_lossy(&diff_output.stdout).to_string();

        let task_prompt = format!("Review the following diff for {aspect} issues.\n\n```diff\n{diff_text}\n```");
        let request = autoreview_core::InvokeRequest {
            prompt: String::new(),
            system_prompt: candidate_instructions,
            allowed_tools: vec!["Read".to_string(), "Grep".to_string()],
            max_turns: 6,
            model: model.to_string(),
            cwd: worktree_dir.clone(),
        };
        let specialist_result = run_specialist(backend, aspect, request, &task_prompt);
        Some(specialist_result.findings)
    })();

    let _ = Command::new("git").args(["worktree", "remove", "--force"]).arg(&worktree_dir).current_dir(&case.repo_root).status();
    result
}

pub fn run_skills_bench(repo_root: &Path, aspect: &str, proposal_id: &str) -> anyhow::Result<()> {
    let proposal_path = repo_root.join(".autoreview").join("skills").join(aspect).join("proposals").join(format!("{proposal_id}.md"));
    let proposal_text = std::fs::read_to_string(&proposal_path).map_err(|_| anyhow::anyhow!("no proposal found at {} — run `autoreview skills mine` first", proposal_path.display()))?;
    let Some(drafted_line) = extract_drafted_line(&proposal_text) else {
        anyhow::bail!("proposal {proposal_id} has no drafted instruction line yet (drafting was skipped or failed when it was mined) — nothing to bench");
    };

    let config = autoreview_core::load_config(&repo_root.join(".autoreview").join("config.yaml"))?;
    let backend_kind = config.agents.backend;
    if !backend_available(backend_kind, &config) {
        anyhow::bail!("no agent backend available — bench needs to actually run the candidate skill against replayed diffs");
    }
    let backend = build_backend(backend_kind, &config);
    let model = cheap_model_for(backend_kind, &config).to_string();

    let history_dir = history_dir_for(repo_root);
    let store = HistoryStore::open(&history_dir)?;
    let run_ids = store.runs_with_known_verdicts()?;
    let cases = load_replay_cases(repo_root, &history_dir, &run_ids);

    if cases.is_empty() {
        println!("No replay corpus available: need past `autoreview diff` runs on this repo with `--fp`/`--tp` feedback recorded against their findings.");
        return Ok(());
    }

    println!("Replaying {} case(s) for aspect '{aspect}' with proposal '{proposal_id}':", cases.len());
    let mut total = ReplayComparison::default();
    let mut replayed = 0usize;
    for case in &cases {
        let known = store.known_verdicts_for_run(&case.run_id)?;
        if known.is_empty() {
            continue;
        }
        match run_candidate_for_case(case, aspect, &drafted_line, backend.as_ref(), &model) {
            Some(candidate_findings) => {
                let comparison = compare_replay(&known, &candidate_findings);
                println!("  run {}: {}/{} FP(s) resolved, {} TP(s) lost", case.run_id, comparison.fp_resolved, comparison.fp_total, comparison.tp_lost.len());
                total.fp_total += comparison.fp_total;
                total.fp_resolved += comparison.fp_resolved;
                total.tp_total += comparison.tp_total;
                total.tp_lost.extend(comparison.tp_lost);
                replayed += 1;
            }
            None => println!("  run {}: skipped (worktree checkout or specialist invocation failed)", case.run_id),
        }
    }

    if replayed == 0 {
        println!("\nNo replay case could actually be run (refs may no longer exist, or no backend reachable).");
        return Ok(());
    }

    println!(
        "\ntotals: {}/{} FP(s) resolved ({:.0}%), {} TP(s) lost{}",
        total.fp_resolved,
        total.fp_total,
        if total.fp_total > 0 { total.fp_resolved as f64 / total.fp_total as f64 * 100.0 } else { 0.0 },
        total.tp_lost.len(),
        if total.tp_lost.is_empty() { String::new() } else { format!(" ({})", total.tp_lost.join(", ")) }
    );
    if total.passes_gate() {
        println!("verdict: passes — ready for `autoreview skills review` (still a stub)");
    } else {
        println!("verdict: does not pass — needs >=70% FP resolution and zero lost TPs");
    }
    Ok(())
}

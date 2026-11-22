use autoreview_core::{run_bench, BenchVerdict, HistoryStore};
use autoreview_core::rule_factory::{rule_store_directory, validate_rule_path_id};

use super::{resolved_packs_for_mining, validate_rule_file};
use crate::commands::{diff::move_shadow_rule_file, history::history_dir_for};

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
        BenchVerdict::Candidate => println!("\nverdict: candidate — ready for `autoreview rules review --approve <clusterId>`"),
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
    println!("\n(use `autoreview feedback <id> --fp|--tp` on a finding's own id to feed the promotion/demotion gates — use --doesnt-apply instead of --fp if the rule is valid but just not relevant here, since that does NOT count toward demotion)");
    Ok(())
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
    validate_rule_path_id(cluster_id)?;
    let dir = candidates_dir(repo_root).join(cluster_id);
    let rule_path = dir.join("rule.yaml");
    anyhow::ensure!(dir.is_dir() && dir.canonicalize()?.starts_with(repo_root.canonicalize()?), "candidate directory must be inside the repository");
    anyhow::ensure!(rule_path.symlink_metadata().is_ok_and(|meta| meta.is_file()), "no regular candidate rule found at {}", rule_path.display());
    let contents = std::fs::read_to_string(&rule_path).map_err(|_| anyhow::anyhow!("no drafted rule found at {} — run `autoreview rules mine` first", rule_path.display()))?;
    let (id, kind) = validate_rule_file(&contents)?;
    validate_rule_path_id(&id)?;
    anyhow::ensure!(kind == "pattern", "only pattern rules can use the learned rule lifecycle");
    anyhow::ensure!(
        autoreview_core::find_rule_definition(&id, &resolved_packs_for_mining(repo_root)).is_none()
            && autoreview_core::rust_review_rule(&id).is_none(),
        "rule '{id}' is already active in the rule catalog"
    );
    let bench = run_bench(repo_root, cluster_id)?;
    anyhow::ensure!(bench.verdict == BenchVerdict::Candidate, "candidate '{cluster_id}' failed its approval checks ({:?}). Run autoreview rules bench {cluster_id} for details", bench.verdict);
    anyhow::ensure!(std::fs::read_to_string(&rule_path)? == contents, "candidate '{cluster_id}' changed during the benchmark. Retry approval");

    let history_dir = history_dir_for(repo_root);
    let store = HistoryStore::open(&history_dir)?;
    anyhow::ensure!(store.rule_state(&id)?.is_none(), "rule '{id}' is already tracked");
    let shadow_dir = rule_store_directory(repo_root, &["shadow"])?;
    let mut output = tempfile::NamedTempFile::new_in(&shadow_dir)?;
    std::io::Write::write_all(&mut output, contents.as_bytes())?;
    let installed = shadow_dir.join(format!("{id}.yaml"));
    output.persist_noclobber(&installed)?;
    if let Err(err) = store.ensure_rule_tracked(&id, "shadow", &chrono::Utc::now().to_rfc3339()) {
        std::fs::remove_file(&installed)?;
        return Err(err);
    }

    let mut approval = tempfile::NamedTempFile::new_in(&dir)?;
    serde_json::to_writer(&mut approval, &serde_json::json!({ "ruleId": id, "approvedAt": chrono::Utc::now().to_rfc3339() }))?;
    approval.persist(dir.join("approved.json"))?;
    println!("Approved '{cluster_id}' — rule '{id}' is now in shadow mode (see `autoreview rules shadow-log {id}`).");
    Ok(())
}

fn reject_candidate(repo_root: &std::path::Path, cluster_id: &str, reason: &str) -> anyhow::Result<()> {
    validate_rule_path_id(cluster_id)?;
    let dir = candidates_dir(repo_root).join(cluster_id);
    if !dir.exists() {
        anyhow::bail!("no candidate '{cluster_id}' found under {}", candidates_dir(repo_root).display());
    }
    anyhow::ensure!(dir.canonicalize()?.starts_with(repo_root.canonicalize()?), "candidate directory is outside the repository");
    let mut rejection = tempfile::NamedTempFile::new_in(&dir)?;
    serde_json::to_writer(&mut rejection, &serde_json::json!({ "reason": reason, "rejectedAt": chrono::Utc::now().to_rfc3339() }))?;
    rejection.persist(dir.join("rejected.json"))?;
    println!("Rejected '{cluster_id}': {reason}");
    Ok(())
}

/// Manually reverses a shadow/promoted rule's most recent lifecycle step —
/// the human override sitting alongside the automatic `should_promote`/
/// `should_demote` gate `diff.rs` already runs on every review. A
/// `"promoted"` rule demotes back to `"shadow"` (same status flip and
/// on-disk file move `diff.rs` does automatically on a real demotion, just
/// triggered by hand instead of the firing-history gate). A `"shadow"`
/// rule rolls all the way back to `"rejected"` — undoing the original
/// `rules review --approve` — moving its file to `.autoreview/rules/
/// rejected/` (kept, not deleted, so it can be inspected or manually
/// restored) and stamping `invalid_at` on its history row.
pub fn run_rules_rollback(repo_root: &std::path::Path, rule_id: &str) -> anyhow::Result<()> {
    let history_dir = history_dir_for(repo_root);
    let store = HistoryStore::open(&history_dir)?;

    let Some(state) = store.rule_state(rule_id)? else {
        anyhow::bail!("rule '{rule_id}' is not tracked (never fired in shadow/promoted mode) — nothing to roll back");
    };

    match state.status.as_str() {
        "promoted" => {
            store.set_rule_status(rule_id, "shadow")?;
            move_shadow_rule_file(repo_root, rule_id, "promoted", "shadow");
            println!("Rolled back '{rule_id}': promoted -> shadow.");
        }
        "shadow" => {
            store.set_rule_status(rule_id, "rejected")?;
            store.invalidate_rule(rule_id, &chrono::Utc::now().to_rfc3339())?;
            move_shadow_rule_file(repo_root, rule_id, "shadow", "rejected");
            println!("Rolled back '{rule_id}': shadow -> rejected (rule file moved to .autoreview/rules/rejected/, not deleted).");
        }
        other => anyhow::bail!("rule '{rule_id}' has status '{other}' — rollback only applies to a 'shadow' or 'promoted' rule"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_test_support::init_repo;

    fn write_rule_file(repo_root: &std::path::Path, status: &str, rule_id: &str) {
        let dir = repo_root.join(".autoreview").join("rules").join(status);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{rule_id}.yaml")), format!("id: {rule_id}\nlanguage: Go\ncategory: correctness\nseverity: warning\nmessage: m\nrule:\n  pattern: $A == $A\n")).unwrap();
    }

    #[test]
    fn rollback_of_an_untracked_rule_errors_clearly() {
        let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
        let err = run_rules_rollback(repo.path(), "never-seen-rule").unwrap_err();
        assert!(err.to_string().contains("not tracked"), "got: {err}");
    }

    fn tracked_rule_fixture(status: &str) -> autoreview_test_support::TestRepo {
        let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
        write_rule_file(repo.path(), status, "go-example");
        let store = HistoryStore::open(&history_dir_for(repo.path())).unwrap();
        store.ensure_rule_tracked("go-example", "shadow", "2026-07-01T00:00:00Z").unwrap();
        store.set_rule_status("go-example", status).unwrap();
        repo
    }

    fn assert_rollback(from: &str, to: &str) {
        let repo = tracked_rule_fixture(from);
        run_rules_rollback(repo.path(), "go-example").unwrap();

        let store = HistoryStore::open(&history_dir_for(repo.path())).unwrap();
        let state = store.rule_state("go-example").unwrap().unwrap();
        assert_eq!(state.status, to);
        assert_eq!(state.invalid_at.is_some(), to == "rejected");
        let rules = repo.path().join(".autoreview/rules");
        assert!(rules.join(to).join("go-example.yaml").exists());
        assert!(!rules.join(from).join("go-example.yaml").exists());
    }

    #[test]
    fn rollback_demotes_a_promoted_rule_back_to_shadow() {
        assert_rollback("promoted", "shadow");
    }

    #[test]
    fn rollback_rejects_a_shadow_rule_and_moves_its_file_without_deleting_it() {
        assert_rollback("shadow", "rejected");
    }

    #[test]
    fn rollback_of_an_already_rejected_rule_errors_with_a_clear_message() {
        let repo = init_repo(&[("main.go", "package main\n\nfunc main() {}\n")]);
        let history_dir = history_dir_for(repo.path());
        let store = HistoryStore::open(&history_dir).unwrap();
        store.ensure_rule_tracked("go-example", "rejected", "2026-07-01T00:00:00Z").unwrap();
        drop(store);

        let err = run_rules_rollback(repo.path(), "go-example").unwrap_err();
        assert!(err.to_string().contains("rejected"), "got: {err}");
    }
}

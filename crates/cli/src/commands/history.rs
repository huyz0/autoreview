//! Shared repo-history location logic (fingerprint, cache dir, hostname) —
//! used by both `diff` (writes history) and `feedback` (reads + appends to
//! it), so the two commands agree on where history lives without duplicating
//! the derivation.

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use autoreview_schema::Finding;

/// Searches this repo's recorded run reports (newest first) for a finding
/// with the given id — findings live in the per-run `report.json`
/// artifacts, not the SQLite index, which only stores enough metadata to
/// resolve an id back to its fingerprint. Shared by `apply` (needs the
/// finding's suggestion patch) and `explain` (needs its source/rule info).
pub fn find_finding_in_run_reports(history_dir: &Path, finding_id: &str) -> anyhow::Result<Option<Finding>> {
    let runs_dir = history_dir.join("runs");
    let mut run_dirs: Vec<_> = match std::fs::read_dir(&runs_dir) {
        Ok(entries) => entries.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(_) => return Ok(None),
    };
    run_dirs.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    run_dirs.reverse();

    for run_dir in run_dirs {
        let report_path = run_dir.join("report.json");
        let Ok(text) = std::fs::read_to_string(&report_path) else { continue };
        let Ok(report) = serde_json::from_str::<autoreview_schema::ReviewReport>(&text) else { continue };
        if let Some(finding) = report.findings.into_iter().find(|f| f.id == finding_id) {
            return Ok(Some(finding));
        }
    }
    Ok(None)
}

pub fn repo_fingerprint(repo_root: &Path, remote_url: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(remote_url.unwrap_or(&repo_root.to_string_lossy()).as_bytes());
    let digest = hasher.finalize();
    format!("{digest:x}")[..16].to_string()
}

pub fn resolve_remote_url(repo_root: &Path) -> Option<String> {
    let output = Command::new("git").args(["remote", "get-url", "origin"]).current_dir(repo_root).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn history_dir_for(repo_root: &Path) -> PathBuf {
    let remote_url = resolve_remote_url(repo_root);
    let fingerprint = repo_fingerprint(repo_root, remote_url.as_deref());
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("autoreview").join(fingerprint)
}

pub fn hostname() -> String {
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.is_empty() {
            return h;
        }
    }
    if let Ok(output) = Command::new("hostname").output() {
        if output.status.success() {
            let h = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !h.is_empty() {
                return h;
            }
        }
    }
    "unknown-host".to_string()
}

/// `autoreview history costs [--since YYYY-MM-DD]` — aggregates every local
/// run's `RunCosts` (already written to `<history_dir>/runs/<run_id>/
/// report.json` by every `autoreview diff`) into totals, a per-stage
/// breakdown, and a per-day trend. Purely a read/report over data that
/// already exists on disk — no new persistence, no budget-cap enforcement.
pub fn run_history_costs(repo_root: &Path, since: Option<&str>) -> anyhow::Result<()> {
    let history_dir = history_dir_for(repo_root);
    let records = autoreview_core::load_run_cost_records(&history_dir);
    if records.is_empty() {
        println!("No local run history found under {} — run `autoreview diff` at least once first.", history_dir.display());
        return Ok(());
    }

    let filtered = match since {
        Some(since) => autoreview_core::filter_cost_records_since(&records, since),
        None => records.iter().collect(),
    };
    if filtered.is_empty() {
        println!("No runs on or after {} (out of {} total run(s) in history).", since.unwrap_or(""), records.len());
        return Ok(());
    }

    let dashboard = autoreview_core::summarize_costs(&filtered);

    let range_label = match since {
        Some(since) => format!("since {since}"),
        None => "all time".to_string(),
    };
    println!("autoreview history costs  ({range_label})\n");
    println!("  runs:             {}", dashboard.run_count);
    if dashboard.any_usd_reported {
        println!("  total cost:       ${:.2}", dashboard.total_usd);
    } else {
        println!("  total cost:       (no backend in this history reported USD pricing)");
    }
    println!("  total tokens:     {} in / {} out", dashboard.total_input_tokens, dashboard.total_output_tokens);
    println!("  total wall time:  {:.1}s", dashboard.total_wall_ms as f64 / 1000.0);

    if !dashboard.by_stage_usd.is_empty() {
        println!("\n  by stage:");
        for (stage, usd) in &dashboard.by_stage_usd {
            println!("    {stage:<20} ${usd:.2}");
        }
    }

    if !dashboard.by_day_usd.is_empty() {
        println!("\n  by day:");
        for (day, usd) in &dashboard.by_day_usd {
            println!("    {day}  ${usd:.2}");
        }
    }

    Ok(())
}

/// `autoreview history sync` — manually pulls the team's synced event log
/// (`storage.sync.mode: git`) onto this machine, on top of the best-effort
/// push `diff` already does at the end of every run. Useful right after
/// enabling sync for the first time, or to pull down teammates' signal
/// without also running a full review.
pub fn run_history_sync(repo_root: &Path) -> anyhow::Result<()> {
    let config = autoreview_core::load_config(&repo_root.join(".autoreview").join("config.yaml"))?;
    let history_dir = history_dir_for(repo_root);
    match config.storage.sync.mode {
        autoreview_schema::SyncMode::None => {
            println!("storage.sync.mode is \"none\" in .autoreview/config.yaml — nothing to sync.");
        }
        autoreview_schema::SyncMode::Git => {
            let pulled = autoreview_core::sync_pull(repo_root, &history_dir, &config.storage.sync)?;
            println!("Pulled {pulled} event log file(s) from the team's sync branch ({}).", config.storage.sync.branch);
        }
        autoreview_schema::SyncMode::Remote => {
            let pulled = autoreview_core::sync_pull(repo_root, &history_dir, &config.storage.sync)?;
            println!("Pulled {pulled} event log file(s) from the shared directory ({}).", config.storage.sync.location.as_deref().unwrap_or("(none configured)"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_schema::{
        DiffStats, FindingFingerprints, FindingSource, FindingSourceKind, Location, LocationRange, PlanBudgets, ReviewPlan, ReviewSummary, ReviewTarget, RunCosts, Severity, Side, Tier,
    };
    use std::collections::HashMap;

    fn make_finding(id: &str) -> Finding {
        Finding {
            id: id.to_string(),
            fingerprints: FindingFingerprints { primary: format!("fp-{id}"), secondary: None },
            source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "ast-grep".into(), rule_id: Some("go-no-self-comparison".into()), aspect: None, backend: None },
            category: "correctness".into(),
            severity: Severity::Medium,
            confidence: 1.0,
            title: "t".into(),
            message: "m".into(),
            location: Location { path: "a.go".into(), range: LocationRange { start_line: 1, start_col: None, end_line: None, end_col: None }, snippet: "x".into(), side: Side::New },
            related_locations: None,
            suggestion: None,
            tags: None,
            meta: None,
        }
    }

    fn write_report(history_dir: &Path, run_id: &str, findings: Vec<Finding>) {
        let run_dir = history_dir.join("runs").join(run_id);
        std::fs::create_dir_all(&run_dir).unwrap();
        let report = autoreview_schema::ReviewReport {
            schema_version: "1".into(),
            run_id: run_id.into(),
            created_at: "2026-07-12T00:00:00Z".into(),
            target: ReviewTarget { repo_root: "/repo".into(), base_ref: "main~1".into(), head_ref: "main".into(), diff_stats: DiffStats { files: 1, additions: 1, deletions: 0, languages: HashMap::new() } },
            plan: ReviewPlan { tier: Tier::Quick, score: 1.0, signals: vec![], specialists: vec![], budgets: PlanBudgets { max_agents: 1, total_token_cap: 1, wall_clock_sec: 1 }, overrides: vec![] },
            findings,
            suppressed: vec![],
            costs: RunCosts { total: autoreview_schema::CostEntry { input_tokens: 0, output_tokens: 0, usd: None, wall_ms: 0 }, per_stage: HashMap::new() },
            summary: ReviewSummary { by_severity: HashMap::new(), by_category: HashMap::new(), gate: None },
            spec_verdicts: vec![],
        };
        std::fs::write(run_dir.join("report.json"), serde_json::to_string(&report).unwrap()).unwrap();
    }

    #[test]
    fn finds_a_finding_by_id_across_run_reports() {
        let dir = tempfile::tempdir().unwrap();
        write_report(dir.path(), "run-1", vec![make_finding("f-abc")]);
        let found = find_finding_in_run_reports(dir.path(), "f-abc").unwrap();
        assert_eq!(found.unwrap().id, "f-abc");
    }

    #[test]
    fn returns_none_for_an_unknown_id() {
        let dir = tempfile::tempdir().unwrap();
        write_report(dir.path(), "run-1", vec![make_finding("f-abc")]);
        assert!(find_finding_in_run_reports(dir.path(), "f-nope").unwrap().is_none());
    }

    #[test]
    fn a_missing_history_dir_returns_none_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_finding_in_run_reports(&dir.path().join("nonexistent"), "f-abc").unwrap().is_none());
    }
}

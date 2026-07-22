//! Cross-run cost aggregation over the local per-run report history —
//! backs `autoreview history costs`. Every `autoreview diff` run already
//! writes a full cost breakdown (`RunCosts`) into
//! `<history_dir>/runs/<run_id>/report.json`; nothing aggregates them
//! across runs today, so answering "how much have we spent this week"
//! means opening report.json files one at a time by hand. This only reads
//! what `diff.rs` already writes — no new persistence, no new budget-cap
//! config.

use std::collections::HashMap;
use std::path::Path;

use autoreview_schema::{CostEntry, ReviewReport};

#[derive(Debug, Clone)]
pub struct RunCostRecord {
    pub run_id: String,
    pub created_at: String,
    pub base_ref: String,
    pub head_ref: String,
    pub total: CostEntry,
    pub per_stage: HashMap<String, CostEntry>,
}

/// Reads every `<history_dir>/runs/*/report.json`, oldest first by
/// `created_at`. A run whose report.json is missing or fails to parse is
/// skipped rather than failing the whole load — a dashboard over N-1 runs
/// is still useful when one run's file is corrupt or from an older,
/// incompatible schema version.
pub fn load_run_cost_records(history_dir: &Path) -> Vec<RunCostRecord> {
    let runs_dir = history_dir.join("runs");
    let Ok(entries) = std::fs::read_dir(&runs_dir) else {
        return Vec::new();
    };
    let mut records: Vec<RunCostRecord> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let text = std::fs::read_to_string(entry.path().join("report.json")).ok()?;
            let report: ReviewReport = serde_json::from_str(&text).ok()?;
            Some(RunCostRecord {
                run_id: report.run_id,
                created_at: report.created_at,
                base_ref: report.target.base_ref,
                head_ref: report.target.head_ref,
                total: report.costs.total,
                per_stage: report.costs.per_stage,
            })
        })
        .collect();
    records.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    records
}

/// `records` filtered to those whose `created_at` (an RFC3339 timestamp, so
/// lexical comparison sorts correctly) is on or after `since` — a plain
/// `YYYY-MM-DD` string compares correctly against an RFC3339 prefix without
/// needing a date-parsing dependency here.
pub fn filter_since<'a>(records: &'a [RunCostRecord], since: &str) -> Vec<&'a RunCostRecord> {
    records.iter().filter(|r| r.created_at.as_str() >= since).collect()
}

#[derive(Debug, Default)]
pub struct CostDashboard {
    pub run_count: usize,
    pub total_usd: f64,
    pub any_usd_reported: bool,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_wall_ms: u64,
    /// (stage name, usd) sorted descending by usd.
    pub by_stage_usd: Vec<(String, f64)>,
    /// (day, usd) sorted ascending by day, one entry per day that had at least one run.
    pub by_day_usd: Vec<(String, f64)>,
}

/// Aggregates a slice of run records into dashboard totals. `usd` is
/// `Option` per-run (a backend that doesn't report pricing leaves it
/// `None`) — summed only over runs that reported it, with
/// `any_usd_reported` telling the caller whether the totals mean "$0.00
/// spent" or "no backend here reports USD, don't trust this number."
///
/// `records` need not be pre-sorted: day totals are bucketed by a
/// `HashMap` keyed on the day string (same approach `by_stage` already
/// uses), not by coalescing adjacent same-day entries, so out-of-order
/// input can't silently fragment a day's total into multiple entries.
pub fn summarize(records: &[&RunCostRecord]) -> CostDashboard {
    let mut dashboard = CostDashboard { run_count: records.len(), ..Default::default() };
    let mut by_stage: HashMap<String, f64> = HashMap::new();
    let mut by_day: HashMap<String, f64> = HashMap::new();

    for record in records {
        dashboard.total_input_tokens += record.total.input_tokens;
        dashboard.total_output_tokens += record.total.output_tokens;
        dashboard.total_wall_ms += record.total.wall_ms;
        if let Some(usd) = record.total.usd {
            dashboard.any_usd_reported = true;
            dashboard.total_usd += usd;
            let day = record.created_at.get(..10).unwrap_or(&record.created_at).to_string();
            *by_day.entry(day).or_insert(0.0) += usd;
        }
        for (stage, entry) in &record.per_stage {
            if let Some(usd) = entry.usd {
                *by_stage.entry(stage.clone()).or_insert(0.0) += usd;
            }
        }
    }

    let mut by_stage: Vec<(String, f64)> = by_stage.into_iter().collect();
    by_stage.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    dashboard.by_stage_usd = by_stage;

    let mut by_day: Vec<(String, f64)> = by_day.into_iter().collect();
    by_day.sort_by(|a, b| a.0.cmp(&b.0));
    dashboard.by_day_usd = by_day;

    dashboard
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_schema::{DiffStats, PlanBudgets, ReviewPlan, ReviewSummary, ReviewTarget, RunCosts, Tier};
    use std::collections::HashMap as StdHashMap;

    fn write_report(dir: &Path, run_id: &str, created_at: &str, total_usd: Option<f64>, per_stage: Vec<(&str, f64)>) {
        let run_dir = dir.join("runs").join(run_id);
        std::fs::create_dir_all(&run_dir).unwrap();
        let report = ReviewReport {
            schema_version: "1".into(),
            run_id: run_id.into(),
            created_at: created_at.into(),
            target: ReviewTarget { repo_root: "/repo".into(), base_ref: "main~1".into(), head_ref: "main".into(), diff_stats: DiffStats { files: 1, additions: 1, deletions: 0, languages: StdHashMap::new() } },
            plan: ReviewPlan { tier: Tier::Quick, score: 1.0, signals: vec![], specialists: vec![], budgets: PlanBudgets { max_agents: 1, total_token_cap: 1, wall_clock_sec: 1 }, overrides: vec![] },
            findings: vec![],
            suppressed: vec![],
            costs: RunCosts {
                total: CostEntry { input_tokens: 100, output_tokens: 50, usd: total_usd, wall_ms: 1000 },
                per_stage: per_stage.into_iter().map(|(name, usd)| (name.to_string(), CostEntry { input_tokens: 10, output_tokens: 5, usd: Some(usd), wall_ms: 100 })).collect(),
            },
            summary: ReviewSummary { by_severity: StdHashMap::new(), by_category: StdHashMap::new(), gate: None },
            spec_verdicts: vec![],
        };
        std::fs::write(run_dir.join("report.json"), serde_json::to_string(&report).unwrap()).unwrap();
    }

    #[test]
    fn loads_and_sorts_records_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        write_report(dir.path(), "run-2", "2026-07-13T00:00:00Z", Some(0.50), vec![]);
        write_report(dir.path(), "run-1", "2026-07-12T00:00:00Z", Some(0.25), vec![]);
        let records = load_run_cost_records(dir.path());
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].run_id, "run-1");
        assert_eq!(records[1].run_id, "run-2");
    }

    #[test]
    fn missing_runs_dir_returns_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_run_cost_records(dir.path()).is_empty());
    }

    #[test]
    fn a_corrupt_report_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        write_report(dir.path(), "run-good", "2026-07-12T00:00:00Z", Some(0.25), vec![]);
        std::fs::create_dir_all(dir.path().join("runs").join("run-bad")).unwrap();
        std::fs::write(dir.path().join("runs").join("run-bad").join("report.json"), "not json").unwrap();
        let records = load_run_cost_records(dir.path());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run_id, "run-good");
    }

    #[test]
    fn filter_since_keeps_only_records_on_or_after_the_given_day() {
        let records = vec![
            RunCostRecord { run_id: "a".into(), created_at: "2026-07-10T00:00:00Z".into(), base_ref: "m".into(), head_ref: "h".into(), total: CostEntry { input_tokens: 0, output_tokens: 0, usd: None, wall_ms: 0 }, per_stage: HashMap::new() },
            RunCostRecord { run_id: "b".into(), created_at: "2026-07-15T00:00:00Z".into(), base_ref: "m".into(), head_ref: "h".into(), total: CostEntry { input_tokens: 0, output_tokens: 0, usd: None, wall_ms: 0 }, per_stage: HashMap::new() },
        ];
        let filtered = filter_since(&records, "2026-07-12");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].run_id, "b");
    }

    #[test]
    fn summarize_sums_totals_and_reports_whether_any_run_had_usd() {
        let dir = tempfile::tempdir().unwrap();
        write_report(dir.path(), "run-1", "2026-07-12T00:00:00Z", Some(1.00), vec![("triage", 0.10), ("verify", 0.20)]);
        write_report(dir.path(), "run-2", "2026-07-12T00:00:00Z", Some(2.00), vec![("triage", 0.30)]);
        let records = load_run_cost_records(dir.path());
        let refs: Vec<&RunCostRecord> = records.iter().collect();
        let dashboard = summarize(&refs);
        assert_eq!(dashboard.run_count, 2);
        assert!(dashboard.any_usd_reported);
        assert!((dashboard.total_usd - 3.00).abs() < 1e-9);
        assert_eq!(dashboard.total_input_tokens, 200);
        assert_eq!(dashboard.by_day_usd, vec![("2026-07-12".to_string(), 3.00)]);
        assert_eq!(dashboard.by_stage_usd[0], ("triage".to_string(), 0.40));
        assert_eq!(dashboard.by_stage_usd[1], ("verify".to_string(), 0.20));
    }

    #[test]
    fn summarize_with_no_usd_anywhere_reports_that_clearly() {
        let dir = tempfile::tempdir().unwrap();
        write_report(dir.path(), "run-1", "2026-07-12T00:00:00Z", None, vec![]);
        let records = load_run_cost_records(dir.path());
        let refs: Vec<&RunCostRecord> = records.iter().collect();
        let dashboard = summarize(&refs);
        assert!(!dashboard.any_usd_reported);
        assert_eq!(dashboard.total_usd, 0.0);
        assert_eq!(dashboard.total_input_tokens, 100, "token totals must still aggregate even with no usd reported");
    }

    #[test]
    fn summarize_merges_same_day_totals_correctly_even_when_records_are_out_of_order() {
        // `summarize` takes `&[&RunCostRecord]` directly — callers aren't
        // required to route through `load_run_cost_records`'s sort, so this
        // constructs out-of-order, non-adjacent same-day records by hand to
        // prove day totals bucket by day rather than by adjacency.
        let a = RunCostRecord { run_id: "a".into(), created_at: "2026-07-12T09:00:00Z".into(), base_ref: "m".into(), head_ref: "h".into(), total: CostEntry { input_tokens: 0, output_tokens: 0, usd: Some(1.0), wall_ms: 0 }, per_stage: HashMap::new() };
        let b = RunCostRecord { run_id: "b".into(), created_at: "2026-07-13T09:00:00Z".into(), base_ref: "m".into(), head_ref: "h".into(), total: CostEntry { input_tokens: 0, output_tokens: 0, usd: Some(5.0), wall_ms: 0 }, per_stage: HashMap::new() };
        let c = RunCostRecord { run_id: "c".into(), created_at: "2026-07-12T15:00:00Z".into(), base_ref: "m".into(), head_ref: "h".into(), total: CostEntry { input_tokens: 0, output_tokens: 0, usd: Some(2.0), wall_ms: 0 }, per_stage: HashMap::new() };

        // Deliberately out of chronological order: a (07-12), b (07-13), c (07-12) again.
        let dashboard = summarize(&[&a, &b, &c]);

        assert_eq!(
            dashboard.by_day_usd,
            vec![("2026-07-12".to_string(), 3.0), ("2026-07-13".to_string(), 5.0)],
            "the two 07-12 records must merge into one $3.00 entry even though a 07-13 record sits between them in the input"
        );
    }
}

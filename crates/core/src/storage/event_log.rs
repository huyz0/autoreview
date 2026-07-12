use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use autoreview_schema::ReviewReport;

/// One compact, immutable record per finding — deliberately not the full
/// report (too heavy to sync across a team), matching the plan's Storage
/// design: `{findingFingerprint, category, ruleId/aspect, severity,
/// feedback?, runId, host, timestamp}`. `feedback` is always `None` at
/// write time in M1; the `feedback` command (M2) is what ever sets it,
/// appending a new event rather than mutating this one — events are
/// immutable and append-only by construction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventRecord {
    pub finding_fingerprint: String,
    pub category: String,
    pub rule_id_or_aspect: String,
    pub severity: String,
    pub feedback: Option<String>,
    pub run_id: String,
    pub host: String,
    pub timestamp: String,
}

/// Pure mapping from a report's findings to event records — no I/O, so it's
/// trivially testable independent of where/how the log file gets written.
pub fn events_from_report(report: &ReviewReport, host: &str) -> Vec<EventRecord> {
    report
        .findings
        .iter()
        .map(|f| EventRecord {
            finding_fingerprint: f.fingerprints.primary.clone(),
            category: f.category.clone(),
            rule_id_or_aspect: f.source.rule_id.clone().or_else(|| f.source.aspect.clone()).unwrap_or_else(|| f.source.tool.clone()),
            severity: format!("{:?}", f.severity).to_lowercase(),
            feedback: None,
            run_id: report.run_id.clone(),
            host: host.to_string(),
            timestamp: report.created_at.clone(),
        })
        .collect()
}

/// Builds the event record for a `feedback` call — always a *new* append-only
/// event, never a rewrite of the original finding's event (see the struct
/// docs above), so a finding's full feedback history stays reconstructable by
/// replaying the log in order.
pub fn feedback_event(lookup: &crate::storage::FindingLookup, verdict: &str, run_id: &str, host: &str, timestamp: &str) -> EventRecord {
    EventRecord {
        finding_fingerprint: lookup.fingerprint.clone(),
        category: lookup.category.clone(),
        rule_id_or_aspect: lookup.rule_id_or_tool.clone(),
        severity: lookup.severity.clone(),
        feedback: Some(verdict.to_string()),
        run_id: run_id.to_string(),
        host: host.to_string(),
        timestamp: timestamp.to_string(),
    }
}

/// Appends events as JSONL to `<history_dir>/events/<date>-<host>.jsonl`,
/// one host-day file per host so concurrent writers (different machines, or
/// even the same machine on different days) never contend on the same file
/// — this is what makes team sync (M3) a fetch-and-concatenate rather than a
/// real merge. `date` is caller-supplied (e.g. "2026-07-12") rather than
/// computed here, so this function has no wall-clock dependency and stays
/// deterministic/testable.
pub fn append_event_log(history_dir: &Path, date: &str, host: &str, events: &[EventRecord]) -> anyhow::Result<PathBuf> {
    let events_dir = history_dir.join("events");
    std::fs::create_dir_all(&events_dir)?;
    let log_path = events_dir.join(format!("{date}-{host}.jsonl"));

    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&log_path)?;
    for event in events {
        writeln!(file, "{}", serde_json::to_string(event)?)?;
    }
    Ok(log_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_schema::{
        CostEntry, DiffStats, Finding, FindingFingerprints, FindingSource, FindingSourceKind, Location, LocationRange, PlanBudgets, ReviewPlan,
        ReviewSummary, ReviewTarget, RunCosts, Severity, Side, Tier,
    };
    use std::collections::HashMap;

    fn make_report(findings: Vec<Finding>) -> ReviewReport {
        ReviewReport {
            schema_version: "1".into(),
            run_id: "run-1".into(),
            created_at: "2026-07-12T00:00:00Z".into(),
            target: ReviewTarget { repo_root: "/repo".into(), base_ref: "main~1".into(), head_ref: "main".into(), diff_stats: DiffStats { files: 1, additions: 1, deletions: 0, languages: HashMap::new() } },
            plan: ReviewPlan { tier: Tier::Quick, score: 1.0, signals: vec![], specialists: vec![], budgets: PlanBudgets { max_agents: 1, total_token_cap: 1, wall_clock_sec: 1 }, overrides: vec![] },
            findings,
            suppressed: vec![],
            costs: RunCosts { total: CostEntry { input_tokens: 0, output_tokens: 0, usd: None, wall_ms: 0 }, per_stage: HashMap::new() },
            summary: ReviewSummary { by_severity: HashMap::new(), by_category: HashMap::new(), gate: None },
        }
    }

    fn make_finding(fingerprint: &str, rule_id: Option<&str>, aspect: Option<&str>) -> Finding {
        Finding {
            id: format!("f-{fingerprint}"),
            fingerprints: FindingFingerprints { primary: fingerprint.to_string(), secondary: None },
            source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "ast-grep".into(), rule_id: rule_id.map(String::from), aspect: aspect.map(String::from), backend: None },
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

    #[test]
    fn maps_findings_to_events_with_no_feedback_at_write_time() {
        let report = make_report(vec![make_finding("abc", Some("go-no-self-comparison"), None)]);
        let events = events_from_report(&report, "test-host");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].finding_fingerprint, "abc");
        assert_eq!(events[0].rule_id_or_aspect, "go-no-self-comparison");
        assert_eq!(events[0].severity, "medium");
        assert_eq!(events[0].run_id, "run-1");
        assert_eq!(events[0].host, "test-host");
        assert!(events[0].feedback.is_none());
    }

    #[test]
    fn falls_back_to_aspect_then_tool_when_no_rule_id() {
        let report = make_report(vec![make_finding("agent-finding", None, Some("security"))]);
        let events = events_from_report(&report, "h");
        assert_eq!(events[0].rule_id_or_aspect, "security");
    }

    #[test]
    fn append_event_log_writes_one_jsonl_line_per_event_and_appends_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let report1 = make_report(vec![make_finding("a", Some("rule-a"), None)]);
        let report2 = make_report(vec![make_finding("b", Some("rule-b"), None)]);

        let path1 = append_event_log(dir.path(), "2026-07-12", "host1", &events_from_report(&report1, "host1")).unwrap();
        let path2 = append_event_log(dir.path(), "2026-07-12", "host1", &events_from_report(&report2, "host1")).unwrap();
        assert_eq!(path1, path2, "same date+host must append to the same file");

        let contents = std::fs::read_to_string(&path1).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: EventRecord = serde_json::from_str(lines[0]).unwrap();
        let second: EventRecord = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(first.finding_fingerprint, "a");
        assert_eq!(second.finding_fingerprint, "b");
    }

    #[test]
    fn different_hosts_on_the_same_date_get_separate_files() {
        let dir = tempfile::tempdir().unwrap();
        let report = make_report(vec![make_finding("a", Some("rule-a"), None)]);
        let path_host1 = append_event_log(dir.path(), "2026-07-12", "host1", &events_from_report(&report, "host1")).unwrap();
        let path_host2 = append_event_log(dir.path(), "2026-07-12", "host2", &events_from_report(&report, "host2")).unwrap();
        assert_ne!(path_host1, path_host2, "different hosts must never write to the same file — that's what makes team sync conflict-free");
    }

    #[test]
    fn feedback_event_carries_the_looked_up_fields_and_the_verdict() {
        use crate::storage::FindingLookup;
        let lookup = FindingLookup { fingerprint: "abc".into(), category: "correctness".into(), severity: "medium".into(), rule_id_or_tool: "go-no-self-comparison".into() };
        let event = feedback_event(&lookup, "fp", "feedback", "host1", "2026-07-12T00:00:00Z");
        assert_eq!(event.finding_fingerprint, "abc");
        assert_eq!(event.category, "correctness");
        assert_eq!(event.rule_id_or_aspect, "go-no-self-comparison");
        assert_eq!(event.severity, "medium");
        assert_eq!(event.feedback.as_deref(), Some("fp"));
        assert_eq!(event.run_id, "feedback");
    }
}

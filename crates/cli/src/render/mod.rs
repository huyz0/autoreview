pub mod markdown;

pub use markdown::render_markdown;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use autoreview_schema::{
        DiffStats, Finding, FindingFingerprints, FindingSource, FindingSourceKind, Location, LocationRange, PlanBudgets, ReviewPlan, ReviewReport,
        ReviewSummary, ReviewTarget, RunCosts, Severity, Side, SuppressedFinding, SuppressedReason, Tier, TriageSignalScore,
    };

    fn make_finding(severity: Severity, category: &str, title: &str, path: &str, line: u32) -> Finding {
        Finding {
            id: format!("f-{title}"),
            fingerprints: FindingFingerprints { primary: title.to_string(), secondary: None },
            source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "ast-grep".into(), rule_id: Some("some-rule".into()), aspect: None, backend: None },
            category: category.to_string(),
            severity,
            confidence: 1.0,
            title: title.to_string(),
            message: format!("{title} is a problem"),
            location: Location { path: path.to_string(), range: LocationRange { start_line: line, start_col: None, end_line: None, end_col: None }, snippet: "x == x".into(), side: Side::New },
            related_locations: None,
            suggestion: None,
            tags: None,
            meta: None,
        }
    }

    fn make_report(findings: Vec<Finding>, suppressed: Vec<SuppressedFinding>) -> ReviewReport {
        ReviewReport {
            schema_version: "1".into(),
            run_id: "run-123".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            target: ReviewTarget {
                repo_root: "/repo".into(),
                base_ref: "main~1".into(),
                head_ref: "main".into(),
                diff_stats: DiffStats { files: 1, additions: 5, deletions: 1, languages: HashMap::new() },
            },
            plan: ReviewPlan {
                tier: Tier::Standard,
                score: 22.7,
                signals: vec![TriageSignalScore { signal: "linesChanged".into(), points: 1.2, detail: None }],
                specialists: vec![],
                budgets: PlanBudgets { max_agents: 3, total_token_cap: 400_000, wall_clock_sec: 480 },
                overrides: vec![],
            },
            findings,
            suppressed,
            costs: RunCosts { total: autoreview_schema::CostEntry { input_tokens: 0, output_tokens: 0, usd: None, wall_ms: 0 }, per_stage: HashMap::new() },
            summary: ReviewSummary { by_severity: HashMap::new(), by_category: HashMap::new(), gate: None },
        }
    }

    #[test]
    fn renders_no_findings_as_a_clean_report() {
        let report = make_report(vec![], vec![]);
        let md = render_markdown(&report);
        assert!(md.contains("No findings"));
        assert!(!md.contains("## Findings"));
    }

    #[test]
    fn groups_findings_by_severity_in_order_then_by_category() {
        let report = make_report(
            vec![
                make_finding(Severity::Low, "style", "low-issue", "b.go", 5),
                make_finding(Severity::High, "security", "high-issue", "a.go", 1),
                make_finding(Severity::Medium, "correctness", "medium-issue", "c.go", 3),
            ],
            vec![],
        );
        let md = render_markdown(&report);

        let high_pos = md.find("### High").unwrap();
        let medium_pos = md.find("### Medium").unwrap();
        let low_pos = md.find("### Low").unwrap();
        assert!(high_pos < medium_pos && medium_pos < low_pos, "severities must render in blocker>...>info order:\n{md}");

        assert!(md.contains("high-issue"));
        assert!(md.contains("a.go:1"));
    }

    #[test]
    fn includes_summary_tables_by_severity_and_category() {
        let report = make_report(vec![make_finding(Severity::High, "security", "x", "a.go", 1), make_finding(Severity::High, "security", "y", "b.go", 2)], vec![]);
        let md = render_markdown(&report);
        assert!(md.contains("| High | 2 |"));
        assert!(md.contains("| security | 2 |"));
    }

    #[test]
    fn lists_suppressed_findings_separately_from_the_main_findings_section() {
        let suppressed_finding = make_finding(Severity::Low, "style", "dup", "a.go", 1);
        let report = make_report(vec![make_finding(Severity::High, "security", "kept", "a.go", 1)], vec![SuppressedFinding { finding: suppressed_finding, reason: SuppressedReason::Duplicate }]);
        let md = render_markdown(&report);
        assert!(md.contains("## Suppressed (1)"));
        assert!(md.contains("dup"));
    }

    #[test]
    fn shows_the_tier_score_and_any_overrides() {
        let mut report = make_report(vec![], vec![]);
        report.plan.overrides = vec!["--tier=quick".to_string()];
        let md = render_markdown(&report);
        assert!(md.contains("**Tier:** standard (score 22.7) — overrides: --tier=quick"));
    }
}

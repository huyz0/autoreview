use std::collections::BTreeMap;

use autoreview_schema::{Finding, ReviewReport, Severity};

fn severity_label(s: Severity) -> &'static str {
    match s {
        Severity::Blocker => "BLOCKER",
        Severity::High => "HIGH",
        Severity::Medium => "MEDIUM",
        Severity::Low => "LOW",
        Severity::Info => "INFO",
    }
}

fn rule_or_tool(f: &Finding) -> String {
    let rule = f.source.rule_id.as_deref().unwrap_or(f.source.tool.as_str());
    format!("{}:{}", f.source.tool, rule)
}

fn index_line(f: &Finding) -> String {
    format!("- `{}` [{}] {}:{} — {} ({})", f.id, severity_label(f.severity), f.location.path, f.location.range.start_line, f.title, rule_or_tool(f))
}

/// Renders a `ReviewReport` as a compact, grep-friendly Markdown index —
/// the machine/agent-navigable counterpart to `render_markdown`'s
/// human-semantic grouping, per the plan's Deliverables section
/// ("index.md, LLM-navigable"). Every line is self-contained (id, severity,
/// exact location, rule) so an agent can jump straight to a finding without
/// re-parsing report.json, and the same finding appears twice — once under
/// its category, once under its file path — because "what's wrong with this
/// category" and "what's wrong with this file" are the two navigation
/// entry points an agent actually needs.
pub fn render_index(report: &ReviewReport) -> String {
    let mut out = String::new();
    out.push_str("# Review Index\n\n");
    out.push_str(&format!("run: `{}` | tier: {} | findings: {} | suppressed: {}\n\n", report.run_id, report.plan.tier, report.findings.len(), report.suppressed.len()));

    if report.findings.is_empty() {
        out.push_str("No findings.\n");
        return out;
    }

    out.push_str("## By category\n\n");
    let mut by_category: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
    for f in &report.findings {
        by_category.entry(f.category.as_str()).or_default().push(f);
    }
    for (category, mut findings) in by_category {
        findings.sort_by(|a, b| a.location.path.cmp(&b.location.path).then(a.location.range.start_line.cmp(&b.location.range.start_line)));
        out.push_str(&format!("### {category} ({})\n\n", findings.len()));
        for f in findings {
            out.push_str(&index_line(f));
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str("## By path\n\n");
    let mut by_path: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
    for f in &report.findings {
        by_path.entry(f.location.path.as_str()).or_default().push(f);
    }
    for (path, mut findings) in by_path {
        findings.sort_by_key(|f| f.location.range.start_line);
        out.push_str(&format!("### {path} ({})\n\n", findings.len()));
        for f in findings {
            out.push_str(&index_line(f));
            out.push('\n');
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use autoreview_schema::{
        CostEntry, DiffStats, FindingFingerprints, FindingSource, FindingSourceKind, Location, LocationRange, PlanBudgets, ReviewPlan, ReviewSummary,
        ReviewTarget, RunCosts, Side, SuppressedFinding, SuppressedReason, Tier,
    };

    fn make_finding(id: &str, category: &str, path: &str, line: u32, severity: Severity) -> Finding {
        Finding {
            id: id.to_string(),
            fingerprints: FindingFingerprints { primary: id.to_string(), secondary: None },
            source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "ast-grep".into(), rule_id: Some("some-rule".into()), aspect: None, backend: None },
            category: category.to_string(),
            severity,
            confidence: 1.0,
            title: format!("issue in {path}"),
            message: "m".into(),
            location: Location { path: path.to_string(), range: LocationRange { start_line: line, ..Default::default() }, snippet: "x".into(), side: Side::New },
            related_locations: None,
            suggestion: None,
            tags: None,
            meta: None,
        }
    }

    fn make_report(findings: Vec<Finding>, suppressed: Vec<SuppressedFinding>) -> ReviewReport {
        ReviewReport {
            schema_version: "1".into(),
            run_id: "run-1".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            target: ReviewTarget { repo_root: "/repo".into(), base_ref: "main~1".into(), head_ref: "main".into(), diff_stats: DiffStats { files: 1, additions: 1, deletions: 0, languages: HashMap::new() } },
            plan: ReviewPlan { tier: Tier::Standard, score: 1.0, signals: vec![], specialists: vec![], budgets: PlanBudgets { max_agents: 1, total_token_cap: 1, wall_clock_sec: 1 }, overrides: vec![] },
            findings,
            suppressed,
            costs: RunCosts { total: CostEntry { input_tokens: 0, output_tokens: 0, usd: None, wall_ms: 0 }, per_stage: HashMap::new() },
            summary: ReviewSummary { by_severity: HashMap::new(), by_category: HashMap::new(), gate: None },
        }
    }

    #[test]
    fn renders_no_findings_as_a_clean_index() {
        let index = render_index(&make_report(vec![], vec![]));
        assert!(index.contains("No findings."));
        assert!(!index.contains("## By category"));
    }

    #[test]
    fn groups_the_same_finding_under_both_category_and_path() {
        let f = make_finding("f-1", "security", "a.ts", 10, Severity::High);
        let index = render_index(&make_report(vec![f], vec![]));
        assert!(index.contains("## By category"));
        assert!(index.contains("### security (1)"));
        assert!(index.contains("## By path"));
        assert!(index.contains("### a.ts (1)"));
        assert_eq!(index.matches("f-1").count(), 2, "the finding should appear once per navigation axis");
    }

    #[test]
    fn every_line_is_self_contained_with_id_severity_location_and_rule() {
        let f = make_finding("f-1", "security", "a.ts", 10, Severity::Blocker);
        let index = render_index(&make_report(vec![f], vec![]));
        assert!(index.contains("`f-1` [BLOCKER] a.ts:10"));
        assert!(index.contains("ast-grep:some-rule"));
    }

    #[test]
    fn categories_and_paths_render_in_deterministic_alphabetical_order() {
        let findings = vec![make_finding("f-1", "style", "z.ts", 1, Severity::Low), make_finding("f-2", "correctness", "a.ts", 1, Severity::Low)];
        let index = render_index(&make_report(findings, vec![]));
        let category_pos = index.find("### correctness").unwrap();
        let style_pos = index.find("### style").unwrap();
        assert!(category_pos < style_pos);
        let a_pos = index.find("### a.ts").unwrap();
        let z_pos = index.find("### z.ts").unwrap();
        assert!(a_pos < z_pos);
    }

    #[test]
    fn header_reports_run_tier_and_counts() {
        let f = make_finding("f-1", "security", "a.ts", 1, Severity::High);
        let suppressed = SuppressedFinding { finding: make_finding("f-2", "security", "a.ts", 2, Severity::Low), reason: SuppressedReason::Duplicate };
        let index = render_index(&make_report(vec![f], vec![suppressed]));
        assert!(index.contains("run: `run-1`"));
        assert!(index.contains("tier: standard"));
        assert!(index.contains("findings: 1"));
        assert!(index.contains("suppressed: 1"));
    }
}

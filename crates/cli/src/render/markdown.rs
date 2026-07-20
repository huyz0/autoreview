use std::collections::BTreeMap;

use autoreview_schema::{CriterionResult, CriterionVerdict, Finding, ReviewReport, Severity};

fn severity_rank(s: Severity) -> u8 {
    match s {
        Severity::Blocker => 0,
        Severity::High => 1,
        Severity::Medium => 2,
        Severity::Low => 3,
        Severity::Info => 4,
    }
}

fn severity_label(s: Severity) -> &'static str {
    match s {
        Severity::Blocker => "Blocker",
        Severity::High => "High",
        Severity::Medium => "Medium",
        Severity::Low => "Low",
        Severity::Info => "Info",
    }
}

fn render_finding(f: &Finding) -> String {
    let mut out = String::new();
    let rule = f.source.rule_id.as_deref().unwrap_or(f.source.tool.as_str());
    out.push_str(&format!("#### {} — `{}:{}`\n\n", f.title, f.location.path, f.location.range.start_line));
    out.push_str(&format!("- **Source:** {} (`{}`)\n", f.source.tool, rule));
    out.push_str(&format!("- **Category:** {}\n", f.category));
    out.push_str(&format!("- **Confidence:** {:.0}%\n", f.confidence * 100.0));
    out.push('\n');
    out.push_str(&format!("{}\n\n", f.message));
    if !f.location.snippet.is_empty() {
        out.push_str(&format!("```\n{}\n```\n\n", f.location.snippet));
    }
    if let Some(suggestion) = &f.suggestion {
        out.push_str(&format!("**Suggestion:** {}\n\n", suggestion.description));
        if let Some(patch) = &suggestion.patch {
            out.push_str(&format!("```diff\n{patch}\n```\n\n"));
        }
    }
    out
}

fn verdict_marker(v: CriterionVerdict) -> &'static str {
    match v {
        CriterionVerdict::Satisfied => "✅",
        CriterionVerdict::NotSatisfied => "❌",
        CriterionVerdict::Uncertain => "❓",
    }
}

fn render_spec_verdicts(verdicts: &[CriterionResult]) -> String {
    let mut out = String::new();
    out.push_str("## Acceptance Criteria\n\n");
    for r in verdicts {
        out.push_str(&format!("- {} **{}** — {}\n", verdict_marker(r.verdict), r.criterion, r.evidence));
    }
    out.push('\n');
    out
}

/// Renders a `ReviewReport` as a human-readable Markdown document, grouped
/// by severity then category — the semantic-grouping half of the plan's
/// "one report, rendered for humans and for machines" design (report.json
/// is the machine side; this is the human side of the same data).
pub fn render_markdown(report: &ReviewReport) -> String {
    let mut out = String::new();

    out.push_str("# Code Review Report\n\n");
    out.push_str(&format!("- **Repo:** `{}`\n", report.target.repo_root));
    out.push_str(&format!("- **Diff:** `{}...{}`\n", report.target.base_ref, report.target.head_ref));
    out.push_str(&format!("- **Run:** `{}` at {}\n", report.run_id, report.created_at));
    out.push_str(&format!(
        "- **Tier:** {} (score {:.1}){}\n",
        report.plan.tier,
        report.plan.score,
        if report.plan.overrides.is_empty() { String::new() } else { format!(" — overrides: {}", report.plan.overrides.join(" ")) }
    ));
    out.push_str(&format!(
        "- **Files changed:** {} (+{}/-{})\n",
        report.target.diff_stats.files, report.target.diff_stats.additions, report.target.diff_stats.deletions
    ));
    if report.costs.total.input_tokens > 0 || report.costs.total.output_tokens > 0 {
        let usd = report.costs.total.usd.map(|u| format!(", ${u:.4}")).unwrap_or_default();
        out.push_str(&format!(
            "- **Cost:** {} input / {} output tokens, {}ms wall time{usd}\n",
            report.costs.total.input_tokens, report.costs.total.output_tokens, report.costs.total.wall_ms
        ));
    }
    out.push('\n');

    if !report.spec_verdicts.is_empty() {
        out.push_str(&render_spec_verdicts(&report.spec_verdicts));
    }

    if report.findings.is_empty() {
        out.push_str("No findings. ✅\n");
        return out;
    }

    out.push_str("## Summary\n\n");
    out.push_str("| Severity | Count |\n|---|---|\n");
    let mut by_severity: BTreeMap<u8, (Severity, usize)> = BTreeMap::new();
    for f in &report.findings {
        let entry = by_severity.entry(severity_rank(f.severity)).or_insert((f.severity, 0));
        entry.1 += 1;
    }
    for (severity, count) in by_severity.values() {
        out.push_str(&format!("| {} | {} |\n", severity_label(*severity), count));
    }
    out.push('\n');

    out.push_str("| Category | Count |\n|---|---|\n");
    let mut by_category: BTreeMap<String, usize> = BTreeMap::new();
    for f in &report.findings {
        *by_category.entry(f.category.clone()).or_insert(0) += 1;
    }
    for (category, count) in &by_category {
        out.push_str(&format!("| {category} | {count} |\n"));
    }
    out.push('\n');

    out.push_str("## Findings\n\n");
    let mut grouped: BTreeMap<u8, Vec<&Finding>> = BTreeMap::new();
    for f in &report.findings {
        grouped.entry(severity_rank(f.severity)).or_default().push(f);
    }
    for (_, mut findings) in grouped {
        findings.sort_by(|a, b| a.location.path.cmp(&b.location.path).then(a.location.range.start_line.cmp(&b.location.range.start_line)));
        out.push_str(&format!("### {} ({})\n\n", severity_label(findings[0].severity), findings.len()));
        for f in findings {
            out.push_str(&render_finding(f));
        }
    }

    if !report.suppressed.is_empty() {
        out.push_str(&format!("## Suppressed ({})\n\n", report.suppressed.len()));
        out.push_str("Findings suppressed as duplicates or below-confidence — kept for audit, not shown above.\n\n");
        for s in &report.suppressed {
            out.push_str(&format!("- `{}` at {}:{} — {:?}\n", s.finding.title, s.finding.location.path, s.finding.location.range.start_line, s.reason));
        }
    }

    out
}

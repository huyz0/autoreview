//! SARIF 2.1.0 export — interchange format for GitHub code scanning and IDE
//! viewers, per the plan's Core data model section ("SARIF is export/ingest
//! interchange, not the native store"). `report.json` remains canonical;
//! this is a pure, lossy projection of it.

use autoreview_schema::{ReviewReport, Severity};

fn sarif_level(severity: Severity) -> &'static str {
    match severity {
        Severity::Blocker | Severity::High => "error",
        Severity::Medium => "warning",
        Severity::Low | Severity::Info => "note",
    }
}

fn rule_id_for(finding: &autoreview_schema::Finding) -> String {
    finding.source.rule_id.clone().unwrap_or_else(|| format!("{}/{}", finding.source.tool, finding.category))
}

pub fn render_sarif(report: &ReviewReport) -> String {
    let mut rule_ids: Vec<String> = report.findings.iter().map(rule_id_for).collect();
    rule_ids.sort();
    rule_ids.dedup();
    let rules: Vec<serde_json::Value> = rule_ids
        .iter()
        .map(|id| {
            serde_json::json!({
                "id": id,
                "shortDescription": { "text": id },
            })
        })
        .collect();

    let results: Vec<serde_json::Value> = report
        .findings
        .iter()
        .map(|finding| {
            serde_json::json!({
                "ruleId": rule_id_for(finding),
                "level": sarif_level(finding.severity),
                "message": { "text": finding.message },
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": { "uri": finding.location.path },
                        "region": { "startLine": finding.location.range.start_line.max(1) }
                    }
                }],
                "partialFingerprints": {
                    "primaryLocationLineHash": finding.fingerprints.primary
                },
                "properties": {
                    "category": finding.category,
                    "confidence": finding.confidence
                }
            })
        })
        .collect();

    let doc = serde_json::json!({
        "$schema": "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/Schemata/sarif-schema-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "autoreview",
                    "informationUri": "https://github.com/anthropics/autoreview",
                    "version": env!("CARGO_PKG_VERSION"),
                    "rules": rules
                }
            },
            "results": results
        }]
    });

    serde_json::to_string_pretty(&doc).expect("SARIF document is always valid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_schema::{Finding, FindingFingerprints, FindingSource, FindingSourceKind, Location, LocationRange, Side};

    fn make_finding(severity: Severity, rule_id: Option<&str>, path: &str, line: u32) -> Finding {
        Finding {
            id: "f-abc".into(),
            fingerprints: FindingFingerprints { primary: "abc123".into(), secondary: None },
            source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "ast-grep".into(), rule_id: rule_id.map(String::from), aspect: None, backend: None },
            category: "correctness".into(),
            severity,
            confidence: 1.0,
            title: "t".into(),
            message: "self comparison is always true".into(),
            location: Location { path: path.into(), range: LocationRange { start_line: line, ..Default::default() }, snippet: "x == x".into(), side: Side::New },
            related_locations: None,
            suggestion: None,
            tags: None,
            meta: None,
        }
    }

    fn make_report(findings: Vec<Finding>) -> ReviewReport {
        ReviewReport { created_at: "2026-01-01T00:00:00Z".into(), findings, ..autoreview_test_support::sample_review_report() }
    }

    #[test]
    fn produces_valid_json_with_the_sarif_2_1_0_shape() {
        let report = make_report(vec![make_finding(Severity::High, Some("go-no-self-comparison"), "main.go", 5)]);
        let sarif = render_sarif(&report);
        let doc: serde_json::Value = serde_json::from_str(&sarif).unwrap();
        assert_eq!(doc["version"], "2.1.0");
        assert_eq!(doc["runs"][0]["tool"]["driver"]["name"], "autoreview");
        assert_eq!(doc["runs"][0]["results"][0]["ruleId"], "go-no-self-comparison");
        assert_eq!(doc["runs"][0]["results"][0]["level"], "error");
        assert_eq!(doc["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"], "main.go");
        assert_eq!(doc["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"]["startLine"], 5);
        assert_eq!(doc["runs"][0]["results"][0]["partialFingerprints"]["primaryLocationLineHash"], "abc123");
    }

    #[test]
    fn maps_severity_to_sarif_level() {
        assert_eq!(sarif_level(Severity::Blocker), "error");
        assert_eq!(sarif_level(Severity::High), "error");
        assert_eq!(sarif_level(Severity::Medium), "warning");
        assert_eq!(sarif_level(Severity::Low), "note");
        assert_eq!(sarif_level(Severity::Info), "note");
    }

    #[test]
    fn falls_back_to_tool_and_category_when_no_rule_id() {
        let report = make_report(vec![make_finding(Severity::Medium, None, "a.ts", 1)]);
        let sarif = render_sarif(&report);
        let doc: serde_json::Value = serde_json::from_str(&sarif).unwrap();
        assert_eq!(doc["runs"][0]["results"][0]["ruleId"], "ast-grep/correctness");
    }

    #[test]
    fn deduplicates_rules_in_the_driver_rules_list() {
        let report = make_report(vec![
            make_finding(Severity::High, Some("go-no-self-comparison"), "a.go", 1),
            make_finding(Severity::High, Some("go-no-self-comparison"), "b.go", 2),
        ]);
        let sarif = render_sarif(&report);
        let doc: serde_json::Value = serde_json::from_str(&sarif).unwrap();
        assert_eq!(doc["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap().len(), 1);
        assert_eq!(doc["runs"][0]["results"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn renders_no_findings_as_an_empty_results_list() {
        let report = make_report(vec![]);
        let sarif = render_sarif(&report);
        let doc: serde_json::Value = serde_json::from_str(&sarif).unwrap();
        assert_eq!(doc["runs"][0]["results"].as_array().unwrap().len(), 0);
    }
}

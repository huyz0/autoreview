//! Generic SARIF 2.1.0 ingest — normalizes any SARIF-emitting analyzer's
//! output into `AgentFinding`s through one adapter, per the plan's Existing
//! tools vs building new section: "most emit SARIF, so one SARIF-ingest
//! adapter covers the bulk" (detekt, ktlint, checkstyle, clippy, eslint).
//! This module implements the parsing/normalization core; wiring a specific
//! tool's invocation (its CLI flags, where it writes its SARIF file) is a
//! separate, per-tool adapter that calls into this — same shape as
//! `ast_grep.rs`/`golangci_lint.rs` calling their own analyzer, then handing
//! off to shared normalization.

use std::path::Path;

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

fn map_level(level: Option<&str>) -> Severity {
    match level {
        Some("error") => Severity::High,
        Some("warning") => Severity::Medium,
        Some("note") => Severity::Low,
        Some("none") => Severity::Info,
        // SARIF's own spec: a result with no "level" defaults to "warning".
        _ => Severity::Medium,
    }
}

/// Parses a SARIF 2.1.0 document (already read into a string) into
/// `AgentFinding`s. Pure and I/O-free so it's trivially testable against
/// literal SARIF fixtures without needing a real installed analyzer.
pub fn parse_sarif_document(json: &str, tool_name_override: Option<&str>) -> anyhow::Result<Vec<AgentFinding>> {
    let doc: serde_json::Value = serde_json::from_str(json)?;
    let mut findings = Vec::new();

    for run in doc.get("runs").and_then(|v| v.as_array()).into_iter().flatten() {
        let driver_name = run.get("tool").and_then(|t| t.get("driver")).and_then(|d| d.get("name")).and_then(|n| n.as_str()).unwrap_or("sarif");
        let tool = tool_name_override.unwrap_or(driver_name).to_string();

        for result in run.get("results").and_then(|v| v.as_array()).into_iter().flatten() {
            let rule_id = result.get("ruleId").and_then(|v| v.as_str()).map(str::to_string);
            let level = result.get("level").and_then(|v| v.as_str());
            let message = result.get("message").and_then(|m| m.get("text")).and_then(|v| v.as_str()).unwrap_or("(no message)").to_string();
            let category = result.get("properties").and_then(|p| p.get("category")).and_then(|v| v.as_str()).unwrap_or("correctness").to_string();

            let location = result.get("locations").and_then(|v| v.as_array()).and_then(|locs| locs.first());
            let Some(physical) = location.and_then(|l| l.get("physicalLocation")) else {
                // A result with no physical location can't be placed in a
                // diff-anchored review — skip it rather than fabricating a
                // location, matching the other adapters' "skip what can't
                // be mapped cleanly" behavior.
                continue;
            };
            let Some(path) = physical.get("artifactLocation").and_then(|a| a.get("uri")).and_then(|v| v.as_str()) else { continue };
            let start_line = physical.get("region").and_then(|r| r.get("startLine")).and_then(|v| v.as_u64()).unwrap_or(1) as u32;

            let title = rule_id.clone().map(|id| id.replace(['-', '_'], " ")).unwrap_or_else(|| tool.clone());

            findings.push(AgentFinding {
                source: FindingSource { kind: FindingSourceKind::Analyzer, tool: tool.clone(), rule_id, aspect: None, backend: None },
                category,
                severity: map_level(level),
                confidence: 1.0,
                title,
                message,
                location: Location { path: path.to_string(), range: LocationRange { start_line, ..Default::default() }, snippet: String::new(), side: Side::New },
                related_locations: None,
                suggestion: None,
                tags: None,
                meta: None,
                suggested_patch: None,
            });
        }
    }

    Ok(findings)
}

/// Reads and parses a SARIF file from disk, filtering results to
/// `changed_files` (results on files outside the current diff aren't
/// actionable in a diff review) — the same "scope to what changed" behavior
/// as the ast-grep and golangci-lint adapters.
pub fn ingest_sarif_file(sarif_path: &Path, changed_files: &[String], tool_name_override: Option<&str>) -> anyhow::Result<Vec<AgentFinding>> {
    let text = std::fs::read_to_string(sarif_path)?;
    let all = parse_sarif_document(&text, tool_name_override)?;
    Ok(all.into_iter().filter(|f| changed_files.iter().any(|c| c == &f.location.path)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_sarif(level: &str, rule_id: &str, path: &str, line: u64) -> String {
        serde_json::json!({
            "version": "2.1.0",
            "runs": [{
                "tool": { "driver": { "name": "eslint" } },
                "results": [{
                    "ruleId": rule_id,
                    "level": level,
                    "message": { "text": "some problem" },
                    "locations": [{
                        "physicalLocation": {
                            "artifactLocation": { "uri": path },
                            "region": { "startLine": line }
                        }
                    }]
                }]
            }]
        })
        .to_string()
    }

    #[test]
    fn parses_a_well_formed_result_into_a_finding() {
        let findings = parse_sarif_document(&sample_sarif("error", "no-unused-vars", "src/a.ts", 12), None).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.tool, "eslint");
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("no-unused-vars"));
        assert_eq!(findings[0].location.path, "src/a.ts");
        assert_eq!(findings[0].location.range.start_line, 12);
        assert_eq!(findings[0].severity, Severity::High);
        assert_eq!(findings[0].confidence, 1.0);
    }

    #[test]
    fn maps_all_four_sarif_levels() {
        assert_eq!(map_level(Some("error")), Severity::High);
        assert_eq!(map_level(Some("warning")), Severity::Medium);
        assert_eq!(map_level(Some("note")), Severity::Low);
        assert_eq!(map_level(Some("none")), Severity::Info);
    }

    #[test]
    fn defaults_missing_level_to_warning_per_the_sarif_spec() {
        assert_eq!(map_level(None), Severity::Medium);
    }

    #[test]
    fn respects_a_tool_name_override() {
        let findings = parse_sarif_document(&sample_sarif("warning", "x", "a.ts", 1), Some("detekt")).unwrap();
        assert_eq!(findings[0].source.tool, "detekt");
    }

    #[test]
    fn skips_results_with_no_physical_location() {
        let doc = serde_json::json!({
            "version": "2.1.0",
            "runs": [{ "tool": { "driver": { "name": "x" } }, "results": [{ "ruleId": "r", "level": "error", "message": { "text": "m" }, "locations": [] }] }]
        });
        let findings = parse_sarif_document(&doc.to_string(), None).unwrap();
        assert_eq!(findings.len(), 0);
    }

    #[test]
    fn returns_empty_for_a_document_with_no_runs() {
        let findings = parse_sarif_document(r#"{"version": "2.1.0", "runs": []}"#, None).unwrap();
        assert_eq!(findings.len(), 0);
    }

    #[test]
    fn errors_clearly_on_malformed_json() {
        assert!(parse_sarif_document("not json", None).is_err());
    }

    #[test]
    fn ingest_sarif_file_filters_to_changed_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let doc = serde_json::json!({
            "version": "2.1.0",
            "runs": [{
                "tool": { "driver": { "name": "eslint" } },
                "results": [
                    { "ruleId": "a", "level": "error", "message": { "text": "m1" }, "locations": [{ "physicalLocation": { "artifactLocation": { "uri": "src/a.ts" }, "region": { "startLine": 1 } } }] },
                    { "ruleId": "b", "level": "error", "message": { "text": "m2" }, "locations": [{ "physicalLocation": { "artifactLocation": { "uri": "src/unrelated.ts" }, "region": { "startLine": 1 } } }] }
                ]
            }]
        });
        let path = dir.path().join("report.sarif");
        std::fs::write(&path, doc.to_string()).unwrap();

        let findings = ingest_sarif_file(&path, &["src/a.ts".to_string()], None).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].location.path, "src/a.ts");
    }
}

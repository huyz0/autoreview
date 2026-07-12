use std::collections::HashMap;

use autoreview_schema::{Finding, SuppressedFinding, SuppressedReason};

pub struct DedupeResult {
    pub findings: Vec<Finding>,
    pub suppressed: Vec<SuppressedFinding>,
}

/// Exact-fingerprint dedupe: when two findings share a primary fingerprint,
/// keep the higher-confidence one (analyzers are always 1.0, so this favors
/// analyzer/learned-rule findings over an agent re-reporting the same code).
/// Fuzzy (cross-tool, near-miss) dedupe is a separate M2 pass.
pub fn dedupe_exact(findings: Vec<Finding>) -> DedupeResult {
    let mut by_fingerprint: HashMap<String, Finding> = HashMap::new();
    let mut suppressed = Vec::new();

    for finding in findings {
        let key = finding.fingerprints.primary.clone();
        match by_fingerprint.remove(&key) {
            None => {
                by_fingerprint.insert(key, finding);
            }
            Some(existing) => {
                let (kept, dropped) = if existing.confidence >= finding.confidence { (existing, finding) } else { (finding, existing) };
                suppressed.push(SuppressedFinding { finding: dropped, reason: SuppressedReason::Duplicate });
                by_fingerprint.insert(key, kept);
            }
        }
    }

    DedupeResult { findings: by_fingerprint.into_values().collect(), suppressed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_schema::{FindingSource, FindingSourceKind, Location, LocationRange, Side};

    fn make_finding(fingerprint: &str, confidence: f64, kind: FindingSourceKind) -> Finding {
        Finding {
            id: format!("f-{fingerprint}"),
            fingerprints: autoreview_schema::FindingFingerprints { primary: fingerprint.into(), secondary: None },
            source: FindingSource { kind, tool: "claude-code".into(), rule_id: if kind == FindingSourceKind::Analyzer { Some("x".into()) } else { None }, aspect: Some("security".into()), backend: None },
            category: "security".into(),
            severity: autoreview_schema::Severity::High,
            confidence,
            title: "t".into(),
            message: "m".into(),
            location: Location { path: "a.ts".into(), range: LocationRange { start_line: 1, ..Default::default() }, snippet: "x".into(), side: Side::New },
            related_locations: None,
            suggestion: None,
            tags: None,
            meta: None,
        }
    }

    #[test]
    fn keeps_a_single_finding_untouched() {
        let result = dedupe_exact(vec![make_finding("abc", 0.7, FindingSourceKind::Agent)]);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.suppressed.len(), 0);
    }

    #[test]
    fn suppresses_lower_confidence_duplicate() {
        let agent_finding = make_finding("abc", 0.6, FindingSourceKind::Agent);
        let analyzer_finding = make_finding("abc", 1.0, FindingSourceKind::Analyzer);

        let result = dedupe_exact(vec![agent_finding, analyzer_finding]);

        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].source.kind, FindingSourceKind::Analyzer);
        assert_eq!(result.suppressed.len(), 1);
        assert_eq!(result.suppressed[0].reason, SuppressedReason::Duplicate);
        assert_eq!(result.suppressed[0].finding.source.kind, FindingSourceKind::Agent);
    }

    #[test]
    fn does_not_merge_different_fingerprints() {
        let a = make_finding("aaa", 0.7, FindingSourceKind::Agent);
        let b = make_finding("bbb", 0.7, FindingSourceKind::Agent);
        let result = dedupe_exact(vec![a, b]);
        assert_eq!(result.findings.len(), 2);
        assert_eq!(result.suppressed.len(), 0);
    }
}

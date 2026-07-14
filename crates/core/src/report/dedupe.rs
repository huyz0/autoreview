use std::collections::HashMap;

use autoreview_schema::{Finding, SuppressedFinding, SuppressedReason};

pub struct DedupeResult {
    pub findings: Vec<Finding>,
    pub suppressed: Vec<SuppressedFinding>,
}

/// Exact-fingerprint dedupe: when two findings share a primary fingerprint,
/// keep the higher-confidence one (analyzers are always 1.0, so this favors
/// analyzer/learned-rule findings over an agent re-reporting the same code).
/// Fuzzy (cross-tool, near-miss) dedupe is `dedupe_fuzzy`, a separate pass.
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

/// Trigram-shingle Jaccard similarity over normalized text — the same
/// lexical clustering method the plan specifies for the rule-factory's
/// candidate mining (chosen there for the same reason it fits here: it's
/// explainable to a human reviewer, unlike an embedding distance). Used to
/// catch the case exact-fingerprint dedupe structurally can't: an analyzer
/// and an agent (or two agents) independently flagging the same underlying
/// issue with different rule keys/wording.
pub(crate) fn normalized_shingles(text: &str) -> std::collections::HashSet<String> {
    let normalized: String = text.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' }).collect();
    // Shingle over the whitespace-collapsed character stream (not words) so
    // short titles still produce enough shingles to compare meaningfully.
    let collapsed: String = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    let collapsed_chars: Vec<char> = collapsed.chars().collect();
    if collapsed_chars.len() < 3 {
        return std::iter::once(collapsed).collect();
    }
    collapsed_chars.windows(3).map(|w| w.iter().collect()).collect()
}

pub(crate) fn title_similarity(a: &str, b: &str) -> f64 {
    let sa = normalized_shingles(a);
    let sb = normalized_shingles(b);
    if sa.is_empty() || sb.is_empty() {
        return 0.0;
    }
    let intersection = sa.intersection(&sb).count();
    let union = sa.union(&sb).count();
    intersection as f64 / union as f64
}

/// Fuzzy dedupe: findings on the same file within `line_window` lines of
/// each other, whose titles are lexically similar above `title_threshold`
/// (the plan's rule-mining cluster threshold, 0.55, is reused as the default
/// — same method, same "these are probably the same underlying issue" bar),
/// are treated as duplicates. Greedy and order-independent: findings are
/// sorted by (path, line) first so the pass is deterministic across runs
/// regardless of the order specialists/analyzers returned results in.
pub fn dedupe_fuzzy(findings: Vec<Finding>, line_window: u32, title_threshold: f64) -> DedupeResult {
    let mut sorted = findings;
    sorted.sort_by(|a, b| (a.location.path.as_str(), a.location.range.start_line).cmp(&(b.location.path.as_str(), b.location.range.start_line)));

    let mut kept: Vec<Finding> = Vec::with_capacity(sorted.len());
    let mut suppressed = Vec::new();

    'outer: for finding in sorted {
        for existing in kept.iter_mut() {
            let same_file = existing.location.path == finding.location.path;
            let near_line = (existing.location.range.start_line as i64 - finding.location.range.start_line as i64).unsigned_abs() as u32 <= line_window;
            if same_file && near_line && title_similarity(&existing.title, &finding.title) >= title_threshold {
                if finding.confidence > existing.confidence {
                    let dropped = std::mem::replace(existing, finding);
                    suppressed.push(SuppressedFinding { finding: dropped, reason: SuppressedReason::Duplicate });
                } else {
                    suppressed.push(SuppressedFinding { finding, reason: SuppressedReason::Duplicate });
                }
                continue 'outer;
            }
        }
        kept.push(finding);
    }

    DedupeResult { findings: kept, suppressed }
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

    fn make_fuzzy_finding(fingerprint: &str, path: &str, line: u32, title: &str, confidence: f64) -> Finding {
        let mut f = make_finding(fingerprint, confidence, FindingSourceKind::Agent);
        f.location.path = path.to_string();
        f.location.range.start_line = line;
        f.title = title.to_string();
        f
    }

    #[test]
    fn title_similarity_is_1_for_identical_text() {
        assert_eq!(title_similarity("unvalidated redirect target", "unvalidated redirect target"), 1.0);
    }

    #[test]
    fn title_similarity_is_low_for_unrelated_text() {
        assert!(title_similarity("unvalidated redirect target", "missing null check on config") < 0.2);
    }

    #[test]
    fn fuzzy_dedupe_merges_near_duplicate_findings_on_the_same_line() {
        let a = make_fuzzy_finding("aaa", "src/redirect.ts", 42, "Unvalidated redirect target from user input", 0.7);
        let b = make_fuzzy_finding("bbb", "src/redirect.ts", 42, "Redirect target is unvalidated user input", 1.0);
        let result = dedupe_fuzzy(vec![a, b], 3, 0.55);
        assert_eq!(result.findings.len(), 1, "near-duplicate titles on the same line should merge");
        assert_eq!(result.suppressed.len(), 1);
        assert_eq!(result.findings[0].fingerprints.primary, "bbb", "higher-confidence finding should be kept");
    }

    #[test]
    fn fuzzy_dedupe_does_not_merge_across_files() {
        let a = make_fuzzy_finding("aaa", "src/a.ts", 10, "Unvalidated redirect target", 0.7);
        let b = make_fuzzy_finding("bbb", "src/b.ts", 10, "Unvalidated redirect target", 0.7);
        let result = dedupe_fuzzy(vec![a, b], 3, 0.55);
        assert_eq!(result.findings.len(), 2);
    }

    #[test]
    fn fuzzy_dedupe_does_not_merge_findings_outside_the_line_window() {
        let a = make_fuzzy_finding("aaa", "src/a.ts", 10, "Unvalidated redirect target", 0.7);
        let b = make_fuzzy_finding("bbb", "src/a.ts", 200, "Unvalidated redirect target", 0.7);
        let result = dedupe_fuzzy(vec![a, b], 3, 0.55);
        assert_eq!(result.findings.len(), 2);
    }

    #[test]
    fn fuzzy_dedupe_does_not_merge_dissimilar_titles_on_the_same_line() {
        let a = make_fuzzy_finding("aaa", "src/a.ts", 10, "Unvalidated redirect target", 0.7);
        let b = make_fuzzy_finding("bbb", "src/a.ts", 10, "Missing null check on config value", 0.7);
        let result = dedupe_fuzzy(vec![a, b], 3, 0.55);
        assert_eq!(result.findings.len(), 2);
    }
}

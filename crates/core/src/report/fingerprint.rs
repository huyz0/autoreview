use sha2::{Digest, Sha256};
use std::collections::HashMap;

use autoreview_schema::{AgentFinding, Finding, FindingFingerprints, FindingSourceKind, FingerprintedFinding};

/// Finds the byte offset of a trailing `//` or `#` line-comment start,
/// ignoring any `//`/`#` that appears inside a `"..."` or `'...'` string
/// literal — a bare `line.find("//")` would truncate
/// `res.redirect("http://a.com")` at the string's own `//`, dropping real
/// code content (and colliding two different URLs into one fingerprint).
/// Deliberately simple: no escape-sequence handling, just quote-state
/// tracking, since this only needs to avoid the common false-positive case.
fn trailing_comment_start(line: &str) -> Option<usize> {
    let mut in_string: Option<char> = None;
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    for (i, &(byte_idx, c)) in chars.iter().enumerate() {
        match in_string {
            Some(quote) => {
                if c == quote {
                    in_string = None;
                }
            }
            None => match c {
                '"' | '\'' => in_string = Some(c),
                '/' if chars.get(i + 1).is_some_and(|&(_, next)| next == '/') => return Some(byte_idx),
                '#' => return Some(byte_idx),
                _ => {}
            },
        }
    }
    None
}

/// Strip trailing line comments and collapse whitespace so line-number drift
/// and incidental reformatting don't change the fingerprint, but the code itself does.
pub fn normalize_snippet(snippet: &str) -> String {
    let stripped: Vec<String> = snippet
        .lines()
        .map(|line| match trailing_comment_start(line) {
            Some(idx) => line[..idx].trim().to_string(),
            None => line.trim().to_string(),
        })
        .filter(|line| !line.is_empty())
        .collect();

    let joined = stripped.join("\n");
    let collapsed: String = joined.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.trim().to_string()
}

/// The stable identity of a finding: analyzer findings key off (tool, ruleId);
/// agent findings have no ruleId, so they key off (aspect, category, normalizedTitle)
/// instead — fuzzy by design, exact-dedupe only needs to survive re-runs of the
/// same tool/agent on the same code, not cross-tool matching.
pub fn rule_key_for(source_kind: FindingSourceKind, tool: &str, rule_id: Option<&str>, aspect: Option<&str>, category: &str, title: &str) -> String {
    let _ = source_kind;
    if let Some(rule_id) = rule_id {
        return format!("{tool}:{rule_id}");
    }
    let normalized_title: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c.is_whitespace() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    format!("{}:{}:{}", aspect.unwrap_or(tool), category, normalized_title)
}

pub struct FingerprintInput<'a> {
    pub rule_key: &'a str,
    pub path: &'a str,
    pub normalized_snippet: &'a str,
    pub occurrence: u32,
}

pub fn compute_fingerprint(input: &FingerprintInput) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.rule_key.as_bytes());
    hasher.update([0u8]);
    hasher.update(input.path.as_bytes());
    hasher.update([0u8]);
    hasher.update(input.normalized_snippet.as_bytes());
    hasher.update([0u8]);
    hasher.update(input.occurrence.to_string().as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn finding_id_from_fingerprint(fingerprint: &str) -> String {
    format!("f-{}", &fingerprint[..16.min(fingerprint.len())])
}

/// Assigns stable ids + fingerprints to a batch of agent findings, handling the
/// "duplicated code in one file" case via an occurrence counter per (ruleKey, path,
/// snippet) tuple, ordered by line so re-runs land on the same occurrence index.
pub fn assign_fingerprints(findings: Vec<AgentFinding>) -> Vec<FingerprintedFinding> {
    let mut sorted = findings;
    sorted.sort_by_key(|f| f.location.range.start_line);

    let mut seen: HashMap<String, u32> = HashMap::new();
    let mut result = Vec::with_capacity(sorted.len());

    for finding in sorted {
        let rule_key = rule_key_for(
            finding.source.kind,
            &finding.source.tool,
            finding.source.rule_id.as_deref(),
            finding.source.aspect.as_deref(),
            &finding.category,
            &finding.title,
        );
        let normalized_snippet = normalize_snippet(&finding.location.snippet);
        let occurrence_key = format!("{rule_key}\0{}\0{normalized_snippet}", finding.location.path);
        let occurrence = *seen.get(&occurrence_key).unwrap_or(&0);
        seen.insert(occurrence_key, occurrence + 1);

        let primary = compute_fingerprint(&FingerprintInput {
            rule_key: &rule_key,
            path: &finding.location.path,
            normalized_snippet: &normalized_snippet,
            occurrence,
        });

        result.push(FingerprintedFinding {
            id: finding_id_from_fingerprint(&primary),
            fingerprints: FindingFingerprints { primary, secondary: None },
            finding,
        });
    }

    result
}

/// Lifts a fingerprinted agent finding into the final `Finding` schema used
/// in the report — the shapes are identical apart from id/fingerprints, so
/// this is a pure field-for-field move, not a transformation.
pub fn to_finding(ff: FingerprintedFinding) -> Finding {
    Finding {
        id: ff.id,
        fingerprints: ff.fingerprints,
        source: ff.finding.source,
        category: ff.finding.category,
        severity: ff.finding.severity,
        confidence: ff.finding.confidence,
        title: ff.finding.title,
        message: ff.finding.message,
        location: ff.finding.location,
        related_locations: ff.finding.related_locations,
        suggestion: ff.finding.suggestion,
        tags: ff.finding.tags,
        meta: ff.finding.meta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_schema::{FindingSource, Location, LocationRange, Side};

    fn make_finding(start_line: u32, snippet: &str, title: &str) -> AgentFinding {
        AgentFinding {
            source: FindingSource { kind: FindingSourceKind::Agent, tool: "claude-code".into(), rule_id: None, aspect: Some("security".into()), backend: None },
            category: "security".into(),
            severity: autoreview_schema::Severity::High,
            confidence: 0.8,
            title: title.into(),
            message: "The redirect target comes from user input without validation.".into(),
            location: Location { path: "src/redirect.ts".into(), range: LocationRange { start_line, end_line: Some(start_line + 2), ..Default::default() }, snippet: snippet.into(), side: Side::New },
            related_locations: None,
            suggestion: None,
            tags: None,
            meta: None,
            suggested_patch: None,
        }
    }

    const SNIPPET: &str = "  if (target) {\n    res.redirect(target);\n  }";

    #[test]
    fn normalize_snippet_strips_comments_and_reformatting() {
        let with_comment = normalize_snippet("  if (x) { // check\n    doThing();\n  }");
        let reformatted = normalize_snippet("if (x) {\n  doThing();\n}");
        assert_eq!(with_comment, reformatted);
    }

    #[test]
    fn normalize_snippet_does_not_collapse_different_code() {
        assert_ne!(normalize_snippet("if (x) { doThing(); }"), normalize_snippet("if (y) { doThing(); }"));
    }

    #[test]
    fn normalize_snippet_does_not_treat_a_url_slash_as_a_comment() {
        let a = normalize_snippet(r#"res.redirect("http://a.com");"#);
        let b = normalize_snippet(r#"res.redirect("http://b.com");"#);
        assert_ne!(a, b, "different URLs must not collapse to the same fingerprint");
        assert!(a.contains("http://a.com"));
    }

    #[test]
    fn normalize_snippet_still_strips_a_real_trailing_comment() {
        let with_comment = normalize_snippet("doThing(); // a note");
        let without = normalize_snippet("doThing();");
        assert_eq!(with_comment, without);
    }

    #[test]
    fn fingerprint_survives_line_drift() {
        let at_10 = make_finding(10, SNIPPET, "Unvalidated redirect target");
        let at_200 = make_finding(200, SNIPPET, "Unvalidated redirect target");

        let fp1 = assign_fingerprints(vec![at_10]);
        let fp2 = assign_fingerprints(vec![at_200]);

        assert_eq!(fp1[0].fingerprints.primary, fp2[0].fingerprints.primary);
        assert_eq!(fp1[0].id, fp2[0].id);
    }

    #[test]
    fn fingerprint_changes_when_code_changes() {
        let original = make_finding(10, SNIPPET, "Unvalidated redirect target");
        let changed = make_finding(10, "  if (target) {\n    res.redirect(sanitize(target));\n  }", "Unvalidated redirect target");

        let fp_original = assign_fingerprints(vec![original]);
        let fp_changed = assign_fingerprints(vec![changed]);

        assert_ne!(fp_original[0].fingerprints.primary, fp_changed[0].fingerprints.primary);
    }

    #[test]
    fn fingerprint_disambiguates_duplicated_code_via_occurrence() {
        let first = make_finding(10, SNIPPET, "Unvalidated redirect target");
        let second = make_finding(50, SNIPPET, "Unvalidated redirect target");

        let assigned = assign_fingerprints(vec![first, second]);
        assert_ne!(assigned[0].fingerprints.primary, assigned[1].fingerprints.primary);
    }

    #[test]
    fn rule_key_uses_tool_and_rule_id_for_analyzer_findings() {
        let key_a = rule_key_for(FindingSourceKind::Analyzer, "eslint", Some("no-unused-vars"), None, "style", "unused variable x");
        let key_b = rule_key_for(FindingSourceKind::Analyzer, "eslint", Some("no-unused-vars"), None, "style", "unused variable y");
        assert_eq!(key_a, key_b);
    }

    #[test]
    fn compute_fingerprint_is_pure() {
        let input = FingerprintInput { rule_key: "security:unvalidated-redirect", path: "a.ts", normalized_snippet: "if (x) { y(); }", occurrence: 0 };
        assert_eq!(compute_fingerprint(&input), compute_fingerprint(&input));
    }
}

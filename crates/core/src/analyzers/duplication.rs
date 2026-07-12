//! Self-contained duplicate-code detector — the plan calls for a
//! "jscpd/PMD CPD duplication adapter", but neither is installed in this
//! environment to verify an adapter against, and wrapping either is a wiring
//! task, not a detection algorithm. This implements the same core idea
//! directly (sliding-window exact match over normalized lines, jscpd's own
//! approach) scoped to within a single file's *current* content — the
//! classic "you just pasted the same block twice" catch — rather than
//! cross-file/whole-repo matching, which would need hashing the entire
//! codebase on every review rather than just the diff.

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

/// A duplicate block shorter than this is far more likely to be
/// coincidental structure (e.g. a run of closing braces) than a real
/// copy-paste — jscpd's own default minimum is 5 lines / 50 tokens.
const MIN_DUPLICATE_LINES: usize = 5;
/// Windows whose trimmed content totals under this many characters are
/// skipped even if they technically repeat — mostly whitespace/punctuation.
const MIN_MEANINGFUL_CHARS: usize = 40;

fn normalized_window(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|l| l.trim().to_string()).collect()
}

/// Finds duplicate blocks within one file's content. Reports one finding per
/// *second-and-later* occurrence, pointing back at the first — not one per
/// sliding-window position, which would spam a finding for every 1-line
/// shift through the same duplicated region.
pub fn detect_duplication_in_file(path: &str, content: &str) -> Vec<AgentFinding> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() < MIN_DUPLICATE_LINES * 2 {
        return Vec::new();
    }

    let mut first_seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut findings = Vec::new();
    let mut covered_until = 0usize; // suppress overlapping reports within the same duplicate stretch

    for start in 0..=(lines.len() - MIN_DUPLICATE_LINES) {
        let window = &lines[start..start + MIN_DUPLICATE_LINES];
        let normalized = normalized_window(window);
        if normalized.iter().map(|l| l.len()).sum::<usize>() < MIN_MEANINGFUL_CHARS {
            continue;
        }
        let key = normalized.join("\n");

        match first_seen.get(&key) {
            None => {
                first_seen.insert(key, start);
            }
            Some(&first_start) if start >= covered_until => {
                findings.push(AgentFinding {
                    source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-duplication".to_string(), rule_id: Some("duplicate-code-block".to_string()), aspect: None, backend: None },
                    category: "duplication".to_string(),
                    severity: Severity::Medium,
                    confidence: 1.0,
                    title: format!("Duplicated code block ({MIN_DUPLICATE_LINES}+ lines)"),
                    message: format!("This block duplicates code already at {path}:{} — consider extracting a shared function.", first_start + 1),
                    location: Location { path: path.to_string(), range: LocationRange { start_line: (start + 1) as u32, end_line: Some((start + MIN_DUPLICATE_LINES) as u32), ..Default::default() }, snippet: window.join("\n"), side: Side::New },
                    related_locations: None,
                    suggestion: None,
                    tags: None,
                    meta: None,
                    suggested_patch: None,
                });
                covered_until = start + MIN_DUPLICATE_LINES;
            }
            Some(_) => {}
        }
    }

    findings
}

/// Runs duplication detection across a set of changed files that still
/// exist on disk (deleted files can't be read) — same "scope to what
/// changed, skip what's gone" pattern as the ast-grep/golangci-lint
/// adapters.
pub fn run_duplication_check(repo_root: &std::path::Path, changed_files: &[String]) -> Vec<AgentFinding> {
    changed_files
        .iter()
        .filter_map(|path| {
            let full_path = repo_root.join(path);
            std::fs::read_to_string(&full_path).ok().map(|content| detect_duplication_in_file(path, &content))
        })
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repeated_block() -> String {
        "    let x = compute(a, b);\n    if x > threshold {\n        log(\"exceeded\");\n        notify(x);\n    }\n".to_string()
    }

    #[test]
    fn finds_a_duplicated_block_within_one_file() {
        let block = repeated_block();
        let content = format!("fn one() {{\n{block}}}\n\nfn two() {{\n{block}}}\n");
        let findings = detect_duplication_in_file("a.rs", &content);
        assert_eq!(findings.len(), 1, "expected exactly one finding for the second occurrence, got: {findings:#?}");
        assert_eq!(findings[0].category, "duplication");
        assert!(findings[0].message.contains("a.rs:"));
    }

    #[test]
    fn does_not_flag_a_file_with_no_duplication() {
        let content = "fn one() {\n    println(\"a\");\n}\n\nfn two() {\n    println(\"b\");\n}\n";
        let findings = detect_duplication_in_file("a.rs", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_blocks_shorter_than_the_minimum() {
        let content = "if x {\n    y();\n}\nif x {\n    y();\n}\n";
        let findings = detect_duplication_in_file("a.rs", content);
        assert!(findings.is_empty(), "2-line repeats are below the minimum duplicate length");
    }

    #[test]
    fn does_not_flag_trivial_low_content_repeats() {
        // Five blank-ish lines repeated — structurally "duplicate" but not
        // meaningful, should be filtered by the min-characters threshold.
        let content = "}\n}\n}\n}\n}\n}\n}\n}\n}\n}\n";
        let findings = detect_duplication_in_file("a.rs", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_double_report_overlapping_window_positions() {
        // A longer duplicated stretch (7 meaningfully-long lines) still
        // produces multiple candidate 5-line windows internally — only one
        // finding should surface per contiguous duplicate region.
        let block = "    compute_first(a);\n    compute_second(b);\n    compute_third(c);\n    compute_fourth(d);\n    compute_fifth(e);\n    compute_sixth(f);\n    compute_seventh(g);\n";
        let content = format!("fn one() {{\n{block}}}\n\nfn two() {{\n{block}}}\n");
        let findings = detect_duplication_in_file("a.rs", &content);
        assert_eq!(findings.len(), 1, "got: {findings:#?}");
    }

    #[test]
    fn run_duplication_check_skips_files_that_no_longer_exist() {
        let dir = tempfile::tempdir().unwrap();
        let findings = run_duplication_check(dir.path(), &["deleted.rs".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn run_duplication_check_finds_duplication_in_a_real_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let block = repeated_block();
        let content = format!("fn one() {{\n{block}}}\n\nfn two() {{\n{block}}}\n");
        std::fs::write(dir.path().join("a.rs"), &content).unwrap();
        let findings = run_duplication_check(dir.path(), &["a.rs".to_string()]);
        assert_eq!(findings.len(), 1);
    }
}

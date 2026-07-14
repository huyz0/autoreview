//! Track 4 (code practices) of the rule-pack expansion plan. Deliberately
//! the smallest track — Go practices are already substantially covered by
//! the existing `golangci-lint` wrapper (revive/staticcheck/govet), and
//! Java/Kotlin practices are better served by wrapping real linters
//! (detekt/ktlint/checkstyle) through the generic SARIF ingest adapter
//! (`analyzers::sarif`) than by re-implementing their rules here — that's
//! per-tool wiring work, tracked separately, not done in this module.
//!
//! Implemented as plain text/regex scanning (no ast-grep, no new parse
//! dependency) since none of these three checks are structural in the way
//! ast-grep patterns are suited for — they're about comment *content* and
//! import *text*, not code shape.
//!
//! Scope note: "debug print statements left in non-test code" was in the
//! plan's original sketch and is deliberately dropped here. Distinguishing
//! a genuine debug leftover from legitimate stdout usage (a CLI's own
//! output, a logger shim, deliberate diagnostics) needs context this kind
//! of scanning can't reliably provide — a bare "flag every println" rule
//! would be low-precision noise, which the project's own deterministic-
//! first philosophy explicitly argues against generating.

use std::path::Path;

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PracticesLanguage {
    Go,
    JavaOrKotlin,
}

fn language_for_file(path: &str) -> Option<PracticesLanguage> {
    match Path::new(path).extension().and_then(|e| e.to_str()) {
        Some("go") => Some(PracticesLanguage::Go),
        Some("java") | Some("kt") | Some("kts") => Some(PracticesLanguage::JavaOrKotlin),
        _ => None,
    }
}

fn line_comment_prefix(language: PracticesLanguage) -> &'static str {
    match language {
        PracticesLanguage::Go | PracticesLanguage::JavaOrKotlin => "//",
    }
}

fn make_finding(rule_id: &str, path: &str, start_line: u32, end_line: u32, title: String, message: String, snippet: String) -> AgentFinding {
    AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-practices".to_string(), rule_id: Some(rule_id.to_string()), aspect: None, backend: None },
        category: "style".to_string(),
        severity: Severity::Low,
        confidence: 1.0,
        title,
        message,
        location: Location { path: path.to_string(), range: LocationRange { start_line, end_line: Some(end_line), ..Default::default() }, snippet, side: Side::New },
        related_locations: None,
        suggestion: None,
        tags: None,
        meta: None,
        suggested_patch: None,
    }
}

/// A commented-out code line is one that, stripped of its `//` prefix,
/// still ends in a statement terminator — a strong, low-false-positive
/// signal (real prose comments essentially never end every line with a
/// semicolon) that deliberately trades recall for precision.
fn looks_like_commented_out_code(comment_text: &str) -> bool {
    comment_text.trim_end().ends_with(';')
}

/// Flags runs of 3+ consecutive `//` comment lines that all look like
/// commented-out code, per `looks_like_commented_out_code`.
fn detect_commented_out_code(path: &str, content: &str, language: PracticesLanguage) -> Vec<AgentFinding> {
    let prefix = line_comment_prefix(language);
    let mut findings = Vec::new();
    let mut run_start: Option<usize> = None;
    let mut run_len = 0usize;

    let lines: Vec<&str> = content.lines().collect();
    for (idx, raw_line) in lines.iter().enumerate() {
        let trimmed = raw_line.trim();
        let is_code_like_comment = trimmed.strip_prefix(prefix).map(looks_like_commented_out_code).unwrap_or(false);

        if is_code_like_comment {
            if run_start.is_none() {
                run_start = Some(idx);
            }
            run_len += 1;
        } else {
            if run_len >= 3 {
                let start = run_start.unwrap();
                findings.push(make_finding(
                    "commented-out-code",
                    path,
                    (start + 1) as u32,
                    idx as u32,
                    format!("Commented-out code block ({run_len} lines)"),
                    "A run of consecutive commented-out statements was left in the file — delete it (version control already has the history) or add a comment explaining why it's kept.".to_string(),
                    lines[start..idx].join("\n"),
                ));
            }
            run_start = None;
            run_len = 0;
        }
    }
    if run_len >= 3 {
        let start = run_start.unwrap();
        findings.push(make_finding(
            "commented-out-code",
            path,
            (start + 1) as u32,
            lines.len() as u32,
            format!("Commented-out code block ({run_len} lines)"),
            "A run of consecutive commented-out statements was left in the file — delete it (version control already has the history) or add a comment explaining why it's kept.".to_string(),
            lines[start..].join("\n"),
        ));
    }

    findings
}

/// A TODO/FIXME with no ticket-like reference on the same line (`#123`,
/// `JIRA-123`, `ABC-4567`) is easy to lose track of forever — this doesn't
/// require a *specific* tracker, just some reference a reader could follow.
fn has_ticket_reference(text: &str) -> bool {
    let bytes = text.as_bytes();
    // #123
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '#' && bytes.get(i + 1).is_some_and(|b| b.is_ascii_digit()) {
            return true;
        }
    }
    // ABC-123 (letters, hyphen, digits — Jira-style)
    let word_chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < word_chars.len() {
        if word_chars[i].is_ascii_uppercase() {
            let start = i;
            while i < word_chars.len() && word_chars[i].is_ascii_uppercase() {
                i += 1;
            }
            if i > start && i < word_chars.len() && word_chars[i] == '-' {
                let mut j = i + 1;
                let digit_start = j;
                while j < word_chars.len() && word_chars[j].is_ascii_digit() {
                    j += 1;
                }
                if j > digit_start {
                    return true;
                }
            }
        } else {
            i += 1;
        }
    }
    false
}

fn detect_todo_without_ticket(path: &str, content: &str, language: PracticesLanguage) -> Vec<AgentFinding> {
    let prefix = line_comment_prefix(language);
    let mut findings = Vec::new();

    for (idx, raw_line) in content.lines().enumerate() {
        let trimmed = raw_line.trim();
        let Some(comment) = trimmed.strip_prefix(prefix) else { continue };
        let upper = comment.to_uppercase();
        if !(upper.contains("TODO") || upper.contains("FIXME")) {
            continue;
        }
        if has_ticket_reference(comment) {
            continue;
        }
        findings.push(make_finding(
            "todo-without-ticket",
            path,
            (idx + 1) as u32,
            (idx + 1) as u32,
            "TODO/FIXME with no ticket reference".to_string(),
            "This TODO/FIXME has no ticket/issue reference (e.g. #123, JIRA-456) — bare TODOs are easy to lose track of forever. Link it to a tracked issue, or resolve it now.".to_string(),
            trimmed.to_string(),
        ));
    }

    findings
}

fn detect_wildcard_imports(path: &str, content: &str) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    for (idx, raw_line) in content.lines().enumerate() {
        let trimmed = raw_line.trim();
        let Some(rest) = trimmed.strip_prefix("import ") else { continue };
        let rest = rest.trim_end_matches(';').trim();
        if rest.ends_with(".*") {
            findings.push(make_finding(
                "wildcard-import",
                path,
                (idx + 1) as u32,
                (idx + 1) as u32,
                "Wildcard import".to_string(),
                "A wildcard import pulls in every symbol from the package, obscuring which specific classes/functions this file actually depends on and risking name collisions as the package grows. Import the specific names used instead.".to_string(),
                trimmed.to_string(),
            ));
        }
    }
    findings
}

/// Runs all Track 4 checks against one changed file's current content.
pub fn run_practices_check(repo_root: &Path, changed_files: &[String]) -> Vec<AgentFinding> {
    changed_files
        .iter()
        .filter_map(|path| {
            let language = language_for_file(path)?;
            let full_path = repo_root.join(path);
            let content = std::fs::read_to_string(&full_path).ok()?;

            let mut findings = detect_commented_out_code(path, &content, language);
            findings.extend(detect_todo_without_ticket(path, &content, language));
            if language == PracticesLanguage::JavaOrKotlin {
                findings.extend(detect_wildcard_imports(path, &content));
            }
            Some(findings)
        })
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_a_run_of_three_or_more_commented_out_statements() {
        let content = "func f() {\n\t// x := 1;\n\t// y := 2;\n\t// z := 3;\n\treal := 4\n\t_ = real\n}\n";
        let findings = detect_commented_out_code("main.go", content, PracticesLanguage::Go);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("commented-out-code"));
    }

    #[test]
    fn does_not_flag_two_commented_out_statements() {
        let content = "func f() {\n\t// x := 1;\n\t// y := 2;\n\treal := 4\n\t_ = real\n}\n";
        let findings = detect_commented_out_code("main.go", content, PracticesLanguage::Go);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_prose_comments() {
        let content = "func f() {\n\t// This function does something important.\n\t// It has multiple lines of explanation.\n\t// See the docs for more info.\n\treal := 4\n\t_ = real\n}\n";
        let findings = detect_commented_out_code("main.go", content, PracticesLanguage::Go);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_a_bare_todo() {
        let content = "func f() {\n\t// TODO: fix this properly\n}\n";
        let findings = detect_todo_without_ticket("main.go", content, PracticesLanguage::Go);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("todo-without-ticket"));
    }

    #[test]
    fn does_not_flag_a_todo_with_a_github_issue_reference() {
        let content = "func f() {\n\t// TODO(#123): fix this properly\n}\n";
        let findings = detect_todo_without_ticket("main.go", content, PracticesLanguage::Go);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_a_todo_with_a_jira_style_reference() {
        let content = "func f() {\n\t// TODO(JIRA-456): fix this properly\n}\n";
        let findings = detect_todo_without_ticket("main.go", content, PracticesLanguage::Go);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_fixme_case_insensitively() {
        let content = "func f() {\n\t// fixme this is broken\n}\n";
        let findings = detect_todo_without_ticket("main.go", content, PracticesLanguage::Go);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn flags_a_wildcard_import_in_java() {
        let content = "import java.util.*;\n\npublic class S {}\n";
        let findings = detect_wildcard_imports("S.java", content);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("wildcard-import"));
    }

    #[test]
    fn does_not_flag_a_specific_import_in_java() {
        let content = "import java.util.List;\n\npublic class S {}\n";
        let findings = detect_wildcard_imports("S.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_a_wildcard_import_in_kotlin() {
        let content = "import java.util.*\n\nfun f() {}\n";
        let findings = detect_wildcard_imports("S.kt", content);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn run_practices_check_skips_go_files_for_wildcard_imports() {
        // Go has no wildcard import syntax at all — sanity check that the
        // Go path never even calls detect_wildcard_imports.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.go"), "package main\n\nimport \"fmt\"\n\nfunc main() { fmt.Println(1) }\n").unwrap();
        let findings = run_practices_check(dir.path(), &["main.go".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn run_practices_check_finds_issues_in_a_real_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("S.java"), "import java.util.*;\n\npublic class S {\n    // TODO fix this\n}\n").unwrap();
        let findings = run_practices_check(dir.path(), &["S.java".to_string()]);
        assert_eq!(findings.len(), 2);
    }

    #[test]
    fn run_practices_check_skips_files_that_no_longer_exist() {
        let dir = tempfile::tempdir().unwrap();
        let findings = run_practices_check(dir.path(), &["deleted.go".to_string()]);
        assert!(findings.is_empty());
    }
}

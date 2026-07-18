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

/// Fowler's "Comments" smell, the padding variant: a comment block sitting
/// directly above a method that's wildly disproportionate to the method
/// body it documents. Distinguishing a genuinely *stale* comment (one that
/// no longer matches the code) needs semantic understanding this line-scan
/// can't provide, so this only catches the syntactic proxy the session's
/// prior pass flagged as tractable — comment volume vastly exceeding the
/// code volume is itself a maintenance smell (it's either padding that
/// should be trimmed or a sign the method needs splitting up, not more
/// prose). Reuses `complexity::opens_function`'s same brace-style heuristic
/// so both scanners treat function boundaries identically.
fn detect_padding_comment(path: &str, content: &str, language: PracticesLanguage) -> Vec<AgentFinding> {
    use super::complexity::opens_function;

    const MIN_COMMENT_LINES: usize = 10;
    const RATIO: usize = 3;

    let prefix = line_comment_prefix(language);
    let lines: Vec<&str> = content.lines().collect();
    let mut findings = Vec::new();

    let mut idx = 0usize;
    while idx < lines.len() {
        let trimmed = lines[idx].trim();
        if trimmed.strip_prefix(prefix).is_some() {
            let comment_start = idx;
            let mut j = idx;
            while j < lines.len() && lines[j].trim().strip_prefix(prefix).is_some() {
                j += 1;
            }
            let comment_len = j - comment_start;

            if j < lines.len() && opens_function(lines[j].trim()).is_some() && comment_len >= MIN_COMMENT_LINES {
                // Measure the function body by brace depth from its opening line.
                let mut depth = 0i32;
                let mut body_end = j;
                for (k, l) in lines.iter().enumerate().skip(j) {
                    depth += l.matches('{').count() as i32;
                    depth -= l.matches('}').count() as i32;
                    if depth == 0 {
                        body_end = k;
                        break;
                    }
                }
                let body_len = body_end - j + 1;

                if comment_len > RATIO * body_len {
                    findings.push(make_finding(
                        "excessive-comment-padding",
                        path,
                        (comment_start + 1) as u32,
                        j as u32,
                        format!("Comment block ({comment_len} lines) is disproportionate to the method it documents ({body_len} lines)"),
                        "This comment block is far longer than the method it precedes, which often means it's stale/padding rather than useful documentation, or a sign the method itself has grown too complex to explain briefly. Trim it to what's still accurate, or split the method.".to_string(),
                        lines[comment_start..j].join("\n"),
                    ));
                }
            }
            idx = j.max(idx + 1);
        } else {
            idx += 1;
        }
    }

    findings
}

/// A near-universal check across every tool researched for the rule-pack
/// gap analysis (Checkstyle UnusedImports, detekt UnusedImport, Sonar
/// S1128) that this project had zero coverage of. Single-file syntactic:
/// an import's simple name (the last `.`-segment) must appear as a whole
/// word somewhere else in the file, or it's unused. Deliberately skips
/// wildcard imports (already covered by `detect_wildcard_imports`, and
/// there's no single "simple name" to search for), static/Kotlin
/// extension-function imports with an `as` alias (search for the alias
/// instead — same reasoning, just a different name to look for), and
/// annotation-only imports used purely for their side effect on
/// `@Target`-adjacent processors are not special-cased since those still
/// use their simple name at the annotation site (`@MyAnnotation`), so the
/// generic whole-word search already covers them correctly.
fn detect_unused_import(path: &str, content: &str) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    let lines: Vec<&str> = content.lines().collect();

    for (idx, raw_line) in lines.iter().enumerate() {
        let trimmed = raw_line.trim();
        let Some(rest) = trimmed.strip_prefix("import ") else { continue };
        let rest = rest.trim_end_matches(';').trim();
        if rest.ends_with(".*") || rest.starts_with("static ") {
            continue;
        }

        let simple_name = if let Some((_, alias)) = rest.rsplit_once(" as ") {
            alias.trim()
        } else {
            rest.rsplit('.').next().unwrap_or(rest).trim()
        };
        if simple_name.is_empty() {
            continue;
        }

        let used_elsewhere = lines.iter().enumerate().any(|(other_idx, other_line)| {
            if other_idx == idx {
                return false;
            }
            contains_whole_word(other_line, simple_name)
        });

        if !used_elsewhere {
            findings.push(make_finding(
                "unused-import",
                path,
                (idx + 1) as u32,
                (idx + 1) as u32,
                format!("Unused import ({simple_name})"),
                format!("`{simple_name}` is imported but never referenced anywhere else in this file — dead code that adds noise to the file's real dependency list."),
                trimmed.to_string(),
            ));
        }
    }
    findings
}

/// Whole-word substring search — a plain `contains` would also match
/// `simple_name` as a substring of a longer identifier (e.g. `Foo` inside
/// `FooBar`), which isn't a real usage.
fn contains_whole_word(haystack: &str, word: &str) -> bool {
    let bytes = haystack.as_bytes();
    let word_bytes = word.as_bytes();
    if word_bytes.is_empty() {
        return false;
    }
    let is_word_char = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(word) {
        let abs = start + pos;
        let before_ok = abs == 0 || !is_word_char(bytes[abs - 1]);
        let after_idx = abs + word_bytes.len();
        let after_ok = after_idx >= bytes.len() || !is_word_char(bytes[after_idx]);
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
        if start >= haystack.len() {
            break;
        }
    }
    false
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
            findings.extend(detect_padding_comment(path, &content, language));
            if language == PracticesLanguage::JavaOrKotlin {
                findings.extend(detect_wildcard_imports(path, &content));
                findings.extend(detect_unused_import(path, &content));
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
    fn flags_a_comment_block_wildly_disproportionate_to_its_method() {
        let mut lines = vec!["// line".to_string(); 12];
        lines.push("func f() {".to_string());
        lines.push("\treturn".to_string());
        lines.push("}".to_string());
        let content = lines.join("\n");
        let findings = detect_padding_comment("main.go", &content, PracticesLanguage::Go);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("excessive-comment-padding"));
    }

    #[test]
    fn does_not_flag_a_reasonably_sized_doc_comment() {
        let content = "// f does something.\n// It takes no args.\nfunc f() {\n\treturn\n}\n";
        let findings = detect_padding_comment("main.go", content, PracticesLanguage::Go);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_a_long_comment_over_a_long_method() {
        let mut lines = vec!["// line".to_string(); 12];
        lines.push("func f() {".to_string());
        for _ in 0..40 {
            lines.push("\tdoWork()".to_string());
        }
        lines.push("}".to_string());
        let content = lines.join("\n");
        let findings = detect_padding_comment("main.go", &content, PracticesLanguage::Go);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_an_unused_import_in_java() {
        let content = "import java.util.List;\n\npublic class S {\n    int x;\n}\n";
        let findings = detect_unused_import("S.java", content);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some("unused-import"));
    }

    #[test]
    fn does_not_flag_a_used_import_in_java() {
        let content = "import java.util.List;\n\npublic class S {\n    List<String> items;\n}\n";
        let findings = detect_unused_import("S.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_a_wildcard_import_as_unused() {
        let content = "import java.util.*;\n\npublic class S {}\n";
        let findings = detect_unused_import("S.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn does_not_flag_a_static_import_as_unused() {
        let content = "import static java.util.Collections.emptyList;\n\npublic class S {}\n";
        let findings = detect_unused_import("S.java", content);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_only_the_simple_name_not_a_substring_match() {
        // "Foo" imported but only "FooBar" appears elsewhere — not a real usage.
        let content = "import com.example.Foo;\n\npublic class S {\n    FooBar x;\n}\n";
        let findings = detect_unused_import("S.java", content);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn checks_the_alias_for_a_kotlin_aliased_import() {
        let content = "import com.example.Foo as Bar\n\nclass S {\n    val x: Bar? = null\n}\n";
        let findings = detect_unused_import("S.kt", content);
        assert!(findings.is_empty());
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

//! Track 2 (metric-based half) of the rule-pack expansion plan: long
//! method, long parameter list, deep nesting, and god class. ast-grep's
//! declarative YAML has no aggregate/threshold primitive ("more than 4
//! parameters", "nesting depth > 3") — confirmed repeatedly this session
//! fighting its pattern-parsing ambiguities for far simpler syntactic
//! rules — so rather than a hybrid "ast-grep locates, Rust measures"
//! design, this is a single self-contained brace-depth scanner, the same
//! shape as `duplication.rs`. A real, if approximate, cost: it's a line
//! scan with a heuristic for "this line opens a function/class", not a
//! real parse, so unusual formatting (Allman brace style, e.g.) can throw
//! it off. Verified against real representative samples per language.

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

const DEFAULT_MAX_METHOD_LINES: usize = 80;
const DEFAULT_MAX_PARAMS: usize = 5;
const DEFAULT_MAX_NESTING: usize = 4;
const DEFAULT_MAX_CLASS_MEMBERS: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComplexityLanguage {
    Go,
    JavaOrKotlin,
}

pub fn language_for_file(path: &str) -> Option<ComplexityLanguage> {
    match std::path::Path::new(path).extension().and_then(|e| e.to_str()) {
        Some("go") => Some(ComplexityLanguage::Go),
        Some("java") | Some("kt") | Some("kts") => Some(ComplexityLanguage::JavaOrKotlin),
        _ => None,
    }
}

const CONTROL_KEYWORDS: &[&str] = &["if", "for", "while", "switch", "select", "catch", "else"];

/// Heuristic: does this line open a function/method body? Requires a
/// parenthesized parameter list followed by an opening brace on the same
/// line (the common "brace on same line as signature" style in all three
/// languages), and excludes lines that are actually control-flow statements
/// or type declarations that happen to also end in `) {` or `{`.
fn opens_function(trimmed: &str) -> Option<&str> {
    if !trimmed.ends_with('{') {
        return None;
    }
    let before_brace = trimmed[..trimmed.len() - 1].trim_end();
    let Some(close_paren) = before_brace.rfind(')') else { return None };
    let Some(open_paren) = before_brace[..close_paren].rfind('(') else { return None };
    let head = before_brace[..open_paren].trim();
    let first_word = head.split_whitespace().next().unwrap_or("");
    if CONTROL_KEYWORDS.contains(&first_word) {
        return None;
    }
    if head.is_empty() {
        return None;
    }
    Some(&before_brace[open_paren + 1..close_paren])
}

fn opens_class(trimmed: &str) -> bool {
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    trimmed.ends_with('{') && words.iter().any(|w| *w == "class")
}

fn opens_control_block(trimmed: &str) -> bool {
    if !trimmed.ends_with('{') {
        return false;
    }
    let first_word = trimmed.split_whitespace().next().unwrap_or("");
    CONTROL_KEYWORDS.contains(&first_word) || trimmed.starts_with("} else")
}

/// Counts top-level (bracket/paren/angle-depth-0) commas in a parameter
/// list — so `foo(a []int, b map[string]int)` counts 2 params, not more,
/// despite the internal commas a naive split would trip on.
fn count_params(param_list: &str) -> usize {
    let trimmed = param_list.trim();
    if trimmed.is_empty() {
        return 0;
    }
    let mut depth = 0i32;
    let mut count = 1usize;
    for c in trimmed.chars() {
        match c {
            '(' | '[' | '<' | '{' => depth += 1,
            ')' | ']' | '>' | '}' => depth -= 1,
            ',' if depth == 0 => count += 1,
            _ => {}
        }
    }
    count
}

struct OpenSpan {
    start_line: usize,
    start_depth: i32,
    kind: SpanKind,
}

enum SpanKind {
    Function { name_hint: String, param_count: usize, max_nesting: usize },
    Class { member_count: usize },
}

fn make_finding(rule_id: &str, path: &str, start_line: u32, end_line: u32, title: String, message: String) -> AgentFinding {
    AgentFinding {
        source: FindingSource { kind: FindingSourceKind::Analyzer, tool: "autoreview-complexity".to_string(), rule_id: Some(rule_id.to_string()), aspect: None, backend: None },
        category: "design".to_string(),
        severity: Severity::Medium,
        confidence: 1.0,
        title,
        message,
        location: Location { path: path.to_string(), range: LocationRange { start_line, end_line: Some(end_line), ..Default::default() }, snippet: String::new(), side: Side::New },
        related_locations: None,
        suggestion: None,
        tags: None,
        meta: None,
        suggested_patch: None,
    }
}

/// Scans one file's content for long methods, long parameter lists, deep
/// nesting, and (Java/Kotlin only) god classes. A single brace-depth pass:
/// opening a function or class pushes a tracked span; closing braces that
/// return to a span's start depth close it and evaluate its thresholds.
pub fn detect_complexity_in_file(path: &str, content: &str, language: ComplexityLanguage) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    let mut depth: i32 = 0;
    let mut stack: Vec<OpenSpan> = Vec::new();

    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = raw_line.trim();

        if let Some(param_list) = opens_function(trimmed) {
            // A class member's method body sits one level inside its
            // enclosing class span — count it there before pushing our own.
            if let Some(OpenSpan { kind: SpanKind::Class { member_count }, start_depth, .. }) = stack.last_mut() {
                // A class's own body content lives one level deeper than
                // its `start_depth` (recorded *before* its opening brace
                // incremented `depth`) — so a direct member sits at
                // `start_depth + 1`, not `start_depth` itself.
                if depth == *start_depth + 1 {
                    *member_count += 1;
                }
            }
            stack.push(OpenSpan { start_line: line_no, start_depth: depth, kind: SpanKind::Function { name_hint: trimmed.to_string(), param_count: count_params(param_list), max_nesting: 0 } });
            depth += 1;
            continue;
        }

        if language == ComplexityLanguage::JavaOrKotlin && opens_class(trimmed) {
            stack.push(OpenSpan { start_line: line_no, start_depth: depth, kind: SpanKind::Class { member_count: 0 } });
            depth += 1;
            continue;
        }

        if trimmed.ends_with('{') {
            if opens_control_block(trimmed) {
                if let Some(OpenSpan { kind: SpanKind::Function { max_nesting, .. }, start_depth, .. }) = stack.last_mut() {
                    let relative = (depth - *start_depth + 1).max(0) as usize;
                    *max_nesting = (*max_nesting).max(relative);
                }
            }
            depth += 1;
            continue;
        }

        if trimmed == "}" || trimmed.starts_with("} ") {
            depth -= 1;
            while let Some(span) = stack.last() {
                if depth != span.start_depth {
                    break;
                }
                let span = stack.pop().unwrap();
                match span.kind {
                    SpanKind::Function { name_hint, param_count, max_nesting } => {
                        let end_line = line_no as u32;
                        let line_count = end_line - span.start_line as u32;
                        if line_count as usize > DEFAULT_MAX_METHOD_LINES {
                            findings.push(make_finding(
                                "long-method",
                                path,
                                span.start_line as u32,
                                end_line,
                                format!("Long method ({line_count} lines)"),
                                format!("This function/method body is {line_count} lines long (over the {DEFAULT_MAX_METHOD_LINES}-line threshold) — consider extracting smaller, named helper functions for its distinct steps."),
                            ));
                        }
                        if param_count > DEFAULT_MAX_PARAMS {
                            findings.push(make_finding(
                                "long-parameter-list",
                                path,
                                span.start_line as u32,
                                span.start_line as u32,
                                format!("Long parameter list ({param_count} parameters)"),
                                format!("This function/method takes {param_count} parameters (over the {DEFAULT_MAX_PARAMS}-parameter threshold) — consider grouping related parameters into a struct/options object."),
                            ));
                        }
                        if max_nesting > DEFAULT_MAX_NESTING {
                            findings.push(make_finding(
                                "deep-nesting",
                                path,
                                span.start_line as u32,
                                end_line,
                                format!("Deep nesting ({max_nesting} levels)"),
                                format!("This function/method nests control-flow blocks {max_nesting} levels deep (over the {DEFAULT_MAX_NESTING}-level threshold) — consider early returns/guard clauses or extracting the innermost blocks into their own functions."),
                            ));
                        }
                        let _ = name_hint;
                    }
                    SpanKind::Class { member_count } => {
                        if member_count > DEFAULT_MAX_CLASS_MEMBERS {
                            findings.push(make_finding(
                                "god-class",
                                path,
                                span.start_line as u32,
                                line_no as u32,
                                format!("Large class ({member_count} methods)"),
                                format!("This class defines {member_count} methods (over the {DEFAULT_MAX_CLASS_MEMBERS}-method threshold) — a class this large is often doing more than one job; consider splitting it by responsibility."),
                            ));
                        }
                    }
                }
            }
            continue;
        }
    }

    findings
}

pub fn run_complexity_check(repo_root: &std::path::Path, changed_files: &[String]) -> Vec<AgentFinding> {
    changed_files
        .iter()
        .filter_map(|path| {
            let language = language_for_file(path)?;
            let full_path = repo_root.join(path);
            let content = std::fs::read_to_string(&full_path).ok()?;
            Some(detect_complexity_in_file(path, &content, language))
        })
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_params_counts_top_level_commas_only() {
        assert_eq!(count_params(""), 0);
        assert_eq!(count_params("a int"), 1);
        assert_eq!(count_params("a int, b string"), 2);
        assert_eq!(count_params("a []int, b map[string]int, c func(int, int) bool"), 3);
    }

    #[test]
    fn flags_a_long_go_function() {
        let mut body = String::from("func doIt() {\n");
        for i in 0..90 {
            body.push_str(&format!("\tx{i} := {i}\n\t_ = x{i}\n"));
        }
        body.push_str("}\n");
        let findings = detect_complexity_in_file("main.go", &body, ComplexityLanguage::Go);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("long-method")), "expected a long-method finding, got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_short_go_function() {
        let content = "func doIt() {\n\tx := 1\n\t_ = x\n}\n";
        let findings = detect_complexity_in_file("main.go", content, ComplexityLanguage::Go);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_a_long_parameter_list_in_go() {
        let content = "func doIt(a int, b int, c int, d int, e int, f int) {\n\t_ = a\n}\n";
        let findings = detect_complexity_in_file("main.go", content, ComplexityLanguage::Go);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("long-parameter-list")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_reasonable_parameter_list_in_go() {
        let content = "func doIt(a int, b int) {\n\t_ = a\n}\n";
        let findings = detect_complexity_in_file("main.go", content, ComplexityLanguage::Go);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_deep_nesting_in_go() {
        let content = "func doIt(x int) {\n\tif x > 0 {\n\t\tif x > 1 {\n\t\t\tif x > 2 {\n\t\t\t\tif x > 3 {\n\t\t\t\t\tif x > 4 {\n\t\t\t\t\t\tprintln(x)\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t}\n}\n";
        let findings = detect_complexity_in_file("main.go", content, ComplexityLanguage::Go);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("deep-nesting")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_shallow_nesting_in_go() {
        let content = "func doIt(x int) {\n\tif x > 0 {\n\t\tif x > 1 {\n\t\t\tprintln(x)\n\t\t}\n\t}\n}\n";
        let findings = detect_complexity_in_file("main.go", content, ComplexityLanguage::Go);
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_a_god_class_in_java() {
        let mut content = String::from("public class Big {\n");
        for i in 0..25 {
            content.push_str(&format!("    void method{i}() {{\n        int x = {i};\n    }}\n"));
        }
        content.push_str("}\n");
        let findings = detect_complexity_in_file("Big.java", &content, ComplexityLanguage::JavaOrKotlin);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("god-class")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_small_class_in_java() {
        let content = "public class Small {\n    void method1() {\n        int x = 1;\n    }\n    void method2() {\n        int x = 2;\n    }\n}\n";
        let findings = detect_complexity_in_file("Small.java", content, ComplexityLanguage::JavaOrKotlin);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("god-class")), "got: {findings:#?}");
    }

    #[test]
    fn run_complexity_check_skips_files_it_has_no_language_for() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "# hi\n").unwrap();
        let findings = run_complexity_check(dir.path(), &["README.md".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn run_complexity_check_skips_files_that_no_longer_exist() {
        let dir = tempfile::tempdir().unwrap();
        let findings = run_complexity_check(dir.path(), &["deleted.go".to_string()]);
        assert!(findings.is_empty());
    }

    #[test]
    fn run_complexity_check_finds_issues_in_a_real_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.go"), "func doIt(a int, b int, c int, d int, e int, f int) {\n\t_ = a\n}\n").unwrap();
        let findings = run_complexity_check(dir.path(), &["main.go".to_string()]);
        assert!(!findings.is_empty());
    }
}

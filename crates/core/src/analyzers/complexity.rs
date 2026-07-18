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
/// Fowler's Switch Statements smell doesn't give a specific arm count, so
/// this follows the same style as the other thresholds here: conservative
/// enough that a legitimately large, unavoidable dispatch (a state machine,
/// a protocol opcode table) doesn't get flagged constantly, while still
/// catching the "this should probably be polymorphism/a lookup table"
/// cases the smell is about.
const DEFAULT_MAX_SWITCH_CASES: usize = 8;
/// A class needs at least this many trivial-accessor-shaped methods before
/// Data Class is even considered — a 1- or 2-method match (e.g. a real
/// class that happens to have one getter) is too common to be meaningful.
const MIN_DATA_CLASS_ACCESSORS: usize = 3;
/// A class needs at least this many static methods before Utility Class
/// is worth flagging — a single static helper on an otherwise-instantiable
/// class is normal and not what this smell is about.
const MIN_UTILITY_CLASS_STATIC_METHODS: usize = 2;
/// Cyclomatic complexity = decision points + 1. This counts only
/// control-block openings (if/for/while/switch/catch/case, already
/// detected by `opens_control_block`/case-line scanning elsewhere in this
/// file) as decision points — deliberately not `&&`/`||`/`?:`, which would
/// need real expression parsing to count without false positives from
/// string literals or comments containing those characters. This
/// under-counts true cyclomatic complexity somewhat, the same conservative
/// direction as this file's other line-scan-based heuristics.
const DEFAULT_MAX_CYCLOMATIC_COMPLEXITY: usize = 10;
/// A method with many return points is often doing too much conditional
/// branching to be followed easily — same threshold family as the other
/// metrics here (Checkstyle/detekt's own ReturnCount default is also
/// single digits).
const DEFAULT_MAX_RETURNS: usize = 4;
/// detekt's CognitiveComplexMethod default (not on-by-default upstream,
/// but this is the threshold it documents) — cognitive complexity grows
/// faster than cyclomatic complexity for nested code, so its threshold is
/// noticeably higher than `DEFAULT_MAX_CYCLOMATIC_COMPLEXITY`.
const DEFAULT_MAX_COGNITIVE_COMPLEXITY: usize = 15;

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
pub(crate) fn opens_function(trimmed: &str) -> Option<&str> {
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

/// Extracts a method's name from its declaration line (the same shape
/// `opens_function` already validated) — the last identifier before the
/// parameter list, e.g. `"public String getName()"` -> `"getName"`. Used
/// only for the Data Class check's naming-convention heuristic.
pub(crate) fn function_name(trimmed: &str) -> Option<&str> {
    if !trimmed.ends_with('{') {
        return None;
    }
    let before_brace = trimmed[..trimmed.len() - 1].trim_end();
    let close_paren = before_brace.rfind(')')?;
    let open_paren = before_brace[..close_paren].rfind('(')?;
    let head = before_brace[..open_paren].trim();
    head.split_whitespace().next_back()
}

fn opens_class(trimmed: &str) -> bool {
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    trimmed.ends_with('{') && words.iter().any(|w| *w == "class")
}

fn opens_interface(trimmed: &str) -> bool {
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    trimmed.ends_with('{') && words.iter().any(|w| *w == "interface")
}

/// Heuristic: does this line look like an abstract method signature inside
/// an interface body (no braces, just `name(...);`)? Distinct from
/// `opens_function`, which requires a same-line opening brace — interface
/// members are usually just a signature ending in `;`.
fn is_abstract_member_signature(trimmed: &str) -> bool {
    if !trimmed.ends_with(';') || trimmed.starts_with("//") || trimmed.starts_with('@') {
        return false;
    }
    let Some(open_paren) = trimmed.find('(') else { return false };
    trimmed[..open_paren].trim().chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// The class's own name, for the Utility Class check's constructor-name
/// match — the token immediately after the `class` keyword.
fn class_name(trimmed: &str) -> Option<&str> {
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    let idx = words.iter().position(|w| *w == "class")?;
    words.get(idx + 1).map(|s| s.trim_end_matches('{').trim())
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
    Function { name_hint: String, param_count: usize, max_nesting: usize, branch_count: usize, return_count: usize, cognitive_score: usize },
    /// `is_java` (not Kotlin) gates the Data Class check: Kotlin has
    /// first-class `data class` support, an intentional, encouraged
    /// language feature, not a smell — flagging it would be actively wrong
    /// for idiomatic Kotlin. Data Class is fundamentally a JavaBean-era
    /// Java pattern (manual getters/setters where a `record` would do).
    Class {
        member_count: usize,
        trivial_accessor_count: usize,
        is_java: bool,
        name: String,
        saw_any_method: bool,
        all_methods_static: bool,
        has_private_constructor: bool,
    },
    /// Go/Java `switch` only (deliberately not Kotlin's `when`: its arms use
    /// a bare `->` with no keyword prefix, which is indistinguishable from a
    /// lambda expression by this line-scan — counting them would be a
    /// meaningfully higher false-positive risk than the keyword-anchored
    /// `case`/`default` lines Go/Java use).
    Switch { case_count: usize },
    /// A member is either an abstract signature (`foo();`, counted per-line
    /// as the scan passes over it — it has no body/brace to trigger on) or
    /// a default/static method with a body (counted the same way a Class
    /// span counts its methods, at push time in the `opens_function` arm).
    Interface { member_count: usize },
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

        if let Some(OpenSpan { kind: SpanKind::Switch { case_count }, start_depth, .. }) = stack.last_mut() {
            if depth == *start_depth + 1 && (trimmed.starts_with("case ") || trimmed == "default:" || trimmed.starts_with("default:")) {
                *case_count += 1;
            }
        }

        if let Some(OpenSpan { kind: SpanKind::Interface { member_count }, start_depth, .. }) = stack.last_mut() {
            if depth == *start_depth + 1 && is_abstract_member_signature(trimmed) {
                *member_count += 1;
            }
        }

        if let Some(param_list) = opens_function(trimmed) {
            // A class member's method body sits one level inside its
            // enclosing class span — count it there before pushing our own.
            if let Some(OpenSpan { kind: SpanKind::Class { member_count, name, saw_any_method, all_methods_static, has_private_constructor, .. }, start_depth, .. }) = stack.last_mut() {
                // A class's own body content lives one level deeper than
                // its `start_depth` (recorded *before* its opening brace
                // incremented `depth`) — so a direct member sits at
                // `start_depth + 1`, not `start_depth` itself.
                if depth == *start_depth + 1 {
                    *member_count += 1;
                    let is_constructor = function_name(trimmed).map(|n| n == name.as_str()).unwrap_or(false);
                    if is_constructor {
                        if trimmed.contains("private") {
                            *has_private_constructor = true;
                        }
                    } else {
    *saw_any_method = true;
                        if !trimmed.contains("static") {
                            *all_methods_static = false;
                        }
                    }
                }
            }
            if let Some(OpenSpan { kind: SpanKind::Interface { member_count }, start_depth, .. }) = stack.last_mut() {
                if depth == *start_depth + 1 {
                    *member_count += 1;
                }
            }
            stack.push(OpenSpan {
                start_line: line_no,
                start_depth: depth,
                kind: SpanKind::Function { name_hint: trimmed.to_string(), param_count: count_params(param_list), max_nesting: 0, branch_count: 0, return_count: 0, cognitive_score: 0 },
            });
            depth += 1;
            continue;
        }

        if language == ComplexityLanguage::JavaOrKotlin && opens_class(trimmed) {
            stack.push(OpenSpan {
                start_line: line_no,
                start_depth: depth,
                kind: SpanKind::Class {
                    member_count: 0,
                    trivial_accessor_count: 0,
                    is_java: path.ends_with(".java"),
                    name: class_name(trimmed).unwrap_or_default().to_string(),
                    saw_any_method: false,
                    all_methods_static: true,
                    has_private_constructor: false,
                },
            });
            depth += 1;
            continue;
        }

        if language == ComplexityLanguage::JavaOrKotlin && opens_interface(trimmed) {
            stack.push(OpenSpan { start_line: line_no, start_depth: depth, kind: SpanKind::Interface { member_count: 0 } });
            depth += 1;
            continue;
        }

        if trimmed == "return" || trimmed.starts_with("return ") || trimmed.starts_with("return;") || trimmed.starts_with("return(") {
            // A `return` inside a `switch` sits under a Switch span, not
            // directly under its enclosing Function span (Switch spans are
            // pushed onto the stack same as Function/Class) — walk up to
            // the nearest Function span rather than assuming it's on top.
            if let Some(OpenSpan { kind: SpanKind::Function { return_count, .. }, .. }) = stack.iter_mut().rev().find(|s| matches!(s.kind, SpanKind::Function { .. })) {
                *return_count += 1;
            }
        }

        if trimmed.ends_with('{') {
            if opens_control_block(trimmed) {
                if let Some(OpenSpan { kind: SpanKind::Function { max_nesting, branch_count, cognitive_score, .. }, start_depth, .. }) = stack.last_mut() {
                    let relative = (depth - *start_depth + 1).max(0) as usize;
                    *max_nesting = (*max_nesting).max(relative);
                    *branch_count += 1;
                    // Cognitive Complexity (Sonar's metric): each nested
                    // control structure costs 1 base increment plus its
                    // nesting depth, so deeply nested branches compound
                    // instead of adding linearly like cyclomatic complexity.
                    *cognitive_score += relative;
                }
                if trimmed.split_whitespace().next() == Some("switch") {
                    stack.push(OpenSpan { start_line: line_no, start_depth: depth, kind: SpanKind::Switch { case_count: 0 } });
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
                    SpanKind::Function { name_hint, param_count, max_nesting, branch_count, return_count, cognitive_score } => {
                        let end_line = line_no as u32;
                        let line_count = end_line - span.start_line as u32;
                        let cyclomatic_complexity = branch_count + 1;
                        if cyclomatic_complexity > DEFAULT_MAX_CYCLOMATIC_COMPLEXITY {
                            findings.push(make_finding(
                                "cyclomatic-complexity",
                                path,
                                span.start_line as u32,
                                end_line,
                                format!("High cyclomatic complexity ({cyclomatic_complexity})"),
                                format!("This function/method has an estimated cyclomatic complexity of {cyclomatic_complexity} (over the {DEFAULT_MAX_CYCLOMATIC_COMPLEXITY}-threshold) — {branch_count} branch point(s) plus the base path. Consider extracting some of its branches into named helper functions, or replacing a long if/else-if chain with a lookup table or polymorphism."),
                            ));
                        }
                        if cognitive_score > DEFAULT_MAX_COGNITIVE_COMPLEXITY {
                            findings.push(make_finding(
                                "cognitive-complexity",
                                path,
                                span.start_line as u32,
                                end_line,
                                format!("High cognitive complexity ({cognitive_score})"),
                                format!("This function/method has an estimated cognitive complexity of {cognitive_score} (over the {DEFAULT_MAX_COGNITIVE_COMPLEXITY}-threshold, detekt's CognitiveComplexMethod) — unlike cyclomatic complexity, nested control structures compound this score instead of adding linearly, so it tracks how hard the function is to hold in your head better than branch count alone. Consider flattening nesting with early returns/guard clauses, or extracting inner blocks into named helpers."),
                            ));
                        }
                        if return_count > DEFAULT_MAX_RETURNS {
                            findings.push(make_finding(
                                "too-many-returns",
                                path,
                                span.start_line as u32,
                                end_line,
                                format!("Too many return points ({return_count})"),
                                format!("This function/method has {return_count} separate `return` statements (over the {DEFAULT_MAX_RETURNS}-threshold) — that many exit points makes it hard to reason about every path through the function. Consider consolidating logic or extracting some branches into their own functions."),
                            ));
                        }
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
                        // Trivial-accessor detection for the enclosing
                        // class's Data Class check: a short (<=3 line)
                        // getX/setX/isX method. Checked here (at the
                        // method's own close, once its line count is known)
                        // rather than at push time.
                        if let Some(OpenSpan { kind: SpanKind::Class { trivial_accessor_count, is_java: true, .. }, start_depth, .. }) = stack.last_mut() {
                            if depth == *start_depth + 1 && line_count as usize <= 3 {
                                let is_accessor_name = function_name(&name_hint)
                                    .map(|n| n.starts_with("get") || n.starts_with("set") || n.starts_with("is"))
                                    .unwrap_or(false);
                                if is_accessor_name {
                                    *trivial_accessor_count += 1;
                                }
                            }
                        }
                    }
                    SpanKind::Class { member_count, trivial_accessor_count, is_java, saw_any_method, all_methods_static, has_private_constructor, .. } => {
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
                        if is_java && member_count >= MIN_DATA_CLASS_ACCESSORS && trivial_accessor_count == member_count {
                            findings.push(make_finding(
                                "data-class",
                                path,
                                span.start_line as u32,
                                line_no as u32,
                                format!("Possible Data Class ({member_count} trivial accessors, no other behavior)"),
                                format!("Every method in this class ({member_count} of them) is a short get/set/is accessor — the class holds data but no behavior (Fowler's Data Class smell). Heuristic — a DTO/value holder is often intentional, so confirm this actually needs behavior before restructuring; if it's genuinely just data, a Java `record` may be a better fit than manual accessors."),
                            ));
                        }
                        if is_java && saw_any_method && all_methods_static && !has_private_constructor && member_count >= MIN_UTILITY_CLASS_STATIC_METHODS {
                            findings.push(make_finding(
                                "utility-class-public-constructor",
                                path,
                                span.start_line as u32,
                                line_no as u32,
                                "Utility class with a public constructor".to_string(),
                                "Every method in this class is static and there's no private constructor — this class can be misleadingly instantiated even though instances have no purpose (Checkstyle's HideUtilityClassConstructor / detekt's UtilityClassWithPublicConstructor). Add a private no-arg constructor, or make the class `final` with all-static access if it's meant purely as a namespace.".to_string(),
                            ));
                        }
                    }
                    SpanKind::Interface { member_count } => {
                        const MAX_INTERFACE_MEMBERS: usize = 10;
                        if member_count > MAX_INTERFACE_MEMBERS {
                            findings.push(make_finding(
                                "complex-interface",
                                path,
                                span.start_line as u32,
                                line_no as u32,
                                format!("Complex interface ({member_count} members)"),
                                format!("This interface declares {member_count} members (over the {MAX_INTERFACE_MEMBERS}-member threshold, detekt's ComplexInterface) — an interface this large is likely handling more than one responsibility. Consider splitting it into smaller, more focused interfaces."),
                            ));
                        }
                    }
                    SpanKind::Switch { case_count } => {
                        if case_count > DEFAULT_MAX_SWITCH_CASES {
                            findings.push(make_finding(
                                "large-switch",
                                path,
                                span.start_line as u32,
                                line_no as u32,
                                format!("Large switch statement ({case_count} cases)"),
                                format!("This switch statement has {case_count} cases (over the {DEFAULT_MAX_SWITCH_CASES}-case threshold, Fowler's Switch Statements smell) — if the cases dispatch on an object's type/kind, consider polymorphism (a method per type) or a lookup table instead."),
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
    fn flags_high_cyclomatic_complexity_in_go() {
        let mut content = String::from("func doIt(x int) {\n");
        for i in 0..11 {
            content.push_str(&format!("\tif x == {i} {{\n\t\tprintln({i})\n\t}}\n"));
        }
        content.push_str("}\n");
        let findings = detect_complexity_in_file("main.go", &content, ComplexityLanguage::Go);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("cyclomatic-complexity")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_low_cyclomatic_complexity_in_go() {
        let content = "func doIt(x int) {\n\tif x > 0 {\n\t\tprintln(x)\n\t}\n}\n";
        let findings = detect_complexity_in_file("main.go", content, ComplexityLanguage::Go);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("cyclomatic-complexity")), "got: {findings:#?}");
    }

    #[test]
    fn flags_high_cognitive_complexity_in_go() {
        let mut content = String::from("func doIt(x int) {\n");
        for i in 0..6 {
            content.push_str(&"\t".repeat(i + 1));
            content.push_str(&format!("if x == {i} {{\n"));
        }
        for i in (0..6).rev() {
            content.push_str(&"\t".repeat(i + 1));
            content.push_str("}\n");
        }
        content.push_str("}\n");
        let findings = detect_complexity_in_file("main.go", &content, ComplexityLanguage::Go);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("cognitive-complexity")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_low_cognitive_complexity_in_go() {
        let content = "func doIt(x int) {\n\tif x > 0 {\n\t\tprintln(x)\n\t}\n}\n";
        let findings = detect_complexity_in_file("main.go", content, ComplexityLanguage::Go);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("cognitive-complexity")), "got: {findings:#?}");
    }

    #[test]
    fn flags_too_many_returns_in_go() {
        let mut content = String::from("func doIt(x int) int {\n");
        for i in 0..5 {
            content.push_str(&format!("\tif x == {i} {{\n\t\treturn {i}\n\t}}\n"));
        }
        content.push_str("\treturn -1\n}\n");
        let findings = detect_complexity_in_file("main.go", &content, ComplexityLanguage::Go);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("too-many-returns")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_few_returns_in_go() {
        let content = "func doIt(x int) int {\n\tif x > 0 {\n\t\treturn 1\n\t}\n\treturn 0\n}\n";
        let findings = detect_complexity_in_file("main.go", content, ComplexityLanguage::Go);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("too-many-returns")), "got: {findings:#?}");
    }

    #[test]
    fn counts_returns_inside_a_switch_toward_the_enclosing_function_not_the_switch_span() {
        let mut content = String::from("func doIt(x int) int {\n\tswitch x {\n");
        for i in 0..5 {
            content.push_str(&format!("\tcase {i}:\n\t\treturn {i}\n"));
        }
        content.push_str("\t}\n\treturn -1\n}\n");
        let findings = detect_complexity_in_file("main.go", &content, ComplexityLanguage::Go);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("too-many-returns")), "got: {findings:#?}");
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
    fn flags_a_complex_interface_in_java() {
        let mut content = String::from("public interface Big {\n");
        for i in 0..11 {
            content.push_str(&format!("    void method{i}();\n"));
        }
        content.push_str("}\n");
        let findings = detect_complexity_in_file("Big.java", &content, ComplexityLanguage::JavaOrKotlin);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("complex-interface")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_small_interface_in_java() {
        let content = "public interface Small {\n    void method1();\n    void method2();\n}\n";
        let findings = detect_complexity_in_file("Small.java", content, ComplexityLanguage::JavaOrKotlin);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("complex-interface")), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_large_switch_statement_in_go() {
        let mut content = String::from("func doIt(x int) {\n\tswitch x {\n");
        for i in 0..10 {
            content.push_str(&format!("\tcase {i}:\n\t\tprintln({i})\n"));
        }
        content.push_str("\t}\n}\n");
        let findings = detect_complexity_in_file("main.go", &content, ComplexityLanguage::Go);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("large-switch")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_small_switch_statement_in_go() {
        let content = "func doIt(x int) {\n\tswitch x {\n\tcase 1:\n\t\tprintln(1)\n\tcase 2:\n\t\tprintln(2)\n\tdefault:\n\t\tprintln(0)\n\t}\n}\n";
        let findings = detect_complexity_in_file("main.go", content, ComplexityLanguage::Go);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("large-switch")), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_large_switch_statement_in_java() {
        let mut content = String::from("public class Big {\n    void doIt(int x) {\n        switch (x) {\n");
        for i in 0..10 {
            content.push_str(&format!("        case {i}:\n            System.out.println({i});\n            break;\n"));
        }
        content.push_str("        }\n    }\n}\n");
        let findings = detect_complexity_in_file("Big.java", &content, ComplexityLanguage::JavaOrKotlin);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("large-switch")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_kotlin_when_expressions_as_a_switch() {
        // Deliberate scope decision: Kotlin's `when` arms use a bare `->`
        // with no keyword, indistinguishable from a lambda by this line
        // scan — see the SpanKind::Switch doc comment.
        let mut content = String::from("fun doIt(x: Int) {\n    when (x) {\n");
        for i in 0..10 {
            content.push_str(&format!("        {i} -> println({i})\n"));
        }
        content.push_str("    }\n}\n");
        let findings = detect_complexity_in_file("Big.kt", &content, ComplexityLanguage::JavaOrKotlin);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("large-switch")), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_utility_class_with_a_public_constructor() {
        let content = "public class StringUtils {\n    public static String reverse(String s) {\n        return s;\n    }\n    public static boolean isBlank(String s) {\n        return s.isEmpty();\n    }\n}\n";
        let findings = detect_complexity_in_file("StringUtils.java", content, ComplexityLanguage::JavaOrKotlin);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("utility-class-public-constructor")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_utility_class_with_a_private_constructor() {
        let content = "public class StringUtils {\n    private StringUtils() {\n    }\n    public static String reverse(String s) {\n        return s;\n    }\n    public static boolean isBlank(String s) {\n        return s.isEmpty();\n    }\n}\n";
        let findings = detect_complexity_in_file("StringUtils.java", content, ComplexityLanguage::JavaOrKotlin);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("utility-class-public-constructor")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_class_with_a_mix_of_static_and_instance_methods() {
        let content = "public class Widget {\n    public static Widget create() {\n        return new Widget();\n    }\n    public int size() {\n        return 1;\n    }\n}\n";
        let findings = detect_complexity_in_file("Widget.java", content, ComplexityLanguage::JavaOrKotlin);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("utility-class-public-constructor")), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_data_class_in_java() {
        let content = "public class Point {\n    private int x;\n    private int y;\n    private String label;\n\n    public int getX() {\n        return x;\n    }\n    public void setX(int x) {\n        this.x = x;\n    }\n    public int getY() {\n        return y;\n    }\n    public void setY(int y) {\n        this.y = y;\n    }\n    public String getLabel() {\n        return label;\n    }\n}\n";
        let findings = detect_complexity_in_file("Point.java", content, ComplexityLanguage::JavaOrKotlin);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("data-class")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_class_with_real_behavior_as_a_data_class() {
        let content = "public class Account {\n    private int balance;\n\n    public int getBalance() {\n        return balance;\n    }\n    public void deposit(int amount) {\n        balance += amount;\n        notifyListeners();\n        log(\"deposit\");\n    }\n    public void withdraw(int amount) {\n        balance -= amount;\n    }\n}\n";
        let findings = detect_complexity_in_file("Account.java", content, ComplexityLanguage::JavaOrKotlin);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("data-class")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_kotlin_data_class_as_the_data_class_smell() {
        // Kotlin's `data class` is an intentional, blessed language
        // feature, not the JavaBean-era smell — this rule is Java-only.
        let content = "class Point {\n    fun getX(): Int {\n        return x\n    }\n    fun getY(): Int {\n        return y\n    }\n    fun getZ(): Int {\n        return z\n    }\n}\n";
        let findings = detect_complexity_in_file("Point.kt", content, ComplexityLanguage::JavaOrKotlin);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("data-class")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_small_number_of_accessors_as_a_data_class() {
        let content = "public class Pair {\n    private int a;\n    private int b;\n\n    public int getA() {\n        return a;\n    }\n    public int getB() {\n        return b;\n    }\n}\n";
        let findings = detect_complexity_in_file("Pair.java", content, ComplexityLanguage::JavaOrKotlin);
        assert!(!findings.iter().any(|f| f.source.rule_id.as_deref() == Some("data-class")), "got: {findings:#?}");
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

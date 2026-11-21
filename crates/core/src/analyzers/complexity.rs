//! Threshold checks use syntax-tree ranges for Java and Rust and line-based metrics elsewhere.

mod java;
mod lexical;
mod limits;
mod rust;

use std::collections::HashMap;

use autoreview_schema::{AgentFinding, FindingSource, FindingSourceKind, Location, LocationRange, Severity, Side};

pub use limits::ComplexityThresholds;
pub(crate) use lexical::{function_name, opens_function};
use lexical::{count_params, is_abstract_member_signature, opens_class, opens_control_block, opens_interface};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComplexityLanguage {
    Go,
    JavaOrKotlin,
    JavaScriptOrTypeScript,
    Rust,
}

pub fn language_for_file(path: &str) -> Option<ComplexityLanguage> {
    match std::path::Path::new(path).extension().and_then(|e| e.to_str()) {
        Some("go") => Some(ComplexityLanguage::Go),
        Some("java") | Some("kt") | Some("kts") => Some(ComplexityLanguage::JavaOrKotlin),
        Some("ts") | Some("tsx") | Some("mts") | Some("cts") | Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => Some(ComplexityLanguage::JavaScriptOrTypeScript),
        Some("rs") => Some(ComplexityLanguage::Rust),
        _ => None,
    }
}

fn java_has_modifier(node: tree_sitter::Node<'_>, modifier: &str) -> bool {
    let mut cursor = node.walk();
    let Some(modifiers) = node.named_children(&mut cursor).find(|child| child.kind() == "modifiers") else { return false };
    let mut cursor = modifiers.walk();
    let present = modifiers.children(&mut cursor).any(|child| child.kind() == modifier);
    present
}


struct OpenSpan {
    start_line: usize,
    start_depth: i32,
    kind: SpanKind,
}

enum SpanKind {
    Function { param_count: usize, max_nesting: usize, branch_count: usize, return_count: usize, cognitive_score: usize },
    Class { member_count: usize },
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

fn build_finding(rule_id: &str, path: &str, start_line: u32, end_line: u32, title: String, message: String, meta: Option<HashMap<String, serde_json::Value>>) -> AgentFinding {
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
        meta,
        suggested_patch: None,
    }
}

struct FunctionMetrics {
    start_line: u32,
    end_line: u32,
    line_count: usize,
    param_count: usize,
    max_nesting: usize,
    branch_count: usize,
    return_count: usize,
    cognitive_score: usize,
}

type MakeFinding<'a> = dyn Fn(&str, &str, u32, u32, String, String) -> AgentFinding + 'a;

fn append_function_findings(
    findings: &mut Vec<AgentFinding>, path: &str, metrics: FunctionMetrics,
    thresholds: &ComplexityThresholds, make_finding: &MakeFinding<'_>,
) {
    let FunctionMetrics { end_line, line_count, param_count, max_nesting, branch_count, return_count, cognitive_score, .. } = metrics;
    let cyclomatic_complexity = branch_count + 1;
    if cyclomatic_complexity > thresholds.cyclomatic_complexity {
        findings.push(make_finding(
            "cyclomatic-complexity",
            path,
            metrics.start_line,
            end_line,
            format!("High cyclomatic complexity ({cyclomatic_complexity})"),
            format!(
                "This function/method has an estimated cyclomatic complexity of {cyclomatic_complexity} (over the {}-threshold) — {branch_count} branch point(s) plus the base path. Consider extracting some of its branches into named helper functions, or replacing a long if/else-if chain with a lookup table or polymorphism.",
                thresholds.cyclomatic_complexity
            ),
        ));
    }
    if cognitive_score > thresholds.cognitive_complexity {
        findings.push(make_finding(
            "cognitive-complexity",
            path,
            metrics.start_line,
            end_line,
            format!("High cognitive complexity ({cognitive_score})"),
            format!("This function/method has an estimated cognitive complexity of {cognitive_score}, above the configured limit of {}. The score increases with the depth of nested control flow. Consider flattening deeply nested branches with guard clauses or extracting named helpers.", thresholds.cognitive_complexity),
        ));
    }
    if return_count > thresholds.too_many_returns {
        findings.push(make_finding(
            "too-many-returns",
            path,
            metrics.start_line,
            end_line,
            format!("Too many return points ({return_count})"),
            format!(
                "This function/method has {return_count} separate `return` statements (over the {}-threshold) — that many exit points makes it hard to reason about every path through the function. Consider consolidating logic or extracting some branches into their own functions.",
                thresholds.too_many_returns
            ),
        ));
    }
    if line_count > thresholds.long_method {
        findings.push(make_finding(
            "long-method",
            path,
            metrics.start_line,
            end_line,
            format!("Long method ({line_count} lines)"),
            format!("This function/method body is {line_count} lines long (over the {}-line threshold) — consider extracting smaller, named helper functions for its distinct steps.", thresholds.long_method),
        ));
    }
    if param_count > thresholds.long_parameter_list {
        findings.push(make_finding(
            "long-parameter-list",
            path,
            metrics.start_line,
            metrics.start_line,
            format!("Long parameter list ({param_count} parameters)"),
            format!("This function/method takes {param_count} parameters (over the {}-parameter threshold) — consider grouping related parameters into a struct/options object.", thresholds.long_parameter_list),
        ));
    }
    if max_nesting > thresholds.deep_nesting {
        findings.push(make_finding(
            "deep-nesting",
            path,
            metrics.start_line,
            end_line,
            format!("Deep nesting ({max_nesting} levels)"),
            format!("This function/method nests control-flow blocks {max_nesting} levels deep (over the {}-level threshold) — consider early returns/guard clauses or extracting the innermost blocks into their own functions.", thresholds.deep_nesting),
        ));
    }
}

/// Scans one file's content for long methods, long parameter lists, deep
/// nesting, and (Java/Kotlin only) god classes, using the default
/// thresholds. Thin wrapper over `detect_complexity_in_file_with_thresholds`
/// kept so the dozens of existing unit tests in this file (and any other
/// caller not concerned with YAML-configured thresholds) don't need to
/// pass one explicitly.
pub fn detect_complexity_in_file(path: &str, content: &str, language: ComplexityLanguage) -> Vec<AgentFinding> {
    detect_complexity_in_file_with_thresholds(path, content, language, &ComplexityThresholds::default(), &HashMap::new())
}

/// Scans one file's content for long methods, long parameter lists, deep
/// nesting, and (Java/Kotlin only) god classes. A single brace-depth pass:
/// opening a function or class pushes a tracked span; closing braces that
/// return to a span's start depth close it and evaluate its thresholds.
/// `pack_ids_by_metric` (metric name -> pack id) tags a finding's `meta`
/// with `rulePackId` when the threshold that fired came from a registered
/// pack rather than the builtin default.
pub fn detect_complexity_in_file_with_thresholds(path: &str, content: &str, language: ComplexityLanguage, thresholds: &ComplexityThresholds, pack_ids_by_metric: &HashMap<String, String>) -> Vec<AgentFinding> {
    let make_finding = |rule_id: &str, path: &str, start_line: u32, end_line: u32, title: String, message: String| -> AgentFinding {
        let meta = pack_ids_by_metric.get(rule_id).map(|pack_id| {
            let mut m = HashMap::new();
            m.insert("rulePackId".to_string(), serde_json::Value::String(pack_id.clone()));
            m
        });
        build_finding(rule_id, path, start_line, end_line, title, message, meta)
    };
    if language == ComplexityLanguage::JavaOrKotlin && path.ends_with(".java") {
        return java::findings(path, content, thresholds, &make_finding);
    }
    if language == ComplexityLanguage::Rust {
        return rust::findings(path, content, thresholds, &make_finding);
    }

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
            if let Some(OpenSpan { kind: SpanKind::Class { member_count, .. }, start_depth, .. }) = stack.last_mut() {
                // A class's own body content lives one level deeper than
                // its `start_depth` (recorded *before* its opening brace
                // incremented `depth`) — so a direct member sits at
                // `start_depth + 1`, not `start_depth` itself.
                if depth == *start_depth + 1 {
                    *member_count += 1;
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
                kind: SpanKind::Function { param_count: count_params(param_list), max_nesting: 0, branch_count: 0, return_count: 0, cognitive_score: 0 },
            });
            depth += 1;
            continue;
        }

        if matches!(language, ComplexityLanguage::JavaOrKotlin | ComplexityLanguage::JavaScriptOrTypeScript) && opens_class(trimmed) {
            stack.push(OpenSpan {
                start_line: line_no,
                start_depth: depth,
                kind: SpanKind::Class { member_count: 0 },
            });
            depth += 1;
            continue;
        }

        if matches!(language, ComplexityLanguage::JavaOrKotlin | ComplexityLanguage::JavaScriptOrTypeScript) && opens_interface(trimmed) {
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

        // Broader than `== "}"`/`starts_with("} ")` on purpose: JS/TS
        // routinely closes a callback-as-last-argument as `});`/`},` (e.g.
        // `items.map(x => {...});`), and by this point in the loop any
        // line that *also* opens a new block (e.g. `} else {`) has already
        // matched the `ends_with('{')` branch above and `continue`d — so
        // anything reaching here starting with `}` is a pure close.
        if trimmed.starts_with('}') {
            depth -= 1;
            while let Some(span) = stack.last() {
                if depth != span.start_depth {
                    break;
                }
                let span = stack.pop().unwrap();
                match span.kind {
                    SpanKind::Function { param_count, max_nesting, branch_count, return_count, cognitive_score } => {
                        let end_line = line_no as u32;
                        let line_count = end_line - span.start_line as u32;
                        append_function_findings(&mut findings, path, FunctionMetrics {
                            start_line: span.start_line as u32, end_line, line_count: line_count as usize,
                            param_count, max_nesting, branch_count, return_count, cognitive_score,
                        }, thresholds, &make_finding);
                    }
                    SpanKind::Class { member_count } => {
                        if member_count > thresholds.god_class {
                            findings.push(make_finding(
                                "god-class",
                                path,
                                span.start_line as u32,
                                line_no as u32,
                                format!("Large class ({member_count} methods)"),
                                format!("This class defines {member_count} methods (over the {}-method threshold) — a class this large is often doing more than one job; consider splitting it by responsibility.", thresholds.god_class),
                            ));
                        }
                    }
                    SpanKind::Interface { member_count } => {
                        if member_count > thresholds.complex_interface {
                            findings.push(make_finding(
                                "complex-interface",
                                path,
                                span.start_line as u32,
                                line_no as u32,
                                format!("Complex interface ({member_count} members)"),
                                format!("This interface declares {member_count} members (over the {}-member threshold, detekt's ComplexInterface) — an interface this large is likely handling more than one responsibility. Consider splitting it into smaller, more focused interfaces.", thresholds.complex_interface),
                            ));
                        }
                    }
                    SpanKind::Switch { case_count } => {
                        if case_count > thresholds.large_switch {
                            findings.push(make_finding(
                                "large-switch",
                                path,
                                span.start_line as u32,
                                line_no as u32,
                                format!("Large switch statement ({case_count} cases)"),
                                format!("This switch statement has {case_count} cases (over the {}-case threshold, Fowler's Switch Statements smell) — if the cases dispatch on an object's type/kind, consider polymorphism (a method per type) or a lookup table instead.", thresholds.large_switch),
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

pub fn run_complexity_check(repo_root: &std::path::Path, changed_files: &[String], registered_packs: &[crate::rule_packs::ResolvedRulePack]) -> Vec<AgentFinding> {
    let (thresholds, pack_ids_by_metric) = super::threshold_rules::resolve_complexity_thresholds_with_provenance(registered_packs);
    changed_files
        .iter()
        .filter_map(|path| {
            let language = language_for_file(path)?;
            let full_path = repo_root.join(path);
            let content = std::fs::read_to_string(&full_path).ok()?;
            Some(detect_complexity_in_file_with_thresholds(path, &content, language, &thresholds, &pack_ids_by_metric))
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
    fn a_yaml_configured_threshold_actually_changes_whether_the_rule_fires() {
        // 5 branches, cyclomatic complexity 6 — over a threshold of 5,
        // under a threshold of 20. Proves ComplexityThresholds is read,
        // not just parsed and ignored.
        let mut content = String::from("func doIt(x int) {\n");
        for i in 0..5 {
            content.push_str(&format!("\tif x == {i} {{\n\t\tprintln({i})\n\t}}\n"));
        }
        content.push_str("}\n");

        let strict = ComplexityThresholds { cyclomatic_complexity: 5, ..ComplexityThresholds::default() };
        let lenient = ComplexityThresholds { cyclomatic_complexity: 20, ..ComplexityThresholds::default() };

        let strict_findings = detect_complexity_in_file_with_thresholds("main.go", &content, ComplexityLanguage::Go, &strict, &HashMap::new());
        let lenient_findings = detect_complexity_in_file_with_thresholds("main.go", &content, ComplexityLanguage::Go, &lenient, &HashMap::new());

        assert!(strict_findings.iter().any(|f| f.source.rule_id.as_deref() == Some("cyclomatic-complexity")), "got: {strict_findings:#?}");
        assert!(!lenient_findings.iter().any(|f| f.source.rule_id.as_deref() == Some("cyclomatic-complexity")), "got: {lenient_findings:#?}");
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
    fn language_for_file_recognizes_typescript_and_javascript_extensions() {
        for ext in ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"] {
            assert_eq!(language_for_file(&format!("main.{ext}")), Some(ComplexityLanguage::JavaScriptOrTypeScript), "extension: {ext}");
        }
    }

    #[test]
    fn flags_a_long_typescript_function() {
        let mut body = String::from("function doIt() {\n");
        for i in 0..90 {
            body.push_str(&format!("\tconst x{i} = {i};\n"));
        }
        body.push_str("}\n");
        let findings = detect_complexity_in_file("main.ts", &body, ComplexityLanguage::JavaScriptOrTypeScript);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("long-method")), "expected a long-method finding, got: {findings:#?}");
    }

    #[test]
    fn flags_high_cyclomatic_complexity_in_an_unparenthesized_arrow_function() {
        // Regression test: `x => { ... }` (no parens around the single
        // param) has no `(`/`)` on the line at all, which the paren-based
        // half of `opens_function` can't see — without the dedicated arrow
        // fallback, this callback's own branches would silently count
        // toward whatever span happens to be on the stack instead of its
        // own function, and none of this file's checks would ever fire on
        // it directly.
        let mut content = String::from("items.map(x => {\n");
        for i in 0..11 {
            content.push_str(&format!("\tif (x === {i}) {{\n\t\tconsole.log({i});\n\t}}\n"));
        }
        content.push_str("\treturn x;\n});\n");
        let findings = detect_complexity_in_file("main.ts", &content, ComplexityLanguage::JavaScriptOrTypeScript);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("cyclomatic-complexity")), "got: {findings:#?}");
    }

    #[test]
    fn does_not_flag_a_short_typescript_function() {
        let content = "function doIt(): void {\n\tconst x = 1;\n\tconsole.log(x);\n}\n";
        let findings = detect_complexity_in_file("main.ts", content, ComplexityLanguage::JavaScriptOrTypeScript);
        assert!(findings.is_empty(), "got: {findings:#?}");
    }

    #[test]
    fn flags_a_god_class_in_typescript() {
        let mut content = String::from("class Big {\n");
        for i in 0..25 {
            content.push_str(&format!("    method{i}() {{\n        const x = {i};\n    }}\n"));
        }
        content.push_str("}\n");
        let findings = detect_complexity_in_file("Big.ts", &content, ComplexityLanguage::JavaScriptOrTypeScript);
        assert!(findings.iter().any(|f| f.source.rule_id.as_deref() == Some("god-class")), "got: {findings:#?}");
    }

    #[test]
    fn run_complexity_check_skips_files_it_has_no_language_for() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "# hi\n").unwrap();
        let findings = run_complexity_check(dir.path(), &["README.md".to_string()], &[]);
        assert!(findings.is_empty());
    }

    #[test]
    fn run_complexity_check_skips_files_that_no_longer_exist() {
        let dir = tempfile::tempdir().unwrap();
        let findings = run_complexity_check(dir.path(), &["deleted.go".to_string()], &[]);
        assert!(findings.is_empty());
    }

    #[test]
    fn run_complexity_check_finds_issues_in_a_real_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.go"), "func doIt(a int, b int, c int, d int, e int, f int) {\n\t_ = a\n}\n").unwrap();
        let findings = run_complexity_check(dir.path(), &["main.go".to_string()], &[]);
        assert!(!findings.is_empty());
    }

    #[test]
    fn a_pack_sourced_threshold_finding_carries_rule_pack_id_in_meta() {
        let dir = tempfile::tempdir().unwrap();
        let mut content = String::from("func doIt(x int) {\n");
        for i in 0..5 {
            content.push_str(&format!("\tif x == {i} {{\n\t\tprintln({i})\n\t}}\n"));
        }
        content.push_str("}\n");
        std::fs::write(dir.path().join("main.go"), &content).unwrap();

        let pack_dir = dir.path().join("pack");
        std::fs::create_dir_all(&pack_dir).unwrap();
        std::fs::write(pack_dir.join("rulepack.yaml"), "id: acme-thresholds\nversion: \"1.0.0\"\n").unwrap();
        std::fs::write(
            pack_dir.join("tight.yml"),
            "id: acme-tight-cyclomatic-complexity\nkind: threshold\nlanguage: Go\ncategory: correctness\nseverity: warning\nmetric: cyclomatic-complexity\nthreshold: 2\nmessage: m\n",
        )
        .unwrap();
        let packs = vec![crate::rule_packs::ResolvedRulePack { id: "acme-thresholds".to_string(), local_path: pack_dir, trust: autoreview_schema::RulePackTrust::Full }];

        let findings = run_complexity_check(dir.path(), &["main.go".to_string()], &packs);
        let finding = findings.iter().find(|f| f.source.rule_id.as_deref() == Some("cyclomatic-complexity")).unwrap_or_else(|| panic!("got: {findings:#?}"));
        let meta = finding.meta.as_ref().expect("expected meta to carry rulePackId");
        assert_eq!(meta.get("rulePackId").and_then(|v| v.as_str()), Some("acme-thresholds"));
    }

    #[test]
    fn a_builtin_threshold_finding_has_no_meta() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.go"), "func doIt(a int, b int, c int, d int, e int, f int) {\n\t_ = a\n}\n").unwrap();
        let findings = run_complexity_check(dir.path(), &["main.go".to_string()], &[]);
        let finding = findings.iter().find(|f| f.source.rule_id.as_deref() == Some("long-parameter-list")).unwrap_or_else(|| panic!("got: {findings:#?}"));
        assert!(finding.meta.is_none());
    }
}

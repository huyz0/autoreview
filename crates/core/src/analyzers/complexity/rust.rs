use autoreview_langsupport::{parser_for, Language};
use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::{append_function_findings, ComplexityThresholds, FunctionMetrics, MakeFinding};

pub(super) fn findings(
    path: &str,
    content: &str,
    thresholds: &ComplexityThresholds,
    make_finding: &MakeFinding<'_>,
) -> Vec<AgentFinding> {
    let Some(tree) = parser_for(Language::Rust).and_then(|mut parser| parser.parse(content, None))
    else {
        return Vec::new();
    };

    let mut findings = Vec::new();
    let mut nodes = vec![tree.root_node()];
    while let Some(node) = nodes.pop() {
        let mut cursor = node.walk();
        let start = nodes.len();
        nodes.extend(node.named_children(&mut cursor));
        nodes[start..].reverse();
        match node.kind() {
            "function_item" if !node.has_error() => {
                if let Some(body) = node.child_by_field_name("body") {
                    append_function_findings(
                        &mut findings,
                        path,
                        measure_function(node, body),
                        thresholds,
                        make_finding,
                    );
                }
            }
            "trait_item" if !node.has_error() => {
                append_trait_finding(
                    &mut findings,
                    path,
                    node,
                    thresholds.complex_interface,
                    make_finding,
                );
            }
            _ => {}
        }
    }
    findings
}

fn append_trait_finding(
    findings: &mut Vec<AgentFinding>,
    path: &str,
    trait_item: Node<'_>,
    limit: usize,
    make_finding: &MakeFinding<'_>,
) {
    let Some(body) = trait_item.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    let count = body
        .named_children(&mut cursor)
        .filter(|member| match member.kind() {
            "function_signature_item" | "associated_type" => true,
            "const_item" => member.child_by_field_name("value").is_none(),
            _ => false,
        })
        .count();
    if count > limit {
        findings.push(make_finding(
            "complex-interface",
            path,
            (trait_item.start_position().row + 1) as u32,
            (trait_item.end_position().row + 1) as u32,
            format!("Complex trait ({count} required members)"),
            format!("This trait declares {count} required members, above the configured limit of {limit}."),
        ));
    }
}

fn parameter_count(function: Node<'_>) -> usize {
    function
        .child_by_field_name("parameters")
        .map_or(0, |parameters| {
            let mut cursor = parameters.walk();
            parameters
                .named_children(&mut cursor)
                .filter(|parameter| {
                    parameter.kind() == "parameter"
                        && parameter
                            .child_by_field_name("pattern")
                            .is_none_or(|pattern| pattern.kind() != "self")
                })
                .count()
        })
}

fn branch_points(node: Node<'_>, let_else: bool) -> usize {
    let mut cursor = node.walk();
    match node.kind() {
        "if_expression" | "for_expression" | "while_expression" => 1,
        "let_declaration" => usize::from(let_else),
        "match_block" => node
            .named_children(&mut cursor)
            .filter(|arm| arm.kind() == "match_arm")
            .count()
            .saturating_sub(1),
        "match_pattern" => usize::from(node.child_by_field_name("condition").is_some()),
        "binary_expression" => usize::from(
            node.child_by_field_name("operator")
                .is_some_and(|op| matches!(op.kind(), "&&" | "||")),
        ),
        "let_chain" => node
            .children(&mut cursor)
            .filter(|token| token.kind() == "&&")
            .count(),
        _ => 0,
    }
}

fn measure_function(function: Node<'_>, body: Node<'_>) -> FunctionMetrics {
    let mut metrics = FunctionMetrics {
        start_line: (function.start_position().row + 1) as u32,
        end_line: (function.end_position().row + 1) as u32,
        line_count: body.end_position().row - body.start_position().row + 1,
        param_count: parameter_count(function),
        max_nesting: 0,
        branch_count: 0,
        return_count: 0,
        cognitive_score: 0,
    };
    let mut nodes = vec![(body, 0)];
    while let Some((node, depth)) = nodes.pop() {
        let kind = node.kind();
        if matches!(
            kind,
            "function_item"
                | "closure_expression"
                | "async_block"
                | "gen_block"
                | "const_block"
                | "const_item"
                | "static_item"
        ) {
            continue;
        }

        let alternative = node.child_by_field_name("alternative");
        let let_else = kind == "let_declaration" && alternative.is_some();
        let control = matches!(
            kind,
            "if_expression"
                | "for_expression"
                | "while_expression"
                | "loop_expression"
                | "match_expression"
        ) || let_else;
        if control {
            metrics.cognitive_score += depth + 1;
            metrics.max_nesting = metrics.max_nesting.max(depth + 1);
        }

        metrics.branch_count += branch_points(node, let_else);
        metrics.return_count += usize::from(kind == "return_expression");

        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            // An else-if belongs to the same decision level as its if.
            let else_if = kind == "if_expression"
                && alternative == Some(child)
                && child
                    .named_children(&mut child.walk())
                    .any(|branch| branch.kind() == "if_expression");
            let nested = control && !else_if && (!let_else || alternative == Some(child));
            nodes.push((child, depth + usize::from(nested)));
        }
    }
    metrics
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_named_rust_functions_without_counting_nested_execution_bodies() {
        let source = r#"
impl Worker {
    fn run(self: Box<Self>, value: Option<i32>, flag: bool) -> Option<i32> {
        fn helper(flag: bool) { if flag { return; } }
        const _: u8 = if true { 1 } else { 2 };
        let _closure = || { if flag { return 1; } 0 };
        let _future = async { if flag { return 1; } 0 };
        let Some(value) = value else { return None; };
        if value > 0 && flag {
            return Some(value);
        } else /* same depth */ if value < 0 {
            return None;
        }
        match value {
            0 | 1 if flag => Some(0),
            2 => Some(2),
            _ => { Some(value.checked_add(1)?) }
        }
    }
}
"#;
        let reports = super::super::detect_complexity_in_file_with_thresholds(
            "worker.rs",
            source,
            super::super::ComplexityLanguage::Rust,
            &ComplexityThresholds {
                cyclomatic_complexity: 1,
                long_parameter_list: 1,
                deep_nesting: 1,
                cognitive_complexity: 0,
                too_many_returns: 0,
                ..Default::default()
            },
            &std::collections::HashMap::new(),
        );
        let titles: Vec<_> = reports.iter().map(|report| report.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "High cyclomatic complexity (8)",
                "High cognitive complexity (4)",
                "Too many return points (3)",
                "Long parameter list (2 parameters)",
                "High cyclomatic complexity (2)",
                "High cognitive complexity (1)",
                "Too many return points (1)",
            ]
        );
    }

    #[test]
    fn flags_rust_traits_above_the_configured_limit_for_required_items() {
        let source = r#"pub trait Worker {
    fn run(&self);
    type Record<'a> where Self: 'a;
    const BATCH_SIZE: usize;
    fn is_ready(&self) -> bool { true }
    const DEFAULT_RETRIES: usize = 3;
}
"#;
        let reports = super::super::detect_complexity_in_file_with_thresholds(
            "worker.rs",
            source,
            super::super::ComplexityLanguage::Rust,
            &ComplexityThresholds {
                complex_interface: 2,
                ..Default::default()
            },
            &std::collections::HashMap::new(),
        );
        assert_eq!(reports.len(), 1);
        let report = &reports[0];
        assert_eq!(report.source.rule_id.as_deref(), Some("complex-interface"));
        assert_eq!(report.title, "Complex trait (3 required members)");
        assert_eq!(report.category, "design");
        assert_eq!(report.location.range.start_line, 1);
        assert_eq!(report.location.range.end_line, Some(7));
    }

    #[test]
    fn skips_default_helpers_and_traits_at_the_configured_limit() {
        let source = r#"
#![feature(associated_type_defaults)]

trait Parent {
    fn inherited(&self);
}

trait Worker: Parent {
    fn run(&self);
    fn first_helper(&self) {}
    fn second_helper(&self) {}
    type RetryState = ();
    const DEFAULT_RETRIES: usize = 3;
}
"#;
        let reports = super::super::detect_complexity_in_file_with_thresholds(
            "worker.rs",
            source,
            super::super::ComplexityLanguage::Rust,
            &ComplexityThresholds {
                complex_interface: 1,
                ..Default::default()
            },
            &std::collections::HashMap::new(),
        );
        assert!(reports.is_empty(), "got {reports:#?}");
    }
}

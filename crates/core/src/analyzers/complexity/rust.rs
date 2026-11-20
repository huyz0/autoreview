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
        if node.kind() == "function_item" && !node.has_error() {
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
    }
    findings
}

fn measure_function(function: Node<'_>, body: Node<'_>) -> FunctionMetrics {
    let param_count = function
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
        });
    let mut metrics = FunctionMetrics {
        start_line: (function.start_position().row + 1) as u32,
        end_line: (function.end_position().row + 1) as u32,
        line_count: body.end_position().row - body.start_position().row + 1,
        param_count,
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

        let mut cursor = node.walk();
        match kind {
            "if_expression" | "for_expression" | "while_expression" => metrics.branch_count += 1,
            "let_declaration" if let_else => metrics.branch_count += 1,
            "match_block" => {
                let arms = node
                    .named_children(&mut cursor)
                    .filter(|arm| arm.kind() == "match_arm")
                    .count();
                metrics.branch_count += arms.saturating_sub(1);
            }
            "match_pattern" if node.child_by_field_name("condition").is_some() => {
                metrics.branch_count += 1
            }
            "binary_expression"
                if node
                    .child_by_field_name("operator")
                    .is_some_and(|op| matches!(op.kind(), "&&" | "||")) =>
            {
                metrics.branch_count += 1;
            }
            "let_chain" => {
                metrics.branch_count += node
                    .children(&mut cursor)
                    .filter(|token| token.kind() == "&&")
                    .count()
            }
            "return_expression" => metrics.return_count += 1,
            _ => {}
        }

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
}

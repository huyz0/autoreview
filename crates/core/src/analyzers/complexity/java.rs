use autoreview_langsupport::{parser_for, Language};
use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::{
    append_function_findings, java_has_modifier, ComplexityThresholds, FunctionMetrics, MakeFinding,
};

pub(super) fn findings(
    path: &str,
    content: &str,
    limits: &ComplexityThresholds,
    make_finding: &MakeFinding<'_>,
) -> Vec<AgentFinding> {
    let Some(tree) = parser_for(Language::Java).and_then(|mut parser| parser.parse(content, None))
    else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    let mut nodes = vec![tree.root_node()];
    while let Some(node) = nodes.pop() {
        let mut cursor = node.walk();
        let offset = nodes.len();
        nodes.extend(node.named_children(&mut cursor));
        nodes[offset..].reverse();
        if node.has_error() {
            continue;
        }
        match node.kind() {
            "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration" => {
                if let Some(body) = node.child_by_field_name("body") {
                    append_function_findings(
                        &mut findings,
                        path,
                        measure_method(node, body),
                        limits,
                        make_finding,
                    );
                }
            }
            "class_declaration" | "record_declaration" | "interface_declaration" => {
                check_type(
                    &mut findings,
                    path,
                    node,
                    content.as_bytes(),
                    limits,
                    make_finding,
                );
            }
            "switch_expression" => {
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    let cases: usize = body
                        .named_children(&mut cursor)
                        .map(|arm| {
                            let mut cursor = arm.walk();
                            arm.named_children(&mut cursor)
                                .filter(|child| child.kind() == "switch_label")
                                .count()
                        })
                        .sum();
                    if cases > limits.large_switch {
                        findings.push(make_finding("large-switch", path, start(node), end(node),
                            format!("Large switch statement ({cases} cases)"),
                            format!("This switch has {cases} case labels, above the configured limit of {}.", limits.large_switch)));
                    }
                }
            }
            _ => {}
        }
    }
    findings
}

fn start(node: Node<'_>) -> u32 {
    node.start_position().row as u32 + 1
}

fn end(node: Node<'_>) -> u32 {
    node.end_position().row as u32 + 1
}

fn measure_method(node: Node<'_>, body: Node<'_>) -> FunctionMetrics {
    let param_count = node
        .child_by_field_name("parameters")
        .map_or(0, |parameters| {
            let mut cursor = parameters.walk();
            parameters
                .named_children(&mut cursor)
                .filter(|p| matches!(p.kind(), "formal_parameter" | "spread_parameter"))
                .count()
        });
    let mut metrics = FunctionMetrics {
        start_line: start(node),
        end_line: end(node),
        line_count: (end(body) - start(body) + 1) as usize,
        param_count,
        max_nesting: 0,
        branch_count: 0,
        return_count: 0,
        cognitive_score: 0,
    };
    let mut nodes = vec![(body, 0)];
    while let Some((part, depth)) = nodes.pop() {
        let kind = part.kind();
        if matches!(
            kind,
            "lambda_expression"
                | "class_body"
                | "class_declaration"
                | "record_declaration"
                | "interface_declaration"
        ) {
            continue; // Their control flow belongs to a different executable body.
        }
        let control = matches!(
            kind,
            "if_statement"
                | "for_statement"
                | "enhanced_for_statement"
                | "while_statement"
                | "do_statement"
                | "catch_clause"
                | "switch_expression"
                | "ternary_expression"
        );
        if control {
            metrics.branch_count += usize::from(kind != "switch_expression");
            metrics.cognitive_score += depth + 1;
            metrics.max_nesting = metrics.max_nesting.max(depth + 1);
        } else if kind == "return_statement" {
            metrics.return_count += 1;
        } else if (kind == "binary_expression"
            && part
                .child_by_field_name("operator")
                .is_some_and(|op| matches!(op.kind(), "&&" | "||")))
            || (kind == "switch_label" && part.child(0).is_some_and(|token| token.kind() == "case"))
        {
            metrics.branch_count += 1;
        }
        let mut cursor = part.walk();
        let alternative = (kind == "if_statement")
            .then(|| part.child_by_field_name("alternative"))
            .flatten();
        for child in part.named_children(&mut cursor) {
            // An else-if belongs to the same decision level as its if.
            let child_depth = if alternative == Some(child) && child.kind() == "if_statement" {
                depth
            } else {
                depth + usize::from(control)
            };
            nodes.push((child, child_depth));
        }
    }
    metrics
}

fn check_type(
    findings: &mut Vec<AgentFinding>,
    path: &str,
    node: Node<'_>,
    source: &[u8],
    limits: &ComplexityThresholds,
    make_finding: &MakeFinding<'_>,
) {
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    let members: Vec<_> = body.named_children(&mut cursor).collect();
    let methods: Vec<_> = members
        .iter()
        .copied()
        .filter(|member| member.kind() == "method_declaration")
        .collect();
    let count = methods.len();
    if node.kind() == "interface_declaration" {
        if count > limits.complex_interface {
            findings.push(make_finding(
                "complex-interface",
                path,
                start(node),
                end(node),
                format!("Complex interface ({count} members)"),
                format!(
                    "This interface declares {count} methods, above the configured limit of {}.",
                    limits.complex_interface
                ),
            ));
        }
        return;
    }
    if count > limits.god_class {
        findings.push(make_finding(
            "god-class",
            path,
            start(node),
            end(node),
            format!("Large class ({count} methods)"),
            format!(
                "This class declares {count} methods, above the configured limit of {}.",
                limits.god_class
            ),
        ));
    }
    if node.kind() != "class_declaration" {
        return;
    }
    if count >= limits.data_class_min_accessors.max(1)
        && methods
            .iter()
            .all(|method| is_trivial_accessor(*method, source))
    {
        findings.push(make_finding("data-class", path, start(node), end(node),
            format!("Possible Data Class ({count} trivial accessors)"),
            "Every declared method reads or writes a field. If this is an intentional value holder, a Java record may replace the accessors.".to_owned()));
    }
    if count >= limits.utility_class_min_static_methods.max(1)
        && !java_has_modifier(node, "abstract")
    {
        let has_instance_state_or_behavior = members.iter().any(|member| match member.kind() {
            "method_declaration" | "field_declaration" => !java_has_modifier(*member, "static"),
            "block" => true,
            _ => false,
        });
        let constructors: Vec<_> = members
            .iter()
            .filter(|member| member.kind() == "constructor_declaration")
            .collect();
        if !has_instance_state_or_behavior
            && (constructors.is_empty()
                || constructors
                    .iter()
                    .any(|member| !java_has_modifier(**member, "private")))
        {
            findings.push(make_finding("utility-class-public-constructor", path, start(node), end(node),
                "Utility class with an accessible constructor".to_owned(),
                "This class has only static methods and state, and callers can instantiate it. Declare a private constructor to make its namespace-only purpose explicit.".to_owned()));
        }
    }
}

fn is_trivial_accessor(method: Node<'_>, source: &[u8]) -> bool {
    let Some(name) = method
        .child_by_field_name("name")
        .and_then(|name| name.utf8_text(source).ok())
    else {
        return false;
    };
    let Some(body) = method.child_by_field_name("body") else {
        return false;
    };
    let mut cursor = body.walk();
    let mut statements = body
        .named_children(&mut cursor)
        .filter(|child| !matches!(child.kind(), "line_comment" | "block_comment"));
    let Some(statement) = statements.next() else {
        return false;
    };
    if statements.next().is_some() {
        return false;
    }
    let Some(expression) = statement.named_child(0) else {
        return false;
    };
    if name.starts_with("get") || name.starts_with("is") {
        return statement.kind() == "return_statement"
            && matches!(expression.kind(), "identifier" | "field_access");
    }
    name.starts_with("set")
        && expression.kind() == "assignment_expression"
        && expression
            .child_by_field_name("left")
            .is_some_and(|left| matches!(left.kind(), "identifier" | "field_access"))
        && expression
            .child_by_field_name("right")
            .is_some_and(|right| right.kind() == "identifier")
}

#[cfg(test)]
mod tests {
    use super::super::{
        detect_complexity_in_file_with_thresholds, ComplexityLanguage, ComplexityThresholds,
    };
    use std::collections::HashMap;

    #[test]
    fn a_multiline_array_initializer_does_not_extend_the_method_range() {
        let source = r#"
class Example {
    private void log() {
        for (String key : new String[] {
            "a", "b" }) {
            if (key.isEmpty()) {
                return;
            }
        }
    }

    private void shortMethod()
    {
        System.out.println("}");
    }

    private int actualLongMethod() {
        int total = 0;
        total += 1;
        total += 2;
        total += 3;
        total += 4;
        total += 5;
        total += 6;
        total += 7;
        total += 8;
        return total;
    }
}
"#;
        let limits = ComplexityThresholds {
            long_method: 9,
            ..Default::default()
        };
        let findings = detect_complexity_in_file_with_thresholds(
            "Example.java",
            source,
            ComplexityLanguage::JavaOrKotlin,
            &limits,
            &HashMap::new(),
        );
        let long: Vec<_> = findings
            .iter()
            .filter(|f| f.source.rule_id.as_deref() == Some("long-method"))
            .collect();
        assert_eq!(long.len(), 1, "{long:#?}");
        assert_eq!(long[0].location.range.start_line, 17);
        assert_eq!(long[0].location.range.end_line, Some(28));
    }

    #[test]
    fn java_counts_multiline_parameters_and_excludes_lambda_returns_from_the_method() {
        let source = r#"
class Example {
    int method(
        java.util.Map<String, Integer> first, int second,
        int third, int fourth, int fifth, String... rest) {
        java.util.function.IntSupplier callback = () -> {
            if (second == 0) return 1;
            return 2;
        };
        return callback.getAsInt();
    }
}
"#;
        let limits = ComplexityThresholds {
            too_many_returns: 1,
            ..Default::default()
        };
        let findings = detect_complexity_in_file_with_thresholds(
            "Example.java",
            source,
            ComplexityLanguage::JavaOrKotlin,
            &limits,
            &HashMap::new(),
        );
        assert!(
            findings
                .iter()
                .any(|f| f.title == "Long parameter list (6 parameters)"),
            "{findings:#?}"
        );
        assert!(findings
            .iter()
            .all(|f| f.source.rule_id.as_deref() != Some("too-many-returns")));
    }
}

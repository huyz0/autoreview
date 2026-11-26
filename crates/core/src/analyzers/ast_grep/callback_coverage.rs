use std::collections::HashMap;
use std::path::Path;

use autoreview_langsupport::{parser_for, Language};
use autoreview_schema::{AgentFinding, AnalysisCoverageIssue};
use tree_sitter::{Node, Point, Tree};

struct ParsedSource {
    source: String,
    tree: Tree,
}

const RULES: &[&str] = &[
    "kotlin-runblocking-in-suspend-function",
    "kotlin-thread-sleep-in-suspend",
    "kotlin-suspend-swallowed-by-runcatching",
    "kotlin-suspend-call-in-finally-without-noncancellable",
];
const DETAIL: &str = "The callback's execution timing and identity were not resolved; review whether its body executes in this suspend function.";

pub(super) fn annotate(root: &Path, findings: &mut [AgentFinding]) -> Vec<AnalysisCoverageIssue> {
    let mut trees: HashMap<String, Option<ParsedSource>> = HashMap::new();
    let mut coverage = Vec::new();
    for finding in findings {
        let Some(rule) = finding.source.rule_id.as_deref().filter(|rule| RULES.contains(rule)) else { continue };
        if finding.meta.as_ref().is_some_and(|meta| meta.contains_key("rulePackId")) {
            continue;
        }
        let tree = trees.entry(finding.location.path.clone()).or_insert_with(|| {
            let source = std::fs::read_to_string(root.join(&finding.location.path)).ok()?;
            let tree = parser_for(Language::Kotlin)?.parse(&source, None)?;
            (!tree.root_node().has_error()).then_some(ParsedSource { source, tree })
        });
        if callback_context(tree.as_ref(), finding) == Some(false) {
            continue;
        }
        finding.confidence = finding.confidence.min(0.5);
        finding.message.push_str(" Callback execution timing is unresolved; verify the execution context.");
        coverage.push(AnalysisCoverageIssue {
            analyzer: "ast-grep".into(), reason: "callback_execution_unresolved".into(),
            scope: "function".into(), path: Some(finding.location.path.clone()),
            line: Some(finding.location.range.start_line), rule: Some(rule.into()), detail: DETAIL.into(),
        });
    }
    coverage
}

fn callback_context(parsed: Option<&ParsedSource>, finding: &AgentFinding) -> Option<bool> {
    let position = Point::new(
        finding.location.range.start_line.checked_sub(1)? as usize,
        finding.location.range.start_col?.checked_sub(1)? as usize,
    );
    let parsed = parsed?;
    let node = parsed.tree.root_node().descendant_for_point_range(position, position)?;
    for ancestor in std::iter::successors(Some(node), |node| node.parent()) {
        match ancestor.kind() {
            "lambda_literal" if !immediately_invoked(ancestor, &parsed.source) => return Some(true),
            "function_declaration" | "anonymous_function" => return Some(false),
            _ => {}
        }
    }
    None
}

fn immediately_invoked(lambda: Node<'_>, source: &str) -> bool {
    let mut expression = lambda;
    while let Some(parent) = expression.parent().filter(|node| node.kind() == "parenthesized_expression") {
        expression = parent;
    }
    direct_call(expression) || explicit_invoke(expression, source) || immediate_local(expression, source)
}

fn direct_call(expression: Node<'_>) -> bool {
    expression.parent().is_some_and(|call| call.kind() == "call_expression"
        && call.named_child(0) == Some(expression)
        && call.named_child(1).is_some_and(|arguments| arguments.kind() == "value_arguments"))
}

fn explicit_invoke(expression: Node<'_>, source: &str) -> bool {
    expression.parent().is_some_and(|navigation| {
        navigation.kind() == "navigation_expression" && navigation.named_child(0) == Some(expression)
            && navigation.child(1).is_some_and(|dot| dot.kind() == ".")
            && navigation.named_child(1).is_some_and(|name| text(name, source) == Some("invoke"))
            && direct_call(navigation)
    })
}

fn immediate_local(expression: Node<'_>, source: &str) -> bool {
    let Some(declaration) = expression.parent().filter(|node| node.kind() == "property_declaration") else { return false };
    if !declaration.parent().is_some_and(|parent| matches!(parent.kind(), "block" | "lambda_literal")) { return false; }
    let Some(mut name) = simple_val(declaration, expression, source) else { return false };
    let mut next = declaration.next_named_sibling();
    for _ in 0..32 {
        let Some(statement) = next else { return false };
        match local_step(statement, name, source) {
            LocalStep::Invoke => return true,
            LocalStep::Alias(alias) => name = alias,
            LocalStep::Comment => {}
            LocalStep::Unknown => return false,
        }
        next = statement.next_named_sibling();
    }
    false
}

enum LocalStep<'a> {
    Invoke,
    Alias(&'a str),
    Comment,
    Unknown,
}

fn local_step<'a>(statement: Node<'_>, name: &str, source: &'a str) -> LocalStep<'a> {
    if statement.kind().contains("comment") { return LocalStep::Comment; }
    if statement.kind() == "call_expression" {
        let receiver = statement.named_child(0).filter(|node| node.kind() == "navigation_expression")
            .and_then(|navigation| navigation.named_child(0));
        return if receiver.is_some_and(|receiver| text(receiver, source) == Some(name) && explicit_invoke(receiver, source)) {
            LocalStep::Invoke
        } else {
            LocalStep::Unknown
        };
    }
    let alias = statement.named_child(1).filter(|value| value.kind() == "identifier" && text(*value, source) == Some(name))
        .and_then(|value| simple_val(statement, value, source));
    alias.map_or(LocalStep::Unknown, LocalStep::Alias)
}

fn simple_val<'a>(declaration: Node<'_>, value: Node<'_>, source: &'a str) -> Option<&'a str> {
    if declaration.kind() != "property_declaration" || declaration.child(0)?.kind() != "val"
        || declaration.named_child_count() != 2 || declaration.named_child(1)? != value { return None; }
    let binding = declaration.named_child(0)?;
    if binding.kind() != "variable_declaration" || binding.named_child_count() != 1 { return None; }
    let identifier = binding.named_child(0)?;
    (identifier.kind() == "identifier").then(|| text(identifier, source)).flatten()
}

fn text<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    node.utf8_text(source.as_bytes()).ok()
}

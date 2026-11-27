use std::collections::HashMap;
use std::path::Path;

use autoreview_langsupport::{parser_for, Language};
use autoreview_schema::{AgentFinding, AnalysisCoverageIssue};
use tree_sitter::{Node, Point, Tree};

mod evidence;

struct ParsedSource {
    source: String,
    tree: Tree,
    remaining_nodes: usize,
}

const RULES: &[&str] = &[
    "kotlin-runblocking-in-suspend-function",
    "kotlin-thread-sleep-in-suspend",
    "kotlin-suspend-swallowed-by-runcatching",
    "kotlin-suspend-call-in-finally-without-noncancellable",
];
const DETAIL: &str = "The callback's execution timing and identity were not resolved; review whether its body executes in this suspend function.";

pub(super) fn annotate(root: &Path, findings: &mut Vec<AgentFinding>) -> Vec<AnalysisCoverageIssue> {
    let mut coverage = Vec::new();
    let relevant = findings.iter().any(|finding| finding.source.rule_id.as_deref().is_some_and(|rule| RULES.contains(&rule))
        && !finding.meta.as_ref().is_some_and(|meta| meta.contains_key("rulePackId")));
    let evidence = match if relevant { evidence::from_environment(root) } else { Ok(None) } {
        Ok(value) => value,
        Err(()) => {
            coverage.push(AnalysisCoverageIssue { analyzer: "ast-grep".into(), reason: "callback_evidence_unavailable".into(),
                scope: "repository".into(), path: None, line: None, rule: None,
                detail: "The configured experimental Kotlin callback evidence could not be read or validated; callback findings remain unresolved.".into() });
            None
        }
    };
    annotate_with(root, findings, evidence.as_ref(), &mut coverage);
    coverage
}

fn annotate_with(root: &Path, findings: &mut Vec<AgentFinding>, evidence: Option<&evidence::Evidence>, coverage: &mut Vec<AnalysisCoverageIssue>) {
    let mut trees: HashMap<String, Option<ParsedSource>> = HashMap::new();
    findings.retain_mut(|finding| {
        let Some(rule) = finding.source.rule_id.as_deref().filter(|rule| RULES.contains(rule)) else { return true };
        if finding.meta.as_ref().is_some_and(|meta| meta.contains_key("rulePackId")) {
            return true;
        }
        let tree = trees.entry(finding.location.path.clone()).or_insert_with(|| {
            let source = std::fs::read_to_string(root.join(&finding.location.path)).ok()?;
            let tree = parser_for(Language::Kotlin)?.parse(&source, None)?;
            (!tree.root_node().has_error()).then_some(ParsedSource { source, tree, remaining_nodes: 200_000 })
        });
        match callback_context(tree.as_mut(), finding) {
            Some(Execution::Deferred) => return false,
            Some(Execution::Immediate) => return true,
            Some(Execution::Unknown) | None => {}
        }
        if let (Some(evidence), Some(parsed)) = (evidence, tree.as_ref()) {
            if let Some(value) = evidence.for_finding(root, &finding.location.path, parsed, finding) {
                finding.meta.get_or_insert_with(HashMap::new).insert("experimentalCallbackEvidence".into(), value);
            }
        }
        finding.confidence = finding.confidence.min(0.5);
        finding.message.push_str(" Callback execution timing is unresolved; verify the execution context.");
        coverage.push(AnalysisCoverageIssue {
            analyzer: "ast-grep".into(), reason: "callback_execution_unresolved".into(),
            scope: "function".into(), path: Some(finding.location.path.clone()),
            line: Some(finding.location.range.start_line), rule: Some(rule.into()), detail: DETAIL.into(),
        });
        true
    });
}

enum Execution { Immediate, Deferred, Unknown }

fn callback_context(parsed: Option<&mut ParsedSource>, finding: &AgentFinding) -> Option<Execution> {
    let position = Point::new(
        finding.location.range.start_line.checked_sub(1)? as usize,
        finding.location.range.start_col?.checked_sub(1)? as usize,
    );
    let ParsedSource { source, tree, remaining_nodes } = parsed?;
    let node = tree.root_node().descendant_for_point_range(position, position)?;
    let mut unknown = false;
    for ancestor in std::iter::successors(Some(node), |node| node.parent()) {
        match ancestor.kind() {
            "lambda_literal" if !immediately_invoked(ancestor, source, remaining_nodes) => {
                if unused_local(ancestor, source, remaining_nodes) == Some(true) { return Some(Execution::Deferred); }
                unknown = true;
            }
            "function_declaration" | "anonymous_function" => return Some(if unknown { Execution::Unknown } else { Execution::Immediate }),
            _ => {}
        }
    }
    None
}

fn unused_local(lambda: Node<'_>, source: &str, remaining_nodes: &mut usize) -> Option<bool> {
    let mut expression = lambda;
    while let Some(parent) = expression.parent().filter(|node| node.kind() == "parenthesized_expression") {
        expression = parent;
    }
    let declaration = expression.parent().filter(|node| node.kind() == "property_declaration")?;
    if !declaration.parent().is_some_and(|node| matches!(node.kind(), "block" | "lambda_literal")) { return None; }
    let name = simple_val(declaration, expression, source)?.trim_matches('`');
    let scope = std::iter::successors(declaration.parent(), |node| node.parent())
        .find(|node| matches!(node.kind(), "function_declaration" | "anonymous_function"))?;
    identifier_count(scope, name, source, remaining_nodes, 1)
}

fn identifier_count(scope: Node<'_>, name: &str, source: &str, remaining_nodes: &mut usize, expected: usize) -> Option<bool> {
    let mut cursor = scope.walk();
    let mut count = 0;
    loop {
        let node = cursor.node();
        *remaining_nodes = remaining_nodes.checked_sub(1)?;
        if node.kind() == "identifier" && text(node, source)?.trim_matches('`') == name {
            count += 1;
            if count > expected { return Some(false); }
        }
        if !advance(&mut cursor, scope)? { return Some(count == expected); }
    }
}

fn advance(cursor: &mut tree_sitter::TreeCursor<'_>, scope: Node<'_>) -> Option<bool> {
    if cursor.goto_first_child() { return Some(true); }
    loop {
        if cursor.node() == scope { return Some(false); }
        if cursor.goto_next_sibling() { return Some(true); }
        if !cursor.goto_parent() { return None; }
    }
}

fn immediately_invoked(lambda: Node<'_>, source: &str, remaining_nodes: &mut usize) -> bool {
    let mut expression = lambda;
    while let Some(parent) = expression.parent().filter(|node| node.kind() == "parenthesized_expression") {
        expression = parent;
    }
    direct_call(expression) || explicit_invoke(expression, source) || immediate_local(expression, lambda, source, remaining_nodes)
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

fn immediate_local(expression: Node<'_>, lambda: Node<'_>, source: &str, remaining_nodes: &mut usize) -> bool {
    let Some(declaration) = expression.parent().filter(|node| node.kind() == "property_declaration") else { return false };
    if !declaration.parent().is_some_and(|parent| matches!(parent.kind(), "block" | "lambda_literal")) { return false; }
    let Some(name) = simple_val(declaration, expression, source) else { return false };
    let no_parameters = !lambda.named_children(&mut lambda.walk()).any(|node| node.kind() == "lambda_parameters");
    follow_local(declaration, name, source, no_parameters, remaining_nodes)
}

fn follow_local<'a>(declaration: Node<'_>, mut name: &'a str, source: &'a str, no_parameters: bool, remaining_nodes: &mut usize) -> bool {
    let mut next = declaration.next_named_sibling();
    for _ in 0..32 {
        let Some(statement) = next else { return false };
        match local_step(statement, name, source) {
            LocalStep::Invoke => return true,
            LocalStep::BareInvoke if no_parameters => return unique_local_name(declaration, name, source, remaining_nodes),
            LocalStep::BareInvoke => return false,
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
    BareInvoke,
    Alias(&'a str),
    Comment,
    Unknown,
}

fn local_step<'a>(statement: Node<'_>, name: &str, source: &'a str) -> LocalStep<'a> {
    if statement.kind().contains("comment") { return LocalStep::Comment; }
    if statement.kind() == "call_expression" {
        if statement.named_child_count() == 2 && statement.named_child(0).is_some_and(|node| node.kind() == "identifier" && text(node, source).is_some_and(|text| text.trim_matches('`') == name.trim_matches('`')))
            && statement.named_child(1).is_some_and(|args| args.kind() == "value_arguments" && args.named_child_count() == 0) {
            return LocalStep::BareInvoke;
        }
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

fn unique_local_name(declaration: Node<'_>, name: &str, source: &str, remaining_nodes: &mut usize) -> bool {
    let scope = std::iter::successors(declaration.parent(), |node| node.parent())
        .find(|node| matches!(node.kind(), "function_declaration" | "anonymous_function"));
    scope.is_some_and(|scope| identifier_count(scope, name.trim_matches('`'), source, remaining_nodes, 2) == Some(true))
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

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn unused_local_exhaustion_never_proves_deferred() {
        let source = "suspend fun run() { val callback = { work() } }";
        let tree = parser_for(Language::Kotlin).unwrap().parse(source, None).unwrap();
        let root = tree.root_node();
        let position = source.find("{ work()").unwrap();
        let node = root.descendant_for_byte_range(position, position + 1).unwrap();
        let lambda = std::iter::successors(Some(node), |node| node.parent()).find(|node| node.kind() == "lambda_literal").unwrap();
        assert_eq!(unused_local(lambda, source, &mut 0), None);
        assert_eq!(unused_local(lambda, source, &mut 100), Some(true));
    }
}

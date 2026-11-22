mod interval;

use std::collections::HashSet;

use super::{children, descendants, location, text, AgentFinding, Node, Rule};
use super::web::{binding_nodes, first_argument, named_imports};

pub(super) const INTERVAL_RULE: Rule = Rule {
    id: "react-use-effect-interval-not-cleared",
    category: "correctness",
    subcategory: "frontend.effect-cleanup",
    title: "React effect leaves an interval running",
    message: "This effect starts an interval that remains active on a cleanup path. Clear the same interval in the returned cleanup function.",
    reference: "https://react.dev/reference/react/useEffect",
};

pub(super) fn inspect(path: &str, source: &str, root: Node<'_>) -> Vec<AgentFinding> {
    let hooks = imported_hooks(root, source);
    if hooks.is_empty() || timer_bindings_shadowed(root, source) {
        return Vec::new();
    }
    let mut findings = Vec::new();
    for call in descendants(root).filter(|node| node.kind() == "call_expression") {
        let Some(function) = call.child_by_field_name("function") else { continue };
        if !hooks.contains(&text(function, source)) {
            continue;
        }
        let Some(body) = first_argument(call).and_then(callback_body) else { continue };
        for (acquire, exit) in interval::findings(body, source) {
            findings.push(INTERVAL_RULE.finding(
                location(path, exit, source), vec![location(path, acquire, source)],
            ));
        }
    }
    findings
}

fn imported_hooks<'s>(root: Node<'_>, source: &'s str) -> HashSet<&'s str> {
    named_imports(root, source, "react")
        .filter_map(|(original, local)| (original == "useEffect").then_some(local))
        .filter(|hook| !shadowed(root, source, hook))
        .collect()
}

fn timer_bindings_shadowed(root: Node<'_>, source: &str) -> bool {
    ["setInterval", "clearInterval"].into_iter().any(|name| {
        shadowed(root, source, name)
            || children(root).filter(|part| part.kind() == "import_statement")
                .flat_map(descendants)
                .any(|node| node.kind() == "identifier" && text(node, source) == name)
    })
}

fn shadowed(root: Node<'_>, source: &str, name: &str) -> bool {
    descendants(root).flat_map(binding_nodes).any(|node| text(node, source) == name)
}

fn callback_body(node: Node<'_>) -> Option<Node<'_>> {
    if !matches!(node.kind(), "arrow_function" | "function_expression")
        || node.child(0).is_some_and(|part| part.kind() == "async")
    {
        return None;
    }
    node.child_by_field_name("body")
}

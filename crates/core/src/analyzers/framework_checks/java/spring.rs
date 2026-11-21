use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::super::Rule;
use super::{children, class_members, containing, descendants, Java};

const TX: &str = "org.springframework.transaction.annotation.Transactional";
pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "java-spring-transaction-self-invocation",
    category: "correctness",
    title: "Self-invocation bypasses Spring transaction advice",
    message: "This call goes directly to a transactional method on the same instance. In Spring proxy mode the call bypasses the proxy, so the callee's transaction settings are not applied. Move the transaction boundary to the caller or invoke the method through a separate bean.",
    reference: "https://docs.spring.io/spring/reference/data-access/transaction/declarative/annotations.html",
};

pub(super) fn findings(java: &Java<'_>) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    if source_enables_aspectj(java) {
        return findings;
    }
    for class in descendants(java.root).filter(|node| node.kind() == "class_declaration") {
        let methods: Vec<_> = class_members(class)
            .filter(|node| node.kind() == "method_declaration")
            .collect();
        for &caller in &methods {
            for call in calls(caller) {
                if let Some(annotation) = target_annotation(java, caller, call, &methods) {
                    findings
                        .push(RULE.finding(java.location(call), vec![java.location(annotation)]));
                }
            }
        }
    }
    findings
}

fn source_enables_aspectj(java: &Java<'_>) -> bool {
    const ENABLE: &str = "org.springframework.transaction.annotation.EnableTransactionManagement";
    descendants(java.root)
        .filter(|node| node.kind() == "class_declaration")
        .filter_map(|class| java.annotation(class, ENABLE))
        .any(|annotation| {
            java.field(annotation, "arguments")
                .is_some_and(|arguments| arguments.contains("ASPECTJ"))
        })
}

fn calls(caller: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    caller
        .child_by_field_name("body")
        .into_iter()
        .flat_map(descendants)
        .filter(move |node| {
            node.kind() == "method_invocation"
                && containing(*node, "method_declaration") == Some(caller)
        })
}

fn target_annotation<'a>(
    java: &Java<'a>,
    caller: Node<'a>,
    call: Node<'a>,
    methods: &[Node<'a>],
) -> Option<Node<'a>> {
    if call
        .child_by_field_name("object")
        .is_some_and(|object| object.kind() != "this")
    {
        return None;
    }
    if containing(call, "class_body") != containing(caller, "class_body") {
        return None;
    }
    let name = java.field(call, "name")?;
    let arity = arity(call, "arguments")?;
    let mut candidates = methods
        .iter()
        .copied()
        .filter(|method| java.field(*method, "name") == Some(name));
    let target = candidates.next()?;
    if candidates.next().is_some()
        || target == caller
        || !proxyable(target)
        || arity_of(target) != Some(arity)
    {
        return None;
    }
    let annotation = java.annotation(target, TX)?;
    let class_transaction =
        containing(caller, "class_declaration").and_then(|class| java.annotation(class, TX));
    if annotation.kind() == "marker_annotation"
        && (java.annotation(caller, TX).is_some() || class_transaction.is_some())
    {
        return None;
    }
    Some(annotation)
}

fn arity(node: Node<'_>, field: &str) -> Option<usize> {
    let list = node.child_by_field_name(field)?;
    Some(
        children(list)
            .filter(|node| !matches!(node.kind(), "line_comment" | "block_comment"))
            .count(),
    )
}

fn arity_of(method: Node<'_>) -> Option<usize> {
    let params = method.child_by_field_name("parameters")?;
    children(params)
        .all(|node| node.kind() != "spread_parameter")
        .then(|| arity(method, "parameters"))
        .flatten()
}

fn proxyable(method: Node<'_>) -> bool {
    children(method)
        .find(|node| node.kind() == "modifiers")
        .is_some_and(|mods| {
            let mut cursor = mods.walk();
            let mut modifiers = mods.children(&mut cursor);
            modifiers.all(|node| !matches!(node.kind(), "private" | "static" | "final"))
        })
}

use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::super::Rule;
use super::{arity, arity_of, class_members, descendants, modifiers, Java};

pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "java-constructor-calls-overridable-method",
    category: "correctness",
    title: "Constructor calls an overridable method",
    message: "This call can dispatch to a subclass before its fields have been initialized. Move the overridable behavior after construction or keep the initialization method private or final.",
    reference: "https://docs.pmd-code.org/pmd-doc-7.22.0/pmd_rules_java_errorprone.html#constructorcallsoverridablemethod",
};

pub(super) fn findings(java: &Java<'_>) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    for class in descendants(java.root).filter(|node| node.kind() == "class_declaration") {
        if modifiers(class).any(|modifier| modifier.kind() == "final") {
            continue;
        }
        let methods: Vec<_> = class_members(class)
            .filter(|member| member.kind() == "method_declaration")
            .collect();
        let has_superclass = class.child_by_field_name("superclass").is_some();
        for constructor in
            class_members(class).filter(|member| member.kind() == "constructor_declaration")
        {
            let Some(body) = constructor.child_by_field_name("body") else {
                continue;
            };
            for call in eager_calls(body) {
                if has_superclass && arity(call, "arguments") != Some(0) {
                    continue;
                }
                if let Some(target) = target(java, call, &methods) {
                    findings.push(RULE.finding(java.location(call), vec![java.location(target)]));
                }
            }
        }
    }
    findings
}

fn eager_calls(body: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut stack = vec![body];
    std::iter::from_fn(move || {
        while let Some(node) = stack.pop() {
            // These bodies execute in a separate scope or after the lambda is invoked.
            if matches!(
                node.kind(),
                "lambda_expression" | "class_body" | "interface_body" | "enum_body"
            ) {
                continue;
            }
            let mut cursor = node.walk();
            let start = stack.len();
            stack.extend(node.named_children(&mut cursor));
            stack[start..].reverse();
            if node.kind() == "method_invocation" {
                return Some(node);
            }
        }
        None
    })
}

fn target<'a>(java: &Java<'a>, call: Node<'a>, methods: &[Node<'a>]) -> Option<Node<'a>> {
    if call
        .child_by_field_name("object")
        .is_some_and(|receiver| receiver.kind() != "this")
    {
        return None;
    }
    let name = java.field(call, "name")?;
    let arguments = arity(call, "arguments")?;
    let mut candidates = methods
        .iter()
        .copied()
        .filter(|method| java.field(*method, "name") == Some(name));
    let target = candidates.next()?;
    if candidates.next().is_some() || arity_of(target) != Some(arguments) {
        return None;
    }
    modifiers(target)
        .all(|modifier| !matches!(modifier.kind(), "private" | "static" | "final"))
        .then_some(target)
}

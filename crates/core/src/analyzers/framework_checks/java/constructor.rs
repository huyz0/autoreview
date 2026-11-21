use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::super::Rule;
use super::{arity, class_members, descendants, local_target, modifiers, Java};

pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "java-constructor-calls-overridable-method",
    category: "correctness",
    title: "Constructor calls an overridable method",
    message: "This call can dispatch to a subclass before its fields have been initialized. Move the overridable behavior after construction or keep the initialization method private or final.",
    reference: "https://docs.pmd-code.org/pmd-doc-7.22.0/pmd_rules_java_errorprone.html#constructorcallsoverridablemethod",
};

pub(super) fn findings(java: &Java<'_>) -> Vec<AgentFinding> {
    descendants(java.root)
        .filter(|node| node.kind() == "class_declaration")
        .filter(|class| modifiers(*class).all(|modifier| modifier.kind() != "final"))
        .flat_map(|class| class_findings(java, class))
        .collect()
}

fn class_findings<'a>(java: &Java<'a>, class: Node<'a>) -> Vec<AgentFinding> {
    let methods: Vec<_> = class_members(class)
        .filter(|member| member.kind() == "method_declaration")
        .collect();
    let has_inherited_members = ["superclass", "interfaces"]
        .iter()
        .any(|field| class.child_by_field_name(field).is_some());
    class_members(class)
        .filter(|member| member.kind() == "constructor_declaration")
        .filter_map(|constructor| constructor.child_by_field_name("body"))
        .flat_map(eager_calls)
        // Without inherited signatures, only a zero-argument call is unambiguous.
        .filter(|call| !has_inherited_members || arity(*call, "arguments") == Some(0))
        .filter_map(|call| {
            let target = local_target(java, call, &methods)?;
            overridable(target)
                .then(|| RULE.finding(java.location(call), vec![java.location(target)]))
        })
        .collect()
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

fn overridable(method: Node<'_>) -> bool {
    modifiers(method).all(|modifier| !matches!(modifier.kind(), "private" | "static" | "final"))
}

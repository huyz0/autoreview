use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::super::Rule;
use super::hierarchy::{Hierarchy, Lookup};
use super::{class_members, descendants, modifiers, Java};

pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "java-constructor-calls-overridable-method",
    category: "correctness",
    title: "Constructor calls an overridable method",
    message: "This call can dispatch to a subclass before its fields have been initialized. Move the overridable behavior after construction or keep the initialization method private or final.",
    reference: "https://docs.pmd-code.org/pmd-doc-7.22.0/pmd_rules_java_errorprone.html#constructorcallsoverridablemethod",
};

pub(super) fn findings(java: &Java<'_>, hierarchy: &Hierarchy) -> Vec<AgentFinding> {
    descendants(java.root)
        .filter(|node| node.kind() == "class_declaration")
        .filter(|class| modifiers(*class).all(|modifier| modifier.kind() != "final"))
        .filter_map(|class| Some((class, hierarchy.owner(java, class)?)))
        .flat_map(|(class, owner)| class_findings(java, class, &Lookup::new(hierarchy, owner)))
        .collect()
}

fn class_findings(java: &Java<'_>, class: Node<'_>, lookup: &Lookup<'_>) -> Vec<AgentFinding> {
    class_members(class)
        .filter(|member| member.kind() == "constructor_declaration")
        .filter_map(|constructor| Some((constructor, constructor.child_by_field_name("body")?)))
        .flat_map(|(constructor, body)| {
            eager_calls(body)
                .filter_map(move |call| {
                    Some((call, lookup.resolve(java, constructor, call, false)?))
                })
                .filter(|(_, target)| target.overridable())
                .map(|(call, target)| {
                    RULE.finding(java.location(call), vec![target.method.location.clone()])
                })
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

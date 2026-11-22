use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::super::Rule;
use super::hierarchy::{Hierarchy, Lookup};
use super::{class_members, containing, descendants, Java};

pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "java-spring-transaction-self-invocation",
    category: "correctness",
    subcategory: "persistence.transaction-boundaries",
    title: "Self-invocation bypasses Spring transaction advice",
    message: "This call goes directly to a transactional method on the same instance. In Spring proxy mode the call bypasses the proxy, so the callee's transaction settings are not applied. Move the transaction boundary to the caller or invoke the method through a separate bean.",
    reference: "https://docs.spring.io/spring/reference/data-access/transaction/declarative/annotations.html",
};

pub(super) fn findings(java: &Java<'_>, hierarchy: &Hierarchy) -> Vec<AgentFinding> {
    if hierarchy.aspectj_enabled(java) {
        return Vec::new();
    }
    descendants(java.root)
        .filter(|node| node.kind() == "class_declaration")
        .filter_map(|class| Some((class, hierarchy.owner(java, class)?)))
        .flat_map(|(class, owner)| {
            class_findings(java, class, &Lookup::new(hierarchy, owner), hierarchy)
        })
        .collect()
}

fn class_findings(
    java: &Java<'_>,
    class: Node<'_>,
    lookup: &Lookup<'_>,
    hierarchy: &Hierarchy,
) -> Vec<AgentFinding> {
    class_members(class)
        .filter(|node| node.kind() == "method_declaration")
        .flat_map(|caller| {
            let declared = lookup.declared(java, caller);
            let caller_transaction = declared.filter(|method| method.overridable())
                .and_then(|method| hierarchy.transaction(method));
            calls(caller).filter_map(move |call| {
                let target = lookup.resolve(java, caller, call, true)?;
                if !target.overridable()
                    || declared.is_some_and(|caller| std::ptr::eq(caller.method, target.method))
                {
                    return None;
                }
                let transaction = hierarchy.transaction(target)?;
                let same_defaults = caller_transaction.is_some_and(|caller| {
                    caller.default_settings && transaction.default_settings
                });
                (!same_defaults)
                    .then(|| RULE.finding(java.location(call), vec![transaction.location.clone()]))
            })
        })
        .collect()
}

fn calls(caller: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let class_body = containing(caller, "class_body");
    caller
        .child_by_field_name("body")
        .into_iter()
        .flat_map(descendants)
        .filter(move |node| {
            node.kind() == "method_invocation"
                && containing(*node, "method_declaration") == Some(caller)
                && containing(*node, "class_body") == class_body
        })
}

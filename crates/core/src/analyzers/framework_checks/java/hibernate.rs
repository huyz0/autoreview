mod mapping;
mod query;

use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::super::Rule;
use super::{containing, descendants, Java};

pub(super) use mapping::index;

pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "java-hibernate-implicit-n-plus-one",
    category: "performance",
    title: "Lazy collection initialized for every query result",
    message: "This loop materializes a mapped lazy collection on each entity from a typed query. Hibernate may issue one extra select per result. Check the generated SQL and consider a fetch join or an entity graph for this association.",
    reference: "https://docs.hibernate.org/stable/core/userguide/html_single/#hql-explicit-fetch-joins",
};

pub(super) fn findings(java: &Java<'_>, entities: &mapping::Entities) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    for loop_node in descendants(java.root).filter(|node| node.kind() == "enhanced_for_statement") {
        let Some((entity, query)) = query::iterated_entity(java, loop_node, entities) else {
            continue;
        };
        let Some(row) = java.field(loop_node, "name") else {
            continue;
        };
        let Some(body) = loop_node.child_by_field_name("body") else {
            continue;
        };
        let mut reported = std::collections::HashSet::new();
        for access in descendants(body).filter(|node| {
            containing(*node, "method_declaration") == containing(loop_node, "method_declaration")
        }) {
            let Some(getter) = collection_receiver(java, access)
                .and_then(|receiver| getter_name(java, receiver, row))
            else {
                continue;
            };
            if let Some(field) = entity
                .collections
                .get(getter)
                .filter(|_| reported.insert(getter))
            {
                findings.push(RULE.finding(
                    java.location(access),
                    vec![field.clone(), java.location(query)],
                ));
            }
        }
    }
    findings
}

fn collection_receiver<'a>(java: &Java<'a>, node: Node<'a>) -> Option<Node<'a>> {
    match node.kind() {
        "enhanced_for_statement" => node.child_by_field_name("value"),
        "method_invocation"
            if matches!(
                java.field(node, "name"),
                Some(
                    "size"
                        | "isEmpty"
                        | "iterator"
                        | "stream"
                        | "parallelStream"
                        | "toArray"
                        | "forEach"
                        | "contains"
                        | "get"
                )
            ) =>
        {
            node.child_by_field_name("object")
        }
        _ => None,
    }
}

fn getter_name<'a>(java: &Java<'a>, receiver: Node<'a>, row: &str) -> Option<&'a str> {
    let object = receiver.child_by_field_name("object")?;
    let same_row = object.kind() == "identifier" && java.text(object) == row;
    let no_args = receiver
        .child_by_field_name("arguments")?
        .named_child_count()
        == 0;
    (receiver.kind() == "method_invocation" && same_row && no_args)
        .then(|| java.field(receiver, "name"))
        .flatten()
}

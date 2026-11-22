use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::super::Rule;
use super::{children, containing, descendants, Java};

const TYPES: [&str; 2] = [
    "io.opentelemetry.api.trace.Span",
    "io.opentelemetry.context.Context",
];

pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "java-otel-discarded-current-scope",
    category: "correctness",
    subcategories: &["observability.trace-propagation"],
    title: "OpenTelemetry scope is activated and discarded",
    message: "makeCurrent returns a Scope that restores the previous context when closed. Keep it in a try-with-resources block so the current context is restored even on exceptions.",
    reference: "https://opentelemetry.io/docs/languages/java/api/#context",
};

pub(super) fn findings(java: &Java<'_>) -> Vec<AgentFinding> {
    descendants(java.root)
        .filter(|node| node.kind() == "expression_statement")
        .filter_map(|stmt| stmt.named_child(0))
        .filter(|call| java.field(*call, "name") == Some("makeCurrent"))
        .filter(|call| open_scope(java, *call))
        .map(|call| RULE.finding(java.location(call), Vec::new()))
        .collect()
}

fn open_scope(java: &Java<'_>, call: Node<'_>) -> bool {
    let Some(receiver) = call.child_by_field_name("object") else {
        return false;
    };
    if receiver.kind() == "method_invocation" {
        return java.field(receiver, "name") == Some("current")
            && java
                .field(receiver, "object")
                .is_some_and(|ty| scoped_type(java, ty));
    }
    if receiver.kind() != "identifier" {
        return false;
    }
    let name = java.text(receiver);
    if let Some(ty) = local_type(java, call, name) {
        return scoped_type(java, ty);
    }
    let method = containing(call, "method_declaration");
    let params = method.and_then(|method| method.child_by_field_name("parameters"));
    params.into_iter().flat_map(children).any(|param| {
        java.field(param, "name") == Some(name)
            && java
                .field(param, "type")
                .is_some_and(|ty| scoped_type(java, ty))
    })
}

fn local_type<'a>(java: &Java<'a>, call: Node<'_>, name: &str) -> Option<&'a str> {
    let statement = call.parent()?;
    std::iter::successors(statement.prev_named_sibling(), |part| {
        part.prev_named_sibling()
    })
    .filter(|part| part.kind() == "local_variable_declaration")
    .find(|declaration| {
        children(*declaration).any(|part| {
            part.kind() == "variable_declarator" && java.field(part, "name") == Some(name)
        })
    })
    .and_then(|declaration| java.field(declaration, "type"))
}

fn scoped_type(java: &Java<'_>, ty: &str) -> bool {
    TYPES
        .into_iter()
        .any(|qualified| java.resolves(ty, qualified))
}

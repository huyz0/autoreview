use autoreview_schema::AgentFinding;
use tree_sitter::Node;

mod client;

use super::web::BindingScopes;
use super::{children, descendants, location, text, Rule};
pub(super) use client::{JAVASCRIPT_RULE, TYPESCRIPT_RULE};

const VARIABLE: &str = "NODE_TLS_REJECT_UNAUTHORIZED";

pub(super) const RULE: Rule = Rule {
    id: "node-tls-verification-disabled-by-environment",
    category: "security",
    subcategories: &["transport.tls"],
    title: "Node.js environment disables TLS certificate verification",
    message: "This assignment disables default TLS certificate verification across the Node.js process. Keep verification enabled and configure a trusted CA for private services.",
    reference: "https://nodejs.org/api/cli.html#node_tls_reject_unauthorizedvalue",
};

pub(super) fn inspect(path: &str, source: &str, root: Node<'_>) -> Vec<AgentFinding> {
    let mut findings = client::inspect(path, source, root);
    if !source.contains(VARIABLE) || imports_binding(root, "process", source) {
        return findings;
    }

    let scopes = BindingScopes::new(root, source);
    findings.extend(
        descendants(root)
            .filter(|node| node.kind() == "assignment_expression")
            .filter_map(|assignment| {
                let target = assignment.child_by_field_name("left")?;
                let value = assignment.child_by_field_name("right")?;
                if !accesses(target, VARIABLE, source) || !is_string(value, "0", source) {
                    return None;
                }
                let env = target.child_by_field_name("object")?;
                let process = env.child_by_field_name("object")?;
                if !accesses(env, "env", source)
                    || process.kind() != "identifier"
                    || text(process, source) != "process"
                    || scopes.visible("process", process).is_some()
                    || inside_named_expression(process, "process", source)
                {
                    return None;
                }
                Some(RULE.finding(location(path, assignment, source), Vec::new()))
            }),
    );
    findings
}

fn inside_named_expression(node: Node<'_>, expected: &str, source: &str) -> bool {
    std::iter::successors(node.parent(), |ancestor| ancestor.parent()).any(|ancestor| {
        matches!(
            ancestor.kind(),
            "function_expression" | "generator_function" | "class"
        ) && ancestor
            .child_by_field_name("name")
            .is_some_and(|name| text(name, source) == expected)
    })
}

fn accesses(node: Node<'_>, name: &str, source: &str) -> bool {
    match node.kind() {
        "member_expression" => node
            .child_by_field_name("property")
            .is_some_and(|property| {
                property.kind() == "property_identifier" && text(property, source) == name
            }),
        "subscript_expression" => node
            .child_by_field_name("index")
            .is_some_and(|index| is_string(index, name, source)),
        _ => false,
    }
}

fn is_string(node: Node<'_>, value: &str, source: &str) -> bool {
    node.kind() == "string"
        && node.named_child_count() == 1
        && node.named_child(0).is_some_and(|content| {
            content.kind() == "string_fragment" && text(content, source) == value
        })
}

fn imports_binding(root: Node<'_>, expected: &str, source: &str) -> bool {
    children(root)
        .filter(|node| node.kind() == "import_statement")
        .flat_map(descendants)
        .filter_map(|node| match node.kind() {
            "identifier"
                if node.parent().is_some_and(|parent| {
                    matches!(
                        parent.kind(),
                        "import_clause" | "namespace_import" | "import_require_clause"
                    )
                }) =>
            {
                Some(node)
            }
            "import_specifier" => node
                .child_by_field_name("alias")
                .or_else(|| node.child_by_field_name("name")),
            _ => None,
        })
        .any(|binding| text(binding, source) == expected)
}

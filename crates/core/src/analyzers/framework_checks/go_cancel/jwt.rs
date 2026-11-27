use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::super::{children, descendants, location, text, Rule};
use super::package::{local_names, package_alias, File};

pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "go-jwt-v5-claims-validation-disabled",
    category: "security",
    subcategories: &["auth.token-validation"],
    title: "Go JWT parser bypasses registered claim checks",
    message: "This golang-jwt v5 option skips parser claim validation. Keep claim validation enabled for authentication, or require an explicit validator before using the claims.",
    reference: "https://pkg.go.dev/github.com/golang-jwt/jwt/v5#WithoutClaimsValidation",
};

pub(super) fn findings(file: &File) -> Vec<AgentFinding> {
    if !file.source.contains("WithoutClaimsValidation") {
        return Vec::new();
    }
    let Some(alias) = package_alias(file, "github.com/golang-jwt/jwt/v5", "jwt") else {
        return Vec::new();
    };
    let source = &file.source;
    children(file.root())
        .filter(|item| {
            matches!(
                item.kind(),
                "function_declaration" | "method_declaration" | "var_declaration"
            )
        })
        .filter(|item| !local_names(*item, source).contains(alias))
        .flat_map(|item| descendants(item).flat_map(|call| options(call, alias, source)))
        .map(|(call, option)| {
            let mut finding = RULE.finding(
                location(&file.path, option, source),
                vec![location(&file.path, call, source)],
            );
            finding
                .meta
                .get_or_insert_with(Default::default)
                .insert("cwe".into(), serde_json::json!(["CWE-287"]));
            finding
        })
        .collect()
}

fn options<'t>(call: Node<'t>, alias: &str, source: &str) -> Vec<(Node<'t>, Node<'t>)> {
    let prefix = match selector_call(call, alias, source) {
        Some("NewParser") => 0,
        Some("Parse") => 2,
        Some("ParseWithClaims") => 3,
        _ => return Vec::new(),
    };
    arguments(call)
        .skip(prefix)
        .filter(|option| {
            selector_call(*option, alias, source) == Some("WithoutClaimsValidation")
                && arguments(*option).next().is_none()
        })
        .map(|option| (call, option))
        .collect()
}

fn arguments(call: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    call.child_by_field_name("arguments")
        .into_iter()
        .flat_map(children)
        .filter(|node| node.kind() != "comment")
}

fn selector_call<'s>(call: Node<'_>, alias: &str, source: &'s str) -> Option<&'s str> {
    if call.kind() != "call_expression" {
        return None;
    }
    let target = call.child_by_field_name("function")?;
    if target.kind() != "selector_expression"
        || text(target.child_by_field_name("operand")?, source) != alias
    {
        return None;
    }
    target
        .child_by_field_name("field")
        .map(|node| text(node, source))
}

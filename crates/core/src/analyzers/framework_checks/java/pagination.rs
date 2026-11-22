use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::super::Rule;
use super::{children, class_members, descendants, Java};

pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "java-spring-unpaged-repository-endpoint",
    category: "performance",
    subcategories: &["persistence.query-efficiency"],
    title: "GET endpoint returns an explicitly unpaged repository read",
    message: "The endpoint sends an unpaged request to findAll and returns its result. Use a bounded page or slice so the amount read and returned does not grow with the table.",
    reference: "https://docs.spring.io/spring-data/commons/reference/repositories/query-methods-details.html#repositories.special-parameters",
};

pub(super) fn findings(java: &Java<'_>) -> Vec<AgentFinding> {
    const REST: &str = "org.springframework.web.bind.annotation.RestController";
    descendants(java.root)
        .filter(|node| node.kind() == "class_declaration" && java.annotation(*node, REST).is_some())
        .flat_map(|class| endpoint_findings(java, class))
        .collect()
}

fn endpoint_findings<'a>(
    java: &'a Java<'a>,
    class: Node<'a>,
) -> impl Iterator<Item = AgentFinding> + 'a {
    const GET: &str = "org.springframework.web.bind.annotation.GetMapping";
    class_members(class)
        .filter(|node| node.kind() == "method_declaration")
        .filter_map(|method| Some((method, java.annotation(method, GET)?)))
        .flat_map(move |(method, mapping)| {
            returned_calls(method)
                .filter(move |call| {
                    is_unpaged_find_all(java, *call) && known_repository(java, class, method, *call)
                })
                .map(move |call| RULE.finding(java.location(call), vec![java.location(mapping)]))
        })
}

fn returned_calls(method: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    method
        .child_by_field_name("body")
        .into_iter()
        .flat_map(children)
        .filter(|node| node.kind() == "return_statement")
        .flat_map(children)
        .filter(|node| node.kind() == "method_invocation")
}

fn known_repository<'a>(
    java: &Java<'a>,
    class: Node<'a>,
    method: Node<'a>,
    call: Node<'a>,
) -> bool {
    let Some(receiver) = call.child_by_field_name("object") else {
        return false;
    };
    let name = match receiver.kind() {
        "identifier" => java.text(receiver),
        "field_access"
            if receiver
                .child_by_field_name("object")
                .is_some_and(|node| node.kind() == "this") =>
        {
            return java
                .field(receiver, "field")
                .is_some_and(|name| known_field(java, class, name));
        }
        _ => return false,
    };
    let parameter = method
        .child_by_field_name("parameters")
        .into_iter()
        .flat_map(children)
        .find(|param| java.field(*param, "name") == Some(name));
    parameter.map_or_else(
        || no_local_declarations(method) && known_field(java, class, name),
        |param| repository_type(java, param),
    )
}

fn known_field(java: &Java<'_>, class: Node<'_>, name: &str) -> bool {
    class_members(class)
        .filter(|member| member.kind() == "field_declaration")
        .any(|field| repository_field(java, field, name))
}

fn no_local_declarations(method: Node<'_>) -> bool {
    // For an implicit receiver, make sure no local variable can shadow the field.
    method.child_by_field_name("body").is_some_and(|body| {
        children(body).all(|node| {
            matches!(
                node.kind(),
                "return_statement" | "line_comment" | "block_comment"
            )
        })
    })
}

fn repository_field(java: &Java<'_>, field: Node<'_>, name: &str) -> bool {
    repository_type(java, field)
        && children(field).any(|node| {
            node.kind() == "variable_declarator" && java.field(node, "name") == Some(name)
        })
}

fn repository_type(java: &Java<'_>, declaration: Node<'_>) -> bool {
    const REPOSITORIES: &[&str] = &[
        "org.springframework.data.jpa.repository.JpaRepository",
        "org.springframework.data.repository.PagingAndSortingRepository",
    ];
    let Some(ty) = declaration.child_by_field_name("type") else {
        return false;
    };
    let raw_type = if ty.kind() == "generic_type" {
        ty.named_child(0).unwrap_or(ty)
    } else {
        ty
    };
    REPOSITORIES
        .iter()
        .any(|repository| java.resolves(java.text(raw_type), repository))
}

fn is_unpaged_find_all(java: &Java<'_>, call: Node<'_>) -> bool {
    if java.field(call, "name") != Some("findAll") || call.child_by_field_name("object").is_none() {
        return false;
    }
    let Some(args) = call.child_by_field_name("arguments") else {
        return false;
    };
    let mut args =
        children(args).filter(|node| !matches!(node.kind(), "line_comment" | "block_comment"));
    let Some(unpaged) = args
        .next()
        .filter(|node| node.kind() == "method_invocation")
    else {
        return false;
    };
    args.next().is_none()
        && java.field(unpaged, "name") == Some("unpaged")
        && java
            .field(unpaged, "object")
            .is_some_and(|ty| java.resolves(ty, "org.springframework.data.domain.Pageable"))
}

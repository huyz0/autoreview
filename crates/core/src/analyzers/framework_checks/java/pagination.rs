use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::super::Rule;
use super::{children, class_members, descendants, Java};

pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "java-spring-unpaged-repository-endpoint",
    category: "performance",
    title: "GET endpoint returns an explicitly unpaged repository read",
    message: "The endpoint sends an unpaged request to findAll and returns its result. Use a bounded page or slice so the amount read and returned does not grow with the table.",
    reference: "https://docs.spring.io/spring-data/commons/reference/repositories/query-methods-details.html#repositories.special-parameters",
};

pub(super) fn findings(java: &Java<'_>) -> Vec<AgentFinding> {
    const REST: &str = "org.springframework.web.bind.annotation.RestController";
    const GET: &str = "org.springframework.web.bind.annotation.GetMapping";
    let mut findings = Vec::new();
    for class in descendants(java.root).filter(|node| node.kind() == "class_declaration") {
        if java.annotation(class, REST).is_none() {
            continue;
        }
        for method in class_members(class).filter(|node| node.kind() == "method_declaration") {
            let Some(mapping) = java.annotation(method, GET) else {
                continue;
            };
            let Some(body) = method.child_by_field_name("body") else {
                continue;
            };
            for statement in children(body).filter(|node| node.kind() == "return_statement") {
                if let Some(call) =
                    children(statement).find(|node| node.kind() == "method_invocation")
                {
                    if is_unpaged_find_all(java, call)
                        && known_repository(java, class, method, call)
                    {
                        findings
                            .push(RULE.finding(java.location(call), vec![java.location(mapping)]));
                    }
                }
            }
        }
    }
    findings
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
    let fields = class_members(class).filter(|member| member.kind() == "field_declaration");
    let name = match receiver.kind() {
        "identifier" => java.text(receiver),
        "field_access"
            if receiver
                .child_by_field_name("object")
                .is_some_and(|node| node.kind() == "this") =>
        {
            let Some(name) = java.field(receiver, "field") else {
                return false;
            };
            return fields
                .into_iter()
                .any(|field| repository_field(java, field, name));
        }
        _ => return false,
    };
    let mut parameters = method
        .child_by_field_name("parameters")
        .into_iter()
        .flat_map(children);
    if let Some(parameter) = parameters.find(|param| java.field(*param, "name") == Some(name)) {
        return repository_type(java, parameter);
    }
    // A local variable can shadow a field. For an implicit receiver require a
    // parameter or a method body consisting only of the return.
    method.child_by_field_name("body").is_some_and(|body| {
        children(body).all(|node| {
            matches!(
                node.kind(),
                "return_statement" | "line_comment" | "block_comment"
            )
        })
    }) && fields
        .into_iter()
        .any(|field| repository_field(java, field, name))
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

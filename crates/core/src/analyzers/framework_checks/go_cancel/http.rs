use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::super::{children, descendants, location, text, Rule};
use super::package::{local_names, File};

pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "go-http-handler-discards-request-context",
    category: "correctness",
    subcategory: "reliability.deadlines",
    title: "Outbound HTTP request loses the incoming request context",
    message: "This handler constructs an outbound request with a fresh context. Use the incoming request's context or derive a deadline from it so cancellations reach the outbound call.",
    reference: "https://pkg.go.dev/net/http#NewRequestWithContext",
};

pub(super) fn findings(file: &File) -> Vec<AgentFinding> {
    let Some(http) = package_alias(file, "net/http", "http") else {
        return Vec::new();
    };
    let context = package_alias(file, "context", "context");
    file.functions()
        .filter_map(|function| {
            let request = handler_request(function, &file.source, http)?;
            Some((
                function.child_by_field_name("body")?,
                request,
                local_names(function, &file.source),
            ))
        })
        .filter(|(_, _, locals)| !locals.contains(http))
        .flat_map(|(body, request, locals)| {
            let context = context.filter(|alias| !locals.contains(alias));
            descendants(body)
                .filter(move |node| same_handler(*node, body))
                .filter(move |node| loses_context(*node, &file.source, http, context))
                .map(move |call| {
                    RULE.finding(
                        location(&file.path, call, &file.source),
                        vec![location(&file.path, request, &file.source)],
                    )
                })
        })
        .collect()
}

fn package_alias<'a>(file: &'a File, path: &str, default: &'a str) -> Option<&'a str> {
    descendants(file.root())
        .filter(|node| node.kind() == "import_spec")
        .find_map(|import| {
            let value = text(import.child_by_field_name("path")?, &file.source);
            if value.trim_matches(['"', '`']) != path {
                return None;
            }
            let alias = import
                .child_by_field_name("name")
                .map_or(default, |name| text(name, &file.source));
            (!matches!(alias, "." | "_")).then_some(alias)
        })
}

fn handler_request<'tree>(function: Node<'tree>, source: &str, http: &str) -> Option<Node<'tree>> {
    let mut params = children(function.child_by_field_name("parameters")?);
    let writer = params.next()?.child_by_field_name("type")?;
    let request = params.next()?;
    let ty = request.child_by_field_name("type")?;
    if params.next().is_some() || !qualified(writer, source, http, "ResponseWriter") {
        return None;
    }
    let named_type = ty.named_child(0).filter(|_| ty.kind() == "pointer_type")?;
    let name = request.child_by_field_name("name")?;
    (qualified(named_type, source, http, "Request") && text(name, source) != "_").then_some(name)
}

fn qualified(node: Node<'_>, source: &str, package: &str, name: &str) -> bool {
    text(node, source).split_once('.') == Some((package, name))
}

fn same_handler(node: Node<'_>, body: Node<'_>) -> bool {
    node.kind() == "call_expression"
        && std::iter::successors(node.parent(), |parent| parent.parent())
            .take_while(|parent| *parent != body)
            .all(|parent| !matches!(parent.kind(), "func_literal" | "go_statement"))
}

fn loses_context(call: Node<'_>, source: &str, http: &str, context: Option<&str>) -> bool {
    let Some(callee) = call.child_by_field_name("function") else {
        return false;
    };
    match text(callee, source).split_once('.') {
        Some((package, "NewRequest")) if package == http => true,
        Some((package, "NewRequestWithContext")) if package == http => {
            context.is_some_and(|context| fresh_context(call, source, context))
        }
        _ => false,
    }
}

fn fresh_context(call: Node<'_>, source: &str, context: &str) -> bool {
    call.child_by_field_name("arguments")
        .and_then(|args| children(args).find(|node| node.kind() != "comment"))
        .and_then(|first| first.child_by_field_name("function"))
        .is_some_and(|function| {
            qualified(function, source, context, "Background")
                || qualified(function, source, context, "TODO")
        })
}

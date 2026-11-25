use tree_sitter::Node;
use super::PromiseReturn;

/// Inspect a callable declaration without resolving its type names.
pub fn declared_return(node: Node<'_>, source: &[u8]) -> Option<PromiseReturn> {
    if !matches!(node.kind(), "function_declaration" | "function_expression" | "function_signature" | "arrow_function" | "method_definition" | "method_signature")
        || node.children(&mut node.walk()).any(|child| child.kind() == "*")
    {
        return None;
    }
    if node.children(&mut node.walk()).any(|child| child.kind() == "async") {
        return Some(PromiseReturn::AsyncFunction);
    }
    let annotation = node.child_by_field_name("return_type")?;
    let value = unwrap_type(annotation)?;
    if promise_type(value, source) {
        return Some(PromiseReturn::PromiseAnnotation);
    }
    if value.kind() == "function_type"
        && value.child_by_field_name("return_type").and_then(unwrap_type).is_some_and(|value| promise_type(value, source))
    {
        let (minimum, maximum) = parameter_bounds(value, source)?;
        return Some(PromiseReturn::FactoryAnnotation { minimum, maximum });
    }
    None
}

pub(super) fn parameter_bounds(function: Node<'_>, source: &[u8]) -> Option<(usize, usize)> {
    let parameters = function.child_by_field_name("parameters")?;
    let mut minimum = 0;
    let mut maximum = 0;
    let mut optional_seen = false;
    for parameter in parameters.named_children(&mut parameters.walk()).filter(|node| node.kind() != "comment") {
        let pattern = parameter.child_by_field_name("pattern")?;
        if pattern.kind() != "identifier" { return None; }
        if pattern.utf8_text(source) == Ok("this") { continue; }
        match parameter.kind() {
            "required_parameter" if !optional_seen => minimum += 1,
            "optional_parameter" => optional_seen = true,
            _ => return None,
        }
        maximum += 1;
    }
    Some((minimum, maximum))
}

/// Conservatively rule out local bindings named `Promise` in a source unit.
///
/// This deliberately treats other non-type references as uncertain too; it is
/// a syntactic guard, not a substitute for project-wide TypeScript resolution.
pub fn unshadowed_promise_name(root: Node<'_>, source: &[u8]) -> bool {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if matches!(node.kind(), "identifier" | "type_identifier" | "shorthand_property_identifier_pattern")
            && node.utf8_text(source) == Ok("Promise")
            && !node.parent().is_some_and(|parent| parent.kind() == "generic_type" && parent.child_by_field_name("name") == Some(node))
        {
            return false;
        }
        pending.extend(node.named_children(&mut node.walk()));
    }
    true
}

fn unwrap_type(mut node: Node<'_>) -> Option<Node<'_>> {
    while matches!(node.kind(), "type_annotation" | "parenthesized_type") {
        node = node.named_child(0)?;
    }
    Some(node)
}

fn promise_type(node: Node<'_>, source: &[u8]) -> bool {
    node.kind() == "generic_type"
        && node.child_by_field_name("name").is_some_and(|name| name.utf8_text(source) == Ok("Promise"))
        && node.child_by_field_name("type_arguments").is_some_and(|arguments| arguments.named_child_count() == 1)
}


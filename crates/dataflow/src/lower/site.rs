use tree_sitter::Node;

use crate::cfg::{CallSite, CallTarget, IdentityOperand, Stmt, VarId};

pub(super) fn named(
    name: String,
    args: Vec<VarId>,
    assigned_to: Option<VarId>,
    call: Node<'_>,
    source: &[u8],
) -> Stmt {
    Stmt::Call {
        target: CallTarget::Named(name),
        args,
        assigned_to,
        site: capture(call, source),
    }
}

pub(crate) fn capture(call: Node<'_>, source: &[u8]) -> Option<Box<CallSite>> {
    let args = arguments(call)?;
    let receiver = receiver(call)
        .filter(|node| identifier(*node))
        .and_then(|node| node.utf8_text(source).ok())
        .map(str::to_owned);
    if optional(call) || children(args).any(has_spread) {
        return None;
    }
    let arguments = children(args)
        .map(|node| operand(argument(node)?, source))
        .collect();
    Some(Box::new(CallSite {
        start: call.start_byte(),
        end: call.end_byte(),
        discarded: result_is_discarded(call, source),
        receiver,
        arguments,
        local: local_call(call),
    }))
}

fn result_is_discarded(call: Node<'_>, source: &[u8]) -> bool {
    let Some(parent) = call.parent() else { return false };
    match parent.kind() {
        "expression_statement" => true,
        "expression_list" => {
            let Some(assignment) = parent.parent().filter(|node| node.kind() == "assignment_statement") else {
                return false;
            };
            children(parent).count() == 1 && assignment.child_by_field_name("left")
                .is_some_and(|lhs| lhs.utf8_text(source) == Ok("_"))
        }
        _ => false,
    }
}

fn local_call(call: Node<'_>) -> bool {
    if call.kind() == "method_invocation" {
        return call.child_by_field_name("object").is_none_or(|object| object.kind() == "this");
    }
    call.child_by_field_name("function").or_else(|| call.named_child(0)).is_some_and(identifier)
}

fn optional(call: Node<'_>) -> bool {
    if call.child_by_field_name("optional_chain").is_some() {
        return true;
    }
    let Some(callee) = call
        .child_by_field_name("function")
        .or_else(|| call.named_child(0))
    else {
        return false;
    };
    let mut cursor = callee.walk();
    let optional = callee
        .children(&mut cursor)
        .any(|node| matches!(node.kind(), "?." | "optional_chain"));
    optional
}

fn has_spread(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "spread_element" | "spread_expression" | "variadic_argument"
    ) || (node.kind() == "value_argument" && {
        let mut cursor = node.walk();
        let spread = node.children(&mut cursor).any(|child| child.kind() == "*");
        spread
    })
}

pub(crate) fn name(call: Node<'_>, source: &[u8]) -> Option<String> {
    if call.kind() == "method_invocation" {
        return super::java::method_invocation_target_name(call, source);
    }
    let callee = call
        .child_by_field_name("function")
        .or_else(|| call.named_child(0))?;
    match callee.kind() {
        "selector_expression" => super::go::call_target_name(call, source),
        "member_expression" => super::javascript::call_target_name(call, source),
        "navigation_expression" => super::kotlin::call_target_name(call, source),
        "field_expression" => Some(format!(
            "{}.{}",
            callee
                .child_by_field_name("value")?
                .utf8_text(source)
                .ok()?,
            callee
                .child_by_field_name("field")?
                .utf8_text(source)
                .ok()?
        )),
        "generic_function" => {
            let inner = callee.child_by_field_name("function")?;
            Some(inner.utf8_text(source).ok()?.split_whitespace().collect())
        }
        _ => Some(callee.utf8_text(source).ok()?.split_whitespace().collect()),
    }
}

fn arguments(call: Node<'_>) -> Option<Node<'_>> {
    call.child_by_field_name("arguments")
        .or_else(|| children(call).find(|node| node.kind() == "value_arguments"))
}

fn receiver(call: Node<'_>) -> Option<Node<'_>> {
    if call.kind() == "method_invocation" {
        return call.child_by_field_name("object");
    }
    let callee = call
        .child_by_field_name("function")
        .or_else(|| call.named_child(0))?;
    let callee = if callee.kind() == "generic_function" {
        callee.child_by_field_name("function")?
    } else {
        callee
    };
    match callee.kind() {
        "member_expression" => callee.child_by_field_name("object"),
        "selector_expression" => callee.child_by_field_name("operand"),
        "field_expression" => callee.child_by_field_name("value"),
        "navigation_expression" => callee.named_child(0),
        _ => None,
    }
}

fn argument(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "value_argument" => {
            let mut parts = children(node);
            let first = parts.next()?;
            parts.next().is_none().then_some(first)
        }
        _ => Some(node),
    }
}

pub(crate) fn operand(node: Node<'_>, source: &[u8]) -> Option<IdentityOperand> {
    let spelling = node.utf8_text(source).ok()?;
    if node.kind() == "reference_expression"
        && !children(node).any(|child| child.kind() == "mutable_specifier")
    {
        return operand(node.child_by_field_name("value")?, source);
    }
    if matches!(spelling, "true" | "false") {
        return Some(IdentityOperand::Boolean(spelling == "true"));
    }
    if node.kind() == "parenthesized_expression" {
        return operand(node.named_child(0)?, source);
    }
    if identifier(node) {
        return Some(IdentityOperand::Binding(spelling.into()));
    }
    match node.kind() {
        "string" | "string_literal" | "interpreted_string_literal" | "line_string_literal" => {
            simple_string(spelling)
        }
        "number"
        | "number_literal"
        | "integer_literal"
        | "int_literal"
        | "decimal_integer_literal" => decimal_integer(spelling),
        _ => None,
    }
}

fn simple_string(spelling: &str) -> Option<IdentityOperand> {
    let quote = if spelling.starts_with('\'') {
        '\''
    } else {
        '"'
    };
    let content = spelling.strip_prefix(quote)?.strip_suffix(quote)?;
    (!content.contains(['\\', '\n', '\r', '$', quote]))
        .then(|| IdentityOperand::String(content.into()))
}

fn decimal_integer(spelling: &str) -> Option<IdentityOperand> {
    (!spelling.is_empty()
        && spelling.bytes().all(|byte| byte.is_ascii_digit())
        && (spelling == "0" || !spelling.starts_with('0')))
    .then(|| IdentityOperand::Integer(spelling.into()))
}

pub(crate) fn identifier(node: Node<'_>) -> bool {
    matches!(node.kind(), "identifier" | "simple_identifier")
}

pub(crate) fn children(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|part| !matches!(part.kind(), "comment" | "line_comment" | "block_comment"))
        .collect::<Vec<_>>()
        .into_iter()
}

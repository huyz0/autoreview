use tree_sitter::Node;

use crate::cfg::{CallSite, CallTarget, IdentityOperand, Stmt, VarId};

mod arguments;
pub(crate) use arguments::{argument, arguments};
use arguments::capture_arguments;

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
    if optional(call) {
        return None;
    }
    let args = capture_arguments(call, source)?;
    let receiver = receiver(call)
        .filter(|node| identifier(*node))
        .and_then(|node| node.utf8_text(source).ok())
        .map(str::to_owned);
    let (expression, awaited) = call_context(call);
    let returned = expression.parent().is_some_and(|parent| matches!(parent.kind(), "return_statement" | "return_expression"));
    Some(Box::new(CallSite {
        start: call.start_byte(),
        end: call.end_byte(),
        discarded: !awaited && result_is_discarded(expression, source),
        awaited,
        observed: awaited || returned,
        receiver,
        arguments: args.positional,
        named_arguments: args.named,
        local: local_call(call),
    }))
}

fn call_context(mut expression: Node<'_>) -> (Node<'_>, bool) {
    let mut awaited = false;
    while let Some(parent) = expression.parent() {
        match parent.kind() {
            "await_expression" => awaited = true,
            "parenthesized_expression" | "as_expression" | "satisfies_expression" | "non_null_expression" => {}
            _ => break,
        }
        expression = parent;
    }
    (expression, awaited)
}

pub(super) fn await_binding(node: Node<'_>, source: &[u8]) -> Option<Stmt> {
    let value = super::keyed_syntax::expression_value(node.named_child(0)?)?;
    let receiver = identifier(value).then(|| value.utf8_text(source).ok())??;
    Some(Stmt::Call {
        target: CallTarget::Named(format!("{receiver}.await")),
        args: Vec::new(),
        assigned_to: None,
        site: Some(Box::new(CallSite {
            start: node.start_byte(), end: node.end_byte(), discarded: false,
            awaited: true, observed: true, receiver: Some(receiver.to_owned()),
            arguments: Vec::new(), named_arguments: Box::default(), local: false,
        })),
    })
}

fn result_is_discarded(call: Node<'_>, source: &[u8]) -> bool {
    let Some(parent) = call.parent() else { return false };
    match parent.kind() {
        "expression_statement" => true,
        "ternary_expression" => discarded_conditional_arm(call),
        "block" => parent.parent().is_some_and(|owner| owner.kind() == "function_body"
            || (owner.kind() == "match_arm" && discarded_match_arm(owner, source))),
        "match_arm" => discarded_match_arm(parent, source),
        "let_declaration" => parent.children(&mut parent.walk()).any(|part| part.kind() == "_"),
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

pub(crate) fn discarded_conditional_arm(node: Node<'_>) -> bool {
    let mut arm = node;
    while let Some(wrapper) = arm.parent().filter(|parent| conditional_wrapper(parent.kind())) { arm = wrapper; }
    let Some(parent) = arm.parent().filter(|parent| parent.kind() == "ternary_expression") else { return false; };
    if !["consequence", "alternative"].into_iter().any(|field| parent.child_by_field_name(field) == Some(arm)) { return false; }
    discarded_conditional_expression(parent)
}

pub(crate) fn discarded_conditional_expression(node: Node<'_>) -> bool {
    let mut value = node;
    while let Some(wrapper) = value.parent().filter(|parent| conditional_wrapper(parent.kind())) { value = wrapper; }
    value.parent().is_some_and(|owner| owner.kind() == "expression_statement")
}

pub(crate) fn returned_conditional_expression(node: Node<'_>) -> bool {
    let mut value = node;
    while let Some(wrapper) = value.parent().filter(|parent| conditional_wrapper(parent.kind())) { value = wrapper; }
    value.parent().is_some_and(|owner| owner.kind() == "return_statement")
}

pub(crate) fn assigned_conditional_expression(node: Node<'_>) -> bool {
    let mut value = node;
    while let Some(wrapper) = value.parent().filter(|parent| conditional_wrapper(parent.kind())) { value = wrapper; }
    value.parent().is_some_and(|owner| match owner.kind() {
        "variable_declarator" => owner.child_by_field_name("value") == Some(value),
        "assignment_expression" | "augmented_assignment_expression" => owner.child_by_field_name("right") == Some(value),
        _ => false,
    })
}

fn conditional_wrapper(kind: &str) -> bool {
    matches!(kind, "parenthesized_expression" | "as_expression" | "satisfies_expression" | "non_null_expression")
}

fn discarded_match_arm(arm: Node<'_>, source: &[u8]) -> bool {
    arm.parent().and_then(|body| body.parent()).is_some_and(|node| crate::lower::rust::discards_match(node, source))
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

pub(crate) fn name(call: Node<'_>, source: &[u8]) -> Option<String> {
    if call.kind() == "method_invocation" {
        return super::java::method_invocation_target_name(call, source);
    }
    let callee = callee(call)?;
    match callee.kind() {
        "selector_expression" => super::go::call_target_name(call, source),
        "member_expression" => super::javascript::call_target_name(call, source),
        "navigation_expression" | "call_expression" => super::kotlin::call_target_name(call, source),
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

fn callee(call: Node<'_>) -> Option<Node<'_>> {
    call.child_by_field_name("function")
        .or_else(|| call.child_by_field_name("constructor"))
        .or_else(|| call.named_child(0))
}

pub(crate) fn receiver(call: Node<'_>) -> Option<Node<'_>> {
    if call.kind() == "method_invocation" {
        return call.child_by_field_name("object");
    }
    let callee = callee(call)?;
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
        "call_expression" => receiver(callee),
        _ => None,
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

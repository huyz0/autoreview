use tree_sitter::Node;

use crate::cfg::{IdentityOperand, NamedArgument};

use super::{children, identifier, operand};

pub(super) struct CapturedArguments {
    pub(super) positional: Vec<Option<IdentityOperand>>,
    pub(super) named: Box<[NamedArgument]>,
}

pub(super) fn capture_arguments(call: Node<'_>, source: &[u8]) -> Option<CapturedArguments> {
    let args = arguments(call);
    let trailing_lambda = children(call).any(|node| node.kind() == "annotated_lambda");
    if args.is_none() && !trailing_lambda {
        return None;
    }
    let mut named = Vec::new();
    let mut positional = Vec::new();
    for (index, node) in args.into_iter().flat_map(children).enumerate() {
        if has_spread(node) {
            return None;
        }
        if let Some((label, value)) = kotlin_named_argument(node, source) {
            named.push(NamedArgument { index, label: label.to_owned(), value: argument_operand(value, source) });
            positional.push(None);
        } else {
            positional.push(argument(node).and_then(|value| argument_operand(value, source)));
        }
    }
    positional.extend(trailing_lambda.then_some(None));
    Some(CapturedArguments { positional, named: named.into_boxed_slice() })
}

pub(crate) fn arguments(call: Node<'_>) -> Option<Node<'_>> {
    call.child_by_field_name("arguments")
        .or_else(|| children(call).find(|node| node.kind() == "value_arguments"))
}

pub(crate) fn argument(node: Node<'_>) -> Option<Node<'_>> {
    if node.kind() != "value_argument" {
        return Some(node);
    }
    let mut parts = children(node);
    let first = parts.next()?;
    parts.next().is_none().then_some(first)
}

fn has_spread(node: Node<'_>) -> bool {
    matches!(node.kind(), "spread_element" | "spread_expression" | "variadic_argument")
        || (node.kind() == "value_argument" && node.children(&mut node.walk()).any(|child| child.kind() == "*"))
}

fn kotlin_named_argument<'src, 'tree>(node: Node<'tree>, source: &'src [u8]) -> Option<(&'src str, Node<'tree>)> {
    if node.kind() != "value_argument" || node.has_error()
        || !node.children(&mut node.walk()).any(|part| part.kind() == "=") {
        return None;
    }
    let mut parts = children(node);
    let label = parts.next().filter(|node| identifier(*node))?.utf8_text(source).ok()?;
    Some((label, parts.next()?))
}

fn argument_operand(value: Node<'_>, source: &[u8]) -> Option<IdentityOperand> {
    operand(value, source).or_else(|| call_result_argument(value, source))
}

fn call_result_argument(node: Node<'_>, source: &[u8]) -> Option<IdentityOperand> {
    if node.kind() == "reference_expression"
        && !children(node).any(|child| child.kind() == "mutable_specifier")
    {
        return call_result_argument(node.child_by_field_name("value")?, source);
    }
    match super::super::keyed_syntax::result_operand(node, source, false)? {
        result @ IdentityOperand::CallResult { .. } => Some(result),
        _ => None,
    }
}

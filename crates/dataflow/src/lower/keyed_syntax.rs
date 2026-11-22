use tree_sitter::Node;

use crate::cfg::{BindingWrite, IdentityOperand};

use super::site::{children, identifier, operand};

pub(crate) fn writes(node: Node<'_>, source: &[u8]) -> Vec<BindingWrite> {
    match node.kind() {
        "expression_statement" => node
            .named_child(0)
            .map_or_else(Vec::new, |child| writes(child, source)),
        "local_variable_declaration"
        | "lexical_declaration"
        | "variable_declaration"
        | "var_declaration"
            if node.child_by_field_name("pattern").is_none() =>
        {
            children(node)
                .filter_map(|child| write(child, source))
                .collect()
        }
        _ => write(node, source).into_iter().collect(),
    }
}

fn write(node: Node<'_>, source: &[u8]) -> Option<BindingWrite> {
    let (target, value, assignment) = match node.kind() {
        "assignment" | "assignment_expression" | "assignment_statement" => (
            node.child_by_field_name("left")?,
            node.child_by_field_name("right")?,
            true,
        ),
        "short_var_declaration" => (
            node.child_by_field_name("left")?,
            node.child_by_field_name("right")?,
            false,
        ),
        "let_declaration" => (
            node.child_by_field_name("pattern")?,
            node.child_by_field_name("value")?,
            false,
        ),
        "variable_declarator" | "var_spec" => (
            node.child_by_field_name("name")?,
            node.child_by_field_name("value")?,
            false,
        ),
        "property_declaration" => kotlin_initializer(node)?,
        _ => return None,
    };
    if assignment
        && source
            .get(target.end_byte()..value.start_byte())
            .is_none_or(|op| op.trim_ascii() != b"=")
    {
        return None;
    }
    let name = simple(target)
        .filter(|target| identifier(*target))?
        .utf8_text(source)
        .ok()?
        .to_owned();
    let value = simple(value).and_then(|value| {
        if matches!(
            value.kind(),
            "new_expression" | "object_creation_expression"
        ) {
            Some(IdentityOperand::Fresh {
                name: name.clone(),
                start: value.start_byte(),
                age: 0,
            })
        } else {
            binding_value(value, source)
        }
    });
    if !assignment && value.is_none() {
        return None;
    }
    Some(BindingWrite {
        name,
        start: node.start_byte(),
        value,
    })
}

pub(super) fn expression(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "expression_statement" | "return_statement" | "return_expression" => {
            node.named_child(0).and_then(simple).and_then(unpack)
        }
        "if_statement" | "if_expression" => node.child_by_field_name("condition").and_then(unpack),
        "assignment" | "assignment_expression" => {
            node.child_by_field_name("right").and_then(unpack)
        }
        "local_variable_declaration" | "lexical_declaration" | "variable_declaration" => {
            children(node)
                .find(|child| child.kind() == "variable_declarator")?
                .child_by_field_name("value")
                .and_then(unpack)
        }
        "property_declaration" => unpack(kotlin_initializer(node)?.1),
        _ => unpack(node),
    }
}

fn unpack(mut node: Node<'_>) -> Option<Node<'_>> {
    while matches!(node.kind(), "parenthesized_expression" | "await_expression") {
        node = node.named_child(0)?;
    }
    if matches!(node.kind(), "assignment" | "assignment_expression") {
        return expression(node);
    }
    Some(node)
}

pub(crate) fn condition(node: Node<'_>, source: &[u8]) -> Option<(String, bool)> {
    condition_value(node.child_by_field_name("condition")?, source)
}

pub(crate) fn condition_value(node: Node<'_>, source: &[u8]) -> Option<(String, bool)> {
    let test = unpack(node)?;
    if identifier(test) || matches!(test.kind(), "true" | "false" | "boolean_literal") {
        return Some((test.utf8_text(source).ok()?.to_owned(), true));
    }
    let text = test.utf8_text(source).ok()?;
    let bound = test.named_child(0).filter(|child| identifier(*child))?;
    text.trim_start().starts_with('!').then(|| {
        (
            bound.utf8_text(source).unwrap_or_default().to_owned(),
            false,
        )
    })
}

fn kotlin_initializer(node: Node<'_>) -> Option<(Node<'_>, Node<'_>, bool)> {
    let declaration = children(node).find(|child| child.kind() == "variable_declaration")?;
    let value = children(node)
        .last()
        .filter(|child| *child != declaration && child.start_byte() >= declaration.end_byte())?;
    Some((declaration.named_child(0)?, value, false))
}

fn simple(node: Node<'_>) -> Option<Node<'_>> {
    if node.kind() != "expression_list" {
        return Some(node);
    }
    let mut parts = children(node);
    let first = parts.next()?;
    parts.next().is_none().then_some(first)
}

fn binding_value(node: Node<'_>, source: &[u8]) -> Option<IdentityOperand> {
    if node.kind() == "reference_expression" {
        return operand(node.child_by_field_name("value")?, source);
    }
    operand(node, source)
}

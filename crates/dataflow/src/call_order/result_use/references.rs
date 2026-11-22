use std::collections::HashSet;

use tree_sitter::Node;

use crate::call_order::ContractLanguage;
use crate::lower::site;

pub(super) fn captured_names<'src>(function: Node<'_>, source: &'src [u8], language: ContractLanguage) -> HashSet<&'src str> {
    let mut captured = HashSet::new();
    let mut pending: Vec<_> = site::children(function).collect();
    while let Some(node) = pending.pop() {
        if capture_scope(node) {
            walk_names(node, source, language, |name, _| {
                captured.insert(name);
            });
        } else {
            pending.extend(site::children(node));
        }
    }
    captured
}

fn capture_scope(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "func_literal"
            | "function_expression"
            | "function_declaration"
            | "generator_function"
            | "generator_function_declaration"
            | "arrow_function"
            | "method_definition"
            | "lambda_expression"
            | "lambda_literal"
            | "anonymous_function"
            | "closure_expression"
            | "async_block"
            | "class_declaration"
            | "class_body"
            | "object_literal"
    )
}

pub(super) fn walk_names<'src>(
    node: Node<'_>,
    source: &'src [u8],
    language: ContractLanguage,
    mut visit: impl FnMut(&'src str, usize),
) {
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if reference(node, source) {
            if let Ok(name) = node.utf8_text(source) {
                visit(name, node.start_byte());
            }
        }
        string_references(node, source, language, |name| visit(name, node.start_byte()));
        pending.extend(site::children(node));
    }
}

fn string_references<'src>(
    node: Node<'_>,
    source: &'src [u8],
    language: ContractLanguage,
    mut visit: impl FnMut(&'src str),
) {
    if node.kind() != "string_content" {
        return;
    }
    let Ok(text) = node.utf8_text(source) else { return };
    match language {
        // The Kotlin grammar emits $ and the short template name as
        // separate string_content nodes. ${...} has its own expression.
        ContractLanguage::Kotlin if text == "$" => {
            if let Some(part) = node.next_named_sibling()
                .filter(|sibling| sibling.kind() == "string_content" && sibling.start_byte() == node.end_byte())
                .and_then(|sibling| sibling.utf8_text(source).ok()).and_then(identifier_prefix)
            {
                visit(part);
            }
        }
        ContractLanguage::Rust
            if node
                .parent()
                .and_then(|literal| literal.parent())
                .is_some_and(|parent| parent.kind() == "token_tree") =>
        {
            for part in text.split('{').skip(1).filter_map(identifier_prefix) {
                visit(part);
            }
        }
        _ => {}
    }
}

fn identifier_prefix(text: &str) -> Option<&str> {
    let end = text.find(|c: char| c != '_' && !c.is_alphanumeric()).unwrap_or(text.len());
    (end > 0).then(|| &text[..end])
}

fn reference(node: Node<'_>, source: &[u8]) -> bool {
    if matches!(node.kind(), "shorthand_property_identifier" | "shorthand_field_identifier") {
        return true;
    }
    if !site::identifier(node) {
        return false;
    }
    let Some(parent) = node.parent() else { return true };
    match parent.kind() {
        "navigation_expression" => parent.named_child(0) == Some(node),
        "macro_invocation" => parent.child_by_field_name("macro") != Some(node),
        "method_invocation" | "field_access" | "variable_declarator" => {
            parent.child_by_field_name("name") != Some(node)
        }
        "variable_declaration" | "scoped_identifier" | "jsx_attribute" | "jsx_closing_element" => false,
        "jsx_opening_element" | "jsx_self_closing_element" => {
            node.utf8_text(source).is_ok_and(|name| !name.as_bytes().first().is_some_and(u8::is_ascii_lowercase))
        }
        _ => true,
    }
}

use tree_sitter::Node;

use crate::lower::site::children;

use super::{argument_flow, branch_value, simple_binding};

pub(crate) fn projection<'tree>(node: Node<'tree>, source: &[u8]) -> Option<(Node<'tree>, usize)> {
    let (base, index) = match node.kind() {
        "field_expression" => {
            let field = node.child_by_field_name("field")?;
            let spelling = field.utf8_text(source).ok()?;
            if field.kind() != "integer_literal" || (spelling.len() > 1 && spelling.starts_with('0')) {
                return None;
            }
            (node.child_by_field_name("value")?, spelling.parse().ok()?)
        }
        "index_expression" => {
            let mut operands = children(node);
            (operands.next()?, literal_usize(operands.next()?, source)?)
        }
        _ => return None,
    };
    Some((ungroup(base), index))
}

pub(crate) fn element_slot<'tree>(aggregate: Node<'tree>, access: Node<'_>, index: usize, source: &[u8]) -> Option<Node<'tree>> {
    match (aggregate.kind(), access.kind()) {
        ("array_expression", "index_expression") => {
            if let Some(length) = aggregate.child_by_field_name("length") {
                if index >= literal_usize(length, source)? { return None; }
                return argument_flow::aggregate_operands(aggregate)?.next();
            }
        }
        ("tuple_expression", "field_expression") => {}
        _ => return None,
    }
    children(aggregate).nth(index)
}

pub(crate) fn local_aggregate(node: Node<'_>) -> Option<Node<'_>> {
    if !matches!(node.kind(), "array_expression" | "tuple_expression") { return None; }
    let value = branch_value::outermost_group(node);
    let declaration = value.parent()?;
    if declaration.kind() != "let_declaration"
        || declaration.child_by_field_name("value") != Some(value)
        || declaration.child_by_field_name("alternative").is_some()
        || !simple_binding(declaration)
        || children(declaration).any(|child| child.kind() == "mutable_specifier")
    {
        return None;
    }
    declaration.child_by_field_name("pattern").filter(|pattern| pattern.kind() == "identifier")
}

pub(crate) fn direct_projection<'tree>(aggregate: Node<'tree>, source: &[u8]) -> Option<(Node<'tree>, Node<'tree>)> {
    if !matches!(aggregate.kind(), "array_expression" | "tuple_expression") { return None; }
    let access = branch_value::outermost_group(aggregate).parent()?;
    let (base, index) = projection(access, source)?;
    if base != aggregate || !read_only(access) || !argument_flow::can_evaluate(access) { return None; }
    Some((access, element_slot(aggregate, access, index, source)?))
}

pub(crate) fn read_only(access: Node<'_>) -> bool {
    let access = branch_value::outermost_group(access);
    let Some(parent) = access.parent() else { return false };
    match parent.kind() {
        "assignment_expression" | "compound_assignment_expr" => parent.child_by_field_name("left") != Some(access),
        "index_expression" => children(parent).next() != Some(access),
        "field_expression" if parent.child_by_field_name("value") == Some(access) => {
            parent.child_by_field_name("field").is_some_and(|field| field.kind() == "field_identifier")
                && parent.parent().is_some_and(|call| {
                    call.kind() == "call_expression" && call.child_by_field_name("function") == Some(parent)
                })
        }
        "reference_expression" => false,
        _ => true,
    }
}

pub(super) fn literal_usize(node: Node<'_>, source: &[u8]) -> Option<usize> {
    let node = ungroup(node);
    if node.kind() != "integer_literal" { return None; }
    let spelling = node.utf8_text(source).ok()?;
    let digits = spelling.strip_suffix("usize").unwrap_or(spelling);
    let (digits, radix) = [("0x", 16), ("0o", 8), ("0b", 2)].into_iter()
        .find_map(|(prefix, radix)| Some((digits.strip_prefix(prefix)?, radix)))
        .unwrap_or((digits, 10));
    usize::from_str_radix(&digits.replace('_', ""), radix).ok()
}

fn ungroup(mut node: Node<'_>) -> Node<'_> {
    while node.kind() == "parenthesized_expression" {
        let Some(value) = children(node).next() else { break };
        node = value;
    }
    node
}

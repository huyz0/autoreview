use tree_sitter::Node;

use crate::lower::site::children;

use super::{argument_flow, branch_value};

fn projection<'tree>(node: Node<'tree>, source: &[u8], known_index: &impl Fn(Node<'_>) -> Option<usize>) -> Option<(Node<'tree>, usize)> {
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
            let base = operands.next()?;
            let index = ungroup(operands.next()?);
            (base, literal_usize(index, source).or_else(|| known_index(index))?)
        }
        _ => return None,
    };
    Some((ungroup(base), index))
}

fn element_slot<'tree>(aggregate: Node<'tree>, access: Node<'_>, index: usize, source: &[u8]) -> Option<Node<'tree>> {
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
    if declaration.child_by_field_name("value") != Some(value) || !argument_flow::can_evaluate(node) {
        return None;
    }
    immutable_let(declaration)
}

pub(crate) fn immutable_let(declaration: Node<'_>) -> Option<Node<'_>> {
    if declaration.kind() != "let_declaration" || declaration.child_by_field_name("alternative").is_some()
        || children(declaration).any(|child| child.kind() == "mutable_specifier")
    {
        return None;
    }
    declaration.child_by_field_name("pattern").filter(|pattern| pattern.kind() == "identifier")
}

pub(crate) fn direct_projection<'tree>(aggregate: Node<'tree>, source: &[u8], known_index: &impl Fn(Node<'_>) -> Option<usize>) -> Option<(Node<'tree>, Node<'tree>)> {
    if !matches!(aggregate.kind(), "array_expression" | "tuple_expression") { return None; }
    projection_chain(aggregate, aggregate, source, known_index)
}

pub(crate) fn projection_chain<'tree>(base: Node<'tree>, aggregate: Node<'tree>, source: &[u8], known_index: &impl Fn(Node<'_>) -> Option<usize>) -> Option<(Node<'tree>, Node<'tree>)> {
    let mut access = base;
    let mut slot = aggregate;
    while let Some(next) = branch_value::outermost_group(access).parent() {
        let Some((receiver, index)) = projection(next, source, known_index) else { break };
        if receiver != access { break; }
        slot = element_slot(ungroup(slot), next, index, source)?;
        access = next;
    }
    (access != base && read_only(access)).then_some((access, slot))
}

pub(super) fn projection_owner(mut aggregate: Node<'_>) -> Option<usize> {
    if !matches!(aggregate.kind(), "array_expression" | "tuple_expression") { return None; }
    while let Some(parent) = branch_value::outermost_group(aggregate).parent().filter(|parent| is_list(*parent)) {
        aggregate = parent;
    }
    // Record candidates here so result analysis can prove them using lexical bindings.
    (local_aggregate(aggregate).is_some() || has_projection_receiver(aggregate))
        .then_some(aggregate.start_byte())
}

fn has_projection_receiver(aggregate: Node<'_>) -> bool {
    let receiver = branch_value::outermost_group(aggregate);
    receiver.parent().is_some_and(|parent| match parent.kind() {
        "index_expression" => children(parent).next() == Some(receiver),
        "field_expression" => parent.child_by_field_name("value") == Some(receiver)
            && parent.child_by_field_name("field").is_some_and(|field| field.kind() == "integer_literal"),
        _ => false,
    })
}

pub(crate) fn nested_aggregates<'tree, 'src>(root: Node<'tree>, source: &'src [u8]) -> impl Iterator<Item = Node<'tree>> + use<'tree, 'src> {
    let mut pending = vec![root];
    std::iter::from_fn(move || {
        let aggregate = pending.pop()?;
        pending.extend(argument_flow::aggregate_operands(aggregate).into_iter().flatten().map(ungroup)
            .filter(|child| has_static_shape(*child, source)));
        Some(aggregate)
    })
}

fn has_static_shape(node: Node<'_>, source: &[u8]) -> bool {
    is_list(node) || (node.kind() == "array_expression"
        && node.child_by_field_name("length").and_then(|length| literal_usize(length, source)).is_some()
        && argument_flow::aggregate_operands(node).is_some())
}

fn is_list(node: Node<'_>) -> bool {
    node.kind() == "tuple_expression"
        || (node.kind() == "array_expression" && node.child_by_field_name("length").is_none())
}

fn read_only(access: Node<'_>) -> bool {
    if !argument_flow::can_evaluate(access) { return false; }
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

pub(crate) fn literal_usize(node: Node<'_>, source: &[u8]) -> Option<usize> {
    let node = ungroup(node);
    if node.kind() != "integer_literal" { return None; }
    let spelling = node.utf8_text(source).ok()?;
    let digits = spelling.strip_suffix("usize").unwrap_or(spelling);
    let (digits, radix) = [("0x", 16), ("0o", 8), ("0b", 2)].into_iter()
        .find_map(|(prefix, radix)| Some((digits.strip_prefix(prefix)?, radix)))
        .unwrap_or((digits, 10));
    usize::from_str_radix(&digits.replace('_', ""), radix).ok()
}

pub(crate) fn ungroup(mut node: Node<'_>) -> Node<'_> {
    while node.kind() == "parenthesized_expression" {
        let Some(value) = children(node).next() else { break };
        node = value;
    }
    node
}

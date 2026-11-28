use super::{pattern_projection, Projection, Node};
use crate::lower::site::children;

const MAX_ALIAS_DEPTH: usize = 8;

fn definition<'tree>(consumer: Node<'tree>, source: &[u8]) -> Option<Node<'tree>> {
    let reference = reference(consumer)?;
    let statement = statement(consumer)?;
    if direct_consumer(statement) != Some(consumer) { return None; }
    let block = statement.parent().filter(|parent| parent.kind() == "block")?;
    let name = reference.utf8_text(source).ok()?;
    let local = std::iter::successors(statement.prev_named_sibling(), Node::prev_named_sibling)
        .find(|declaration| declared_name(*declaration, source) == Some(name))?;
    (sole_reference(local, block, name, source) == Some(reference.start_byte())).then_some(local)
}

fn statement(consumer: Node<'_>) -> Option<Node<'_>> {
    let expr = match consumer.kind() {
        "let_declaration" | "for_expression" | "match_expression" => consumer,
        "let_condition" => consumer.parent().filter(|owner| owner.kind() == "if_expression")?,
        _ => return None,
    };
    let value = super::branch_value::outermost_group(expr);
    let owner = value.parent()?;
    match owner.kind() {
        "expression_statement" => Some(owner),
        "let_declaration" if owner.child_by_field_name("value") == Some(value) => Some(owner),
        "block" => Some(value),
        _ => None,
    }
}

fn declared_name<'src>(node: Node<'_>, source: &'src [u8]) -> Option<&'src str> {
    if node.kind() != "let_declaration" || node.child_by_field_name("alternative").is_some()
        || children(node).any(|child| child.kind() == "mutable_specifier")
    {
        return None;
    }
    let binding = node.child_by_field_name("pattern")?;
    if binding.kind() != "identifier" { return None; }
    let name = binding.utf8_text(source).ok()?;
    name.bytes().all(|ch| ch.is_ascii_alphanumeric() || ch == b'_').then_some(name)
}

fn reference(consumer: Node<'_>) -> Option<Node<'_>> {
    let value = super::branch_value::ungroup_value(consumer.child_by_field_name("value")?);
    (value.kind() == "identifier").then_some(value)
}

fn sole_reference(local: Node<'_>, block: Node<'_>, name: &str, source: &[u8]) -> Option<usize> {
    let start = local.end_byte();
    let tail = std::str::from_utf8(source.get(start..block.end_byte())?).ok()?;
    // Count every spelling, including macro tokens, shorthand fields and
    // shadowing. More than one occurrence needs lexical and escape analysis.
    let mut matches = tail.match_indices(name).filter(|(offset, token)| {
        !tail[..*offset].chars().next_back().is_some_and(identifier_char)
            && !tail[offset + token.len()..].chars().next().is_some_and(identifier_char)
    });
    let (first, _) = matches.next()?;
    matches.next().is_none().then_some(start + first)
}

fn identifier_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

pub(super) fn source<'tree>(consumer: Node<'tree>, source: &[u8]) -> Option<(Node<'tree>, Node<'tree>)> {
    let mut declaration = definition(consumer, source)?;
    for _ in 0..=MAX_ALIAS_DEPTH {
        let value = super::branch_value::ungroup_value(declaration.child_by_field_name("value")?);
        if value.kind() != "identifier" { return Some((declaration, value)); }
        declaration = definition(declaration, source)?;
    }
    None
}

pub(super) fn projection<'tree>(consumer: Node<'tree>, source: &[u8]) -> Option<Projection<'tree>> {
    let pattern = if consumer.kind() == "match_expression" {
        super::super::match_flow::single_match_pattern(consumer)?
    } else {
        consumer.child_by_field_name("pattern")?
    };
    if !matches!(pattern.kind(), "tuple_pattern" | "slice_pattern" | "struct_pattern" | "tuple_struct_pattern") {
        return None;
    }
    let (_, value) = self::source(consumer, source)?;
    pattern_projection(consumer, value, source)
}

pub(super) enum Initializer<'tree> {
    Snapshot(Vec<Node<'tree>>),
    Forward,
}

pub(super) fn initializer<'tree>(declaration: Node<'tree>, source: &[u8]) -> Option<Initializer<'tree>> {
    let consumer = pattern_consumer(declaration, source)?;
    let (origin, _) = self::source(consumer, source)?;
    let projection = if consumer.kind() == "for_expression" {
        super::iteration::plan(consumer, source)?.items.into_iter().flatten().collect()
    } else {
        projection(consumer, source)?
    };
    if origin == declaration {
        Some(Initializer::Snapshot(projection.into_iter().map(|(_, value)| value).collect()))
    } else {
        Some(Initializer::Forward)
    }
}

fn pattern_consumer<'tree>(mut declaration: Node<'tree>, source: &[u8]) -> Option<Node<'tree>> {
    for _ in 0..=MAX_ALIAS_DEPTH {
        let consumer = next_consumer(declaration, source)?;
        if !is_alias(consumer, source) { return Some(consumer); }
        declaration = consumer;
    }
    None
}

fn is_alias(node: Node<'_>, source: &[u8]) -> bool {
    declared_name(node, source).is_some()
        && node.child_by_field_name("value").map(super::branch_value::ungroup_value)
            .is_some_and(|value| value.kind() == "identifier")
}

fn next_consumer<'tree>(declaration: Node<'tree>, source: &[u8]) -> Option<Node<'tree>> {
    let name = declared_name(declaration, source)?;
    let block = declaration.parent().filter(|parent| parent.kind() == "block")?;
    let offset = sole_reference(declaration, block, name, source)?;
    let statement = std::iter::successors(declaration.next_named_sibling(), Node::next_named_sibling)
        .find(|node| node.byte_range().contains(&offset))?;
    let consumer = direct_consumer(statement)?;
    (reference(consumer)?.start_byte() == offset).then_some(consumer)
}

fn direct_consumer(statement: Node<'_>) -> Option<Node<'_>> {
    let expr = if statement.kind() == "expression_statement" {
        children(statement).next()?
    } else {
        statement
    };
    match expr.kind() {
        "if_expression" => condition_consumer(expr),
        "let_declaration" => expr.child_by_field_name("value").map(super::branch_value::ungroup_value)
            .and_then(|value| match value.kind() {
                "match_expression" => Some(value),
                "if_expression" => condition_consumer(value),
                _ => None,
            })
            .or(Some(expr)),
        "match_expression" | "for_expression" => Some(expr),
        _ => None,
    }
}

fn condition_consumer(expr: Node<'_>) -> Option<Node<'_>> {
    expr.child_by_field_name("condition").filter(|node| node.kind() == "let_condition")
}

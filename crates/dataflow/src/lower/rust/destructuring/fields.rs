use super::{branch_value, patterns, Node};
use crate::lower::site::children;

pub(super) fn fields<'tree>(pattern: Node<'tree>, value: Node<'tree>, source: &[u8]) -> Option<Vec<(Node<'tree>, Node<'tree>)>> {
    match (pattern.kind(), value.kind()) {
        ("tuple_pattern", "tuple_expression") | ("slice_pattern", "array_expression") => sequence(pattern, value),
        ("struct_pattern", "struct_expression") => structure(pattern, value, source),
        ("tuple_struct_pattern", "call_expression") => tuple_variant(pattern, value, source),
        _ => None,
    }
}

fn sequence<'tree>(pattern: Node<'tree>, value: Node<'tree>) -> Option<Vec<(Node<'tree>, Node<'tree>)>> {
    if value.child_by_field_name("length").is_some() { return None; }
    let bindings: Vec<_> = patterns::pattern_parts(pattern).collect();
    if pattern.kind() == "tuple_pattern" && bindings.len() == 1
        && bindings[0].kind() != "remaining_field_pattern"
        && !pattern.children(&mut pattern.walk()).any(|child| child.kind() == ",")
    {
        return None;
    }
    align(bindings, children(value).collect())
}

fn tuple_variant<'tree>(pattern: Node<'tree>, value: Node<'tree>, source: &[u8]) -> Option<Vec<(Node<'tree>, Node<'tree>)>> {
    let ty = pattern.child_by_field_name("type")?;
    let constructor = value.child_by_field_name("function")?;
    if ty.utf8_text(source).ok()? != constructor.utf8_text(source).ok()? {
        return None;
    }
    let arity = super::super::match_flow::exhaustiveness::tuple_constructor_arity(constructor, source)?;
    let values: Vec<_> = children(value.child_by_field_name("arguments")?).collect();
    if values.len() == arity {
        align(patterns::pattern_parts(pattern).filter(|part| *part != ty).collect(), values)
    } else {
        None
    }
}

fn align<'tree>(bindings: Vec<Node<'tree>>, values: Vec<Node<'tree>>) -> Option<Vec<(Node<'tree>, Node<'tree>)>> {
    let Some(rest) = bindings.iter().position(|binding| binding.kind() == "remaining_field_pattern") else {
        return (bindings.len() == values.len()).then(|| bindings.into_iter().zip(values).collect());
    };
    let width = values.len().checked_sub(bindings.len() - 1)?;
    let (before, after) = bindings.split_at(rest);
    let pattern = before.iter().copied()
        .chain(std::iter::repeat_n(after[0], width))
        .chain(after[1..].iter().copied());
    Some(pattern.zip(values).collect())
}

fn structure<'tree>(pattern: Node<'tree>, value: Node<'tree>, source: &[u8]) -> Option<Vec<(Node<'tree>, Node<'tree>)>> {
    let ty = pattern.child_by_field_name("type")?;
    if ty.utf8_text(source).ok()? != value.child_by_field_name("name")?.utf8_text(source).ok()? {
        return None;
    }
    let mut members = std::collections::BTreeMap::new();
    let mut rest = None;
    for field in children(pattern).filter(|field| *field != ty) {
        if field.kind() == "remaining_field_pattern" {
            rest = Some(field);
        } else {
            let name = field.child_by_field_name("name")?;
            let binding = field.child_by_field_name("pattern").unwrap_or(name);
            if members.insert(name.utf8_text(source).ok()?, binding).is_some() { return None; }
        }
    }
    let mut paired = Vec::new();
    for field in children(value.child_by_field_name("body")?) {
        let (name, init) = field_initializer(field)?;
        let target = members.remove(name.utf8_text(source).ok()?).or(rest)?;
        paired.push((target, branch_value::ungroup_value(init)));
    }
    members.is_empty().then_some(paired)
}

fn field_initializer(field: Node<'_>) -> Option<(Node<'_>, Node<'_>)> {
    if children(field).any(|child| child.kind() == "attribute_item") {
        return None;
    }
    match field.kind() {
        "field_initializer" => Some((field.child_by_field_name("field")?, field.child_by_field_name("value")?)),
        "shorthand_field_initializer" => children(field).next().map(|name| (name, name)),
        _ => None,
    }
}

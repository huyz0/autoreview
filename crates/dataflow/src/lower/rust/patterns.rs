use super::Node;
use std::collections::BTreeMap;

pub(super) fn projected_names<'tree>(pattern: Node<'tree>, source: &[u8]) -> Option<Vec<Node<'tree>>> {
    let mut names = Vec::new();
    collect(pattern, &mut names, false, source)?;
    Some(names)
}

fn collect<'tree>(pattern: Node<'tree>, names: &mut Vec<Node<'tree>>, nested: bool, source: &[u8]) -> Option<()> {
    match pattern.kind() {
        "_" | "scoped_identifier" | "boolean_literal" | "integer_literal" | "char_literal"
        | "string_literal" => Some(()),
        "remaining_field_pattern" if nested => Some(()),
        "identifier" if nested => {
            names.push(pattern);
            Some(())
        }
        "tuple_pattern" | "tuple_struct_pattern" | "slice_pattern" | "struct_pattern" => {
            collect_fields(pattern, names, source)
        }
        "ref_pattern" | "mut_pattern" | "reference_pattern" => {
            let nested = pattern_parts(pattern)
                .find(|child| child.kind() != "mutable_specifier")?;
            collect(nested, names, true, source)
        }
        "captured_pattern" => collect_capture(pattern, names, source),
        "or_pattern" => collect_alternatives(pattern, names, source),
        _ => None,
    }
}

fn pattern_parts(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    node.children(&mut node.walk())
        .filter(|part| !part.is_extra() && (part.is_named() || part.kind() == "_"))
        .collect::<Vec<_>>()
        .into_iter()
}

fn collect_fields<'tree>(pattern: Node<'tree>, names: &mut Vec<Node<'tree>>, source: &[u8]) -> Option<()> {
    let constructor = pattern.child_by_field_name("type");
    if constructor.is_some_and(|node| !matches!(node.kind(),
        "identifier" | "scoped_identifier" | "type_identifier" | "scoped_type_identifier")) {
        return None;
    }
    for part in pattern_parts(pattern).filter(|part| Some(*part) != constructor) {
        if part.kind() == "field_pattern" {
            collect_field(part, names, source)?;
        } else {
            collect(part, names, true, source)?;
        }
    }
    Some(())
}

fn collect_field<'tree>(field: Node<'tree>, names: &mut Vec<Node<'tree>>, source: &[u8]) -> Option<()> {
    if let Some(pattern) = field.child_by_field_name("pattern") {
        return collect(pattern, names, true, source);
    }
    let shorthand = field.child_by_field_name("name")?;
    (shorthand.kind() == "shorthand_field_identifier").then(|| names.push(shorthand))
}

fn collect_capture<'tree>(pattern: Node<'tree>, names: &mut Vec<Node<'tree>>, source: &[u8]) -> Option<()> {
    let mut parts = pattern_parts(pattern);
    let binding = parts.next().filter(|part| part.kind() == "identifier")?;
    names.push(binding);
    // A bare identifier to the right of `@` may be a constant.
    let inner = parts.next()?;
    collect(inner, names, inner.kind() == "remaining_field_pattern", source)
}

fn collect_alternatives<'tree>(pattern: Node<'tree>, names: &mut Vec<Node<'tree>>, source: &[u8]) -> Option<()> {
    let mut alternatives = pattern_parts(pattern);
    let first = alternative_bindings(alternatives.next()?, source)?;
    let expected = binding_signature(&first, source)?;
    for alternative in alternatives {
        let bindings = alternative_bindings(alternative, source)?;
        if binding_signature(&bindings, source)? != expected {
            return None;
        }
    }
    names.extend(first);
    Some(())
}

fn alternative_bindings<'tree>(pattern: Node<'tree>, source: &[u8]) -> Option<Vec<Node<'tree>>> {
    // An irrefutable alternative would need a different failure edge.
    (!catch_all_alternative(pattern)).then(|| projected_names(pattern, source)).flatten()
}

fn catch_all_alternative(pattern: Node<'_>) -> bool {
    match pattern.kind() {
        "_" => true,
        "ref_pattern" | "mut_pattern" => pattern_parts(pattern)
            .find(|child| child.kind() != "mutable_specifier")
            .is_some_and(|child| child.kind() == "identifier" || catch_all_alternative(child)),
        "captured_pattern" => pattern_parts(pattern).nth(1).is_some_and(catch_all_alternative),
        "reference_pattern" => pattern_parts(pattern)
            .find(|child| child.kind() != "mutable_specifier")
            .is_some_and(catch_all_alternative),
        _ => false,
    }
}

#[derive(Default, PartialEq, Eq)]
struct BindingModifiers {
    by_ref: bool,
    mutable: bool,
}

fn binding_signature<'src>(bindings: &[Node<'_>], source: &'src [u8]) -> Option<BTreeMap<&'src str, BindingModifiers>> {
    let signature: BTreeMap<_, _> = bindings.iter().map(|binding| {
        let name = std::str::from_utf8(source.get(binding.byte_range())?).ok()?;
        Some((name, binding_modifiers(*binding)))
    }).collect::<Option<_>>()?;
    (signature.len() == bindings.len()).then_some(signature)
}

fn binding_modifiers(mut node: Node<'_>) -> BindingModifiers {
    let mut modifiers = BindingModifiers::default();
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "ref_pattern" => modifiers.by_ref = true,
            "mut_pattern" => modifiers.mutable = true,
            "captured_pattern" if parent.named_child(0) == Some(node) => {},
            "field_pattern" if parent.child_by_field_name("name") == Some(node) => {
                return shorthand_modifiers(parent);
            }
            _ => break,
        }
        node = parent;
    }
    modifiers
}

fn shorthand_modifiers(field: Node<'_>) -> BindingModifiers {
    let mut modifiers = BindingModifiers::default();
    for part in field.children(&mut field.walk()) {
        match part.kind() {
            "ref" => modifiers.by_ref = true,
            "mutable_specifier" => modifiers.mutable = true,
            _ => {},
        }
    }
    modifiers
}

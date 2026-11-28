use super::Node;

pub(super) fn projected_names(pattern: Node<'_>) -> Option<Vec<Node<'_>>> {
    let mut names = Vec::new();
    collect(pattern, &mut names, false)?;
    Some(names)
}

fn collect<'tree>(pattern: Node<'tree>, names: &mut Vec<Node<'tree>>, nested: bool) -> Option<()> {
    match pattern.kind() {
        "_" | "scoped_identifier" | "boolean_literal" | "integer_literal" | "char_literal"
        | "string_literal" => Some(()),
        "remaining_field_pattern" if nested => Some(()),
        "identifier" if nested => {
            names.push(pattern);
            Some(())
        }
        "tuple_pattern" | "tuple_struct_pattern" | "slice_pattern" | "struct_pattern" => {
            collect_fields(pattern, names)
        }
        "ref_pattern" | "mut_pattern" | "reference_pattern" => {
            let nested = pattern_parts(pattern)
                .find(|child| child.kind() != "mutable_specifier")?;
            collect(nested, names, true)
        }
        "captured_pattern" => collect_capture(pattern, names),
        "or_pattern" => collect_alternatives(pattern),
        _ => None,
    }
}

fn pattern_parts(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    node.children(&mut node.walk())
        .filter(|part| !part.is_extra() && (part.is_named() || part.kind() == "_"))
        .collect::<Vec<_>>()
        .into_iter()
}

fn collect_fields<'tree>(pattern: Node<'tree>, names: &mut Vec<Node<'tree>>) -> Option<()> {
    let constructor = pattern.child_by_field_name("type");
    if constructor.is_some_and(|node| !matches!(node.kind(),
        "identifier" | "scoped_identifier" | "type_identifier" | "scoped_type_identifier")) {
        return None;
    }
    for part in pattern_parts(pattern).filter(|part| Some(*part) != constructor) {
        if part.kind() == "field_pattern" {
            collect_field(part, names)?;
        } else {
            collect(part, names, true)?;
        }
    }
    Some(())
}

fn collect_field<'tree>(field: Node<'tree>, names: &mut Vec<Node<'tree>>) -> Option<()> {
    if let Some(pattern) = field.child_by_field_name("pattern") {
        return collect(pattern, names, true);
    }
    let shorthand = field.child_by_field_name("name")?;
    (shorthand.kind() == "shorthand_field_identifier").then(|| names.push(shorthand))
}

fn collect_capture<'tree>(pattern: Node<'tree>, names: &mut Vec<Node<'tree>>) -> Option<()> {
    let mut parts = pattern_parts(pattern);
    let binding = parts.next().filter(|part| part.kind() == "identifier")?;
    names.push(binding);
    // A bare identifier to the right of `@` may be a constant.
    let inner = parts.next()?;
    collect(inner, names, inner.kind() == "remaining_field_pattern")
}

fn collect_alternatives(pattern: Node<'_>) -> Option<()> {
    let mut bindings = Vec::new();
    for arm in pattern_parts(pattern) {
        if arm.kind() == "_" {
            return None;
        }
        collect(arm, &mut bindings, false)?;
    }
    // Binding alternatives need one shared identity across the matches.
    bindings.is_empty().then_some(())
}

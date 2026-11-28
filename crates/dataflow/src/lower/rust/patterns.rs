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
        "identifier" if nested => {
            names.push(pattern);
            Some(())
        }
        "tuple_pattern" | "tuple_struct_pattern" => collect_tuple(pattern, names),
        "ref_pattern" | "mut_pattern" | "reference_pattern" => {
            let nested = crate::lower::site::children(pattern)
                .find(|child| child.kind() != "mutable_specifier")?;
            collect(nested, names, true)
        }
        "or_pattern" => {
            let mut alternatives = Vec::new();
            for arm in pattern.children(&mut pattern.walk()).filter(|arm| !arm.is_extra() && arm.kind() != "|") {
                if arm.kind() == "_" {
                    return None;
                }
                collect(arm, &mut alternatives, false)?;
            }
            // Binding alternatives need one shared identity across the matches.
            alternatives.is_empty().then_some(())
        }
        _ => None,
    }
}

fn collect_tuple<'tree>(pattern: Node<'tree>, names: &mut Vec<Node<'tree>>) -> Option<()> {
    let constructor = pattern.child_by_field_name("type");
    if constructor.is_some_and(|node| !matches!(node.kind(), "identifier" | "scoped_identifier")) {
        return None;
    }
    let mut cursor = pattern.walk();
    for part in pattern.children(&mut cursor).filter(|part| {
        Some(*part) != constructor && !part.is_extra() && !matches!(part.kind(), "(" | ")" | ",")
    }) {
        collect(part, names, true)?;
    }
    Some(())
}

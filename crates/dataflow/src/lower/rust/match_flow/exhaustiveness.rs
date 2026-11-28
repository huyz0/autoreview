use super::{arm_pattern, children, Node};
use std::collections::BTreeSet;

#[derive(PartialEq)]
enum Payload {
    Unit,
    Tuple(usize),
    Struct,
}

pub(super) fn covers_enum(node: Node<'_>, arms: &[Node<'_>], source: &[u8]) -> bool {
    if node.end_byte() > source.len() {
        return false;
    }
    let Some((owner, _, _)) = arms.first().and_then(|arm| variant_pattern(*arm, source)) else {
        return false;
    };
    let Some(variants) = enum_variants(node, owner, source) else {
        return false;
    };
    let mut seen = BTreeSet::new();
    arms.len() == variants.len() && arms.iter().all(|arm| {
        let Some((path, name, pattern)) = variant_pattern(*arm, source) else { return false };
        path == owner && seen.insert(name) && variants.iter().any(|(variant, payload)| {
            name == *variant && covers_payload(pattern, payload)
        })
    })
}

fn variant_pattern<'tree, 'src>(arm: Node<'tree>, source: &'src [u8]) -> Option<(&'src str, &'src str, Node<'tree>)> {
    let pattern = arm.child_by_field_name("pattern")?;
    if pattern.child_by_field_name("condition").is_some()
        || children(arm).any(|node| matches!(node.kind(), "attribute_item" | "inner_attribute_item"))
    {
        return None;
    }
    let variant = arm_pattern(arm, source)?;
    let path = variant.child_by_field_name("type").unwrap_or(variant);
    let owner = path.child_by_field_name("path")?;
    if owner.child_by_field_name("path")?.kind() != "self" {
        return None;
    }
    let owner = owner.child_by_field_name("name")?.utf8_text(source).ok()?;
    let name = path.child_by_field_name("name")?.utf8_text(source).ok()?;
    Some((owner, name, variant))
}

fn enum_variants<'src>(node: Node<'_>, owner: &str, source: &'src [u8]) -> Option<Vec<(&'src str, Payload)>> {
    let module = std::iter::successors(node.parent(), |node| node.parent())
        .find(|scope| matches!(scope.kind(), "source_file" | "mod_item"))?;
    if module.end_byte() > source.len() {
        return None;
    }
    let items = module.child_by_field_name("body").unwrap_or(module);
    let mut matching = children(items).filter(|item| {
        item.kind() == "enum_item"
            && item.child_by_field_name("name").is_some_and(|name| name.utf8_text(source) == Ok(owner))
    });
    let declaration = matching.next()?;
    if matching.next().is_some() || declaration.has_error() || has_item_attributes(declaration) {
        return None;
    }
    children(declaration.child_by_field_name("body")?)
        .map(|variant| variant_decl(variant, source))
        .collect()
}

fn has_item_attributes(item: Node<'_>) -> bool {
    let mut previous = item.prev_named_sibling();
    while let Some(node) = previous {
        match node.kind() {
            "attribute_item" => return true,
            "line_comment" | "block_comment" => previous = node.prev_named_sibling(),
            _ => return false,
        }
    }
    false
}

fn variant_decl<'src>(variant: Node<'_>, source: &'src [u8]) -> Option<(&'src str, Payload)> {
    if variant.kind() != "enum_variant" {
        return None;
    }
    let name = variant.child_by_field_name("name")?.utf8_text(source).ok()?;
    let payload = match variant.child_by_field_name("body") {
        None => Payload::Unit,
        Some(body) if body.kind() == "field_declaration_list" => Payload::Struct,
        Some(body) if body.kind() == "ordered_field_declaration_list" => {
            if children(body).any(|field| field.kind() == "attribute_item") {
                return None;
            }
            Payload::Tuple(body.children_by_field_name("type", &mut body.walk()).count())
        }
        Some(_) => return None,
    };
    Some((name, payload))
}

fn covers_payload(pattern: Node<'_>, payload: &Payload) -> bool {
    let constructor = pattern.child_by_field_name("type");
    let mut cursor = pattern.walk();
    let mut parts = pattern.children(&mut cursor)
        .filter(|part| Some(*part) != constructor && !part.is_extra())
        .filter(|part| part.is_named() || part.kind() == "_");
    match (payload, pattern.kind()) {
        (Payload::Unit, "scoped_identifier") => true,
        (Payload::Struct, "struct_pattern") => parts.next().is_some_and(|part| {
            part.kind() == "remaining_field_pattern" && parts.next().is_none()
        }),
        (Payload::Tuple(arity), "tuple_struct_pattern") => {
            let parts: Vec<_> = parts.collect();
            if let [part] = parts.as_slice() {
                if part.kind() == "remaining_field_pattern" {
                    return true;
                }
            }
            parts.len() == *arity && parts.into_iter().all(wildcard_payload)
        }
        _ => false,
    }
}

fn wildcard_payload(part: Node<'_>) -> bool {
    part.kind() == "_" || (part.kind() == "captured_pattern"
        && part.children(&mut part.walk()).any(|child| child.kind() == "_"))
}

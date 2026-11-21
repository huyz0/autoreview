use tree_sitter::Node;

use super::super::{children, class_members, containing, descendants, Java};
use super::mapping::{Entities, Entity};

pub(super) fn iterated_entity<'a>(
    java: &Java<'_>,
    loop_node: Node<'_>,
    entities: &'a Entities,
) -> Option<&'a Entity> {
    let row_type = java.field(loop_node, "type")?;
    let entity = resolve_entity(java, row_type, entities)?;
    let iterable = loop_node.child_by_field_name("value")?;
    let results = if iterable.kind() == "identifier" {
        preceding_initializer(java, loop_node, iterable)?
    } else {
        iterable
    };
    let query = typed_query(java, results, row_type)?;
    (selects_entity(java, query, entity) && known_manager(java, loop_node, query)).then_some(entity)
}

fn resolve_entity<'a>(java: &Java<'_>, name: &str, entities: &'a Entities) -> Option<&'a Entity> {
    if name.contains('.') {
        return entities.get(name);
    }
    if let Some(qualified) = java.imports.get(name) {
        return entities.get(*qualified);
    }
    if java.package.is_empty() {
        return entities.get(name);
    }
    entities.get(&format!("{}.{name}", java.package))
}

fn preceding_initializer<'tree>(
    java: &Java<'_>,
    loop_node: Node<'tree>,
    iterable: Node<'tree>,
) -> Option<Node<'tree>> {
    let mut previous = loop_node.prev_named_sibling()?;
    while matches!(previous.kind(), "line_comment" | "block_comment") {
        previous = previous.prev_named_sibling()?;
    }
    if previous.kind() != "local_variable_declaration" {
        return None;
    }
    children(previous)
        .filter(|node| node.kind() == "variable_declarator")
        .find(|node| java.field(*node, "name") == Some(java.text(iterable)))?
        .child_by_field_name("value")
}

fn typed_query<'tree>(
    java: &Java<'_>,
    results: Node<'tree>,
    row_type: &str,
) -> Option<Node<'tree>> {
    let query = results.child_by_field_name("object")?;
    if java.field(results, "name") != Some("getResultList")
        || java.field(query, "name") != Some("createQuery")
    {
        return None;
    }
    let args = query.child_by_field_name("arguments")?;
    let literal = args.named_child(1)?.named_child(0)?;
    (args.named_child_count() == 2
        && java.text(literal) == row_type
        && args.named_child(1)?.kind() == "class_literal")
        .then_some(query)
}

fn selects_entity(java: &Java<'_>, query: Node<'_>, entity: &Entity) -> bool {
    let Some(args) = query.child_by_field_name("arguments") else {
        return false;
    };
    let Some(literal) = args
        .named_child(0)
        .filter(|node| node.kind() == "string_literal")
    else {
        return false;
    };
    let Some(fragment) = literal
        .named_child(0)
        .filter(|node| node.kind() == "string_fragment")
    else {
        return false;
    };
    let words: Vec<_> = java.text(fragment).split_whitespace().collect();
    let Some(from) = words
        .iter()
        .position(|word| word.eq_ignore_ascii_case("from"))
    else {
        return false;
    };
    simple_entity_projection(&words, from)
        && words
            .get(from + 1)
            .is_some_and(|name| *name == entity.simple_name)
        && !words.iter().any(|word| word.eq_ignore_ascii_case("fetch"))
}

fn simple_entity_projection(words: &[&str], from: usize) -> bool {
    if from == 0 {
        return true;
    }
    let Some(alias) = words.get(from + 2) else {
        return false;
    };
    let selection = &words[..from];
    match selection {
        [select, value] => select.eq_ignore_ascii_case("select") && value == alias,
        [select, distinct, value] => {
            select.eq_ignore_ascii_case("select")
                && distinct.eq_ignore_ascii_case("distinct")
                && value == alias
        }
        _ => false,
    }
}

fn known_manager(java: &Java<'_>, at: Node<'_>, query: Node<'_>) -> bool {
    let Some(object) = query.child_by_field_name("object") else {
        return false;
    };
    let name = match object.kind() {
        "identifier" => java.text(object),
        "field_access"
            if object
                .child_by_field_name("object")
                .is_some_and(|obj| obj.kind() == "this") =>
        {
            let Some(name) = java.field(object, "field") else {
                return false;
            };
            name
        }
        _ => return false,
    };
    if object.kind() == "identifier" {
        if let Some(is_manager) = local_manager(java, at, name) {
            return is_manager;
        }
    }
    containing(at, "class_declaration").is_some_and(|class| {
        class_members(class)
            .filter(|field| field.kind() == "field_declaration")
            .any(|field| {
                manager_type(java, field)
                    && children(field).any(|var| {
                        var.kind() == "variable_declarator" && java.field(var, "name") == Some(name)
                    })
            })
    })
}

fn local_manager(java: &Java<'_>, at: Node<'_>, name: &str) -> Option<bool> {
    if containing(at, "lambda_expression").is_some() {
        return Some(false);
    }
    let method = containing(at, "method_declaration")?;
    let params = method.child_by_field_name("parameters")?;
    if let Some(parameter) = children(params).find(|param| java.field(*param, "name") == Some(name))
    {
        return Some(manager_type(java, parameter));
    }
    let shadows_field = descendants(method).any(|node| {
        matches!(
            node.kind(),
            "variable_declarator" | "enhanced_for_statement" | "catch_formal_parameter"
        ) && java.field(node, "name") == Some(name)
    });
    shadows_field.then_some(false)
}

fn manager_type(java: &Java<'_>, node: Node<'_>) -> bool {
    java.field(node, "type").is_some_and(|ty| {
        [
            "jakarta.persistence.EntityManager",
            "javax.persistence.EntityManager",
            "org.hibernate.Session",
        ]
        .into_iter()
        .any(|qualified| java.resolves(ty, qualified))
    })
}

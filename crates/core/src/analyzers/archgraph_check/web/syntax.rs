use std::ops::RangeInclusive;

use autoreview_langsupport::{parser_for, Language};
use tree_sitter::Node;

pub(super) fn imports(
    language: Language,
    source: &str,
    common_js: bool,
) -> Vec<(String, RangeInclusive<u32>)> {
    let Some(tree) = parser_for(language).and_then(|mut parser| parser.parse(source, None)) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let mut cursor = root.walk();
    let statements: Vec<_> = root.named_children(&mut cursor).collect();
    let mut imports: Vec<_> = statements
        .iter()
        .copied()
        .filter(|stmt| {
            matches!(stmt.kind(), "import_statement" | "export_statement") && !stmt.has_error()
        })
        .filter(|stmt| !type_only(*stmt))
        .filter_map(|stmt| literal(stmt, stmt.child_by_field_name("source")?, source))
        .collect();
    if common_js
        && !statements
            .iter()
            .any(|stmt| declares_require(*stmt, source))
    {
        imports.extend(
            statements
                .into_iter()
                .flat_map(require_calls)
                .filter_map(|call| literal(call, require_source(call, source)?, source)),
        );
    }
    imports
}

fn literal(
    statement: Node<'_>,
    node: Node<'_>,
    source: &str,
) -> Option<(String, RangeInclusive<u32>)> {
    if node.kind() != "string" {
        return None;
    }
    let raw = node.utf8_text(source.as_bytes()).ok()?;
    let specifier = raw
        .strip_prefix('"')
        .and_then(|raw| raw.strip_suffix('"'))
        .or_else(|| {
            raw.strip_prefix('\'')
                .and_then(|raw| raw.strip_suffix('\''))
        })?;
    let start = statement.start_position().row as u32 + 1;
    let end = node.end_position().row as u32 + 1;
    Some((specifier.to_owned(), start..=end))
}

fn declarations(stmt: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    children(stmt).filter(|node| node.kind() == "variable_declarator")
}

fn declares_require(stmt: Node<'_>, source: &str) -> bool {
    let is_require = |node: Node<'_>| node.utf8_text(source.as_bytes()) == Ok("require");
    stmt.child_by_field_name("name").is_some_and(is_require)
        || declarations(stmt).any(|node| node.child_by_field_name("name").is_some_and(is_require))
}

fn require_calls(stmt: Node<'_>) -> Vec<Node<'_>> {
    match stmt.kind() {
        "lexical_declaration" | "variable_declaration" => declarations(stmt)
            .filter_map(|binding| binding.child_by_field_name("value"))
            .collect(),
        "expression_statement" => stmt.named_child(0).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn require_source<'a>(call: Node<'a>, source: &str) -> Option<Node<'a>> {
    if call.kind() != "call_expression" || call.has_error() {
        return None;
    }
    let function = call.child_by_field_name("function")?;
    if function.kind() != "identifier" || function.utf8_text(source.as_bytes()).ok()? != "require" {
        return None;
    }
    let args = call.child_by_field_name("arguments")?;
    (args.named_child_count() == 1)
        .then(|| args.named_child(0))
        .flatten()
}

fn type_only(statement: Node<'_>) -> bool {
    if has_type_token(statement) {
        return true;
    }
    let Some(clause) =
        children(statement).find(|node| matches!(node.kind(), "import_clause" | "export_clause"))
    else {
        return false;
    };
    if clause.kind() == "export_clause" {
        return all_type_specifiers(clause);
    }
    let parts: Vec<_> = children(clause)
        .filter(|node| node.kind() != "comment")
        .collect();
    matches!(parts.as_slice(), [named] if named.kind() == "named_imports" && all_type_specifiers(*named))
}

fn all_type_specifiers(clause: Node<'_>) -> bool {
    let mut specifiers = children(clause)
        .filter(|node| matches!(node.kind(), "import_specifier" | "export_specifier"));
    specifiers
        .next()
        .is_some_and(|first| has_type_token(first) && specifiers.all(has_type_token))
}

fn has_type_token(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    let mut parts = node.children(&mut cursor);
    parts.any(|part| !part.is_named() && part.kind() == "type")
}

fn children(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .collect::<Vec<_>>()
        .into_iter()
}

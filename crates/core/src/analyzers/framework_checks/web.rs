mod bindings;

pub(super) use bindings::BindingScopes;

use std::path::Path;

use autoreview_langsupport::Language;
use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::{children, descendants, text};

pub(super) fn imports_from<'t, 's>(
    root: Node<'t>, source: &'s str, module: &'s str,
) -> impl Iterator<Item = Node<'t>> + use<'t, 's> {
    children(root).filter(move |node| {
        node.kind() == "import_statement"
            && field_text(*node, "source", source)
                .is_some_and(|name| name.trim_matches(['\'', '"']) == module)
    })
}

pub(super) fn named_imports<'t, 's>(
    root: Node<'t>, source: &'s str, module: &'s str,
) -> impl Iterator<Item = (&'s str, &'s str)> + use<'t, 's> {
    imports_from(root, source, module).flat_map(descendants).filter_map(move |node| {
        if node.kind() != "import_specifier" {
            return None;
        }
        let original = field_text(node, "name", source)?;
        Some((original, field_text(node, "alias", source).unwrap_or(original)))
    })
}

pub(super) fn run(
    root: &Path, paths: &[String], check: impl Fn(&str, &str, Node<'_>) -> Vec<AgentFinding>,
) -> Vec<AgentFinding> {
    super::run_sources(root, paths, |language| matches!(language, Language::JavaScript | Language::TypeScript | Language::Tsx),
        |path, source, root, _| check(path, source, root))
}

pub(super) fn first_argument(call: Node<'_>) -> Option<Node<'_>> {
    children(call.child_by_field_name("arguments")?).find(|node| node.kind() != "comment")
}

pub(super) fn binding_nodes(node: Node<'_>) -> Vec<Node<'_>> {
    let binding = match node.kind() {
        "variable_declarator" | "function_declaration" | "class_declaration" => node.child_by_field_name("name"),
        "required_parameter" | "optional_parameter" => node.child_by_field_name("pattern"),
        "catch_clause" | "arrow_function" => node.child_by_field_name("parameter"),
        "for_in_statement" if loop_binding_kind(node).is_some() => node.child_by_field_name("left"),
        "formal_parameters" => Some(node),
        _ => None,
    };
    binding.into_iter().flat_map(pattern_bindings).collect()
}

fn loop_binding_kind(node: Node<'_>) -> Option<&str> {
    let mut cursor = node.walk();
    let token = node.children_by_field_name("kind", &mut cursor)
        .find(|token| matches!(token.kind(), "const" | "let" | "var" | "using"))?;
    Some(token.kind())
}

fn pattern_bindings(pattern: Node<'_>) -> Vec<Node<'_>> {
    match pattern.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => vec![pattern],
        "object_pattern" | "array_pattern" | "rest_pattern" | "formal_parameters" => {
            children(pattern).flat_map(pattern_bindings).collect()
        }
        "assignment_pattern" => pattern.child_by_field_name("left")
            .into_iter().flat_map(pattern_bindings).collect(),
        "pair_pattern" => pattern.child_by_field_name("value")
            .into_iter().flat_map(pattern_bindings).collect(),
        _ => Vec::new(),
    }
}

pub(super) fn is_function(node: Node<'_>) -> bool {
    matches!(node.kind(), "arrow_function" | "function_expression" | "function_declaration" | "method_definition")
}

pub(super) fn field_text<'s>(node: Node<'_>, field: &str, source: &'s str) -> Option<&'s str> {
    node.child_by_field_name(field).map(|part| text(part, source))
}

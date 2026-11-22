use std::path::Path;

use autoreview_langsupport::{language_for_path, parser_for, Language};
use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::{children, parse_source, text};

pub(super) fn run(
    root: &Path, paths: &[String], check: impl Fn(&str, &str, Node<'_>) -> Vec<AgentFinding>,
) -> Vec<AgentFinding> {
    let Ok(root) = root.canonicalize() else { return Vec::new() };
    let mut findings = Vec::new();
    for path in paths {
        let absolute = root.join(path);
        let Some(language @ (Language::JavaScript | Language::TypeScript | Language::Tsx)) =
            language_for_path(&absolute)
        else {
            continue;
        };
        let Some(mut parser) = parser_for(language) else { continue };
        let Some(resolved) = absolute.canonicalize().ok().filter(|file| file.starts_with(&root)) else { continue };
        if !absolute.symlink_metadata().is_ok_and(|meta| meta.is_file()) {
            continue;
        }
        let Some((source, tree)) = parse_source(&mut parser, &resolved) else { continue };
        findings.extend(check(path, &source, tree.root_node()));
    }
    findings
}

pub(super) fn first_argument(call: Node<'_>) -> Option<Node<'_>> {
    children(call.child_by_field_name("arguments")?).find(|node| node.kind() != "comment")
}

pub(super) fn binding_nodes(node: Node<'_>) -> Vec<Node<'_>> {
    let binding = match node.kind() {
        "variable_declarator" | "function_declaration" | "class_declaration" => node.child_by_field_name("name"),
        "required_parameter" | "optional_parameter" => node.child_by_field_name("pattern"),
        "catch_clause" | "arrow_function" => node.child_by_field_name("parameter"),
        "formal_parameters" => Some(node),
        _ => None,
    };
    binding.into_iter().flat_map(pattern_bindings).collect()
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

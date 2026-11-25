use crate::lower::site::{children, identifier};
use tree_sitter::Node;

pub(super) struct Export {
    pub(super) name: String,
    pub(super) owner: Option<String>,
    pub(super) visible: bool,
    pub(super) default: bool,
}

pub(super) fn export(node: Node<'_>, source: &[u8], name: String) -> Export {
    let parent = node.parent();
    let owner = parent
        .filter(|parent| matches!(parent.kind(), "class_body" | "interface_body" | "enum_body"))
        .and_then(|parent| parent.parent())
        .and_then(|parent| parent.child_by_field_name("name"))
        .and_then(|name| name.utf8_text(source).ok())
        .map(str::to_owned);
    let signature = node
        .child_by_field_name("name")
        .and_then(|name| source.get(node.start_byte()..name.start_byte()))
        .unwrap_or_default();
    let has = |expected: &[u8]| {
        signature
            .split(|byte| !byte.is_ascii_alphanumeric())
            .any(|word| word == expected)
    };
    let visible = match node.kind() {
        "method_declaration" => has(b"static") && !has(b"private"),
        "function_item" => has(b"pub"),
        "arrow_function" | "function_expression" => web_value_visible(parent),
        _ if parent.is_some_and(|parent| parent.kind() == "export_statement") => true,
        _ if node
            .child_by_field_name("parameters")
            .is_some_and(|params| params.kind() == "parameter_list") =>
        {
            name.chars().next().is_some_and(char::is_uppercase)
        }
        _ => parent.is_some_and(|parent| parent.kind() != "program") && !has(b"private"),
    };
    let default = parent.is_some_and(|parent| {
        parent.kind() == "export_statement"
            && parent
                .children(&mut parent.walk())
                .any(|node| node.kind() == "default")
    });
    Export {
        name,
        owner,
        visible,
        default,
    }
}

fn web_value_visible(parent: Option<Node<'_>>) -> bool {
    parent.is_some_and(|parent| parent.kind() == "export_statement"
        || (parent.kind() == "variable_declarator"
            && parent.parent().and_then(|node| node.parent()).is_some_and(|node| node.kind() == "export_statement")))
}

pub(super) fn candidate(node: Node<'_>, source: &[u8]) -> Option<String> {
    if matches!(node.kind(), "arrow_function" | "function_expression") {
        return web_binding_name(node, source);
    }
    let owner = node.parent()?;
    let local_owner = match node.kind() {
        "method_declaration" => {
            matches!(owner.kind(), "class_body" | "interface_body" | "enum_body")
        }
        "function_declaration" => matches!(
            owner.kind(),
            "source_file" | "program" | "export_statement" | "class_body"
        ),
        "function_item" => owner.kind() == "source_file",
        _ => false,
    };
    if !local_owner
        || node.has_error()
        || children(node).any(|child| child.kind() == "receiver_type")
    {
        return None;
    }
    let name = node.child_by_field_name("name")?;
    let signature = source.get(node.start_byte()..name.start_byte())?;
    if signature
        .split(|byte| !byte.is_ascii_alphanumeric())
        .any(|word| word == b"async" || word == b"suspend")
    {
        return None;
    }
    Some(name.utf8_text(source).ok()?.to_owned())
}

fn web_binding_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    if node.has_error() || node.children(&mut node.walk()).any(|child| child.kind() == "async") {
        return None;
    }
    if let Some(export) = node.parent().filter(|parent| parent.kind() == "export_statement") {
        return export.children(&mut export.walk()).any(|child| child.kind() == "default").then(|| "default".to_owned());
    }
    let binding = node.parent().filter(|parent| parent.kind() == "variable_declarator")?;
    let declaration = binding.parent().filter(|parent| parent.kind() == "lexical_declaration")?;
    if binding.child_by_field_name("value") != Some(node)
        || !declaration.children(&mut declaration.walk()).any(|child| child.kind() == "const")
        || !declaration.parent().is_some_and(|parent| matches!(parent.kind(), "program" | "export_statement"))
    {
        return None;
    }
    let name = binding.child_by_field_name("name").filter(|name| identifier(*name))?;
    Some(name.utf8_text(source).ok()?.to_owned())
}

pub(super) fn parameters(node: Node<'_>, source: &[u8]) -> Option<Vec<String>> {
    if node.kind() == "arrow_function" {
        if let Some(parameter) = node.child_by_field_name("parameter") {
            return identifier(parameter).then(|| parameter.utf8_text(source).ok().map(|name| vec![name.to_owned()])).flatten();
        }
    }
    let list = node
        .child_by_field_name("parameters")
        .or_else(|| children(node).find(|child| child.kind() == "function_value_parameters"))?;
    let mut params = Vec::new();
    for parameter in children(list).filter(|param| param.kind() != "comment") {
        if matches!(
            parameter.kind(),
            "variadic_parameter_declaration" | "spread_parameter" | "rest_pattern"
        ) {
            return None;
        }
        let mut names = parameter
            .children_by_field_name("name", &mut parameter.walk())
            .collect::<Vec<_>>();
        if names.is_empty() {
            names.push(parameter_name(parameter)?);
        }
        for name in names {
            if !identifier(name) {
                return None;
            }
            params.push(name.utf8_text(source).ok()?.to_owned());
        }
    }
    Some(params)
}

fn parameter_name(parameter: Node<'_>) -> Option<Node<'_>> {
    if identifier(parameter) {
        return Some(parameter);
    }
    if !matches!(
        parameter.kind(),
        "parameter" | "formal_parameter" | "required_parameter" | "parameter_declaration"
    ) {
        return None;
    }
    parameter
        .child_by_field_name("pattern")
        .or_else(|| children(parameter).find(|child| identifier(*child)))
}

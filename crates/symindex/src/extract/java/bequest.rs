use std::sync::Arc;

use tree_sitter::Node;

use super::text;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct JavaScope {
    pub(crate) package: String,
    pub(crate) local_types: Vec<String>,
    pub(crate) imports: Vec<String>,
    pub(crate) wildcards: Vec<String>,
    pub(crate) static_imports: Vec<String>,
    pub(crate) static_wildcard: bool,
}

impl JavaScope {
    pub(crate) fn extract(root: Node, source: &[u8]) -> Self {
        let mut scope = Self::default();
        let mut cursor = root.walk();
        for node in root.named_children(&mut cursor) {
            match node.kind() {
                "package_declaration" => scope.package = declaration_path(node, source).unwrap_or_default(),
                "import_declaration" => scope.add_import(node, source),
                _ => scope.local_types.extend(type_declaration_name(node, source).map(str::to_owned)),
            }
        }
        scope
    }

    fn add_import(&mut self, node: Node, source: &[u8]) {
        let Some(path) = declaration_path(node, source) else { return };
        let mut cursor = node.walk();
        let wildcard = node.children(&mut cursor).any(|child| child.kind() == "asterisk");
        let is_static = node.children(&mut cursor).any(|child| child.kind() == "static");
        match (is_static, wildcard) {
            (false, false) => self.imports.push(path),
            (false, true) => self.wildcards.push(path),
            (true, false) => self.static_imports.push(path),
            (true, true) => self.static_wildcard = true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaClass {
    pub(crate) scope: Arc<JavaScope>,
    pub(crate) parent: Option<String>,
    pub(crate) public: bool,
    pub(crate) member_types: Vec<String>,
    pub(crate) signatures: Vec<Option<JavaMethod>>,
    type_parameters: Vec<String>,
}

impl JavaClass {
    pub(super) fn extract(node: Node, source: &[u8], scope: &Arc<JavaScope>) -> Option<Self> {
        // Nested and local classes need lexical type resolution.
        if node.parent()?.kind() != "program" {
            return None;
        }
        let body = node.child_by_field_name("body")?;
        let mut cursor = body.walk();
        let member_types = body.named_children(&mut cursor)
            .filter_map(|child| type_declaration_name(child, source).map(str::to_owned)).collect();
        let parent = node.child_by_field_name("superclass").and_then(first_syntax_child)
            .and_then(|node| name_path(type_head(node)?, source));
        Some(Self {
            scope: Arc::clone(scope),
            parent,
            public: has_modifier(node, "public"),
            member_types,
            signatures: Vec::new(),
            type_parameters: type_parameters(node, source),
        })
    }

    pub(super) fn add_method(&mut self, node: Node, source: &[u8]) {
        self.signatures.push(JavaMethod::extract(node, source, &self.type_parameters));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JavaMethod {
    pub(crate) parameters: Vec<JavaParameter>,
    pub(crate) package_private: bool,
    pub(crate) is_final: bool,
}

impl JavaMethod {
    fn extract(node: Node, source: &[u8], type_parameters: &[String]) -> Option<Self> {
        if node.has_error() || has_modifier(node, "static") || has_modifier(node, "private")
            || node.child_by_field_name("type_parameters").is_some() {
            return None;
        }
        let parameters = node.child_by_field_name("parameters")?;
        let mut cursor = parameters.walk();
        let parameters = parameters.named_children(&mut cursor)
            .filter(|param| !param.is_extra() && param.kind() != "receiver_parameter")
            .map(|param| read_parameter(param, source, type_parameters)).collect::<Option<_>>()?;
        Some(Self {
            parameters,
            package_private: !has_modifier(node, "public") && !has_modifier(node, "protected"),
            is_final: has_modifier(node, "final"),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JavaParameter {
    pub(crate) name: String,
    pub(crate) primitive: bool,
    pub(crate) dimensions: usize,
    pub(crate) arguments: Vec<Self>,
}

fn read_parameter(node: Node, source: &[u8], type_parameters: &[String]) -> Option<JavaParameter> {
    let (ty, dimensions) = match node.kind() {
        "formal_parameter" => (node.child_by_field_name("type")?, array_rank(node.child_by_field_name("dimensions"))),
        "spread_parameter" => {
            let mut cursor = node.walk();
            let ty = node.named_children(&mut cursor).find(|child| !child.is_extra()
                && !matches!(child.kind(), "modifiers" | "annotation" | "marker_annotation" | "variable_declarator"))?;
            (ty, 1)
        }
        _ => return None,
    };
    let mut ty = read_type(ty, source, type_parameters)?;
    ty.dimensions += dimensions;
    Some(ty)
}

fn read_type(node: Node, source: &[u8], type_parameters: &[String]) -> Option<JavaParameter> {
    match node.kind() {
        "array_type" => {
            let mut ty = read_type(node.child_by_field_name("element")?, source, type_parameters)?;
            ty.dimensions += array_rank(node.child_by_field_name("dimensions"));
            Some(ty)
        }
        "annotated_type" => {
            let mut cursor = node.walk();
            let ty = node.named_children(&mut cursor).find(|child| !child.is_extra()
                && !matches!(child.kind(), "annotation" | "marker_annotation"))?;
            read_type(ty, source, type_parameters)
        }
        _ => read_named_type(node, source, type_parameters),
    }
}

fn read_named_type(node: Node, source: &[u8], type_parameters: &[String]) -> Option<JavaParameter> {
    let head = type_head(node)?;
    let primitive = matches!(head.kind(), "integral_type" | "floating_point_type" | "boolean_type");
    let name = if primitive { text(head, source).to_owned() } else { name_path(head, source)? };
    if type_parameters.contains(&name) {
        return None;
    }
    let mut cursor = node.walk();
    let mut arguments = Vec::new();
    if let Some(list) = node.named_children(&mut cursor).find(|child| child.kind() == "type_arguments") {
        let mut cursor = list.walk();
        arguments = list.named_children(&mut cursor).filter(|child| !child.is_extra())
            .map(|ty| read_type(ty, source, type_parameters)).collect::<Option<_>>()?;
    }
    Some(JavaParameter { name, primitive, dimensions: 0, arguments })
}

fn array_rank(node: Option<Node>) -> usize {
    let Some(node) = node else { return 0 };
    let mut cursor = node.walk();
    let rank = node.children(&mut cursor).filter(|child| child.kind() == "[").count();
    rank
}

fn type_head(node: Node) -> Option<Node> {
    if node.kind() == "generic_type" { first_syntax_child(node) } else { Some(node) }
}

fn first_syntax_child(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    let child = node.named_children(&mut cursor).find(|child| !child.is_extra());
    child
}

fn has_modifier(node: Node, modifier: &str) -> bool {
    let mut cursor = node.walk();
    let Some(modifiers) = node.named_children(&mut cursor).find(|child| child.kind() == "modifiers") else { return false };
    let mut cursor = modifiers.walk();
    let found = modifiers.children(&mut cursor).any(|child| child.kind() == modifier);
    found
}

fn type_parameters(node: Node, source: &[u8]) -> Vec<String> {
    let Some(parameters) = node.child_by_field_name("type_parameters") else { return Vec::new() };
    let mut cursor = parameters.walk();
    parameters.named_children(&mut cursor).filter_map(|param| {
        let mut cursor = param.walk();
        let name = param.named_children(&mut cursor).find(|child| child.kind() == "type_identifier")?;
        Some(text(name, source).to_owned())
    }).collect()
}

fn type_declaration_name<'a>(node: Node, source: &'a [u8]) -> Option<&'a str> {
    if !matches!(node.kind(), "class_declaration" | "interface_declaration" | "enum_declaration"
        | "record_declaration" | "annotation_type_declaration") {
        return None;
    }
    node.child_by_field_name("name").map(|name| text(name, source))
}

fn declaration_path(node: Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    let name = node.named_children(&mut cursor).find(|child| matches!(child.kind(), "identifier" | "scoped_identifier"))?;
    name_path(name, source)
}

fn name_path(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "type_identifier" => Some(text(node, source).to_owned()),
        "scoped_identifier" | "scoped_type_identifier" => {
            let mut cursor = node.walk();
            let parts = node.named_children(&mut cursor).filter(|child| !child.is_extra()
                && !matches!(child.kind(), "annotation" | "marker_annotation"))
                .map(|child| name_path(child, source)).collect::<Option<Vec<_>>>()?;
            Some(parts.join("."))
        }
        _ => None,
    }
}

use super::{children, descendants, field_text, text, Java, Kotlin, Language, Node};

pub(super) fn declared_name(node: Node<'_>) -> Option<Node<'_>> {
    let name = match node.kind() {
        "variable_declarator"
        | "formal_parameter"
        | "parameter_declaration"
        | "var_spec"
        | "const_spec"
        | "function_declaration"
        | "function_expression"
        | "class_declaration" => node.child_by_field_name("name"),
        "required_parameter" | "optional_parameter" => node.child_by_field_name("pattern"),
        "parameter" | "variable_declaration" if node.child_by_field_name("type").is_none() => {
            node.named_child(0)
        }
        "identifier"
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "formal_parameters") =>
        {
            Some(node)
        }
        "arrow_function" => node.child_by_field_name("parameter"),
        _ => None,
    }?;
    (name.kind() == "identifier").then_some(name)
}

pub(super) fn web_string_type(spelling: &str) -> bool {
    let mut has_string = false;
    spelling
        .trim_start_matches(':')
        .split('|')
        .all(|part| match part.trim() {
            "string" => {
                has_string = true;
                true
            }
            "null" | "undefined" => true,
            _ => false,
        })
        && has_string
}

pub(super) fn binding_scope(node: Node<'_>) -> Node<'_> {
    if node.kind() == "arrow_function" {
        return node;
    }
    let function_scoped = node.kind() == "variable_declarator"
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "variable_declaration");
    node.ancestors()
        .find(|scope| {
            if function_scoped {
                matches!(
                    scope.kind(),
                    "program"
                        | "function_declaration"
                        | "function_expression"
                        | "method_definition"
                        | "arrow_function"
                )
            } else {
                lexical_scope(*scope)
            }
        })
        .unwrap_or(node)
}

fn lexical_scope(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "program"
            | "source_file"
            | "block"
            | "statement_block"
            | "class_body"
            | "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition"
            | "method_declaration"
            | "constructor_declaration"
            | "lambda_literal"
            | "lambda_expression"
            | "catch_clause"
            | "for_statement"
            | "for_in_statement"
    )
}

pub(super) fn contains(scope: Node<'_>, site: Node<'_>) -> bool {
    scope.start_byte() <= site.start_byte() && site.end_byte() <= scope.end_byte()
}

pub(super) fn binding_type(node: Node<'_>, language: Language) -> Option<Node<'_>> {
    let ty = match (language, node.kind()) {
        (Java, "variable_declarator") => node.parent()?.child_by_field_name("type")?,
        (Kotlin, "parameter" | "variable_declaration") => node.named_child(1)?,
        _ => node.child_by_field_name("type")?,
    };
    Some(ty)
}

pub(super) fn initializer(node: Node<'_>, language: Language) -> Option<Node<'_>> {
    if language == Kotlin && node.kind() == "variable_declaration" {
        let property = node
            .parent()
            .filter(|parent| parent.kind() == "property_declaration")?;
        return children(property).find(|child| {
            child.start_byte() >= node.end_byte() && child.kind() != "property_delegate"
        });
    }
    node.child_by_field_name("value")
}

pub(super) fn shadows_string(node: Node<'_>, source: &str) -> bool {
    match node.kind() {
        "class_declaration"
        | "interface_declaration"
        | "record_declaration"
        | "type_alias"
        | "typealias_declaration" => field_text(node, "name", source)
            .is_some_and(|name| matches!(name, "String" | "CharSequence")),
        "type_parameter" => node
            .named_child(0)
            .is_some_and(|name| text(name, source) == "String"),
        "import_declaration" | "import" | "import_header" => {
            let path = text(node, source).trim_end_matches(';').trim();
            (path.ends_with(".String") || path.ends_with(" as String"))
                && !matches!(path, "import java.lang.String" | "import kotlin.String")
        }
        _ => false,
    }
}

pub(super) fn go_shadows(function: Node<'_>, name: &str, source: &str) -> bool {
    descendants(function).any(|node| {
        let left = match node.kind() {
            "short_var_declaration" | "range_clause" => node.child_by_field_name("left"),
            "parameter_declaration"
            | "variadic_parameter_declaration"
            | "var_spec"
            | "const_spec" => Some(node),
            _ => None,
        };
        left.into_iter()
            .flat_map(children)
            .any(|binding| binding.kind() == "identifier" && text(binding, source) == name)
    })
}

pub(super) trait Ancestors<'t> {
    fn ancestors(self) -> impl Iterator<Item = Node<'t>>;
}

impl<'t> Ancestors<'t> for Node<'t> {
    fn ancestors(self) -> impl Iterator<Item = Node<'t>> {
        std::iter::successors(self.parent(), |node| node.parent())
    }
}

use autoreview_schema::AgentFinding;
use tree_sitter::Node;

use super::{
    super::{children, descendants, location, text, Rule},
    package::{local_names, package_alias, File},
};

pub(in crate::analyzers::framework_checks) const RULE: Rule = Rule {
    id: "go-tls-verification-disabled-by-assignment",
    category: "security",
    subcategories: &["transport.tls"],
    title: "Go TLS config disables default certificate verification",
    message: "This assignment disables the default certificate and hostname checks for clients using this config. Keep verification enabled and add trusted roots for private services. If replacing verification, ensure the custom verifier checks peer identity.",
    reference: "https://pkg.go.dev/crypto/tls#Config",
};

pub(super) fn findings(file: &File, peers: &[&File]) -> Vec<AgentFinding> {
    if !file.source.contains("InsecureSkipVerify") || peers.iter().any(|peer| shadows_true(peer)) {
        return Vec::new();
    }
    let Some(tls) = package_alias(file, "crypto/tls", "tls") else {
        return Vec::new();
    };
    let source = &file.source;
    file.functions()
        .filter(|function| {
            let local = local_names(*function, source);
            !local.contains(tls) && !shadows_true_below(*function, source)
        })
        .flat_map(|function| {
            descendants(function).filter_map(move |assignment| {
                let receiver = assigned_receiver(assignment, source)?;
                let statements = assignment.parent()?.parent()?;
                if assignment.parent()?.kind() != "statement_list"
                    || statements.kind() != "block"
                    || crosses_literal_function(statements, function)
                {
                    return None;
                }
                let name = text(receiver, source);
                let declaration = previous_binding(assignment, name, source).or_else(|| {
                    (function.child_by_field_name("body")? == statements)
                        .then(|| typed_parameter(function, name, source))
                        .flatten()
                })?;
                is_tls_binding(declaration, tls, source).then(|| {
                    RULE.finding(
                        location(&file.path, assignment, source),
                        vec![location(&file.path, declaration, source)],
                    )
                })
            })
        })
        .collect()
}

fn shadows_true(file: &File) -> bool {
    children(file.root())
        .filter(|item| matches!(item.kind(), "const_declaration" | "var_declaration"))
        .any(|item| shadows_true_below(item, &file.source))
}

fn shadows_true_below(root: Node<'_>, source: &str) -> bool {
    descendants(root).any(|node| match node.kind() {
        "short_var_declaration" | "range_clause" => node
            .child_by_field_name("left")
            .is_some_and(|left| children(left).any(|part| text(part, source) == "true")),
        "parameter_declaration" | "var_spec" | "const_spec" => declares_name(node, "true", source),
        "type_switch_statement" => node
            .child_by_field_name("alias")
            .is_some_and(|alias| children(alias).any(|part| text(part, source) == "true")),
        _ => false,
    })
}

fn crosses_literal_function(block: Node<'_>, function: Node<'_>) -> bool {
    std::iter::successors(block.parent(), |node| node.parent())
        .take_while(|node| *node != function)
        .any(|node| node.kind() == "func_literal")
}

fn assigned_receiver<'t>(node: Node<'t>, source: &str) -> Option<Node<'t>> {
    if node.kind() != "assignment_statement" || node.child_by_field_name("operator")?.kind() != "="
    {
        return None;
    }
    let left = single_expression(node.child_by_field_name("left")?)?;
    let right = single_expression(node.child_by_field_name("right")?)?;
    if left.kind() != "selector_expression"
        || right.kind() != "true"
        || text(left.child_by_field_name("field")?, source) != "InsecureSkipVerify"
    {
        return None;
    }
    left.child_by_field_name("operand")
        .filter(|receiver| receiver.kind() == "identifier")
}

fn single_expression(list: Node<'_>) -> Option<Node<'_>> {
    let mut values = children(list).filter(|node| node.kind() != "comment");
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn previous_binding<'t>(site: Node<'t>, name: &str, source: &str) -> Option<Node<'t>> {
    // A nearer declaration wins even when its type is unknown.
    std::iter::successors(site.prev_named_sibling(), |node| node.prev_named_sibling()).find_map(
        |statement| match statement.kind() {
            "short_var_declaration" => {
                let left = statement.child_by_field_name("left")?;
                if !children(left)
                    .any(|part| part.kind() == "identifier" && text(part, source) == name)
                {
                    return None;
                }
                Some(statement)
            }
            "var_declaration" => descendants(statement)
                .filter(|node| node.kind() == "var_spec")
                .find(|spec| declares_name(*spec, name, source)),
            _ => None,
        },
    )
}

fn typed_parameter<'t>(function: Node<'t>, name: &str, source: &str) -> Option<Node<'t>> {
    children(function.child_by_field_name("parameters")?).find(|parameter| {
        parameter.kind() == "parameter_declaration" && declares_name(*parameter, name, source)
    })
}

fn declares_name(node: Node<'_>, name: &str, source: &str) -> bool {
    let mut cursor = node.walk();
    let result = node
        .children_by_field_name("name", &mut cursor)
        .any(|part| text(part, source) == name);
    result
}

fn is_tls_binding(binding: Node<'_>, alias: &str, source: &str) -> bool {
    binding
        .child_by_field_name("type")
        .or_else(|| literal_binding_type(binding))
        .is_some_and(|ty| is_tls_type(ty, alias, source))
}

fn literal_binding_type(binding: Node<'_>) -> Option<Node<'_>> {
    let value = match binding.kind() {
        "short_var_declaration"
            if binding
                .child_by_field_name("left")
                .and_then(single_expression)
                .is_some() =>
        {
            binding.child_by_field_name("right")
        }
        "var_spec"
            if children(binding)
                .filter(|part| part.kind() == "identifier")
                .count()
                == 1 =>
        {
            binding.child_by_field_name("value")
        }
        _ => None,
    };
    let value = value.and_then(single_expression)?;
    let literal = match value.kind() {
        "unary_expression" if value.child_by_field_name("operator")?.kind() == "&" => {
            value.child_by_field_name("operand")?
        }
        "composite_literal" => value,
        _ => return None,
    };
    (literal.kind() == "composite_literal")
        .then(|| literal.child_by_field_name("type"))
        .flatten()
}

fn is_tls_type(mut ty: Node<'_>, alias: &str, source: &str) -> bool {
    if ty.kind() == "pointer_type" {
        let Some(pointee) = children(ty).find(|part| part.kind() != "comment") else {
            return false;
        };
        ty = pointee;
    }
    ty.kind() == "qualified_type"
        && ty
            .child_by_field_name("package")
            .is_some_and(|part| text(part, source) == alias)
        && ty
            .child_by_field_name("name")
            .is_some_and(|part| text(part, source) == "Config")
}

use autoreview_langsupport::{language_for_path, Language};
use autoreview_schema::{AgentFinding, Severity};
use tree_sitter::Node;

use super::super::web::{field_text, imports_from, BindingScopes};
use super::super::{children, descendants, location, text, Rule};

pub(in crate::analyzers::framework_checks) const JAVASCRIPT_RULE: Rule = Rule {
    id: "javascript-tls-verification-disabled",
    category: "security",
    subcategories: &["transport.tls"],
    title: "Node.js TLS client disables certificate verification",
    message: "This Node.js client disables certificate verification. Keep verification enabled and configure a trusted CA for private services.",
    reference: "https://nodejs.org/api/tls.html#tlsconnectoptions-callback",
};

pub(in crate::analyzers::framework_checks) const TYPESCRIPT_RULE: Rule = Rule {
    id: "typescript-tls-verification-disabled",
    ..JAVASCRIPT_RULE
};

pub(super) fn inspect(path: &str, source: &str, root: Node<'_>) -> Vec<AgentFinding> {
    if !source.contains("rejectUnauthorized") {
        return Vec::new();
    }
    let rule = match language_for_path(path.as_ref()) {
        Some(Language::TypeScript | Language::Tsx) => &TYPESCRIPT_RULE,
        _ => &JAVASCRIPT_RULE,
    };
    let scopes = BindingScopes::new(root, source);
    descendants(root)
        .filter_map(|call| {
            let (module, member) = api(call, root, source, &scopes)?;
            let mut args = children(call.child_by_field_name("arguments")?)
                .filter(|argument| argument.kind() != "comment");
            let first = args.next()?;
            let options = match (module, member, call.kind()) {
                ("https", "Agent", "new_expression") => first,
                ("tls", "connect", "call_expression") => match first.kind() {
                    "number" => {
                        let next = args.next()?;
                        if next.kind() == "string" {
                            args.next()?
                        } else {
                            next
                        }
                    }
                    "string" => args.next()?,
                    _ => first,
                },
                ("https", "get" | "request", "call_expression") => {
                    if first.kind() == "string" {
                        args.next()?
                    } else {
                        first
                    }
                }
                _ => return None,
            };
            let object = if options.kind() == "identifier" {
                local_options(options, call, source, &scopes)?
            } else {
                options
            };
            let setting = disabling_option(object, source)?;
            let related = (options != object)
                .then(|| location(path, call, source))
                .into_iter()
                .collect();
            let mut finding = rule.finding(location(path, setting, source), related);
            finding.severity = Severity::High;
            if let Some(metadata) = &mut finding.meta {
                metadata.insert("cwe".into(), serde_json::json!(["CWE-295"]));
            }
            Some(finding)
        })
        .collect()
}

fn api<'s>(
    call: Node<'_>,
    root: Node<'_>,
    source: &'s str,
    scopes: &BindingScopes<'_, '_>,
) -> Option<(&'static str, &'s str)> {
    let target = match call.kind() {
        "call_expression" => call.child_by_field_name("function")?,
        "new_expression" => call.child_by_field_name("constructor")?,
        _ => return None,
    };
    let target = if target.kind() == "parenthesized_expression" {
        children(target).find(|child| child.kind() != "comment")?
    } else {
        target
    };
    if target.kind() == "member_expression" {
        let object = target.child_by_field_name("object")?;
        let member = field_text(target, "property", source)?;
        if !matches!(member, "Agent" | "get" | "request" | "connect") {
            return None;
        }
        return Some((module_of(object, root, source, scopes)?, member));
    }
    let local = text(target, source);
    if target.kind() != "identifier"
        || scopes.visible(local, target).is_some()
        || super::inside_named_expression(target, local, source)
    {
        return None;
    }
    MODULES.iter().find_map(|(specifier, module)| {
        runtime_imports(root, source, specifier)
            .flat_map(descendants)
            .filter(|node| node.kind() == "import_specifier" && !has_type_modifier(*node))
            .find_map(|specifier| {
                let name = field_text(specifier, "name", source)?;
                let binding = field_text(specifier, "alias", source).unwrap_or(name);
                (local == binding).then_some((*module, name))
            })
    })
}

const MODULES: [(&str, &str); 4] = [
    ("https", "https"),
    ("node:https", "https"),
    ("tls", "tls"),
    ("node:tls", "tls"),
];

fn module_of(
    object: Node<'_>,
    root: Node<'_>,
    source: &str,
    scopes: &BindingScopes<'_, '_>,
) -> Option<&'static str> {
    if object.kind() == "call_expression" {
        return required_module(object, root, source, scopes);
    }
    if object.kind() != "identifier" {
        return None;
    }
    let local = text(object, source);
    if super::inside_named_expression(object, local, source) {
        return None;
    }
    if let Some(scope) = scopes.visible(local, object) {
        let (_, value) = const_value(scope, local, source)?;
        return (value.start_byte() < object.start_byte())
            .then(|| required_module(value, root, source, scopes))
            .flatten();
    }
    MODULES.iter().find_map(|(specifier, module)| {
        runtime_imports(root, source, specifier)
            .flat_map(children)
            .filter(|part| part.kind() == "import_clause")
            .flat_map(children)
            .any(|part| match part.kind() {
                "identifier" => text(part, source) == local,
                "namespace_import" => children(part).any(|binding| text(binding, source) == local),
                _ => false,
            })
            .then_some(*module)
    })
}

fn required_module(
    call: Node<'_>,
    root: Node<'_>,
    source: &str,
    scopes: &BindingScopes<'_, '_>,
) -> Option<&'static str> {
    if call.kind() != "call_expression"
        || field_text(call, "function", source) != Some("require")
        || scopes.visible("require", call).is_some()
        || super::inside_named_expression(call, "require", source)
        || super::imports_binding(root, "require", source)
    {
        return None;
    }
    let arg = super::super::web::first_argument(call)?;
    MODULES
        .iter()
        .find_map(|(specifier, module)| super::is_string(arg, specifier, source).then_some(*module))
}

fn runtime_imports<'t, 's>(
    root: Node<'t>,
    source: &'s str,
    specifier: &'s str,
) -> impl Iterator<Item = Node<'t>> + use<'t, 's> {
    imports_from(root, source, specifier).filter(|import| !has_type_modifier(*import))
}

fn has_type_modifier(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    let result = node.children(&mut cursor).any(|part| part.kind() == "type");
    result
}

fn const_value<'t>(scope: Node<'t>, name: &str, source: &str) -> Option<(Node<'t>, Node<'t>)> {
    let mut declarations = children(scope)
        .filter(|statement| {
            statement.kind() == "lexical_declaration"
                && statement
                    .child(0)
                    .is_some_and(|token| token.kind() == "const")
        })
        .flat_map(children)
        .filter(|decl| field_text(*decl, "name", source) == Some(name));
    let decl = declarations.next()?;
    (declarations.next().is_none()).then_some((decl, decl.child_by_field_name("value")?))
}

fn local_options<'t>(
    options: Node<'t>,
    call: Node<'t>,
    source: &str,
    scopes: &BindingScopes<'t, '_>,
) -> Option<Node<'t>> {
    let name = text(options, source);
    let scope = scopes.visible(name, options)?;
    let (declaration, object) = const_value(scope, name, source)?;
    let mut statement = call;
    while statement.parent()? != scope {
        statement = statement.parent()?;
        if super::super::web::is_function(statement)
            || matches!(
                statement.kind(),
                "statement_block"
                    | "if_statement"
                    | "for_statement"
                    | "for_in_statement"
                    | "while_statement"
                    | "do_statement"
                    | "switch_statement"
            )
        {
            return None;
        }
    }
    if declaration.end_byte() > statement.start_byte()
        || descendants(scope).any(|node| {
            node.start_byte() < options.start_byte()
                && !declaration.byte_range().contains(&node.start_byte())
                && matches!(node.kind(), "identifier" | "shorthand_property_identifier")
                && text(node, source) == name
        })
    {
        return None;
    }
    Some(object)
}

fn disabling_option<'t>(object: Node<'t>, source: &str) -> Option<Node<'t>> {
    if object.kind() != "object" {
        return None;
    }
    let mut setting = None;
    for member in children(object).filter(|member| member.kind() != "comment") {
        let key = match member.kind() {
            "pair" => member.child_by_field_name("key")?,
            "method_definition" => member.child_by_field_name("name")?,
            "shorthand_property_identifier" => member,
            _ => {
                setting = None;
                continue;
            }
        };
        if key.kind() == "computed_property_name" || text(key, source).contains('\\') {
            setting = None;
        } else if text(key, source) == "rejectUnauthorized"
            || super::is_string(key, "rejectUnauthorized", source)
        {
            setting = member
                .child_by_field_name("value")
                .filter(|value| value.kind() == "false")
                .map(|_| member);
        }
    }
    setting
}

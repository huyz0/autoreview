//! Syntactic evidence for promise-returning callable types.
//!
//! An annotation naming `Promise` is not by itself proof that it refers to
//! the standard type. Callers must resolve shadowing and imports before
//! treating annotation evidence as a runtime promise fact.

use tree_sitter::Node;

/// How a declaration syntactically describes its returned value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromiseReturn {
    /// An async function returns a promise when called.
    AsyncFunction,
    /// The declared return type is `Promise<T>`.
    PromiseAnnotation,
    /// The declared return type is a function returning `Promise<T>`.
    FactoryAnnotation { minimum: usize, maximum: usize },
}

/// A syntactically immutable module binding initialized by a direct call.
/// The callee and its declared return type still require symbol resolution.
#[derive(Debug, PartialEq, Eq)]
pub struct FactoryBinding {
    pub name: String,
    pub callee: String,
    pub arguments: usize,
    pub call_start: usize,
}

pub fn factory_binding(node: Node<'_>, source: &[u8]) -> Option<FactoryBinding> {
    if node.kind() != "variable_declarator" || node.has_error() { return None; }
    let declaration = node.parent().filter(|parent| parent.kind() == "lexical_declaration")?;
    if !declaration.children(&mut declaration.walk()).any(|child| child.kind() == "const")
        || !declaration.parent().is_some_and(|parent| matches!(parent.kind(), "program" | "export_statement"))
    {
        return None;
    }
    let name = node.child_by_field_name("name").filter(|name| name.kind() == "identifier")?;
    let call = node.child_by_field_name("value").filter(|value| value.kind() == "call_expression")?;
    if call.child_by_field_name("optional_chain").is_some() { return None; }
    let callee = call.child_by_field_name("function").filter(|callee| callee.kind() == "identifier")?;
    let arguments = call.child_by_field_name("arguments")?;
    let args: Vec<_> = arguments.named_children(&mut arguments.walk()).filter(|arg| arg.kind() != "comment").collect();
    if args.iter().any(|arg| matches!(arg.kind(), "spread_element" | "spread_expression")) { return None; }
    Some(FactoryBinding {
        name: name.utf8_text(source).ok()?.to_owned(),
        callee: callee.utf8_text(source).ok()?.to_owned(),
        arguments: args.len(),
        call_start: call.start_byte(),
    })
}

/// Inspect a callable declaration without resolving its type names.
pub fn declared_return(node: Node<'_>, source: &[u8]) -> Option<PromiseReturn> {
    if !matches!(node.kind(), "function_declaration" | "function_expression" | "function_signature" | "arrow_function" | "method_definition" | "method_signature")
        || node.children(&mut node.walk()).any(|child| child.kind() == "*")
    {
        return None;
    }
    if node.children(&mut node.walk()).any(|child| child.kind() == "async") {
        return Some(PromiseReturn::AsyncFunction);
    }
    let annotation = node.child_by_field_name("return_type")?;
    let value = unwrap_type(annotation)?;
    if promise_type(value, source) {
        return Some(PromiseReturn::PromiseAnnotation);
    }
    if value.kind() == "function_type"
        && value.child_by_field_name("return_type").and_then(unwrap_type).is_some_and(|value| promise_type(value, source))
    {
        let (minimum, maximum) = parameter_bounds(value, source)?;
        return Some(PromiseReturn::FactoryAnnotation { minimum, maximum });
    }
    None
}

fn parameter_bounds(function: Node<'_>, source: &[u8]) -> Option<(usize, usize)> {
    let parameters = function.child_by_field_name("parameters")?;
    let mut minimum = 0;
    let mut maximum = 0;
    let mut optional_seen = false;
    for parameter in parameters.named_children(&mut parameters.walk()).filter(|node| node.kind() != "comment") {
        let pattern = parameter.child_by_field_name("pattern")?;
        if pattern.kind() != "identifier" { return None; }
        if pattern.utf8_text(source) == Ok("this") { continue; }
        match parameter.kind() {
            "required_parameter" if !optional_seen => minimum += 1,
            "optional_parameter" => optional_seen = true,
            _ => return None,
        }
        maximum += 1;
    }
    Some((minimum, maximum))
}

/// Conservatively rule out local bindings named `Promise` in a source unit.
///
/// This deliberately treats other non-type references as uncertain too; it is
/// a syntactic guard, not a substitute for project-wide TypeScript resolution.
pub fn unshadowed_promise_name(root: Node<'_>, source: &[u8]) -> bool {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if matches!(node.kind(), "identifier" | "type_identifier" | "shorthand_property_identifier_pattern")
            && node.utf8_text(source) == Ok("Promise")
            && !node.parent().is_some_and(|parent| parent.kind() == "generic_type" && parent.child_by_field_name("name") == Some(node))
        {
            return false;
        }
        pending.extend(node.named_children(&mut node.walk()));
    }
    true
}

fn unwrap_type(mut node: Node<'_>) -> Option<Node<'_>> {
    while matches!(node.kind(), "type_annotation" | "parenthesized_type") {
        node = node.named_child(0)?;
    }
    Some(node)
}

fn promise_type(node: Node<'_>, source: &[u8]) -> bool {
    node.kind() == "generic_type"
        && node.child_by_field_name("name").is_some_and(|name| name.utf8_text(source) == Ok("Promise"))
        && node.child_by_field_name("type_arguments").is_some_and(|arguments| arguments.named_child_count() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreview_langsupport::{parser_for, Language};

    #[test]
    fn distinguishes_promise_values_from_factories_and_other_types() {
        let source = r#"function direct(): Promise<void> { throw 1; }
function factory<T>(): (value: T, option?: string) => Promise<void> { throw 1; }
function other(): (value: string) => string { throw 1; }
function notPromise(): PromiseLike<void> { throw 1; }
async function inferred() { return 1; }
async function* generator() { yield 1; }
function invalid(): Promise<void, string> { throw 1; }
function object(): { run(): Promise<void> } { throw 1; }
function union(): Promise<void> | undefined { throw 1; }
"#;
        for language in [Language::TypeScript, Language::Tsx] {
            let tree = parser_for(language).unwrap().parse(source, None).unwrap();
            let functions: Vec<_> = tree.root_node().named_children(&mut tree.root_node().walk()).collect();
            let shapes: Vec<_> = functions.into_iter().map(|node| declared_return(node, source.as_bytes())).collect();
            assert_eq!(shapes, [Some(PromiseReturn::PromiseAnnotation), Some(PromiseReturn::FactoryAnnotation { minimum: 1, maximum: 2 }), None, None, Some(PromiseReturn::AsyncFunction), None, None, None, None]);
        }
    }

    #[test]
    fn rejects_locally_shadowed_or_uncertain_promise_names() {
        for (source, expected) in [
            ("function f(): Promise<void> { throw 1; }", true),
            ("type Promise<T> = T; function f(): Promise<void> { throw 1; }", false),
            ("import { Promise } from './custom'; function f(): Promise<void> { throw 1; }", false),
            ("function f<Promise>(): Promise<void> { throw 1; }", false),
            ("const { Promise } = source; function f(): Promise<void> { throw 1; }", false),
            ("function f(): Promise<void> { return new Promise(() => {}); }", false),
        ] {
            let tree = parser_for(Language::TypeScript).unwrap().parse(source, None).unwrap();
            assert_eq!(unshadowed_promise_name(tree.root_node(), source.as_bytes()), expected, "{source}");
        }
    }

    #[test]
    fn recognizes_only_direct_immutable_module_factory_calls() {
        let source = r#"export const upload = createHandler<Options>({ name: 'upload' });
let mutable = createHandler({});
const spread = createHandler(...options);
const indirect = registry.createHandler({});
function nested() { const local = createHandler({}); }
"#;
        let tree = parser_for(Language::TypeScript).unwrap().parse(source, None).unwrap();
        let mut pending = vec![tree.root_node()];
        let mut bindings = Vec::new();
        while let Some(node) = pending.pop() {
            if let Some(binding) = factory_binding(node, source.as_bytes()) { bindings.push(binding); }
            pending.extend(node.named_children(&mut node.walk()));
        }
        assert_eq!(bindings.len(), 1, "{bindings:?}");
        assert_eq!(bindings[0].name, "upload");
        assert_eq!(bindings[0].callee, "createHandler");
        assert_eq!(bindings[0].arguments, 1);
    }
}

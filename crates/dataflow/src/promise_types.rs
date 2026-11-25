//! Syntactic evidence for promise-returning callable types.
//!
//! An annotation naming `Promise` is not by itself proof that it refers to
//! the standard type. Callers must resolve shadowing and imports before
//! treating annotation evidence as a runtime promise fact.

use tree_sitter::Node;
use crate::call_order::KeyedImport;

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

/// A parsed module paired with the repository-relative path used by imports.
pub struct TypeModule<'a> {
    pub path: &'a str,
    pub source: &'a [u8],
    pub root: Node<'a>,
}

/// Resolve an immutable binding initialized by a directly imported factory.
/// The import must come from the repository's verified module resolver.
pub fn imported_factory_return(
    caller: &TypeModule<'_>, binding: &FactoryBinding, import: &KeyedImport, provider: &TypeModule<'_>,
) -> Option<PromiseReturn> {
    if caller.root.has_error() || provider.root.has_error() || import.local != binding.callee
        || import.path != provider.path || !import.exported || import.owner.is_some()
        || may_reassign(caller.root, caller.source, &binding.callee)
        || !unshadowed_promise_name(provider.root, provider.source)
        || !unique_import(caller.root, caller.source, &binding.callee)
    {
        return None;
    }
    let name = import.name.as_deref()?;
    let function = direct_exported_factory(provider.root, provider.source, name)?;
    if let Some(local) = function.child_by_field_name("name") {
        let name = local.utf8_text(provider.source).ok()?;
        if may_reassign(provider.root, provider.source, name)
            || !unique_provider_binding(provider.root, provider.source, function, name)
        { return None; }
    }
    let (minimum, maximum) = parameter_bounds(function, provider.source)?;
    if !(minimum..=maximum).contains(&binding.arguments) { return None; }
    match declared_return(function, provider.source)? {
        shape @ PromiseReturn::FactoryAnnotation { .. } => Some(shape),
        _ => None,
    }
}

fn unique_provider_binding(root: Node<'_>, source: &[u8], selected: Node<'_>, name: &str) -> bool {
    root.named_children(&mut root.walk()).all(|statement| {
        let declaration = if statement.kind() == "export_statement" {
            statement.child_by_field_name("declaration").unwrap_or(statement)
        } else { statement };
        declaration == selected || !top_level_may_bind(declaration, source, name)
    })
}

fn unique_import(root: Node<'_>, source: &[u8], name: &str) -> bool {
    let mut imports = 0;
    for statement in root.named_children(&mut root.walk()) {
        if statement.kind() == "import_statement" && top_level_may_bind(statement, source, name) {
            imports += 1;
            continue;
        }
        let declaration = if statement.kind() == "export_statement" {
            statement.child_by_field_name("declaration").unwrap_or(statement)
        } else { statement };
        if top_level_may_bind(declaration, source, name) { return false; }
    }
    imports == 1
}

fn direct_exported_factory<'tree>(root: Node<'tree>, source: &[u8], imported: &str) -> Option<Node<'tree>> {
    let mut selected = None;
    for statement in root.named_children(&mut root.walk()).filter(|node| node.kind() == "export_statement") {
        let default = statement.children(&mut statement.walk()).any(|child| child.kind() == "default");
        let Some(function) = statement.child_by_field_name("declaration").filter(|node| node.kind() == "function_declaration") else { continue };
        let name = function.child_by_field_name("name").and_then(|name| name.utf8_text(source).ok());
        if ((imported == "default" && default) || (!default && name == Some(imported)))
            && selected.replace(function).is_some() {
            return None;
        }
    }
    selected
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

/// Resolve a direct module-local factory declaration using only syntax.
///
/// The returned shape is evidence from an annotation, not proof of the
/// project-wide meaning of `Promise`; callers must account for ambient types.
pub fn local_factory_return(root: Node<'_>, source: &[u8], binding: &FactoryBinding) -> Option<PromiseReturn> {
    if root.kind() != "program" || root.has_error() || !unshadowed_promise_name(root, source)
        || may_reassign(root, source, &binding.callee)
    {
        return None;
    }
    let mut selected = None;
    for statement in root.named_children(&mut root.walk()) {
        let declaration = if statement.kind() == "export_statement" {
            statement.child_by_field_name("declaration").unwrap_or(statement)
        } else {
            statement
        };
        if declaration.kind() == "function_declaration"
            && declaration.child_by_field_name("name").is_some_and(|name| name.utf8_text(source) == Ok(binding.callee.as_str()))
        {
            if selected.replace(declaration).is_some() { return None; }
        } else if top_level_may_bind(declaration, source, &binding.callee) {
            return None;
        }
    }
    let function = selected?;
    let (minimum, maximum) = parameter_bounds(function, source)?;
    if !(minimum..=maximum).contains(&binding.arguments) { return None; }
    match declared_return(function, source)? {
        shape @ PromiseReturn::FactoryAnnotation { .. } => Some(shape),
        _ => None,
    }
}

fn may_reassign(root: Node<'_>, source: &[u8], name: &str) -> bool {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        let target = match node.kind() {
            "assignment_expression" | "augmented_assignment_expression" => node.child_by_field_name("left"),
            "update_expression" => node.child_by_field_name("argument").or_else(|| node.named_child(0)),
            "for_in_statement" => node.child_by_field_name("left"),
            _ => None,
        };
        if node.kind() == "call_expression" && node.child_by_field_name("function")
            .is_some_and(|function| function.kind() == "identifier" && function.utf8_text(source) == Ok("eval"))
        {
            return true;
        }
        if target.is_some_and(|target| contains_identifier(target, source, name)) { return true; }
        pending.extend(node.named_children(&mut node.walk()));
    }
    false
}

fn top_level_may_bind(node: Node<'_>, source: &[u8], name: &str) -> bool {
    if node.kind() == "import_statement" {
        let source_node = node.child_by_field_name("source");
        return node.named_children(&mut node.walk()).filter(|child| Some(*child) != source_node)
            .any(|child| contains_identifier(child, source, name));
    }
    if matches!(node.kind(), "class_declaration" | "function_declaration") {
        return node.child_by_field_name("name").is_some_and(|value| value.utf8_text(source) == Ok(name));
    }
    if !matches!(node.kind(), "lexical_declaration" | "variable_declaration") { return false; }
    node.named_children(&mut node.walk()).filter(|item| item.kind() == "variable_declarator").any(|item| {
        item.child_by_field_name("name").is_none_or(|pattern| pattern.kind() != "identifier" || pattern.utf8_text(source) == Ok(name))
    })
}

fn contains_identifier(node: Node<'_>, source: &[u8], name: &str) -> bool {
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if matches!(node.kind(), "identifier" | "type_identifier") && node.utf8_text(source) == Ok(name) { return true; }
        pending.extend(node.named_children(&mut node.walk()));
    }
    false
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

    #[test]
    fn resolves_only_unambiguous_local_factory_annotations() {
        for (prefix, expected) in [
            ("function createHandler<T>(option: T): (ctx: string, next?: string) => Promise<void> { throw 1; }", true),
            ("function createHandler<T>(option: T, next?: T): (ctx: string) => Promise<void> { throw 1; }", true),
            ("async function createHandler<T>(option: T) { return option; }", false),
            ("function createHandler<T>(option: T): (ctx: string) => string { throw 1; }", false),
            ("type Promise<T> = T; function createHandler<T>(option: T): (ctx: string) => Promise<void> { throw 1; }", false),
            ("import { createHandler } from './lib'; function createHandler<T>(option: T): (ctx: string) => Promise<void> { throw 1; }", false),
            ("function createHandler<T>(first: T, second: T): (ctx: string) => Promise<void> { throw 1; }", false),
            ("function createHandler<T>(...options: T[]): (ctx: string) => Promise<void> { throw 1; }", false),
            ("function createHandler<T>(option: T): (ctx: string) => Promise<void> | undefined { throw 1; }", false),
            ("function createHandler<T>(option: T): (ctx: string) => Promise<void> { throw 1; }\nfunction replace() { createHandler = other; }", false),
            ("function createHandler<T>(option: T): (ctx: string) => Promise<void> { throw 1; }\nfunction replace() { for (createHandler of values) {} }", false),
            ("function createHandler<T>(option: T): (ctx: string) => Promise<void> { throw 1; }\nfunction replace() { eval(code); }", false),
        ] {
            let source = format!("{prefix}\nexport const upload = createHandler({{}});\n");
            let tree = parser_for(Language::TypeScript).unwrap().parse(&source, None).unwrap();
            let mut pending = vec![tree.root_node()];
            let mut binding = None;
            while let Some(node) = pending.pop() {
                binding = binding.or_else(|| factory_binding(node, source.as_bytes()));
                pending.extend(node.named_children(&mut node.walk()));
            }
            let shape = local_factory_return(tree.root_node(), source.as_bytes(), &binding.unwrap());
            assert_eq!(shape.is_some(), expected, "{source}: {shape:?}");
        }
    }

    #[test]
    fn resolves_a_verified_direct_factory_import_conservatively() {
        let caller_source = "import create from './factory';\nexport const upload = create({});\n";
        let caller_tree = parser_for(Language::TypeScript).unwrap().parse(caller_source, None).unwrap();
        let caller = TypeModule { path: "caller.ts", source: caller_source.as_bytes(), root: caller_tree.root_node() };
        let mut nodes = vec![caller.root];
        let mut binding = None;
        while let Some(node) = nodes.pop() {
            binding = binding.or_else(|| factory_binding(node, caller.source));
            nodes.extend(node.named_children(&mut node.walk()));
        }
        let binding = binding.unwrap();
        let import = KeyedImport { local: "create".into(), path: "factory.ts".into(), name: Some("default".into()), owner: None, exported: true };
        for (provider_source, expected) in [
            ("export default function make(x: object): (ctx: string, opt?: string) => Promise<void> { throw 1; }", true),
            ("export function make(x: object): (ctx: string) => Promise<void> { throw 1; }", false),
            ("type Promise<T> = T; export default function make(x: object): (ctx: string) => Promise<void> { throw 1; }", false),
            ("export default function make(x: object, y: object): (ctx: string) => Promise<void> { throw 1; }", false),
            ("export default function make(x: object): (ctx: string) => Promise<void> { throw 1; }\nfunction replace() { make = other; }", false),
            ("export default function make(x: object): (ctx: string) => Promise<void> { throw 1; }\nconst make = other;", false),
        ] {
            let tree = parser_for(Language::TypeScript).unwrap().parse(provider_source, None).unwrap();
            let provider = TypeModule { path: "factory.ts", source: provider_source.as_bytes(), root: tree.root_node() };
            let shape = imported_factory_return(&caller, &binding, &import, &provider);
            assert_eq!(shape.is_some(), expected, "{provider_source}: {shape:?}");
            let wrong_path = TypeModule { path: "other.ts", ..provider };
            assert!(imported_factory_return(&caller, &binding, &import, &wrong_path).is_none());
        }
    }
}

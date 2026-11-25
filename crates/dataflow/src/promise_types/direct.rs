use super::*;
use crate::call_order::KeyedImport;

/// Resolve an immutable binding initialized by a directly imported factory.
/// The import must come from the repository's verified module resolver. The
/// result is syntactic evidence only: the caller must also resolve ambient
/// type declarations before treating `Promise` as the standard type.
pub(super) fn imported_factory_return(
    caller: &TypeModule<'_>, binding: &FactoryBinding, import: &KeyedImport, provider: &TypeModule<'_>,
) -> Option<PromiseReturn> {
    if !valid_import(caller, binding, import, provider) { return None; }
    let name = import.name.as_deref()?;
    let function = direct_exported_factory(provider.root, provider.source, name)?;
    if let Some(local) = function.child_by_field_name("name") {
        let name = local.utf8_text(provider.source).ok()?;
        if !valid_factory_declaration(provider, function, name) { return None; }
    }
    let (minimum, maximum) = parameter_bounds(function, provider.source)?;
    if !(minimum..=maximum).contains(&binding.arguments) { return None; }
    match declared_return(function, provider.source)? {
        shape @ PromiseReturn::FactoryAnnotation { .. } => Some(shape),
        _ => None,
    }
}

fn valid_import(caller: &TypeModule<'_>, binding: &FactoryBinding, import: &KeyedImport, provider: &TypeModule<'_>) -> bool {
    import_matches(binding, import, provider) && caller_binding_valid(caller, binding)
        && !provider.root.has_error() && unshadowed_promise_name(provider.root, provider.source)
}

fn import_matches(binding: &FactoryBinding, import: &KeyedImport, provider: &TypeModule<'_>) -> bool {
    import.local == binding.callee && import.path == provider.path && import.exported && import.owner.is_none()
}

fn caller_binding_valid(caller: &TypeModule<'_>, binding: &FactoryBinding) -> bool {
    !caller.root.has_error() && binding_belongs_to(caller, binding)
        && !may_reassign(caller.root, caller.source, &binding.callee)
        && !may_reassign(caller.root, caller.source, &binding.name)
        && unique_import(caller.root, caller.source, &binding.callee)
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
    let function = selected?;
    let owner = function.parent()?;
    if root.named_children(&mut root.walk()).any(|statement| {
        statement != owner && statement.kind() == "export_statement"
            && explicit_export_may_match(statement, source, imported)
    }) {
        return None;
    }
    Some(function)
}

fn explicit_export_may_match(statement: Node<'_>, source: &[u8], imported: &str) -> bool {
    let default = statement.children(&mut statement.walk()).any(|child| child.kind() == "default");
    if default { return imported == "default"; }
    if imported != "default" {
        if let Some(declaration) = statement.child_by_field_name("declaration") {
            return top_level_may_bind(declaration, source, imported);
        }
    }
    statement.named_children(&mut statement.walk()).filter(|node| node.kind() == "export_clause")
        .flat_map(|clause| clause.named_children(&mut clause.walk()).collect::<Vec<_>>())
        .filter(|item| item.kind() == "export_specifier")
        .any(|item| item.child_by_field_name("alias").or_else(|| item.child_by_field_name("name"))
            .is_none_or(|name| name.utf8_text(source) == Ok(imported)))
}


use super::{IdentityOperand, Syntax};

use crate::lower::site::children;
use std::ops::Range;
use tree_sitter::Node;

pub(super) struct LocalImport<'src> {
    scope: Range<usize>,
    name: Option<&'src str>,
}

impl<'src> Syntax<'src> {
    pub(super) fn record_local_import(&mut self, node: Node<'_>, source: &'src [u8]) {
        let Some(scope) = node.parent().filter(|parent| parent.kind() == "block") else { return };
        let argument = node.child_by_field_name("argument");
        let mut names = Vec::new();
        if argument.is_none_or(|node| !import_names(node, source, None, &mut names)) {
            names.push(None);
        }
        self.local_type_imports.extend(names.into_iter().map(|name| LocalImport {
            scope: scope.byte_range(),
            name,
        }));
    }

    pub(in crate::call_order) fn local_import_shadows(&self, name: &str, at: usize) -> bool {
        self.local_type_imports.iter().any(|import| {
            import.scope.contains(&at) && import.name.is_none_or(|local| local == name)
        })
    }
}

fn import_names<'src>(node: Node<'_>, source: &'src [u8], parent: Option<&'src str>, names: &mut Vec<Option<&'src str>>) -> bool {
    match node.kind() {
        "use_list" => children(node).all(|part| import_names(part, source, parent, names)),
        "scoped_use_list" => {
            let path = node.child_by_field_name("path").and_then(|path| last_name(path, source));
            node.child_by_field_name("list").is_some_and(|list| import_names(list, source, path, names))
        }
        "use_wildcard" => {
            names.push(None);
            true
        }
        _ => {
            let name = last_name(node, source).and_then(|name| if name == "self" { parent } else { Some(name) });
            names.push(name);
            name.is_some()
        }
    }
}

fn last_name<'src>(node: Node<'_>, source: &'src [u8]) -> Option<&'src str> {
    let binding = match node.kind() {
        "identifier" | "type_identifier" | "self" => node,
        "use_as_clause" => node.child_by_field_name("alias")?,
        "scoped_identifier" => node.child_by_field_name("name")?,
        _ => return None,
    };
    binding.utf8_text(source).ok()
}

impl Syntax<'_> {
    pub(in crate::call_order) fn taken_name_at(&self, name: &str, at: usize) -> bool {
        self.taken_name(name) || self.bindings.projected_binding(name, at, false).is_some()
    }

    pub(in crate::call_order) fn projected_key(
        &self,
        name: &str,
        at: usize,
        defining: bool,
    ) -> Option<String> {
        let start = self.bindings.projected_binding(name, at, defining)?;
        Some(format!("{name}#{start}"))
    }

    pub(in crate::call_order) fn stable_boolean(&self, name: &str, at: usize) -> Option<bool> {
        if self.bindings.projected_binding(name, at, false).is_some() {
            return None;
        }
        self.bindings.stable_boolean(name, at)
    }

    pub(in crate::call_order) fn stable_identity(
        &self,
        name: &str,
        at: usize,
    ) -> Option<IdentityOperand> {
        if self.bindings.projected_binding(name, at, false).is_some() {
            return None;
        }
        self.bindings
            .stable_identity(name, at)
            .map(|value| self.scope_identity(value))
    }

    pub(super) fn scope_identity(&self, value: IdentityOperand) -> IdentityOperand {
        match value {
            IdentityOperand::Binding(name) => IdentityOperand::Scoped {
                name,
                owner: self.owner,
                unit: self.unit,
            },
            value => value,
        }
    }
}

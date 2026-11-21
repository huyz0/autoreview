use tree_sitter::Node;

use super::{attributes, children, descendants, identifier, Path, Scope, Source};

impl<'tree, 'text> Source<'tree, 'text> {
    pub(super) fn path(&self, node: Node<'_>) -> Option<Path> {
        match node.kind() {
            "generic_function" => self.path(node.child_by_field_name("function")?),
            "generic_type" => self.path(node.child_by_field_name("type")?),
            "scoped_identifier" | "scoped_type_identifier" => {
                let name = node.child_by_field_name("name")?;
                let mut path = match node.child_by_field_name("path") {
                    Some(prefix) => self.path(prefix)?,
                    None => Path {
                        absolute: true,
                        parts: Vec::new(),
                    },
                };
                path.parts.push(identifier(self.text(name)).to_owned());
                Some(path)
            }
            "identifier" | "type_identifier" | "crate" | "self" | "super" => Some(Path {
                absolute: false,
                parts: vec![identifier(self.text(node)).to_owned()],
            }),
            _ => None,
        }
    }

    pub(super) fn index_scope(&self, node: Node<'tree>) -> Scope<'tree> {
        let mut scope = Scope::default();
        for child in children(node) {
            match child.kind() {
                "use_declaration" => {
                    if let Some(argument) = child.child_by_field_name("argument") {
                        let conditional = attributes(child).any(|attr| {
                            children(attr)
                                .next()
                                .is_some_and(|name| matches!(self.text(name), "cfg" | "cfg_attr"))
                        });
                        self.index_import(argument, Path::empty(), conditional, &mut scope);
                    }
                }
                "let_declaration" => {
                    if let Some(pattern) = child.child_by_field_name("pattern") {
                        for value in
                            descendants(pattern).filter(|value| value.kind() == "identifier")
                        {
                            scope
                                .locals
                                .entry(identifier(self.text(value)).to_owned())
                                .or_default()
                                .push(child);
                        }
                    }
                }
                "function_item"
                | "struct_item"
                | "enum_item"
                | "type_item"
                | "mod_item"
                | "trait_item"
                | "union_item"
                | "const_item"
                | "static_item"
                | "macro_definition"
                | "extern_crate_declaration" => {
                    if matches!(child.kind(), "extern_crate_declaration" | "mod_item")
                        && attributes(child).any(|attr| {
                            children(attr)
                                .next()
                                .is_some_and(|name| self.text(name) == "macro_use")
                        })
                    {
                        scope.opaque_macros = true;
                    }
                    if let Some(name) = child
                        .child_by_field_name("alias")
                        .or_else(|| child.child_by_field_name("name"))
                    {
                        scope.items.insert(identifier(self.text(name)).to_owned());
                    }
                }
                _ => {}
            }
        }
        scope
    }

    fn index_import(
        &self,
        node: Node<'_>,
        mut prefix: Path,
        conditional: bool,
        scope: &mut Scope<'tree>,
    ) {
        match node.kind() {
            "use_list" => {
                for child in children(node) {
                    self.index_import(child, prefix.clone(), conditional, scope);
                }
            }
            "scoped_use_list" => {
                if let Some(path) = node
                    .child_by_field_name("path")
                    .and_then(|path| self.path(path))
                {
                    prefix = prefix.join(path);
                } else {
                    prefix.absolute = true;
                }
                if let Some(list) = node.child_by_field_name("list") {
                    self.index_import(list, prefix, conditional, scope);
                }
            }
            "use_wildcard" => scope.glob = true,
            _ => {
                let (leaf, alias) = if node.kind() == "use_as_clause" {
                    (
                        node.child_by_field_name("path"),
                        node.child_by_field_name("alias"),
                    )
                } else {
                    (Some(node), None)
                };
                let Some(path) = leaf.and_then(|leaf| self.path(leaf)) else {
                    return;
                };
                if path.parts != ["self"] || prefix.parts.is_empty() {
                    prefix = prefix.join(path);
                }
                let name = alias
                    .map(|alias| identifier(self.text(alias)).to_owned())
                    .or_else(|| prefix.parts.last().cloned());
                if let Some(name) = name {
                    scope
                        .imports
                        .entry(name)
                        .and_modify(|present| *present = None)
                        .or_insert_with(|| (!conditional).then_some(prefix));
                }
            }
        }
    }
}

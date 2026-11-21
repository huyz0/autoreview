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
                "use_declaration" => self.index_use(child, &mut scope),
                "let_declaration" => self.index_local(child, &mut scope),
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
                | "extern_crate_declaration" => self.index_item(child, &mut scope),
                _ => {}
            }
        }
        scope
    }

    fn index_use(&self, item: Node<'tree>, scope: &mut Scope<'tree>) {
        if let Some(argument) = item.child_by_field_name("argument") {
            let conditional = self.has_attribute(item, &["cfg", "cfg_attr"]);
            self.index_import(argument, Path::empty(), conditional, scope);
        }
    }

    fn index_local(&self, declaration: Node<'tree>, scope: &mut Scope<'tree>) {
        let Some(pattern) = declaration.child_by_field_name("pattern") else {
            return;
        };
        for value in descendants(pattern).filter(|value| value.kind() == "identifier") {
            scope
                .locals
                .entry(identifier(self.text(value)).to_owned())
                .or_default()
                .push(declaration);
        }
    }

    fn index_item(&self, item: Node<'tree>, scope: &mut Scope<'tree>) {
        if matches!(item.kind(), "extern_crate_declaration" | "mod_item")
            && self.has_attribute(item, &["macro_use"])
        {
            scope.opaque_macros = true;
        }
        if let Some(name) = item
            .child_by_field_name("alias")
            .or_else(|| item.child_by_field_name("name"))
        {
            scope.items.insert(identifier(self.text(name)).to_owned());
        }
    }

    fn has_attribute(&self, node: Node<'_>, names: &[&str]) -> bool {
        attributes(node).any(|attribute| {
            children(attribute)
                .next()
                .is_some_and(|name| names.contains(&self.text(name)))
        })
    }

    fn index_import(
        &self,
        node: Node<'_>,
        prefix: Path,
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
                if let Some((list, prefix)) = self.scoped_import(node, prefix) {
                    self.index_import(list, prefix, conditional, scope);
                }
            }
            "use_wildcard" => scope.glob = true,
            _ => self.index_import_leaf(node, prefix, conditional, scope),
        }
    }

    fn scoped_import<'node>(
        &self,
        node: Node<'node>,
        mut prefix: Path,
    ) -> Option<(Node<'node>, Path)> {
        let list = node.child_by_field_name("list")?;
        if let Some(path) = node
            .child_by_field_name("path")
            .and_then(|path| self.path(path))
        {
            prefix = prefix.join(path);
        } else {
            prefix.absolute = true;
        }
        Some((list, prefix))
    }

    fn index_import_leaf(
        &self,
        node: Node<'_>,
        mut prefix: Path,
        conditional: bool,
        scope: &mut Scope<'tree>,
    ) {
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

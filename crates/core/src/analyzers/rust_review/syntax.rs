use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

pub(in crate::analyzers) fn children(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .collect::<Vec<_>>()
        .into_iter()
}

pub(in crate::analyzers) fn descendants(root: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut pending = vec![root];
    std::iter::from_fn(move || {
        let next = pending.pop()?;
        let mut cursor = next.walk();
        let start = pending.len();
        pending.extend(next.named_children(&mut cursor));
        pending[start..].reverse();
        Some(next)
    })
}

pub(super) fn ancestors(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    std::iter::successors(node.parent(), |parent| parent.parent())
}

pub(in crate::analyzers) fn is_comment(node: Node<'_>) -> bool {
    matches!(node.kind(), "line_comment" | "block_comment")
}

pub(super) fn is_execution_boundary(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "function_item"
            | "async_block"
            | "closure_expression"
            | "const_block"
            | "const_item"
            | "static_item"
            | "gen_block"
    )
}

pub(super) fn attributes(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    std::iter::successors(node.prev_named_sibling(), |prev| prev.prev_named_sibling())
        .take_while(|prev| prev.kind() == "attribute_item" || is_comment(*prev))
        .filter(|prev| prev.kind() == "attribute_item")
        .filter_map(|item| children(item).find(|child| child.kind() == "attribute"))
}

pub(super) fn arguments(call: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    call.child_by_field_name("arguments")
        .into_iter()
        .flat_map(children)
        .filter(|node| !is_comment(*node))
}

pub(super) fn method_call(call: Node<'_>) -> Option<(Node<'_>, Node<'_>)> {
    if call.kind() != "call_expression" {
        return None;
    }
    let function = call.child_by_field_name("function")?;
    let function = if function.kind() == "generic_function" {
        function.child_by_field_name("function")?
    } else {
        function
    };
    if function.kind() != "field_expression" {
        return None;
    }
    Some((
        function.child_by_field_name("value")?,
        function.child_by_field_name("field")?,
    ))
}

fn is_module_boundary(node: Node<'_>) -> bool {
    node.kind() == "declaration_list"
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "mod_item")
}

#[derive(Clone)]
struct Path {
    absolute: bool,
    parts: Vec<String>,
}

impl Path {
    fn empty() -> Self {
        Self {
            absolute: false,
            parts: Vec::new(),
        }
    }

    fn join(mut self, suffix: Path) -> Self {
        if suffix.absolute {
            return suffix;
        }
        self.parts.extend(suffix.parts);
        self
    }
}

#[derive(Default)]
struct Scope<'tree> {
    imports: HashMap<String, Option<Path>>,
    items: HashSet<String>,
    locals: HashMap<String, Vec<Node<'tree>>>,
    glob: bool,
    opaque_macros: bool,
}

pub(in crate::analyzers) struct Source<'tree, 'text> {
    input: &'text str,
    scopes: HashMap<usize, Scope<'tree>>,
    defined_macros: HashSet<&'text str>,
}

impl<'tree, 'text> Source<'tree, 'text> {
    pub(in crate::analyzers) fn new(root: Node<'tree>, input: &'text str) -> Self {
        let mut source = Self {
            input,
            scopes: HashMap::new(),
            defined_macros: HashSet::new(),
        };
        for node in descendants(root) {
            match node.kind() {
                "source_file" | "block" | "declaration_list" => {
                    source.scopes.insert(node.id(), source.index_scope(node));
                }
                "macro_definition" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        source.defined_macros.insert(identifier(source.text(name)));
                    }
                }
                _ => {}
            }
        }
        source
    }

    pub(in crate::analyzers) fn text(&self, node: Node<'_>) -> &'text str {
        &self.input[node.byte_range()]
    }

    fn path(&self, node: Node<'_>) -> Option<Path> {
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

    fn index_scope(&self, node: Node<'tree>) -> Scope<'tree> {
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

    pub(super) fn external_path(&self, node: Node<'tree>) -> Option<String> {
        let mut reference = self.path(node)?;
        let head = reference.parts.first()?;
        if reference.absolute {
            return known_crate(head).then(|| reference.parts.join("::"));
        }
        let mut locals_visible = true;
        for scope_node in ancestors(node) {
            if self.binds_type_parameter(scope_node, head)
                || locals_visible && self.binds_in_context(scope_node, node, head)
            {
                return None;
            }
            if scope_node.kind() == "function_item" {
                locals_visible = false;
            }
            if let Some(scope) = self.scopes.get(&scope_node.id()) {
                if scope.items.contains(head)
                    || locals_visible && visible_local(scope, head, node).is_some()
                {
                    return None;
                }
                if let Some(imported) = scope.imports.get(head) {
                    let imported = imported.as_ref()?;
                    let root = imported.parts.first()?;
                    if !known_crate(root)
                        || !imported.absolute && self.crate_root_is_ambiguous(scope_node, root)
                    {
                        return None;
                    }
                    reference.parts.splice(..1, imported.parts.iter().cloned());
                    return Some(reference.parts.join("::"));
                }
                if scope.glob {
                    return None;
                }
            }
            if is_module_boundary(scope_node) {
                break;
            }
        }
        (reference.parts.len() > 1 && known_crate(head)).then(|| reference.parts.join("::"))
    }

    pub(in crate::analyzers) fn standard_macro(&self, node: Node<'tree>, names: &[&str]) -> bool {
        // Textual macros can shadow imports, and an exported definition
        // can enter scope from a nested module.
        if node.kind() == "identifier"
            && (self.defined_macros.contains(identifier(self.text(node)))
                || ancestors(node).any(|parent| {
                    self.scopes
                        .get(&parent.id())
                        .is_some_and(|scope| scope.opaque_macros)
                }))
        {
            return false;
        }
        if let Some(path) = self.external_path(node) {
            return path
                .strip_prefix("std::")
                .or_else(|| path.strip_prefix("core::"))
                .is_some_and(|name| names.contains(&name));
        }
        if node.kind() != "identifier" || !names.contains(&self.text(node)) {
            return false;
        }
        // Textual macro scope can reach nested modules.
        self.unshadowed_prelude(node, self.text(node))
    }

    pub(super) fn unshadowed_prelude(&self, node: Node<'tree>, name: &str) -> bool {
        matches!(node.kind(), "identifier" | "type_identifier")
            && self.text(node) == name
            && !ancestors(node).any(|parent| {
                self.binds_type_parameter(parent, name)
                    || self.scopes.get(&parent.id()).is_some_and(|scope| {
                        scope.items.contains(name)
                            || scope.imports.contains_key(name)
                            || scope.glob
                            || scope.opaque_macros
                    })
            })
    }

    fn binds_type_parameter(&self, scope: Node<'tree>, name: &str) -> bool {
        scope
            .child_by_field_name("type_parameters")
            .into_iter()
            .flat_map(children)
            .filter_map(|parameter| parameter.child_by_field_name("name"))
            .any(|parameter| identifier(self.text(parameter)) == name)
    }

    fn crate_root_is_ambiguous(&self, scope_node: Node<'tree>, name: &str) -> bool {
        for node in std::iter::once(scope_node).chain(ancestors(scope_node)) {
            if self.scopes.get(&node.id()).is_some_and(|scope| {
                scope.items.contains(name)
                    || scope.glob
                    || scope
                        .imports
                        .get(name)
                        .is_some_and(|root| root.as_ref().is_none_or(|root| root.parts != [name]))
            }) {
                return true;
            }
            if is_module_boundary(node) {
                break;
            }
        }
        false
    }

    pub(super) fn local_binding(&self, name: Node<'tree>) -> Option<Node<'tree>> {
        self.declaration(name)
            .filter(|binding| binding.kind() == "let_declaration")
    }

    pub(super) fn declaration(&self, name: Node<'tree>) -> Option<Node<'tree>> {
        if name.kind() != "identifier" {
            return None;
        }
        let text = identifier(self.text(name));
        for scope_node in ancestors(name) {
            if let Some(scope) = self.scopes.get(&scope_node.id()) {
                if let Some(binding) = visible_local(scope, text, name) {
                    return binding
                        .child_by_field_name("pattern")
                        .filter(|pattern| pattern.kind() == "identifier")
                        .map(|_| binding);
                }
            }
            if scope_node.kind() == "function_item" {
                return scope_node
                    .child_by_field_name("parameters")
                    .into_iter()
                    .flat_map(children)
                    .find(|param| {
                        param.child_by_field_name("pattern").is_some_and(|pattern| {
                            pattern.kind() == "identifier" && identifier(self.text(pattern)) == text
                        })
                    });
            }
            if self.binds_in_context(scope_node, name, text)
                || is_execution_boundary(scope_node)
                || is_module_boundary(scope_node)
            {
                return None;
            }
        }
        None
    }

    pub(super) fn references_iteration_binding(
        &self,
        node: Node<'tree>,
        iteration: Node<'tree>,
    ) -> bool {
        if node.kind() != "identifier" {
            return false;
        }
        let name = identifier(self.text(node));
        for scope_node in ancestors(node) {
            if scope_node == iteration {
                return scope_node
                    .child_by_field_name("pattern")
                    .is_some_and(|pattern| {
                        pattern.kind() == "identifier" && identifier(self.text(pattern)) == name
                    });
            }
            if self
                .scopes
                .get(&scope_node.id())
                .is_some_and(|scope| visible_local(scope, name, node).is_some())
                || self.binds_in_context(scope_node, node, name)
                || is_execution_boundary(scope_node)
                || is_module_boundary(scope_node)
            {
                return false;
            }
        }
        false
    }

    fn binds_in_context(&self, scope: Node<'tree>, anchor: Node<'tree>, name: &str) -> bool {
        let matches = |pattern| {
            descendants(pattern)
                .any(|part| part.kind() == "identifier" && identifier(self.text(part)) == name)
        };
        match scope.kind() {
            "function_item" | "closure_expression" => scope
                .child_by_field_name("parameters")
                .is_some_and(|params| {
                    children(params)
                        .any(|param| matches(param.child_by_field_name("pattern").unwrap_or(param)))
                }),
            "for_expression" => {
                scope
                    .child_by_field_name("body")
                    .is_some_and(|body| contains(body, anchor))
                    && scope.child_by_field_name("pattern").is_some_and(matches)
            }
            "match_arm" => scope.child_by_field_name("pattern").is_some_and(matches),
            "if_expression" | "while_expression" => {
                let in_body = ["consequence", "body"]
                    .into_iter()
                    .find_map(|field| scope.child_by_field_name(field))
                    .is_some_and(|body| contains(body, anchor));
                scope
                    .child_by_field_name("condition")
                    .is_some_and(|condition| {
                        (in_body || contains(condition, anchor))
                            && descendants(condition)
                                .filter(|part| {
                                    part.kind() == "let_condition"
                                        && part.end_byte() <= anchor.start_byte()
                                })
                                .filter_map(|part| part.child_by_field_name("pattern"))
                                .any(matches)
                    })
            }
            _ => false,
        }
    }
}

fn contains(outer: Node<'_>, inner: Node<'_>) -> bool {
    outer.start_byte() <= inner.start_byte() && inner.end_byte() <= outer.end_byte()
}

fn visible_local<'tree>(scope: &Scope<'tree>, name: &str, before: Node<'_>) -> Option<Node<'tree>> {
    scope
        .locals
        .get(name)?
        .iter()
        .rev()
        .copied()
        .find(|binding| binding.end_byte() <= before.start_byte())
}

fn identifier(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

fn known_crate(name: &str) -> bool {
    matches!(
        name,
        "std"
            | "core"
            | "alloc"
            | "tokio"
            | "sqlx"
            | "diesel"
            | "rusqlite"
            | "mockall"
            | "reqwest"
            | "tracing"
            | "tempfile"
            | "rustls"
            | "secrecy"
            | "axum"
            | "actix_web"
    )
}

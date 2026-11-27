mod local_values;

use local_values::const_declarations;
use tree_sitter::Node;

use super::{field_text, first_argument, imports_from, BindingScopes};
use crate::analyzers::framework_checks::{children, descendants, text};

pub(in crate::analyzers::framework_checks) struct NodeCalls<'tree, 'source> {
    root: Node<'tree>,
    source: &'source str,
    scopes: BindingScopes<'tree, 'source>,
}

impl<'t, 's> NodeCalls<'t, 's> {
    pub fn new(root: Node<'t>, source: &'s str) -> Self {
        Self {
            root,
            source,
            scopes: BindingScopes::new(root, source),
        }
    }

    pub fn resolve<'m>(&self, call: Node<'t>, modules: &[&'m str]) -> Option<(&'m str, &'s str)> {
        let callee = match call.kind() {
            "call_expression" => call.child_by_field_name("function")?,
            "new_expression" => call.child_by_field_name("constructor")?,
            _ => return None,
        };
        let callee = if callee.kind() == "parenthesized_expression" {
            children(callee).find(|node| node.kind() != "comment")?
        } else {
            callee
        };
        match callee.kind() {
            "member_expression" => {
                let member = field_text(callee, "property", self.source)?;
                (member != "default").then_some(())?;
                let module =
                    self.namespace_module(callee.child_by_field_name("object")?, modules)?;
                Some((module, member))
            }
            "identifier" => self.imported_function(callee, modules),
            "call_expression" => Some((self.required_module(callee, modules)?, "default")),
            _ => None,
        }
    }

    fn imported_function<'m>(
        &self,
        target: Node<'t>,
        modules: &[&'m str],
    ) -> Option<(&'m str, &'s str)> {
        let local = text(target, self.source);
        if inside_named_expression(target, local, self.source) {
            return None;
        }
        if let Some(scope) = self.scopes.visible(local, target) {
            return self.local_require(target, scope, modules);
        }
        modules.iter().find_map(|&module| {
            self.runtime_imports(module)
                .flat_map(descendants)
                .filter_map(|node| imported_name(node, self.source))
                .find_map(|(name, binding)| (local == binding).then_some((module, name)))
        })
    }

    fn local_require<'m>(
        &self,
        use_site: Node<'t>,
        scope: Node<'t>,
        modules: &[&'m str],
    ) -> Option<(&'m str, &'s str)> {
        let name = text(use_site, self.source);
        let mut bindings = const_declarations(scope).filter_map(|decl| {
            let pattern = decl.child_by_field_name("name")?;
            let member = require_binding(pattern, name, self.source)?;
            let value = decl.child_by_field_name("value")?;
            (value.end_byte() <= use_site.start_byte()).then_some((member, value))
        });
        let (member, value) = bindings.next()?;
        (bindings.next().is_none()).then_some(())?;
        Some((self.required_module(value, modules)?, member))
    }

    fn namespace_module<'m>(&self, object: Node<'t>, modules: &[&'m str]) -> Option<&'m str> {
        if object.kind() == "call_expression" {
            return self.required_module(object, modules);
        }
        let local = text(object, self.source);
        if object.kind() != "identifier" || inside_named_expression(object, local, self.source) {
            return None;
        }
        if let Some(scope) = self.scopes.visible(local, object) {
            let (module, member) = self.local_require(object, scope, modules)?;
            return (member == "default").then_some(module);
        }
        modules.iter().find_map(|&module| {
            self.runtime_imports(module)
                .flat_map(descendants)
                .any(|node| match node.kind() {
                    "namespace_import" => {
                        children(node).any(|binding| text(binding, self.source) == local)
                    }
                    _ => imported_name(node, self.source) == Some(("default", local)),
                })
                .then_some(module)
        })
    }

    fn required_module<'m>(&self, call: Node<'t>, modules: &[&'m str]) -> Option<&'m str> {
        if call.kind() != "call_expression"
            || field_text(call, "function", self.source) != Some("require")
            || self.scopes.visible("require", call).is_some()
            || inside_named_expression(call, "require", self.source)
            || imports_binding(self.root, "require", self.source)
        {
            return None;
        }
        let specifier = string_literal(first_argument(call)?, self.source)?;
        modules.iter().copied().find(|module| *module == specifier)
    }

    fn runtime_imports<'a>(
        &'a self,
        specifier: &'a str,
    ) -> impl Iterator<Item = Node<'t>> + use<'t, 'a> {
        imports_from(self.root, self.source, specifier).filter(|import| !has_type_modifier(*import))
    }

    pub fn unshadowed(&self, identifier: Node<'t>) -> bool {
        let name = text(identifier, self.source);
        identifier.kind() == "identifier"
            && self.scopes.visible(name, identifier).is_none()
            && !inside_named_expression(identifier, name, self.source)
            && !imports_binding(self.root, name, self.source)
    }

    pub fn binding_scope(&self, binding: Node<'t>) -> Option<Node<'t>> {
        self.scopes.visible(text(binding, self.source), binding)
    }

    pub fn member(&self, node: Node<'t>) -> Option<(Node<'t>, &'s str)> {
        (node.kind() == "member_expression").then_some(())?;
        Some((
            node.child_by_field_name("object")?,
            field_text(node, "property", self.source)?,
        ))
    }
}

fn has_type_modifier(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    let result = node.children(&mut cursor).any(|part| part.kind() == "type");
    result
}

fn imported_name<'s>(node: Node<'_>, source: &'s str) -> Option<(&'s str, &'s str)> {
    match node.kind() {
        "identifier" if node.parent()?.kind() == "import_clause" => {
            Some(("default", text(node, source)))
        }
        "import_specifier" if !has_type_modifier(node) => {
            let name = field_text(node, "name", source)?;
            Some((name, field_text(node, "alias", source).unwrap_or(name)))
        }
        _ => None,
    }
}

fn destructured_member<'s>(pattern: Node<'_>, binding: &str, source: &'s str) -> Option<&'s str> {
    if pattern.kind() == "shorthand_property_identifier_pattern" {
        return (text(pattern, source) == binding).then_some(text(pattern, source));
    }
    let target = pattern.child_by_field_name("value")?;
    if pattern.kind() != "pair_pattern"
        || target.kind() != "identifier"
        || text(target, source) != binding
    {
        return None;
    }
    let key = pattern.child_by_field_name("key")?;
    if key.kind() == "property_identifier" {
        Some(text(key, source))
    } else {
        string_literal(key, source)
    }
}

fn require_binding<'s>(pattern: Node<'_>, name: &str, source: &'s str) -> Option<&'s str> {
    match pattern.kind() {
        "identifier" if text(pattern, source) == name => Some("default"),
        "object_pattern" => children(pattern)
            .find_map(|part| destructured_member(part, name, source))
            .filter(|member| *member != "default"),
        _ => None,
    }
}

pub(in crate::analyzers::framework_checks) fn property<'t>(
    object: Node<'t>,
    name: &str,
    source: &str,
) -> Option<(Node<'t>, Node<'t>)> {
    if object.kind() != "object" {
        return None;
    }
    // The nearest write wins. A spread or unknown key may replace any earlier value.
    for member in children(object)
        .rev()
        .filter(|member| member.kind() != "comment")
    {
        let key = match member.kind() {
            "pair" => member.child_by_field_name("key")?,
            "method_definition" => member.child_by_field_name("name")?,
            "shorthand_property_identifier" => member,
            _ => return None,
        };
        let spelling = text(key, source);
        if key.kind() == "computed_property_name" || spelling.contains('\\') {
            return None;
        }
        if spelling == name || string_literal(key, source) == Some(name) {
            return Some((member, member.child_by_field_name("value")?));
        }
    }
    None
}

pub(in crate::analyzers::framework_checks) fn string_literal<'s>(
    node: Node<'_>,
    source: &'s str,
) -> Option<&'s str> {
    if node.kind() != "string" || node.named_child_count() != 1 {
        return None;
    }
    let content = node.named_child(0)?;
    (content.kind() == "string_fragment").then(|| text(content, source))
}

pub(in crate::analyzers::framework_checks) fn inside_named_expression(
    node: Node<'_>,
    expected: &str,
    source: &str,
) -> bool {
    std::iter::successors(node.parent(), |ancestor| ancestor.parent()).any(|ancestor| {
        matches!(
            ancestor.kind(),
            "function_expression" | "generator_function" | "class"
        ) && ancestor
            .child_by_field_name("name")
            .is_some_and(|name| text(name, source) == expected)
    })
}

pub(in crate::analyzers::framework_checks) fn imports_binding(
    root: Node<'_>,
    expected: &str,
    source: &str,
) -> bool {
    children(root)
        .filter(|node| node.kind() == "import_statement")
        .flat_map(descendants)
        .any(|node| {
            imported_name(node, source).is_some_and(|(_, binding)| binding == expected)
                || matches!(node.kind(), "namespace_import" | "import_require_clause")
                    && children(node).any(|binding| {
                        binding.kind() == "identifier" && text(binding, source) == expected
                    })
        })
}

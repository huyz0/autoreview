use tree_sitter::Node;

use super::{field_text, first_argument, imports_from, is_function, BindingScopes};
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
            return self
                .const_required_module(scope, local, target, modules)
                .map(|module| (module, "default"))
                .or_else(|| self.destructured_require(scope, local, target, modules));
        }
        modules.iter().find_map(|&module| {
            self.runtime_imports(module)
                .flat_map(descendants)
                .filter_map(|node| imported_name(node, self.source))
                .find_map(|(name, binding)| (local == binding).then_some((module, name)))
        })
    }

    fn destructured_require<'m>(
        &self,
        scope: Node<'t>,
        name: &str,
        use_site: Node<'t>,
        modules: &[&'m str],
    ) -> Option<(&'m str, &'s str)> {
        let mut bindings = const_declarations(scope).filter_map(|decl| {
            let pattern = decl
                .child_by_field_name("name")
                .filter(|part| part.kind() == "object_pattern")?;
            let member =
                children(pattern).find_map(|part| destructured_member(part, name, self.source))?;
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
            return self.const_required_module(scope, local, object, modules);
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

    fn const_required_module<'m>(
        &self,
        scope: Node<'t>,
        name: &str,
        use_site: Node<'t>,
        modules: &[&'m str],
    ) -> Option<&'m str> {
        let (_, value) = const_value(scope, name, self.source)?;
        (value.end_byte() <= use_site.start_byte()).then_some(())?;
        self.required_module(value, modules)
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

    pub fn options_object(&self, argument: Node<'t>, call: Node<'t>) -> Option<Node<'t>> {
        let object = if argument.kind() == "identifier" {
            self.local_options(argument, call)?
        } else {
            argument
        };
        (object.kind() == "object").then_some(object)
    }

    fn local_options(&self, options: Node<'t>, call: Node<'t>) -> Option<Node<'t>> {
        let name = text(options, self.source);
        if inside_named_expression(options, name, self.source) {
            return None;
        }
        let scope = self.scopes.visible(name, options)?;
        let (declaration, object) = const_value(scope, name, self.source)?;
        let statement = containing_statement(call, scope)?;
        if declaration.end_byte() > statement.start_byte()
            || descendants(scope).any(|node| {
                node.start_byte() < options.start_byte()
                    && !declaration.byte_range().contains(&node.start_byte())
                    && matches!(node.kind(), "identifier" | "shorthand_property_identifier")
                    && text(node, self.source) == name
            })
        {
            return None;
        }
        Some(object)
    }
}

fn containing_statement<'t>(call: Node<'t>, scope: Node<'t>) -> Option<Node<'t>> {
    std::iter::successors(Some(call), |node| node.parent())
        .take_while(|node| {
            !is_function(*node)
                && !matches!(
                    node.kind(),
                    "statement_block"
                        | "if_statement"
                        | "for_statement"
                        | "for_in_statement"
                        | "while_statement"
                        | "do_statement"
                        | "switch_statement"
                )
        })
        .find(|node| node.parent() == Some(scope))
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

fn const_value<'t>(scope: Node<'t>, name: &str, source: &str) -> Option<(Node<'t>, Node<'t>)> {
    let mut declarations =
        const_declarations(scope).filter(|decl| field_text(*decl, "name", source) == Some(name));
    let decl = declarations.next()?;
    (declarations.next().is_none()).then_some((decl, decl.child_by_field_name("value")?))
}

fn const_declarations(scope: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    children(scope)
        .filter(|statement| {
            statement.kind() == "lexical_declaration"
                && statement
                    .child(0)
                    .is_some_and(|token| token.kind() == "const")
        })
        .flat_map(children)
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
